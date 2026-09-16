//! V2 shared-memory connection used by all Binary API RPC services.

use std::collections::HashMap;
use std::mem::size_of;
use std::num::NonZeroU32;
use std::path::{Path, PathBuf};
use std::ptr::NonNull;
use std::time::{Duration, Instant};

use hammer_binary_api_protocol::api::Api;
use hammer_binary_api_protocol::codec;
use hammer_binary_api_protocol::memclnt::{
    MEMCLNT_CREATE_V2_ID, MEMCLNT_CREATE_V2_REPLY_ID, MemclntCreateV2, MemclntCreateV2Reply,
    MemclntDelete, MemclntDeleteReply, MemclntKeepalive, MemclntKeepaliveReply,
};
use hammer_binary_api_protocol::memory::{MemoryError, MsgBuf, ShmemHeader};
use hammer_binary_api_protocol::table::deserialize_message_table;
use hammer_binary_api_protocol::value::FixedString;
use hammer_shmem::queue::{
    SvmQueue, SvmQueueConditionalWait, SvmQueueConfig, SvmQueueError, SvmQueueOperation,
};
use hammer_shmem::region::{SvmRegion, SvmRegionError};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const DISCONNECT_TIMEOUT: Duration = Duration::from_secs(2);
const RECEIVE_INTERVAL: Duration = Duration::from_micros(400);

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("Binary API client is already connected")]
    AlreadyConnected,
    #[error("Binary API client is not connected")]
    NotConnected,
    #[error("connect to Binary API segment `{path}` did not become ready in time")]
    ConnectTimeout { path: PathBuf },
    #[error("shared API region is unavailable")]
    Region {
        #[source]
        source: SvmRegionError,
    },
    #[error("shared API memory layout: {source}")]
    Memory {
        #[source]
        source: MemoryError,
    },
    #[error("shared API queue: {source}")]
    Queue {
        #[source]
        source: SvmQueueError,
    },
    #[error("Binary API codec: {source}")]
    Codec {
        #[source]
        source: codec::Error,
    },
    #[error("create-v2 was rejected with response {response}")]
    CreateRejected { response: i32 },
    #[error("required API message `{name_crc}` is unavailable")]
    MessageUnavailable { name_crc: &'static str },
    #[error("request declares no service reply")]
    RequestHasNoReply,
    #[error("request expects reply `{expected}` but invoked reply type is `{actual}`")]
    ReplyTypeMismatch {
        expected: &'static str,
        actual: &'static str,
    },
    #[error("reply for context {context} did not arrive")]
    ResponseTimeout { context: u32 },
    #[error("unexpected API message id {id} while waiting for {expected}")]
    UnexpectedMessage { id: u16, expected: u16 },
    #[error("reply context mismatch: expected {expected}, received {received}")]
    ContextMismatch { expected: u32, received: u32 },
    #[error("delete reply did not arrive for client index {client_index}")]
    DisconnectTimeout { client_index: u32 },
}

/// One opaque Binary API connection. The public type selects the backend
/// internally; no public transport trait or backend wrapper is exposed.
pub struct Client {
    region: SvmRegion,
    header: NonNull<ShmemHeader>,
    input_queue: NonNull<SvmQueue>,
    client_index: Option<u32>,
    message_ids: HashMap<String, u16>,
    context_counter: u32,
    handle_keepalives: bool,
}

impl Client {
    pub async fn connect(
        name: &str,
        api_segment: &Path,
        response_queue_size: NonZeroU32,
        handle_keepalives: bool,
    ) -> Result<Self, Error> {
        let deadline = Instant::now() + CONNECT_TIMEOUT;
        let region = loop {
            match SvmRegion::attach(api_segment) {
                Ok(region) => break region,
                Err(source) if retryable(&source) && Instant::now() < deadline => {
                    tokio::time::sleep(RECEIVE_INTERVAL).await;
                }
                Err(source) => return Err(Error::Region { source }),
            }
        };

        let header_address = loop {
            if let Some(header) = region.user_context() {
                break header;
            }
            if Instant::now() >= deadline {
                return Err(Error::ConnectTimeout {
                    path: api_segment.to_path_buf(),
                });
            }
            tokio::time::sleep(RECEIVE_INTERVAL).await;
        };
        let header = unsafe { ShmemHeader::validate(&region, header_address) }
            .map_err(|source| Error::Memory { source })?;

        let queue = allocate_client_queue(&region, response_queue_size)?;
        let mut client = Self {
            region,
            header,
            input_queue: queue,
            client_index: None,
            message_ids: HashMap::new(),
            context_counter: 0,
            handle_keepalives,
        };

        let request = MemclntCreateV2 {
            id: MEMCLNT_CREATE_V2_ID,
            context: 0,
            ctx_quota: 0,
            input_queue: queue.as_ptr().addr() as u64,
            name: FixedString::from_str(name),
            api_versions: [0; 8],
            keepalive: handle_keepalives,
        };
        client.send_message(&request).await?;

        let reply = loop {
            let Some(message) = client.receive(deadline).await? else {
                return Err(Error::ConnectTimeout {
                    path: api_segment.to_path_buf(),
                });
            };
            let id = message_id(&message)?;
            if id != MEMCLNT_CREATE_V2_REPLY_ID {
                unsafe { message.free(client.region.data_heap()) };
                continue;
            }
            let decoded = unsafe { message.decode::<MemclntCreateV2Reply>() };
            unsafe { message.free(client.region.data_heap()) };
            break decoded.map_err(|source| Error::Codec { source })?;
        };
        if reply.response < 0 {
            client.release_queue();
            return Err(Error::CreateRejected {
                response: reply.response,
            });
        }
        let table = client
            .region
            .remaining_from(reply.message_table as usize)
            .ok_or(Error::Memory {
                source: MemoryError::InvalidHeader,
            })?;
        client.message_ids = deserialize_message_table(table)
            .map_err(|source| Error::Codec { source })?
            .into_iter()
            .collect();
        client.client_index = Some(reply.index);
        Ok(client)
    }

