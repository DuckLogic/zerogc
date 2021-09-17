//! Emulate the `core::alloc::Allocator` API
//!
//! Constructing a `GcAllocWrapper` is `unsafe`,
//! because it is the caller's responsibility to ensure
//! the returned pointers are appropriately traced.
//!
//! If there are any interior pointers,
//! those must also be traced as well.

use core::ptr::NonNull;
use core::alloc::{Layout, AllocError, Allocator};

use crate::{GcSimpleAlloc};


/// A wrapper for a `GcContext` that implements [core::alloc::Allocator]
/// by allocating `GcArray<u8>`
pub struct GcAllocWrapper<'gc, C: GcSimpleAlloc>(&'gc C);

unsafe impl<'gc, C: GcSimpleAlloc> Allocator for GcAllocWrapper<'gc, C> {
    fn allocate(&self, layout: Layout) -> Result<NonNull<[u8]>, AllocError> {
        unsafe {
            let ptr: *mut u8 = match layout.align() {
                1 => self.0.alloc_uninit_slice::<u8>(layout.size()),
                2 => self.0.alloc_uninit_slice::<u16>((layout.size() + 1) / 2).cast(),
                4 => self.0.alloc_uninit_slice::<u32>((layout.size() + 3) / 4).cast(),
                8 => self.0.alloc_uninit_slice::<u64>((layout.size() + 7) / 8).cast(),
                _ => return Err(AllocError)
            };
            Ok(NonNull::new_unchecked(
                core::ptr::slice_from_raw_parts_mut(
                    ptr,
                    layout.size()
                )
            ))
        }
    }
    #[inline]
    unsafe fn deallocate(&self, ptr: NonNull<u8>, layout: Layout) {
        /*
         * with garbage collection, deallocation is a nop
         *
         * If we're in debug mode we will write 
         * 0xDEADBEAF to the memory to be extra sure.
         */
        if cfg!(debug_assertions) {
            const SRC: [u8; 4] = (0xDEAD_BEAFu32).to_ne_bytes();
            ptr.as_ptr().copy_from_nonoverlapping(SRC.as_ptr(), layout.size().min(4));
        }
    }
}
