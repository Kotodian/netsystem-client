//! Read-only stats segment client: socket handoff, mapping, and raw reads.
//!
//! The client knows the segment layout and nothing about any metric family: it
//! lists directory names and returns raw [`MetricValue`]s. Family projections
//! live in their own provider modules.

use std::io;
use std::mem::{MaybeUninit, size_of};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::path::Path;
use std::ptr;
use std::slice;
use std::time::Duration;

use hammer_stats_protocol::protocol::{
    Counter, DirectoryEntry, DirectoryType, MetricValue, PublishedVector, SharedHeader,
};

use crate::error::Error;
use crate::provider::StatsProvider;
use crate::reader::StatsReader;

/// Directory re-reads allowed while writers keep publishing epochs.
const RETRY_LIMIT: usize = 16;
/// How long `connect` waits for the descriptor handoff.
const HANDOFF_TIMEOUT: Duration = Duration::from_secs(10);
/// Symlink hops allowed before the reader reports a cycle.
const SYMLINK_LIMIT: usize = 16;

/// One read-only mapping of the shared stats segment.
///
/// The mapping owns the address range for as long as the client lives and
/// never writes it: the segment is published by the daemon process only.
pub struct StatsClient {
    mapping: SegmentMapping,
}

impl StatsClient {
    /// Connects to a stats segment socket, receives the segment descriptor,
    /// and maps the segment read-only.
    pub fn connect(socket_path: &Path) -> Result<Self, Error> {
        let socket = connect_socket(socket_path)?;
        let segment_fd = receive_segment_fd(&socket)?;
        drop(socket);
        let mapping = SegmentMapping::new(&segment_fd)?;
        Ok(Self { mapping })
    }

    /// Generic projection entry: `P` is monomorphized, so there is no
    /// registry, no `dyn`, and no runtime lookup.
    pub fn report<P: StatsProvider>(&self) -> Result<P::Report, Error> {
        P::report(self)
    }

    /// Every directory name, including free slots and symlinks.
    pub fn names(&self) -> Result<Vec<String>, Error> {
        self.with_published_directory("names", |mapping, _, directory| {
            let mut names = Vec::with_capacity(directory.len());
            for index in 0..directory.len() {
                names.push(read_entry(directory, mapping, index)?.name()?.to_owned());
            }
            Ok(names)
        })
    }

    /// The raw value a directory name publishes.
    ///
    /// A symlink is resolved to its target and cropped to the symlink's
    /// column, like VPP's `copy_data`; it is never returned as a scalar.
    pub fn read(&self, name: &str) -> Result<MetricValue, Error> {
        self.with_published_directory("read", |mapping, base, directory| {
            let Some(index) = find_entry(directory, mapping, name)? else {
                return Err(Error::MetricNotFound {
                    name: name.to_owned(),
                });
            };
            decode_entry(mapping, base, directory, index, name, 0)
        })
    }

    /// Re-reads the published directory until two header reads observe the
    /// same epoch, so a projection never decodes a half-published directory.
    fn with_published_directory<T>(
        &self,
        operation: &'static str,
        decode: impl Fn(&[u8], usize, &PublishedVector) -> Result<T, Error>,
    ) -> Result<T, Error> {
        let mapping = self.mapping.bytes();
        for _ in 0..RETRY_LIMIT {
            let header = read_header(mapping)?;
            header.validate_version()?;
            if header.is_write_in_progress() {
                continue;
            }
            let epoch = header.epoch();
            let base = header.base();
            let directory = PublishedVector::resolve(
                mapping,
                base,
                header.directory_vector(),
                size_of::<DirectoryEntry>(),
            )?;
            let value = decode(mapping, base, &directory)?;
            let end = read_header(mapping)?;
            end.validate_version()?;
            if !end.is_write_in_progress() && end.epoch() == epoch {
                return Ok(value);
            }
        }
        Err(Error::RetryExhausted {
            operation,
            attempts: RETRY_LIMIT,
        })
    }
}