    pub fn client_index(&self) -> Option<u32> {
        self.client_index
    }

    pub async fn invoke<Request, Reply>(&mut self, mut request: Request) -> Result<Reply, Error>
    where
        Request: Api,
        Reply: Api,
    {
        let client_index = self.client_index.ok_or(Error::NotConnected)?;
        let service = Request::SERVICE.ok_or(Error::RequestHasNoReply)?;
        let expected = service.reply.ok_or(Error::RequestHasNoReply)?;
        if expected != Reply::NAME {
            return Err(Error::ReplyTypeMismatch {
                expected,
                actual: Reply::NAME,
            });
        }
        let request_id = self
            .message_id(Request::NAME_CRC)
            .ok_or(Error::MessageUnavailable {
                name_crc: Request::NAME_CRC,
            })?;
        let reply_id = self
            .message_id(Reply::NAME_CRC)
            .ok_or(Error::MessageUnavailable {
                name_crc: Reply::NAME_CRC,
            })?;
        let context = self.next_context();
        request.set_request_header(request_id, client_index, context);
        self.send_message(&request).await?;

        let deadline = Instant::now() + CONNECT_TIMEOUT;
        loop {
            let Some(message) = self.receive(deadline).await? else {
                return Err(Error::ResponseTimeout { context });
            };
            let id = message_id(&message)?;
            if id != reply_id {
                unsafe { message.free(self.region.data_heap()) };
                return Err(Error::UnexpectedMessage {
                    id,
                    expected: reply_id,
                });
            }
            let decoded = unsafe { message.decode::<Reply>() };
            unsafe { message.free(self.region.data_heap()) };
            let reply = decoded.map_err(|source| Error::Codec { source })?;
            match reply.context() {
                Some(received) if received == context => return Ok(reply),
                Some(received) => {
                    return Err(Error::ContextMismatch {
                        expected: context,
                        received,
                    });
                }
                None => return Ok(reply),
            }
        }
    }

    pub async fn disconnect(&mut self) -> Result<(), Error> {
        let client_index = self.client_index.ok_or(Error::NotConnected)?;
        let delete_id =
            self.message_id(MemclntDelete::NAME_CRC)
                .ok_or(Error::MessageUnavailable {
                    name_crc: MemclntDelete::NAME_CRC,
                })?;
        let delete_reply_id =
            self.message_id(MemclntDeleteReply::NAME_CRC)
                .ok_or(Error::MessageUnavailable {
                    name_crc: MemclntDeleteReply::NAME_CRC,
                })?;
        let request = MemclntDelete {
            id: delete_id,
            index: client_index,
            handle: 0,
            do_cleanup: false,
        };
        self.send_message(&request).await?;
        let deadline = Instant::now() + DISCONNECT_TIMEOUT;
        loop {
            let Some(message) = self.receive(deadline).await? else {
                return Err(Error::DisconnectTimeout { client_index });
            };
            let id = message_id(&message)?;
            if id != delete_reply_id {
                unsafe { message.free(self.region.data_heap()) };
                continue;
            }
            let decoded = unsafe { message.decode::<MemclntDeleteReply>() };
            unsafe { message.free(self.region.data_heap()) };
            let reply = decoded.map_err(|source| Error::Codec { source })?;
            if reply.response < 0 {
                return Err(Error::CreateRejected {
                    response: reply.response,
                });
            }
            break;
        }
        self.client_index = None;
        self.message_ids.clear();
        self.release_queue();
        Ok(())
    }

    fn message_id(&self, name_crc: &str) -> Option<u16> {
        self.message_ids.get(name_crc).copied()
    }

    fn next_context(&mut self) -> u32 {
        loop {
            self.context_counter = self.context_counter.wrapping_add(1) & 0x7fff_ffff;
            if self.context_counter != 0 {
                return self.context_counter | 0x8000_0000;
            }
        }
    }

