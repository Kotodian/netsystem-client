//! Read/write attachment to an existing VPP-style SVM region.

use std::ffi::c_void;
use std::fmt;
use std::fs::OpenOptions;
use std::io;
use std::mem::{MaybeUninit, size_of};
use std::os::fd::{AsRawFd, OwnedFd};
use std::path::Path;
use std::ptr::NonNull;
use std::sync::atomic::{AtomicI32, AtomicPtr, AtomicU64, Ordering};

use posix_sync::condvar::RawCondvarAlloc;
use posix_sync::mutex::RawMutexAlloc;

use crate::heap::MemHeap;

pub const SVM_REGION_VERSION: u64 = (2 << 16) | 2;
pub const SVM_PVT_HEAP_SIZE: usize = 128 << 10;

const DATA_HEAP_FLAG: u64 = 1 << 0;
const NODATA_FLAG: u64 = 1 << 2;
const PUBLIC_FLAGS: u64 = DATA_HEAP_FLAG | NODATA_FLAG;

#[repr(transparent)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SvmRegionFlags(u64);

impl SvmRegionFlags {
    pub const NONE: Self = Self(0);
    pub const DATA_HEAP: Self = Self(DATA_HEAP_FLAG);
    pub const NODATA: Self = Self(NODATA_FLAG);

    pub const fn contains(self, flag: Self) -> bool {
        self.0 & flag.0 == flag.0
    }

    pub const fn bits(self) -> u64 {
        self.0
    }
}

#[repr(C, align(64))]
pub(crate) struct SvmRegionHeader {
    version: AtomicU64,
    mutex: MaybeUninit<RawMutexAlloc>,
    condvar: MaybeUninit<RawCondvarAlloc>,
    mutex_owner_pid: AtomicI32,
    mutex_owner_tag: AtomicI32,
    flags: SvmRegionFlags,
    virtual_base: *mut u8,
    virtual_size: usize,
    pvt_heap: *mut MemHeap,
    data_base: *mut c_void,
    data_heap: *mut MemHeap,
    user_ctx: AtomicPtr<c_void>,
    bitmap_size: usize,
    bitmap: *mut c_void,
    region_name: *mut c_void,
    backing_file: *mut c_void,
    filenames: *mut c_void,
    client_pids: *mut c_void,
}

#[derive(Debug, thiserror::Error)]
pub enum SvmRegionError {
    #[error("open SVM backing `{path}`: {source}")]
    Open {
        path: std::path::PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("read SVM backing metadata: {source}")]
    BackingMetadata {
        #[source]
        source: io::Error,
    },
    #[error("SVM backing is not ready")]
    NotReady,
    #[error("unsupported SVM region version {found}; expected {expected}")]
    UnsupportedVersion { found: u64, expected: u64 },
    #[error("SVM region virtual base mismatch: mapped {found:#x}, declared {expected:#x}")]
    VirtualBaseMismatch { found: usize, expected: usize },
    #[error("SVM region virtual size mismatch: mapped {mapped}, declared {declared}")]
    VirtualSizeMismatch { mapped: usize, declared: usize },
    #[error("invalid SVM region flags {bits:#x}")]
    InvalidFlags { bits: u64 },
    #[error("SVM region has no usable Data Heap")]
    MissingDataHeap,
    #[error("SVM region PVT Heap layout is invalid")]
    PvtHeapLayout,
    #[error("SVM region Data Heap layout is invalid")]
    DataHeapLayout,
    #[error("fixed SVM mapping at {base:#x} for {size} bytes failed: {source}")]
    FixedMapping {
        base: usize,
        size: usize,
        #[source]
        source: io::Error,
    },
    #[error("SVM region mapping is occupied at {base:#x} for {size} bytes")]
    AddressRangeOccupied { base: usize, size: usize },
    #[error("unmap SVM region at {base:#x} for {size} bytes: {source}")]
    Unmapping {
        base: usize,
        size: usize,
        #[source]
        source: io::Error,
    },
}

pub struct SvmRegion {
    #[allow(dead_code)]
    backing: OwnedFd,
    base: NonNull<u8>,
    size: usize,
    header: NonNull<SvmRegionHeader>,
}

// SAFETY: the region owns its mapping and the header is immutable topology
// after publication. The shared heap provides its own synchronization.
unsafe impl Send for SvmRegion {}
unsafe impl Sync for SvmRegion {}

impl fmt::Debug for SvmRegion {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SvmRegion")
            .field("base", &self.base)
            .field("size", &self.size)
            .finish_non_exhaustive()
    }
}

