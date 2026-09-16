//! Layout of the shared stats segment as a read-only reader sees it.
//!
//! A reader receives the segment descriptor over the stats socket and maps the
//! segment read-only. Every pointer the segment publishes is a virtual address
//! in the server process, so a reader converts each published pointer into an
//! offset from the segment base before touching the mapping. All bounds checks
//! happen before any dereference.

use std::fmt;
use std::mem::{align_of, offset_of, size_of};
use std::ptr;
use std::sync::atomic::{AtomicU64, Ordering};

/// Version the segment publishes in its shared header.
pub const STAT_SEGMENT_VERSION: u64 = 2;
/// Fixed name capacity of one directory entry.
pub const MAX_NAME_BYTES: usize = 128;
/// Longest usable metric name; the segment keeps two nul bytes of slack.
pub const MAX_NAME_LENGTH: usize = MAX_NAME_BYTES - 2;
/// Bytes of the vector header that sits in front of every vector data pointer.
pub const VECTOR_HEADER_BYTES: usize = 8;
/// Minimum alignment of a vector data pointer, as in `vppinfra/vec.c`.
pub const VECTOR_MIN_ALIGN: usize = 8;
/// Directory index of the fixed heartbeat slot.
pub const STAT_COUNTER_HEARTBEAT: u32 = 0;
/// Directory index of the fixed last-clear slot.
pub const STAT_COUNTER_LAST_STATS_CLEAR: u32 = 1;
/// Directory index of the fixed boot-time slot.
pub const STAT_COUNTER_BOOTTIME: u32 = 2;

/// A malformed or unsupported segment layout.
///
/// Every variant states which published fact failed the check; the reader
/// rejects the mapping before it dereferences anything.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ProtocolError {
    #[error("stats segment publishes version {actual}, expected {expected}")]
    VersionMismatch { actual: u64, expected: u64 },
    #[error("stats segment publishes no base or a directory outside the mapping")]
    InvalidMapping,
    #[error("published pointer {pointer:#x} precedes segment base {base:#x}")]
    PointerBeforeBase { pointer: usize, base: usize },
    #[error("stats vector header or elements fall outside the {length}-byte mapping")]
    VectorOutOfBounds { length: usize },
    #[error("vector element {index} is outside length {length}")]
    ElementOutOfBounds { index: usize, length: usize },
    #[error("vector element is {actual} bytes wide, expected at least {expected}")]
    ElementTooSmall { expected: usize, actual: usize },
    #[error("directory type {raw} is unknown")]
    UnknownDirectoryType { raw: u32 },
    #[error("`{name}` is a {actual} entry, expected {expected}")]
    DirectoryTypeMismatch {
        name: String,
        expected: &'static str,
        actual: &'static str,
    },
    #[error("directory name is not terminated within {MAX_NAME_BYTES} bytes")]
    MissingNameTerminator,
    #[error("directory name of {length} bytes exceeds the usable length")]
    NameTooLong { length: usize },
    #[error("directory name padding is not zero")]
    InvalidNamePadding,
    #[error("directory name is not valid UTF-8")]
    InvalidNameEncoding,
    #[error("symlink resolution exceeded {depth} hops")]
    SymlinkCycle { depth: usize },
}

/// Directory entry type codes, matching the segment's `TypeCode`.
#[repr(u32)]
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum DirectoryType {
    Illegal = 0,
    ScalarIndex = 1,
    CounterVectorSimple = 2,
    CounterVectorCombined = 3,
    NameVector = 4,
    Empty = 5,
    Symlink = 6,
    HistogramLog2 = 7,
    RingBuffer = 8,
    Gauge = 9,
}

impl TryFrom<u32> for DirectoryType {
    type Error = ProtocolError;

    fn try_from(raw: u32) -> Result<Self, Self::Error> {
        match raw {
            0 => Ok(Self::Illegal),
            1 => Ok(Self::ScalarIndex),
            2 => Ok(Self::CounterVectorSimple),
            3 => Ok(Self::CounterVectorCombined),
            4 => Ok(Self::NameVector),
            5 => Ok(Self::Empty),
            6 => Ok(Self::Symlink),
            7 => Ok(Self::HistogramLog2),
            8 => Ok(Self::RingBuffer),
            9 => Ok(Self::Gauge),
            raw => Err(ProtocolError::UnknownDirectoryType { raw }),
        }
    }
}