impl StatsReader for StatsClient {
    fn names(&self) -> Result<Vec<String>, Error> {
        StatsClient::names(self)
    }

    fn read(&self, name: &str) -> Result<MetricValue, Error> {
        StatsClient::read(self, name)
    }
}

/// Resolves one directory entry through symlinks and decodes its value.
fn decode_entry(
    mapping: &[u8],
    base: usize,
    directory: &PublishedVector,
    index: usize,
    name: &str,
    depth: usize,
) -> Result<MetricValue, Error> {
    if depth >= SYMLINK_LIMIT {
        return Err(Error::SymlinkCycle {
            name: name.to_owned(),
            depth: SYMLINK_LIMIT,
        });
    }
    let entry = read_entry(directory, mapping, index)?;
    let directory_type = entry.directory_type()?;
    match directory_type {
        DirectoryType::Symlink => {
            let (target, column) = entry.symlink()?;
            let value = decode_entry(
                mapping,
                base,
                directory,
                usize::try_from(target).expect("u32 fits usize"),
                name,
                depth + 1,
            )?;
            Ok(value.column(usize::try_from(column).expect("u32 fits usize"))?)
        }
        DirectoryType::ScalarIndex => Ok(MetricValue::Scalar(entry.scalar()?)),
        DirectoryType::Gauge => Ok(MetricValue::Gauge(entry.scalar()?)),
        DirectoryType::CounterVectorSimple => {
            let vector = vector_of(mapping, base, &entry)?;
            let mut rows = Vec::with_capacity(vector.len());
            for row_index in 0..vector.len() {
                rows.push(inner_u64_vector(mapping, base, &vector, row_index)?.unwrap_or_default());
            }
            Ok(MetricValue::Simple(rows))
        }
        DirectoryType::CounterVectorCombined => {
            let vector = vector_of(mapping, base, &entry)?;
            let mut rows = Vec::with_capacity(vector.len());
            for row_index in 0..vector.len() {
                let Some(inner) =
                    inner_vector(mapping, base, &vector, row_index, size_of::<Counter>())?
                else {
                    rows.push(Vec::new());
                    continue;
                };
                let mut row = Vec::with_capacity(inner.len());
                for column in 0..inner.len() {
                    let element = inner.element(mapping, column)?;
                    row.push(Counter {
                        packets: u64::from_ne_bytes(element[..8].try_into().expect("8 bytes")),
                        bytes: u64::from_ne_bytes(element[8..16].try_into().expect("8 bytes")),
                    });
                }
                rows.push(row);
            }
            Ok(MetricValue::Combined(rows))
        }
        DirectoryType::NameVector => {
            let names = name_vector(mapping, base, &entry)?;
            Ok(MetricValue::Names(names))
        }
        DirectoryType::HistogramLog2 | DirectoryType::RingBuffer => {
            Err(Error::UnsupportedDirectoryType {
                name: name.to_owned(),
                directory_type: directory_type.into(),
            })
        }
        DirectoryType::Illegal | DirectoryType::Empty => Err(Error::UnsupportedDirectoryType {
            name: name.to_owned(),
            directory_type: directory_type.into(),
        }),
    }
}

/// The published vector one vector-bearing entry names.
fn vector_of(
    mapping: &[u8],
    base: usize,
    entry: &DirectoryEntry,
) -> Result<PublishedVector, Error> {
    PublishedVector::resolve(mapping, base, entry.data_pointer()?, size_of::<*mut u8>())
        .map_err(Error::from)
}

/// Resolves one inner vector of an outer vector, or `None` for a null element.
fn inner_vector(
    mapping: &[u8],
    base: usize,
    outer: &PublishedVector,
    row: usize,
    element_size: usize,
) -> Result<Option<PublishedVector>, Error> {
    let published = outer.pointer(mapping, row)?;
    if published == 0 {
        return Ok(None);
    }
    Ok(Some(PublishedVector::resolve(
        mapping,
        base,
        published,
        element_size,
    )?))
}