impl SvmRegion {
    pub fn attach(path: &Path) -> Result<Self, SvmRegionError> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .map_err(|source| SvmRegionError::Open {
                path: path.to_path_buf(),
                source,
            })?;
        let backing: OwnedFd = file.into();
        let page_size = system_page_size();
        let (base, size) = probe_backing(&backing, page_size)?;
        let mapped = map_fixed_backing(&backing, base, size, 0)?;
        let header = mapped.cast::<SvmRegionHeader>();
        let region = Self {
            backing,
            base: mapped,
            size,
            header,
        };
        if let Err(error) = region.validate(page_size) {
            if unsafe { libc::munmap(region.base.as_ptr().cast(), region.size) } != 0 {
                std::process::abort();
            }
            return Err(error);
        }
        Ok(region)
    }

    pub fn base(&self) -> NonNull<u8> {
        self.base
    }

    pub fn size(&self) -> usize {
        self.size
    }

    pub fn data_heap(&self) -> &MemHeap {
        let heap = unsafe { self.header.as_ref().data_heap };
        unsafe { heap.as_ref() }.expect("validated region has a Data Heap")
    }

    pub fn user_context(&self) -> Option<NonNull<u8>> {
        NonNull::new(
            unsafe { self.header.as_ref() }
                .user_ctx
                .load(Ordering::Acquire)
                .cast(),
        )
    }

    pub fn contains_range(&self, start: NonNull<u8>, bytes: usize) -> bool {
        let base = self.base.as_ptr().addr();
        let Some(end) = base.checked_add(self.size) else {
            return false;
        };
        let start = start.as_ptr().addr();
        start >= base
            && start
                .checked_add(bytes)
                .is_some_and(|end_of_range| end_of_range <= end)
    }

    pub fn remaining_from(&self, address: usize) -> Option<&[u8]> {
        let base = self.base.as_ptr().addr();
        let end = base.checked_add(self.size)?;
        if address < base || address > end {
            return None;
        }
        let length = end - address;
        Some(unsafe { std::slice::from_raw_parts(address as *const u8, length) })
    }

    fn validate(&self, page_size: usize) -> Result<(), SvmRegionError> {
        let header = unsafe { self.header.as_ref() };
        let version = header.version.load(Ordering::Acquire);
        if version == 0 {
            return Err(SvmRegionError::NotReady);
        }
        if version != SVM_REGION_VERSION {
            return Err(SvmRegionError::UnsupportedVersion {
                found: version,
                expected: SVM_REGION_VERSION,
            });
        }
        if header.virtual_base != self.base.as_ptr() {
            return Err(SvmRegionError::VirtualBaseMismatch {
                found: self.base.as_ptr().addr(),
                expected: header.virtual_base.addr(),
            });
        }
        if header.virtual_size != self.size {
            return Err(SvmRegionError::VirtualSizeMismatch {
                mapped: self.size,
                declared: header.virtual_size,
            });
        }
        let flag_bits = header.flags.bits();
        if flag_bits & !PUBLIC_FLAGS != 0
            || header.flags.contains(SvmRegionFlags::DATA_HEAP)
                && header.flags.contains(SvmRegionFlags::NODATA)
        {
            return Err(SvmRegionError::InvalidFlags { bits: flag_bits });
        }
        let expected_data_base = self.base.as_ptr().addr()
            + page_size
            + if header.pvt_heap.is_null() {
                0
            } else {
                unsafe { (*header.pvt_heap).size() }
            };
        let Some(data_heap) = (unsafe { header.data_heap.as_ref() }) else {
            return Err(SvmRegionError::MissingDataHeap);
        };
        if !header.flags.contains(SvmRegionFlags::DATA_HEAP)
            || data_heap.base().as_ptr().addr() != expected_data_base
            || data_heap.size()
                != self
                    .size
                    .saturating_sub(page_size + unsafe { (*header.pvt_heap).size() })
        {
            return Err(SvmRegionError::DataHeapLayout);
        }
        Ok(())
    }
}

impl Drop for SvmRegion {
    fn drop(&mut self) {
        if self.size != 0 && unsafe { libc::munmap(self.base.as_ptr().cast(), self.size) } != 0 {
            std::process::abort();
        }
    }
}

fn system_page_size() -> usize {
    let value = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    assert!(value > 0, "OS page size is available");
    usize::try_from(value).expect("OS page size fits usize")
}