impl From<DirectoryType> for &'static str {
    fn from(directory_type: DirectoryType) -> Self {
        match directory_type {
            DirectoryType::Illegal => "illegal",
            DirectoryType::ScalarIndex => "scalar_index",
            DirectoryType::CounterVectorSimple => "counter_vector_simple",
            DirectoryType::CounterVectorCombined => "counter_vector_combined",
            DirectoryType::NameVector => "name_vector",
            DirectoryType::Empty => "empty",
            DirectoryType::Symlink => "symlink",
            DirectoryType::HistogramLog2 => "histogram_log2",
            DirectoryType::RingBuffer => "ring_buffer",
            DirectoryType::Gauge => "gauge",
        }
    }
}

impl fmt::Display for DirectoryType {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(<&str>::from(*self))
    }
}

/// One combined counter cell, two `u64` values.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Counter {
    pub packets: u64,
    pub bytes: u64,
}

/// The segment header the reader starts from.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct SharedHeader {
    version: u64,
    base: usize,
    epoch: u64,
    in_progress: u64,
    directory_vector: usize,
}

impl SharedHeader {
    #[inline]
    pub fn version(&self) -> u64 {
        // SAFETY: the field is plain data in the reader's mapping.
        unsafe { ptr::read_volatile(ptr::addr_of!(self.version)) }
    }

    #[inline]
    pub fn base(&self) -> usize {
        // SAFETY: see `version`.
        unsafe { ptr::read_volatile(ptr::addr_of!(self.base)) }
    }

    #[inline]
    pub fn epoch(&self) -> u64 {
        epoch_of(self).load(Ordering::Acquire)
    }

    #[inline]
    pub fn is_write_in_progress(&self) -> bool {
        write_in_progress_of(self).load(Ordering::Acquire) != 0
    }

    #[inline]
    pub fn directory_vector(&self) -> usize {
        // SAFETY: see `version`.
        unsafe { ptr::read_volatile(ptr::addr_of!(self.directory_vector)) }
    }

    #[inline]
    pub fn validate_version(&self) -> Result<(), ProtocolError> {
        let actual = self.version();
        if actual == STAT_SEGMENT_VERSION {
            Ok(())
        } else {
            Err(ProtocolError::VersionMismatch {
                actual,
                expected: STAT_SEGMENT_VERSION,
            })
        }
    }
}

/// The header fields the writer publishes with a release store.
///
/// The reader uses the same atomic access the writer does, so a reader never
/// claims a partially published epoch.
fn epoch_of(header: &SharedHeader) -> &AtomicU64 {
    // SAFETY: the mapping is 8-byte aligned, and `epoch` is a live `u64` cell
    // in this process's read-only mapping of the segment.
    unsafe { AtomicU64::from_ptr(ptr::addr_of!(header.epoch).cast_mut()) }
}

fn write_in_progress_of(header: &SharedHeader) -> &AtomicU64 {
    // SAFETY: see `epoch_of`.
    unsafe { AtomicU64::from_ptr(ptr::addr_of!(header.in_progress).cast_mut()) }
}

/// One directory entry, copied out of the read-only mapping.
///
/// The reader owns a value copy of a published slot; it never writes the slot
/// back, so the copy cannot detach a writer from the published entry.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct DirectoryEntry {
    directory_type: u32,
    data: u64,
    name: [u8; MAX_NAME_BYTES],
}

impl DirectoryEntry {
    #[inline]
    pub fn directory_type(&self) -> Result<DirectoryType, ProtocolError> {
        DirectoryType::try_from(self.directory_type)
    }

    /// The entry name, validated as a nul-terminated UTF-8 string.
    pub fn name(&self) -> Result<&str, ProtocolError> {
        let Some(nul) = self.name.iter().position(|byte| *byte == 0) else {
            return Err(ProtocolError::MissingNameTerminator);
        };
        if nul > MAX_NAME_LENGTH {
            return Err(ProtocolError::NameTooLong { length: nul });
        }
        if self.name[nul + 1..].iter().any(|byte| *byte != 0) {
            return Err(ProtocolError::InvalidNamePadding);
        }
        std::str::from_utf8(&self.name[..nul]).map_err(|_| ProtocolError::InvalidNameEncoding)
    }

    /// The scalar cell of a scalar or gauge entry.
    pub fn scalar(&self) -> Result<u64, ProtocolError> {
        match self.directory_type()? {
            DirectoryType::ScalarIndex | DirectoryType::Gauge => Ok(self.data),
            actual => Err(self.type_mismatch("scalar_index or gauge", actual)),
        }
    }

