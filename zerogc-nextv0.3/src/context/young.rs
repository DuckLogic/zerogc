use allocator_api2::alloc::{AllocError, Allocator};
use bumpalo::Bump;
use std::alloc::Layout;
use std::cell::{Cell, UnsafeCell};
use std::marker::PhantomData;
use std::mem::ManuallyDrop;
use std::ptr::NonNull;

use crate::context::alloc::{ArenaAlloc, CountingAlloc};
use crate::context::layout::{AllocInfo, GcHeader, GcMarkBits};
use crate::context::{CollectorState, GenerationId};
use crate::utils::Alignment;
use crate::{CollectorId, Gc};

struct YoungAlloc {
    #[cfg(feature = "debug-alloc")]
    group: ArenaAlloc<allocator_api2::alloc::Global>,
    #[cfg(not(feature = "debug-alloc"))]
    bump: Bump,
}
impl YoungAlloc {
    pub fn new() -> Self {
        #[cfg(feature = "debug-alloc")]
        {
            YoungAlloc {
                group: ArenaAlloc::new(allocator_api2::alloc::Global),
            }
        }
        #[cfg(not(feature = "debug-alloc"))]
        {
            YoungAlloc { bump: Bump::new() }
        }
    }
    fn alloc_impl(&self) -> impl Allocator + '_ {
        #[cfg(feature = "debug-alloc")]
        {
            &self.group
        }
        #[cfg(not(feature = "debug-alloc"))]
        {
            &self.bump
        }
    }
    unsafe fn reset(&mut self) {
        #[cfg(feature = "debug-alloc")]
        {
            self.group.reset();
        }
        #[cfg(not(feature = "debug-alloc"))]
        {
            self.bump.reset();
        }
    }
}
unsafe impl Allocator for YoungAlloc {
    fn allocate(&self, layout: Layout) -> Result<NonNull<[u8]>, AllocError> {
        self.alloc_impl().allocate(layout)
    }

    unsafe fn deallocate(&self, ptr: NonNull<u8>, layout: Layout) {
        self.alloc_impl().deallocate(ptr, layout)
    }
}

/// A young-generation object-space
///
/// If copying is in progress,
/// there may be two young generations for a single collector.
///
/// The design of the allocator is heavily based on [`bumpalo`](https://crates.io/crates/bumpalo)
pub struct YoungGenerationSpace<Id: CollectorId> {
    alloc: CountingAlloc<YoungAlloc>,
    /// A set of objects which need destructors to be run.
    destruction_queue: UnsafeCell<Vec<Option<NonNull<GcHeader<Id>>>>>,
    collector_id: Id,
}
impl<Id: CollectorId> YoungGenerationSpace<Id> {
    pub unsafe fn new(id: Id) -> Self {
        #[cfg(not(feature = "debug-alloc"))]
        let bump = ManuallyDrop::new(Box::new(Bump::new()));
        YoungGenerationSpace {
            alloc: CountingAlloc::new(YoungAlloc::new()),
            destruction_queue: UnsafeCell::new(Vec::new()),
            collector_id: id,
        }
    }

    /// The maximum size to allocate in the young generation.
    ///
    /// Anything larger than this is immediately sent to the old generation.
    pub const SIZE_LIMIT: usize = 1024;

    pub unsafe fn sweep(&mut self, state: &CollectorState<Id>) {
        for &element in self.destruction_queue.get_mut().iter() {
            if let Some(header) = element {
                debug_assert_eq!(
                    header
                        .as_ref()
                        .state_bits
                        .get()
                        .raw_mark_bits()
                        .resolve(state),
                    GcMarkBits::White,
                    "Only white objects should be in destruction queue"
                );
                header.as_ref().invoke_destructor();
            }
        }
        self.destruction_queue.get_mut().clear();
        self.alloc.as_inner_mut().reset();
    }

    #[inline]
    pub unsafe fn remove_destruction_queue(
        &self,
        header: NonNull<GcHeader<Id>>,
        state: &CollectorState<Id>,
    ) {
        debug_assert_ne!(
            header
                .as_ref()
                .state_bits
                .get()
                .raw_mark_bits()
                .resolve(state),
            GcMarkBits::White,
            "Only marked objects  should be removed from the detruction queue"
        );
        debug_assert_eq!(
            header.as_ref().state_bits.get().generation(),
            GenerationId::Young
        );
        let drop_index = header.as_ref().alloc_info.nontrivial_drop_index;
        if drop_index == u32::MAX {
            debug_assert!(header.as_ref().resolve_type_info().drop_func.is_none());
        } else {
            debug_assert!(header.as_ref().resolve_type_info().drop_func.is_some());
            (*self.destruction_queue.get())[drop_index as usize] = None;
            if cfg!(debug_assertions) {
                (*header.as_ptr()).alloc_info.nontrivial_drop_index = u32::MAX - 1;
            }
        }
    }

    #[inline]
    pub unsafe fn alloc_raw<T: super::RawAllocTarget<Id>>(
        &self,
        target: &T,
    ) -> Result<NonNull<T::Header>, YoungAllocError> {
        let overall_layout = target.overall_layout();
        if overall_layout.size() > Self::SIZE_LIMIT {
            return Err(YoungAllocError::SizeExceedsLimit);
        }
        let Ok(raw_ptr) = self.alloc.allocate(overall_layout) else {
            return Err(YoungAllocError::OutOfMemory);
        };
        let header_ptr = raw_ptr.cast::<T::Header>();
        let drop_index = if target.needs_drop() {
            let index = (*self.destruction_queue.get()).len();
            (*self.destruction_queue.get()).push(Some(header_ptr.cast::<GcHeader<Id>>()));
            assert!(index < u32::MAX as usize);
            index as u32
        } else {
            u32::MAX
        };
        target.init_header(
            header_ptr,
            GcHeader {
                state_bits: Cell::new(target.init_state_bits(GenerationId::Young)),
                alloc_info: AllocInfo {
                    nontrivial_drop_index: drop_index,
                },
                metadata: target.header_metadata(),
                collector_id: self.collector_id,
            },
        );
        Ok(header_ptr)
    }

    #[inline]
    pub fn allocated_bytes(&self) -> usize {
        self.alloc.allocated_bytes()
    }
}
impl<Id: CollectorId> Drop for YoungGenerationSpace<Id> {
    fn drop(&mut self) {
        // drop all pending objects
        for header in self.destruction_queue.get_mut().iter() {
            if let Some(header) = header {
                unsafe { header.as_ref().invoke_destructor() }
            }
        }
    }
}
#[derive(Debug, thiserror::Error)]
pub enum YoungAllocError {
    #[error("Out of memory (young-gen)")]
    OutOfMemory,
    #[error("Size exceeds young-alloc limit")]
    SizeExceedsLimit,
}