/// A simple counter row as a `u64` vector; `None` when the row is null.
fn inner_u64_vector(
    mapping: &[u8],
    base: usize,
    outer: &PublishedVector,
    row: usize,
) -> Result<Option<Vec<u64>>, Error> {
    let Some(inner) = inner_vector(mapping, base, outer, row, size_of::<u64>())? else {
        return Ok(None);
    };
    let mut values = Vec::with_capacity(inner.len());
    for column in 0..inner.len() {
        values.push(inner.u64(mapping, column)?);
    }
    Ok(Some(values))
}

/// The names of a name vector.
fn name_vector(mapping: &[u8], base: usize, entry: &DirectoryEntry) -> Result<Vec<String>, Error> {
    let outer = PublishedVector::resolve(
        mapping,
        base,
        entry.name_vector_pointer()?,
        size_of::<*mut u8>(),
    )?;
    let mut names = Vec::with_capacity(outer.len());
    for index in 0..outer.len() {
        let published = outer.pointer(mapping, index)?;
        if published < base {
            return Err(Error::Protocol {
                source: hammer_stats_protocol::protocol::ProtocolError::PointerBeforeBase {
                    pointer: published,
                    base,
                },
            });
        }
        let offset = published - base;
        let bytes = &mapping[offset..];
        let Some(end) = bytes.iter().position(|byte| *byte == 0) else {
            return Err(Error::Protocol {
                source: hammer_stats_protocol::protocol::ProtocolError::MissingNameTerminator,
            });
        };
        names.push(
            std::str::from_utf8(&bytes[..end])
                .map_err(|_| Error::Protocol {
                    source: hammer_stats_protocol::protocol::ProtocolError::InvalidNameEncoding,
                })?
                .to_owned(),
        );
    }
    Ok(names)
}

/// The entry at `index` of the published directory.
fn read_entry(
    directory: &PublishedVector,
    mapping: &[u8],
    index: usize,
) -> Result<DirectoryEntry, Error> {
    let element = directory.element(mapping, index)?;
    let mut bytes = [0_u8; size_of::<DirectoryEntry>()];
    bytes.copy_from_slice(element);
    // SAFETY: the entry is `#[repr(C)]` plain data and every field is valid for
    // all bit patterns; the reader only ever reads the copied value.
    Ok(unsafe { ptr::read_unaligned(bytes.as_ptr().cast::<DirectoryEntry>()) })
}

/// The directory index of `name`, or `None` when no entry owns that name.
fn find_entry(
    directory: &PublishedVector,
    mapping: &[u8],
    name: &str,
) -> Result<Option<usize>, Error> {
    for index in 0..directory.len() {
        if read_entry(directory, mapping, index)?.name()? == name {
            return Ok(Some(index));
        }
    }
    Ok(None)
}

/// The first 40 bytes of the mapping are the shared header.
fn read_header(mapping: &[u8]) -> Result<SharedHeader, Error> {
    let Some(bytes) = mapping.get(..size_of::<SharedHeader>()) else {
        return Err(Error::Protocol {
            source: hammer_stats_protocol::protocol::ProtocolError::InvalidMapping,
        });
    };
    let mut header = [0_u8; size_of::<SharedHeader>()];
    header.copy_from_slice(bytes);
    // SAFETY: the header is `#[repr(C)]` plain data; this only copies bytes.
    Ok(unsafe { ptr::read_unaligned(header.as_ptr().cast::<SharedHeader>()) })
}

/// One read-only mapping of the segment descriptor.
struct SegmentMapping {
    pointer: *mut u8,
    length: usize,
}

// SAFETY: the mapping is read-only for the life of the client, and no interior
// mutability is shared through it.
unsafe impl Send for SegmentMapping {}
unsafe impl Sync for SegmentMapping {}