    /// The published vector pointer of a vector-bearing entry.
    pub fn data_pointer(&self) -> Result<usize, ProtocolError> {
        match self.directory_type()? {
            DirectoryType::CounterVectorSimple
            | DirectoryType::CounterVectorCombined
            | DirectoryType::HistogramLog2
            | DirectoryType::RingBuffer => Ok(self.data as usize),
            actual => Err(self.type_mismatch("counter_vector_simple", actual)),
        }
    }

    /// The published pointer of a name vector.
    pub fn name_vector_pointer(&self) -> Result<usize, ProtocolError> {
        match self.directory_type()? {
            DirectoryType::NameVector => Ok(self.data as usize),
            actual => Err(self.type_mismatch("name_vector", actual)),
        }
    }

    /// The target entry index and column of a symlink.
    pub fn symlink(&self) -> Result<(u32, u32), ProtocolError> {
        match self.directory_type()? {
            DirectoryType::Symlink => {
                let index = u32::try_from(self.data & u64::from(u32::MAX)).expect("low half");
                let column = u32::try_from(self.data >> 32).expect("high half");
                Ok((index, column))
            }
            actual => Err(self.type_mismatch("symlink", actual)),
        }
    }

    fn type_mismatch(&self, expected: &'static str, actual: DirectoryType) -> ProtocolError {
        ProtocolError::DirectoryTypeMismatch {
            name: self.entry_name(),
            expected,
            actual: actual.into(),
        }
    }

    /// Best-effort label for diagnostics; [`DirectoryEntry::name`] is the
    /// validating path callers use when the name itself is the result.
    fn entry_name(&self) -> String {
        let end = self
            .name
            .iter()
            .position(|byte| *byte == 0)
            .unwrap_or(MAX_NAME_BYTES);
        String::from_utf8_lossy(&self.name[..end]).into_owned()
    }
}

/// One decoded metric value, identity preserved by the caller's name.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MetricValue {
    /// `ScalarIndex`: a raw `u64` cell.
    Scalar(u64),
    /// `Gauge`: a `u64` cell.
    Gauge(u64),
    /// `CounterVectorSimple`: one `u64` vector per row.
    Simple(Vec<Vec<u64>>),
    /// `CounterVectorCombined`: one `Counter` vector per row.
    Combined(Vec<Vec<Counter>>),
    /// `NameVector`: one name per element.
    Names(Vec<String>),
    /// `HistogramLog2`: one `u64` vector per row.
    Histogram(Vec<Vec<u64>>),
    /// `RingBuffer`: the raw entry bytes of every thread slot.
    Ring(Vec<Vec<u8>>),
}

impl MetricValue {
    /// Keeps column `column` of every row, like VPP's single-column symlink
    /// read. Scalar values ignore the column, as they have no rows.
    pub fn column(&self, column: usize) -> Result<Self, ProtocolError> {
        match self {
            Self::Simple(rows) => Ok(Self::Simple(crop_rows(rows, column)?)),
            Self::Combined(rows) => Ok(Self::Combined(crop_rows(rows, column)?)),
            Self::Histogram(rows) => Ok(Self::Histogram(crop_rows(rows, column)?)),
            Self::Names(names) => Ok(Self::Names(names.clone())),
            Self::Ring(values) => Ok(Self::Ring(values.clone())),
            Self::Scalar(value) => Ok(Self::Scalar(*value)),
            Self::Gauge(value) => Ok(Self::Gauge(*value)),
        }
    }
}

fn crop_rows<T: Clone>(rows: &[Vec<T>], column: usize) -> Result<Vec<Vec<T>>, ProtocolError> {
    let mut cropped = Vec::with_capacity(rows.len());
    for row in rows {
        let Some(value) = row.get(column) else {
            return Err(ProtocolError::ElementOutOfBounds {
                index: column,
                length: row.len(),
            });
        };
        cropped.push(vec![value.clone()]);
    }
    Ok(cropped)
}

/// One published vector, resolved into offsets inside the reader's mapping.
///
/// Resolution reads the vector header in front of the published data pointer
/// and checks that the header lies inside the mapping; element access checks
/// the element range on every call.
#[derive(Clone, Copy, Debug)]
pub struct PublishedVector {
    data_offset: usize,
    length: usize,
    alignment: usize,
    element_size: usize,
}

