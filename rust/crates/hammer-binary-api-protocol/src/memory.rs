//! Shared Binary API region layout and message storage.

use std::alloc::Layout;
use std::mem::{align_of, size_of};
use std::ptr::{self, NonNull};
use std::sync::atomic::{AtomicI32, AtomicPtr, AtomicU32, Ordering};

use hammer_shmem::heap::MemHeap;
use hammer_shmem::queue::{SvmQueue, SvmQueueError};
use hammer_shmem::region::SvmRegion;
use serde::de::Error as _;

use crate::api::Api;
use crate::codec;

pub const SHMEM_VERSION: u32 = 2;

/// Layout-compatible view of the shared API header. The client only reads the
/// published input queue; server-only ring vectors and lifecycle counters stay
/// opaque after `input_queue`.
#[repr(C)]
pub struct ShmemHeader {
    version: u32,
    server_pid: AtomicI32,
    input_queue: NonNull<SvmQueue>,
    server_rings: [usize; 3],
    client_rings: [usize; 3],
    application_restarts: AtomicU32,
    restart_reclaims: AtomicU32,
    garbage_collects: AtomicU32,
    socket_file_index: u32,
}

// SAFETY: fields after input_queue are immutable topology or independent
// counters; the input queue owns its own synchronization.
unsafe impl Send for ShmemHeader {}
unsafe impl Sync for ShmemHeader {}

impl ShmemHeader {
    pub unsafe fn validate(
        region: &SvmRegion,
        address: NonNull<u8>,
    ) -> Result<NonNull<Self>, MemoryError> {
        if !address.as_ptr().addr().is_multiple_of(align_of::<Self>())
            || !region.contains_range(address, size_of::<Self>())
        {
            return Err(MemoryError::InvalidHeader);
        }
        let header = address.cast::<Self>();
        let shared = unsafe { header.as_ref() };
        if shared.version != SHMEM_VERSION {
            return Err(MemoryError::UnsupportedVersion {
                found: shared.version,
                expected: SHMEM_VERSION,
            });
        }
        let queue = shared.input_queue;
        if !region.contains_range(queue.cast(), size_of::<SvmQueue>()) {
            return Err(MemoryError::InvalidHeader);
        }
        let base = queue.cast::<u8>().as_ptr().addr();
        let region_end = region.base().as_ptr().addr() + region.size();
        let queue = unsafe { SvmQueue::attach(queue.cast(), region_end - base) }
            .map_err(MemoryError::Queue)?;
        if unsafe { queue.as_ref() }.element_size() != size_of::<usize>() {
            return Err(MemoryError::ElementSizeMismatch {
                requested: size_of::<usize>(),
                stored: unsafe { queue.as_ref() }.element_size(),
            });
        }
        Ok(header)
    }

    pub fn server_pid(&self) -> i32 {
        self.server_pid.load(Ordering::Relaxed)
    }