impl SegmentMapping {
    fn new(segment_fd: &OwnedFd) -> Result<Self, Error> {
        let mut metadata = MaybeUninit::<libc::stat>::uninit();
        // SAFETY: `metadata` is valid writable storage for `fstat`.
        if unsafe { libc::fstat(segment_fd.as_raw_fd(), metadata.as_mut_ptr()) } < 0 {
            return Err(Error::Fstat {
                source: io::Error::last_os_error(),
            });
        }
        // SAFETY: `fstat` initialized every field on success.
        let metadata = unsafe { metadata.assume_init() };
        let size = metadata.st_size;
        if size <= 0 {
            return Err(Error::InvalidSegmentSize { size });
        }
        let length = usize::try_from(size).map_err(|_| Error::InvalidSegmentSize { size })?;
        // SAFETY: the descriptor is live, and the mapping is read-only and
        // private to this client.
        let pointer = unsafe {
            libc::mmap(
                ptr::null_mut(),
                length,
                libc::PROT_READ,
                libc::MAP_SHARED,
                segment_fd.as_raw_fd(),
                0,
            )
        };
        if pointer == libc::MAP_FAILED {
            return Err(Error::Mapping {
                source: io::Error::last_os_error(),
            });
        }
        let mapping = Self {
            pointer: pointer.cast(),
            length,
        };
        if let Err(error) = read_header(mapping.bytes()).and_then(|header| {
            header
                .validate_version()
                .map_err(|source| Error::Protocol { source })
        }) {
            drop(mapping);
            return Err(error);
        }
        Ok(mapping)
    }

    fn bytes(&self) -> &[u8] {
        // SAFETY: the mapping owns `pointer..pointer + length` for its life.
        unsafe { slice::from_raw_parts(self.pointer, self.length) }
    }
}

impl Drop for SegmentMapping {
    fn drop(&mut self) {
        // SAFETY: the mapping is owned here and released exactly once.
        if unsafe { libc::munmap(self.pointer.cast(), self.length) } != 0 {
            // There is no recovery and no caller left to report to: leaving the
            // range mapped would silently detach it from its owner.
            std::process::abort();
        }
    }
}

/// Opens the stats listener socket and connects to it.
fn connect_socket(socket_path: &Path) -> Result<OwnedFd, Error> {
    #[cfg(target_os = "linux")]
    let socket_type = libc::SOCK_SEQPACKET;
    #[cfg(not(target_os = "linux"))]
    let socket_type = libc::SOCK_STREAM;
    let raw = unsafe { libc::socket(libc::AF_UNIX, socket_type, 0) };
    if raw < 0 {
        return Err(Error::Connect {
            path: socket_path.to_path_buf(),
            source: io::Error::last_os_error(),
        });
    }
    // SAFETY: `raw` is a fresh descriptor owned by this call.
    let socket = unsafe { OwnedFd::from_raw_fd(raw) };
    // SAFETY: the descriptor is live; the flag only affects `exec`.
    if unsafe { libc::fcntl(socket.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC) } < 0 {
        return Err(Error::Connect {
            path: socket_path.to_path_buf(),
            source: io::Error::last_os_error(),
        });
    }
    let (address, address_length) = socket_address(socket_path)?;
    // SAFETY: `address` is a live `sockaddr_un` of the stated length.
    let connected = unsafe {
        libc::connect(
            socket.as_raw_fd(),
            ptr::addr_of!(address).cast::<libc::sockaddr>(),
            address_length,
        )
    };
    if connected < 0 {
        return Err(Error::Connect {
            path: socket_path.to_path_buf(),
            source: io::Error::last_os_error(),
        });
    }
    // A listener that accepted the connection but never hands over the segment
    // must fail the connect instead of blocking it forever.
    let timeout = libc::timeval {
        tv_sec: HANDOFF_TIMEOUT.as_secs() as libc::time_t,
        tv_usec: 0,
    };
    // SAFETY: the descriptor is live and `timeout` outlives the call.
    let applied = unsafe {
        libc::setsockopt(
            socket.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_RCVTIMEO,
            ptr::addr_of!(timeout).cast(),
            size_of::<libc::timeval>() as libc::socklen_t,
        )
    };
    if applied < 0 {
        return Err(Error::Connect {
            path: socket_path.to_path_buf(),
            source: io::Error::last_os_error(),
        });
    }
    Ok(socket)
}