impl PublishedVector {
    /// Resolves the vector whose data pointer `published` names.
    ///
    /// `mapping` is the reader's whole segment mapping and `base` is the base
    /// the segment publishes. Fails before reading any header byte when the
    /// pointer or header lies outside the mapping.
    pub fn resolve(
        mapping: &[u8],
        base: usize,
        published: usize,
        element_size: usize,
    ) -> Result<Self, ProtocolError> {
        let data_offset = published
            .checked_sub(base)
            .ok_or(ProtocolError::PointerBeforeBase {
                pointer: published,
                base,
            })?;
        let header_offset = data_offset.checked_sub(VECTOR_HEADER_BYTES).ok_or(
            ProtocolError::VectorOutOfBounds {
                length: mapping.len(),
            },
        )?;
        let Some(header) = mapping.get(header_offset..data_offset) else {
            return Err(ProtocolError::VectorOutOfBounds {
                length: mapping.len(),
            });
        };
        let length = usize::try_from(u32::from_ne_bytes([
            header[0], header[1], header[2], header[3],
        ]))
        .expect("u32 fits usize");
        let alignment = VECTOR_MIN_ALIGN << (header[5] & 0x7f);
        Ok(Self {
            data_offset,
            length,
            alignment,
            element_size,
        })
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.length
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.length == 0
    }

    /// Element `index` as raw bytes.
    pub fn element<'mapping>(
        &self,
        mapping: &'mapping [u8],
        index: usize,
    ) -> Result<&'mapping [u8], ProtocolError> {
        if index >= self.length {
            return Err(ProtocolError::ElementOutOfBounds {
                index,
                length: self.length,
            });
        }
        let offset = index
            .checked_mul(self.element_size)
            .and_then(|delta| self.data_offset.checked_add(delta))
            .ok_or(ProtocolError::VectorOutOfBounds {
                length: mapping.len(),
            })?;
        let end =
            offset
                .checked_add(self.element_size)
                .ok_or(ProtocolError::VectorOutOfBounds {
                    length: mapping.len(),
                })?;
        mapping
            .get(offset..end)
            .ok_or(ProtocolError::VectorOutOfBounds {
                length: mapping.len(),
            })
    }

    /// Element `index` as one `u64`.
    pub fn u64(&self, mapping: &[u8], index: usize) -> Result<u64, ProtocolError> {
        if self.element_size < size_of::<u64>() {
            return Err(ProtocolError::ElementTooSmall {
                expected: size_of::<u64>(),
                actual: self.element_size,
            });
        }
        let element = self.element(mapping, index)?;
        Ok(u64::from_ne_bytes([
            element[0], element[1], element[2], element[3], element[4], element[5], element[6],
            element[7],
        ]))
    }

    /// Element `index` as one pointer-sized published address.
    pub fn pointer(&self, mapping: &[u8], index: usize) -> Result<usize, ProtocolError> {
        if self.element_size < size_of::<usize>() {
            return Err(ProtocolError::ElementTooSmall {
                expected: size_of::<usize>(),
                actual: self.element_size,
            });
        }
        let element = self.element(mapping, index)?;
        let mut bytes = [0_u8; size_of::<usize>()];
        bytes.copy_from_slice(&element[..size_of::<usize>()]);
        Ok(usize::from_ne_bytes(bytes))
    }

    /// Data-pointer alignment the vector header declares.
    #[inline]
    pub fn alignment(&self) -> usize {
        self.alignment
    }
}

#[cfg(target_pointer_width = "64")]
const _: () = {
    assert!(size_of::<Counter>() == 16);
    assert!(offset_of!(Counter, packets) == 0);
    assert!(offset_of!(Counter, bytes) == 8);
    assert!(size_of::<SharedHeader>() == 40);
    assert!(align_of::<SharedHeader>() == 8);
    assert!(offset_of!(SharedHeader, version) == 0);
    assert!(offset_of!(SharedHeader, base) == 8);
    assert!(offset_of!(SharedHeader, epoch) == 16);
    assert!(offset_of!(SharedHeader, in_progress) == 24);
    assert!(offset_of!(SharedHeader, directory_vector) == 32);
    assert!(size_of::<DirectoryEntry>() == 144);
    assert!(align_of::<DirectoryEntry>() == 8);
    assert!(offset_of!(DirectoryEntry, directory_type) == 0);
    assert!(offset_of!(DirectoryEntry, data) == 8);
    assert!(offset_of!(DirectoryEntry, name) == 16);
};