    pub unsafe fn input_queue(&self) -> &SvmQueue {
        unsafe { self.input_queue.as_ref() }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum MemoryError {
    #[error("shared API header is invalid")]
    InvalidHeader,
    #[error("unsupported shared API version {found}; expected {expected}")]
    UnsupportedVersion { found: u32, expected: u32 },
    #[error("shared API queue element size is {stored}; expected {requested}")]
    ElementSizeMismatch { requested: usize, stored: usize },
    #[error("shared API queue: {0}")]
    Queue(#[from] SvmQueueError),
    #[error("shared API allocation failed for {bytes} bytes")]
    AllocationFailed { bytes: usize },
    #[error("shared API codec: {0}")]
    Codec(#[from] codec::Error),
}

pub struct MsgBuf {
    payload: NonNull<u8>,
    payload_len: usize,
    initialized_len: usize,
}

impl MsgBuf {
    pub unsafe fn allocate(heap: &MemHeap, payload_len: usize) -> Result<Self, MemoryError> {
        let (layout, payload_offset) = Self::layout(payload_len);
        let base = heap
            .allocate_zeroed(layout)
            .ok_or(MemoryError::AllocationFailed {
                bytes: layout.size(),
            })?;
        unsafe {
            base.as_ptr()
                .cast::<AtomicPtr<SvmQueue>>()
                .write(AtomicPtr::new(ptr::null_mut()));
            base.as_ptr()
                .add(Self::timestamp_offset())
                .cast::<AtomicU32>()
                .write(AtomicU32::new(0));
            base.as_ptr()
                .add(Self::length_offset())
                .cast::<u32>()
                .write((payload_len as u32).to_be());
        }
        Ok(Self {
            payload: unsafe { NonNull::new_unchecked(base.as_ptr().add(payload_offset)) },
            payload_len,
            initialized_len: 0,
        })
    }

    pub fn len(&self) -> usize {
        self.payload_len
    }

    pub unsafe fn from_address(region: &SvmRegion, address: usize) -> Result<Self, MemoryError> {
        let offset = Self::layout(0).1;
        let prefix_address = address
            .checked_sub(offset)
            .ok_or(MemoryError::InvalidHeader)?;
        let prefix = NonNull::new(prefix_address as *mut u8).ok_or(MemoryError::InvalidHeader)?;
        if !prefix_address.is_multiple_of(Self::layout(0).0.align())
            || !region.contains_range(prefix, offset)
        {
            return Err(MemoryError::InvalidHeader);
        }
        let payload = NonNull::new(address as *mut u8).ok_or(MemoryError::InvalidHeader)?;
        let length = unsafe {
            prefix
                .as_ptr()
                .add(Self::length_offset())
                .cast::<u32>()
                .read()
        };
        let payload_len = u32::from_be(length) as usize;
        if !region.contains_range(payload, payload_len) {
            return Err(MemoryError::InvalidHeader);
        }
        Ok(Self {
            payload,
            payload_len,
            initialized_len: payload_len,
        })
    }

    pub unsafe fn as_bytes(&self) -> Result<&[u8], codec::Error> {
        if self.initialized_len < self.payload_len {
            return Err(codec::Error::custom(
                "API message payload is not initialized",
            ));
        }
        Ok(unsafe { std::slice::from_raw_parts(self.payload.as_ptr(), self.payload_len) })
    }

    pub unsafe fn encode<T: Api>(&mut self, value: &T) -> Result<usize, codec::Error> {
        let output =
            unsafe { std::slice::from_raw_parts_mut(self.payload.as_ptr(), self.payload_len) };
        let written = codec::serialize(value, output)?;
        self.initialized_len = self.initialized_len.max(written);
        Ok(written)
    }

    pub unsafe fn decode<T: Api>(&self) -> Result<T, codec::Error> {
        let bytes = unsafe { self.as_bytes()? };
        let mut decoder = codec::Deserializer::new(bytes);
        let value = T::deserialize(&mut decoder)?;
        if decoder.remaining_bytes() != 0 {
            return Err(codec::Error::custom(
                "API message has trailing payload bytes",
            ));
        }
        Ok(value)
    }

    /// Releases the message through the Data Heap that allocated it.
    ///
    /// # Safety
    /// The message must be live and owned by the caller, and `heap` must be
    /// the Data Heap from the originating API region.
    pub unsafe fn free(self, heap: &MemHeap) {
        let base = unsafe { self.payload.as_ptr().sub(Self::layout(0).1) };
        let marker = unsafe { AtomicPtr::<SvmQueue>::from_ptr(base.cast()) };
        if !marker.load(Ordering::Acquire).is_null() {
            unsafe {
                (*base.add(Self::timestamp_offset()).cast::<AtomicU32>())
                    .store(0, Ordering::Relaxed)
            };
            marker.store(ptr::null_mut(), Ordering::Release);
            return;
        }
        let (layout, _) = Self::layout(self.payload_len);
        let base = unsafe { NonNull::new_unchecked(base) };
        unsafe { heap.deallocate(base, layout) };
    }

    const fn layout(payload_len: usize) -> (Layout, usize) {
        let (prefix, _) = match Layout::new::<AtomicPtr<SvmQueue>>().extend(Layout::new::<u32>()) {
            Ok(layout) => layout,
            Err(_) => panic!("message length offset fits"),
        };
        let (prefix, _) = match prefix.extend(Layout::new::<AtomicU32>()) {
            Ok(layout) => layout,
            Err(_) => panic!("GC timestamp offset fits"),
        };
        let payload = match Layout::array::<u8>(payload_len) {
            Ok(layout) => layout,
            Err(_) => panic!("message length fits allocation layout"),
        };
        match prefix.extend(payload) {
            Ok(layout) => layout,
            Err(_) => panic!("message prefix and payload fit allocation layout"),
        }
    }

    const fn timestamp_offset() -> usize {
        let (prefix, _) = match Layout::new::<AtomicPtr<SvmQueue>>().extend(Layout::new::<u32>()) {
            Ok(layout) => layout,
            Err(_) => panic!("message length offset fits"),
        };
        match prefix.extend(Layout::new::<AtomicU32>()) {
            Ok((_, offset)) => offset,
            Err(_) => panic!("GC timestamp offset fits"),
        }
    }

    const fn length_offset() -> usize {
        match Layout::new::<AtomicPtr<SvmQueue>>().extend(Layout::new::<u32>()) {
            Ok((_, offset)) => offset,
            Err(_) => panic!("message length offset fits"),
        }
    }
}

impl From<&MsgBuf> for usize {
    fn from(message: &MsgBuf) -> Self {
        message.payload.as_ptr().addr()
    }
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
const _: () = {
    use std::mem::offset_of;

    assert!(size_of::<ShmemHeader>() == 80);
    assert!(offset_of!(ShmemHeader, version) == 0);
    assert!(offset_of!(ShmemHeader, server_pid) == 4);
    assert!(offset_of!(ShmemHeader, input_queue) == 8);
    assert!(offset_of!(ShmemHeader, server_rings) == 16);
    assert!(offset_of!(ShmemHeader, client_rings) == 40);
    assert!(MsgBuf::layout(0).1 == 16);
    assert!(MsgBuf::length_offset() == 8);
    assert!(MsgBuf::timestamp_offset() == 12);
};
