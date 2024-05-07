//! First-class mimalloc heaps
//!
//! These heaps are not thread-safe.

use std::alloc::Layout;
use std::ffi::c_void;
use std::fmt::{Debug, Formatter};
use std::marker::PhantomData;
use std::ptr::NonNull;

use allocator_api2::alloc::{AllocError, Allocator};

use libmimalloc_sys as sys;

pub enum HeapState {
    Owned,
    Destroyed,
}

/// A heap used for mimalloc allocations.
///
/// This is always an explicitly created heap,
/// never the default one.
///
/// It is implicitly destroyed on drop.
pub struct MimallocHeap {
    raw: NonNull<sys::mi_heap_t>,
    /// The `mimalloc` can only be safely used in the thread that created it.
    ///
    /// This marker prevents both `!Send` and `!Sync` although it is
    /// technically redundant with the `raw` pointer.
    nosend_marker: PhantomData<std::rc::Rc<()>>,
}
impl MimallocHeap {
    /// Create a new heap.
    #[inline]
    pub fn new() -> Self {
        MimallocHeap {
            raw: unsafe { NonNull::new(sys::mi_heap_new()).unwrap() },
            nosend_marker: PhantomData,
        }
    }

    /// A raw pointer to the underlying heap
    ///
    /// ## Safety
    /// Should not unexpectedly free any allocations or destroy the arena,
    /// as that would violate the [`Allocator`] api.
    ///
    /// Technically, those operations would themselves be `unsafe`,
    /// so this is slightly redundant.
    #[inline]
    pub unsafe fn as_raw(&self) -> *mut sys::mi_heap_t {
        self.raw.as_ptr()
    }

    #[inline]
    unsafe fn alloc_from_raw_ptr(ptr: *mut u8, size: usize) -> Result<NonNull<[u8]>, AllocError> {
        if ptr.is_null() {
            Err(AllocError)
        } else {
            Ok(NonNull::from(std::slice::from_raw_parts_mut(ptr, size)))
        }
    }

    /// Shared function used for all realloc functions
    #[inline]
    unsafe fn realloc(
        &self,
        ptr: NonNull<u8>,
        old_layout: Layout,
        new_layout: Layout,
    ) -> Result<NonNull<[u8]>, AllocError> {
        let _ = old_layout; // mimalloc doesn't use this
        Self::alloc_from_raw_ptr(
            sys::mi_heap_realloc_aligned(
                self.as_raw(),
                ptr.as_ptr() as *mut c_void,
                new_layout.size(),
                new_layout.align(),
            ) as *mut u8,
            new_layout.size(),
        )
    }
}
impl Debug for MimallocHeap {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MimallocHeap").finish_non_exhaustive()
    }
}
unsafe impl Allocator for MimallocHeap {
    #[inline]
    fn allocate(&self, layout: Layout) -> Result<NonNull<[u8]>, AllocError> {
        unsafe {
            Self::alloc_from_raw_ptr(
                sys::mi_heap_malloc_aligned(self.as_raw(), layout.size(), layout.align())
                    as *mut u8,
                // do not request actual size to avoid out-of-line call
                layout.size(),
            )
        }
    }

    #[inline]
    fn allocate_zeroed(&self, layout: Layout) -> Result<NonNull<[u8]>, AllocError> {
        unsafe {
            Self::alloc_from_raw_ptr(
                sys::mi_heap_zalloc_aligned(self.as_raw(), layout.size(), layout.align())
                    as *mut u8,
                // don't request actual size to avoid out-of-line call
                layout.size(),
            )
        }
    }

    #[inline]
    unsafe fn deallocate(&self, ptr: NonNull<u8>, layout: Layout) {
        sys::mi_free_size_aligned(ptr.as_ptr() as *mut c_void, layout.size(), layout.align())
    }

    #[inline]
    unsafe fn grow(
        &self,
        ptr: NonNull<u8>,
        old_layout: Layout,
        new_layout: Layout,
    ) -> Result<NonNull<[u8]>, AllocError> {
        debug_assert!(new_layout.size() >= old_layout.size());
        self.realloc(ptr, old_layout, new_layout)
    }

    #[inline]
    unsafe fn grow_zeroed(
        &self,
        ptr: NonNull<u8>,
        old_layout: Layout,
        new_layout: Layout,
    ) -> Result<NonNull<[u8]>, AllocError> {
        /*
         * Using rezalloc is only valid if existing memory is zeroed,
         * which we cannot guarentee her
         */
        self.grow(ptr, old_layout, new_layout)
    }

    #[inline]
    unsafe fn shrink(
        &self,
        ptr: NonNull<u8>,
        old_layout: Layout,
        new_layout: Layout,
    ) -> Result<NonNull<[u8]>, AllocError> {
        debug_assert!(new_layout.size() <= old_layout.size());
        self.realloc(ptr, old_layout, new_layout)
    }
}

impl Drop for MimallocHeap {
    #[inline]
    fn drop(&mut self) {
        unsafe {
            sys::mi_heap_destroy(self.as_raw());
        }
    }
}

impl Default for MimallocHeap {
    #[inline]
    fn default() -> Self {
        Self::new()
    }
}