fn probe_backing(backing: &OwnedFd, page_size: usize) -> Result<(usize, usize), SvmRegionError> {
    let mut status = MaybeUninit::<libc::stat>::uninit();
    if unsafe { libc::fstat(backing.as_raw_fd(), status.as_mut_ptr()) } != 0 {
        return Err(SvmRegionError::BackingMetadata {
            source: io::Error::last_os_error(),
        });
    }
    let available = usize::try_from(unsafe { status.assume_init() }.st_size).map_err(|_| {
        SvmRegionError::BackingMetadata {
            source: io::Error::new(io::ErrorKind::InvalidData, "negative backing size"),
        }
    })?;
    if available < page_size {
        return Err(SvmRegionError::NotReady);
    }
    let mut first_page = vec![0_u8; page_size];
    let read = unsafe {
        libc::pread(
            backing.as_raw_fd(),
            first_page.as_mut_ptr().cast(),
            first_page.len(),
            0,
        )
    };
    if read < 0 {
        return Err(SvmRegionError::BackingMetadata {
            source: io::Error::last_os_error(),
        });
    }
    if read < size_of::<SvmRegionHeader>() as isize {
        return Err(SvmRegionError::NotReady);
    }
    let header = first_page.as_ptr().cast::<SvmRegionHeader>();
    let version = unsafe { (*header).version.load(Ordering::Acquire) };
    if version == 0 {
        return Err(SvmRegionError::NotReady);
    }
    if version != SVM_REGION_VERSION {
        return Err(SvmRegionError::UnsupportedVersion {
            found: version,
            expected: SVM_REGION_VERSION,
        });
    }
    let base = unsafe { (*header).virtual_base.addr() };
    let size = unsafe { (*header).virtual_size };
    if base == 0 || size < page_size || !size.is_multiple_of(page_size) {
        return Err(SvmRegionError::NotReady);
    }
    if available < size {
        return Err(SvmRegionError::BackingMetadata {
            source: io::Error::new(io::ErrorKind::UnexpectedEof, "SVM backing is too short"),
        });
    }
    Ok((base, size))
}

fn map_fixed_backing(
    backing: &OwnedFd,
    base: usize,
    size: usize,
    offset: u64,
) -> Result<NonNull<u8>, SvmRegionError> {
    #[cfg(target_os = "linux")]
    let reservation_flags = libc::MAP_PRIVATE | libc::MAP_ANONYMOUS | libc::MAP_FIXED_NOREPLACE;
    #[cfg(not(target_os = "linux"))]
    let reservation_flags = libc::MAP_PRIVATE | libc::MAP_ANONYMOUS;
    let reservation = unsafe {
        libc::mmap(
            base as *mut c_void,
            size,
            libc::PROT_NONE,
            reservation_flags,
            -1,
            0,
        )
    };
    if reservation == libc::MAP_FAILED {
        let source = io::Error::last_os_error();
        if source.raw_os_error() == Some(libc::EEXIST) {
            return Err(SvmRegionError::AddressRangeOccupied { base, size });
        }
        return Err(SvmRegionError::FixedMapping { base, size, source });
    }
    if reservation.addr() != base {
        if unsafe { libc::munmap(reservation, size) } != 0 {
            std::process::abort();
        }
        return Err(SvmRegionError::AddressRangeOccupied { base, size });
    }
    let offset = libc::off_t::try_from(offset).map_err(|_| SvmRegionError::FixedMapping {
        base,
        size,
        source: io::Error::new(
            io::ErrorKind::InvalidInput,
            "mapping offset does not fit off_t",
        ),
    })?;
    let mapped = unsafe {
        libc::mmap(
            reservation,
            size,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_SHARED | libc::MAP_FIXED,
            backing.as_raw_fd(),
            offset,
        )
    };
    if mapped == libc::MAP_FAILED {
        let source = io::Error::last_os_error();
        if unsafe { libc::munmap(reservation, size) } != 0 {
            std::process::abort();
        }
        return Err(SvmRegionError::FixedMapping { base, size, source });
    }
    Ok(NonNull::new(mapped.cast()).expect("successful fixed mmap is non-null"))
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
const _: () = {
    use std::mem::{align_of, offset_of};

    assert!(align_of::<SvmRegionHeader>() == 64);
    assert!(offset_of!(SvmRegionHeader, version) == 0);
    assert!(offset_of!(SvmRegionHeader, mutex) == 8);
    assert!(offset_of!(SvmRegionHeader, condvar) == 48);
    assert!(offset_of!(SvmRegionHeader, mutex_owner_pid) == 96);
    assert!(offset_of!(SvmRegionHeader, mutex_owner_tag) == 100);
    assert!(offset_of!(SvmRegionHeader, flags) == 104);
    assert!(offset_of!(SvmRegionHeader, virtual_base) == 112);
    assert!(offset_of!(SvmRegionHeader, virtual_size) == 120);
    assert!(offset_of!(SvmRegionHeader, pvt_heap) == 128);
    assert!(offset_of!(SvmRegionHeader, data_base) == 136);
    assert!(offset_of!(SvmRegionHeader, data_heap) == 144);
    assert!(offset_of!(SvmRegionHeader, user_ctx) == 152);
};