    async fn send_message<T: Api>(&self, message: &T) -> Result<(), Error> {
        let payload_len =
            codec::serialized_len(message).map_err(|source| Error::Codec { source })?;
        let mut allocation = unsafe { MsgBuf::allocate(self.region.data_heap(), payload_len) }
            .map_err(|source| Error::Memory { source })?;
        unsafe { allocation.encode(message) }.map_err(|source| Error::Codec { source })?;
        let address = usize::from(&allocation).to_ne_bytes();
        if let Err(source) = unsafe { self.header.as_ref().input_queue() }
            .add(&address, SvmQueueConditionalWait::Nowait)
        {
            if !committed(&source, SvmQueueOperation::Add) {
                unsafe { allocation.free(self.region.data_heap()) };
                return Err(Error::Queue { source });
            }
            tracing::warn!(?source, "request committed despite notification error");
        }
        Ok(())
    }

    async fn receive(&mut self, deadline: Instant) -> Result<Option<MsgBuf>, Error> {
        loop {
            let mut address = [0_u8; size_of::<usize>()];
            match unsafe { self.input_queue.as_ref() }
                .sub(&mut address, SvmQueueConditionalWait::Nowait)
            {
                Ok(()) => {
                    let message = unsafe {
                        MsgBuf::from_address(&self.region, usize::from_ne_bytes(address))
                    }
                    .map_err(|source| Error::Memory { source })?;
                    let id = message_id(&message)?;
                    if self.handle_keepalives
                        && Some(id) == self.message_id(MemclntKeepalive::NAME_CRC)
                    {
                        let decoded = unsafe { message.decode::<MemclntKeepalive>() };
                        unsafe { message.free(self.region.data_heap()) };
                        let request = decoded.map_err(|source| Error::Codec { source })?;
                        let reply_id = self.message_id(MemclntKeepaliveReply::NAME_CRC).ok_or(
                            Error::MessageUnavailable {
                                name_crc: MemclntKeepaliveReply::NAME_CRC,
                            },
                        )?;
                        let reply = MemclntKeepaliveReply {
                            id: reply_id,
                            context: request.client_index,
                            retval: 0,
                        };
                        self.send_message(&reply).await?;
                        continue;
                    }
                    return Ok(Some(message));
                }
                Err(SvmQueueError::Empty) => {
                    if Instant::now() >= deadline {
                        return Ok(None);
                    }
                    tokio::time::sleep(RECEIVE_INTERVAL).await;
                }
                Err(source) => return Err(Error::Queue { source }),
            }
        }
    }

    fn release_queue(&mut self) {
        let heap = self.region.data_heap();
        let queue = unsafe { self.input_queue.as_ref() };
        let config = SvmQueueConfig {
            nels: u32::try_from(queue.capacity()).expect("queue capacity fits u32"),
            elsize: u32::try_from(queue.element_size()).expect("queue element size fits u32"),
            consumer_pid: queue.consumer_pid(),
        };
        let bytes = SvmQueue::size_to_alloc(&config).expect("attached queue geometry is valid");
        let layout = std::alloc::Layout::from_size_align(bytes, 64).expect("queue layout is valid");
        unsafe {
            queue.cleanup();
            heap.deallocate(self.input_queue.cast(), layout);
        }
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        if self.client_index.is_some() {
            tracing::warn!(
                "Binary API client dropped without disconnect; shared state remains server-owned"
            );
        }
    }
}

fn allocate_client_queue(
    region: &SvmRegion,
    response_queue_size: NonZeroU32,
) -> Result<NonNull<SvmQueue>, Error> {
    let config = SvmQueueConfig {
        nels: response_queue_size.get(),
        elsize: u32::try_from(size_of::<usize>()).expect("pointer size fits u32"),
        consumer_pid: std::process::id() as i32,
    };
    let bytes = SvmQueue::size_to_alloc(&config).map_err(|source| Error::Queue { source })?;
    let layout = std::alloc::Layout::from_size_align(bytes, 64).map_err(|_| Error::Queue {
        source: SvmQueueError::LayoutOverflow,
    })?;
    let heap = region.data_heap();
    let queue_address = heap.allocate_zeroed(layout).ok_or_else(|| Error::Queue {
        source: SvmQueueError::LayoutOverflow,
    })?;
    match unsafe { SvmQueue::init(queue_address, &config) } {
        Ok(queue) => Ok(queue),
        Err(source) => {
            unsafe { heap.deallocate(queue_address, layout) };
            Err(Error::Queue { source })
        }
    }
}

fn message_id(message: &MsgBuf) -> Result<u16, Error> {
    let bytes = unsafe { message.as_bytes() }.map_err(|source| Error::Codec { source })?;
    codec::deserialize(bytes).map_err(|source| Error::Codec { source })
}

fn committed(source: &SvmQueueError, operation: SvmQueueOperation) -> bool {
    matches!(source,
        SvmQueueError::SignalAfterCommit { operation: reported, .. }
        | SvmQueueError::EventSignalAfterCommit { operation: reported, .. }
        if *reported == operation
    )
}

fn retryable(source: &SvmRegionError) -> bool {
    matches!(
        source,
        SvmRegionError::Open { source, .. }
            if source.kind() == std::io::ErrorKind::NotFound
                || source.kind() == std::io::ErrorKind::WouldBlock
    ) || matches!(source, SvmRegionError::NotReady)
}
