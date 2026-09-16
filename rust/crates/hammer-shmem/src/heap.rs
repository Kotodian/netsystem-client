//! A view of the locked dlmalloc heap embedded in an SVM region.

use std::alloc::Layout;
use std::ffi::{CStr, c_void};
use std::ptr::{self, NonNull};

const MIN_HEAP_ALIGNMENT: usize = 1 << 3;

type Mspace = *mut c_void;

unsafe extern "C" {
    fn mspace_memalign(mspace: Mspace, alignment: usize, size: usize) -> *mut c_void;
    fn mspace_realloc_in_place(mspace: Mspace, pointer: *mut c_void, size: usize) -> *mut c_void;
    fn mspace_free(mspace: Mspace, pointer: *mut c_void);
    fn mspace_usable_size(pointer: *const c_void) -> usize;
    fn mspace_is_heap_object(mspace: Mspace, pointer: *mut c_void) -> i32;
}

/// Layout-compatible view of `hammer-infra::mem::MemHeap` in shared memory.
///
/// The heap and its `mspace` are created by the server. The client only uses
/// the allocation methods; it never creates, activates, or destroys the heap.
#[repr(C)]
pub struct MemHeap {
    base: *mut c_void,
    mspace: Mspace,
    size: usize,
    page_size_log2: u8,
    locked: u8,
    traced: u8,
    unmap_on_destroy: u8,
    name: [u8; 0],
}

// SAFETY: the heap's mspace is internally locked and its metadata is published
// before a client can observe the region.
unsafe impl Send for MemHeap {}
unsafe impl Sync for MemHeap {}

impl MemHeap {
    pub fn allocate(&self, layout: Layout) -> Option<NonNull<u8>> {
        let layout = Layout::from_size_align(layout.size(), layout.align().max(MIN_HEAP_ALIGNMENT))
            .expect("valid minimum allocation layout");
        let pointer = unsafe { mspace_memalign(self.mspace, layout.align(), layout.size()) };
        NonNull::new(pointer.cast::<u8>())
    }

    pub fn allocate_zeroed(&self, layout: Layout) -> Option<NonNull<u8>> {
        let pointer = self.allocate(layout)?;
        unsafe { ptr::write_bytes(pointer.as_ptr(), 0, layout.size()) };
        Some(pointer)
    }

    /// Reallocates a live allocation in the shared heap.
    ///
    /// # Safety
    /// `pointer` must belong to this heap and `old_layout` must describe it.
    pub unsafe fn reallocate(
        &self,
        pointer: NonNull<u8>,
        old_layout: Layout,
        new_layout: Layout,
    ) -> Option<NonNull<u8>> {
        let new_layout = Layout::from_size_align(
            new_layout.size(),
            new_layout.align().max(MIN_HEAP_ALIGNMENT),
        )
        .expect("valid minimum allocation layout");
        if !self.is_heap_object(pointer) {
            return None;
        }
        let old_size = unsafe { mspace_usable_size(pointer.as_ptr().cast()) };
        debug_assert!(old_layout.size() <= old_size);
        if new_layout.size() == old_size {
            return Some(pointer);
        }
        if pointer.as_ptr().addr().is_multiple_of(new_layout.align())
            && !unsafe {
                mspace_realloc_in_place(self.mspace, pointer.as_ptr().cast(), new_layout.size())
            }
            .is_null()
        {
            return Some(pointer);
        }

        let replacement = self.allocate(new_layout)?;
        unsafe {
            ptr::copy_nonoverlapping(
                pointer.as_ptr(),
                replacement.as_ptr(),
                old_size.min(new_layout.size()),
            );
            mspace_free(self.mspace, pointer.as_ptr().cast());
        }
        Some(replacement)
    }

    /// Releases a live allocation from this heap.
    ///
    /// # Safety
    /// `pointer` must name a live allocation from this heap.
    pub unsafe fn deallocate(&self, pointer: NonNull<u8>, _: Layout) {
        if !self.is_heap_object(pointer) {
            std::process::abort();
        }
        unsafe { mspace_free(self.mspace, pointer.as_ptr().cast()) };
    }

    pub fn is_heap_object(&self, pointer: NonNull<u8>) -> bool {
        unsafe { mspace_is_heap_object(self.mspace, pointer.as_ptr().cast()) != 0 }
    }

    pub fn base(&self) -> NonNull<u8> {
        unsafe { NonNull::new_unchecked(self.base.cast::<u8>()) }
    }

    pub fn size(&self) -> usize {
        self.size
    }

    pub fn name(&self) -> &str {
        let pointer = ptr::addr_of!(self.name).cast::<u8>();
        unsafe { CStr::from_ptr(pointer.cast()) }
            .to_str()
            .expect("heap names are UTF-8")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn private_mspace_symbols_are_linked() {
        let _: unsafe extern "C" fn(Mspace, usize, usize) -> *mut c_void = mspace_memalign;
        let _: unsafe extern "C" fn(Mspace, *mut c_void) = mspace_free;
        let _: unsafe extern "C" fn(*const c_void) -> usize = mspace_usable_size;
        let _: unsafe extern "C" fn(Mspace, *mut c_void) -> i32 = mspace_is_heap_object;
    }
}