/// Builds the `sockaddr_un` of one filesystem socket path.
fn socket_address(path: &Path) -> Result<(libc::sockaddr_un, libc::socklen_t), Error> {
    let mut address: libc::sockaddr_un = unsafe { std::mem::zeroed() };
    address.sun_family = libc::AF_UNIX as libc::sa_family_t;
    let bytes = path.as_os_str().as_encoded_bytes();
    let capacity = address.sun_path.len();
    if bytes.len() >= capacity {
        return Err(Error::SocketPathTooLong {
            path: path.to_path_buf(),
            max: capacity - 1,
        });
    }
    for (index, byte) in bytes.iter().enumerate() {
        address.sun_path[index] = *byte as libc::c_char;
    }
    let length = size_of::<libc::sa_family_t>() + bytes.len() + 1;
    Ok((address, length as libc::socklen_t))
}

/// Receives the segment descriptor the listener hands to a fresh connection.
fn receive_segment_fd(socket: &OwnedFd) -> Result<OwnedFd, Error> {
    let control_bytes = unsafe { libc::CMSG_SPACE(size_of::<RawFd>() as u32) as usize };
    let mut control = vec![0_u8; control_bytes];
    let mut message: libc::msghdr = unsafe { std::mem::zeroed() };
    message.msg_control = control.as_mut_ptr().cast();
    message.msg_controllen = control.len() as _;
    let received = loop {
        // SAFETY: `message` addresses the live control buffer for this
        // synchronous receive, and the socket owns the descriptor.
        let received = unsafe { libc::recvmsg(socket.as_raw_fd(), &mut message, 0) };
        if received >= 0 {
            break received;
        }
        let source = io::Error::last_os_error();
        if source.kind() == io::ErrorKind::Interrupted {
            continue;
        }
        if source.kind() == io::ErrorKind::WouldBlock || source.kind() == io::ErrorKind::TimedOut {
            return Err(Error::HandoffTimeout {
                waited: HANDOFF_TIMEOUT,
            });
        }
        return Err(Error::Receive { source });
    };
    // The listener writes the descriptor without a payload frame, like the
    // server's zero-length `sendmsg`; the length only states that contract.
    debug_assert_eq!(received, 0, "descriptor handoff carries no payload");
    let mut descriptor = None;
    let mut received_fds = 0usize;
    let truncated = (message.msg_flags & libc::MSG_CTRUNC) != 0;
    // SAFETY: `message` was filled by `recvmsg` and its control buffer is
    // valid for the length `recvmsg` reported.
    unsafe {
        let mut current = libc::CMSG_FIRSTHDR(&message);
        while !current.is_null() {
            let length = (*current).cmsg_len as usize;
            let data_offset = libc::CMSG_LEN(0) as usize;
            if (*current).cmsg_level == libc::SOL_SOCKET
                && (*current).cmsg_type == libc::SCM_RIGHTS
                && length >= data_offset + size_of::<RawFd>()
            {
                let data = libc::CMSG_DATA(current).cast::<RawFd>();
                let raw_fd = ptr::read_unaligned(data);
                received_fds += 1;
                if raw_fd >= 0 && descriptor.is_none() {
                    descriptor = Some(OwnedFd::from_raw_fd(raw_fd));
                }
            }
            current = libc::CMSG_NXTHDR(&message, current);
        }
    }
    match descriptor {
        Some(descriptor) if received_fds == 1 && !truncated => Ok(descriptor),
        _ => Err(Error::AncillaryData {
            received_fds,
            truncated,
        }),
    }
}
