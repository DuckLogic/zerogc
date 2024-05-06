use allocator_api2::alloc::{AllocError, Allocator};
use std::alloc::Layout;
use std::cell::{Cell, UnsafeCell};
use std::ptr::NonNull;
use zerogc_next_mimalloc_semisafe::heap::MimallocHeap;

use crate::context::layout::{AllocInfo, GcHeader, GcMarkBits};
use crate::context::{CollectorState, GenerationId};
use crate::CollectorId;

mod fallback {
    use allocator_api2::alloc::AllocError;
    use std::alloc::Layout;
    use std::collections::HashMap;
    use std::ptr::NonNull;

    pub struct HeapAllocFallback;
    impl HeapAllocFallback {
        pub fn new() -> Self {
            HeapAllocFallback
        }
    }

    unsafe impl allocator_api2::alloc::Allocator for HeapAllocFallback {
        fn allocate(&self, layout: Layout) -> Result<NonNull<[u8]>, AllocError> {
            unsafe {
                let ptr = allocator_api2::alloc::alloc(layout);
                if ptr.is_null() {
                    Err(AllocError)
                } else {
                    Ok(NonNull::slice_from_raw_parts(
                        NonNull::new_unchecked(ptr),
                        layout.size(),
                    ))
                }
            }
        }

        unsafe fn deallocate(&self, ptr: NonNull<u8>, layout: Layout) {
            allocator_api2::alloc::dealloc(ptr.as_ptr(), layout)
        }
    }
}

#[cfg(any(miri, feature = "debug-alloc"))]
type HeapAllocator = fallback::HeapAllocFallback;
#[cfg(not(any(miri, feature = "debug-alloc")))]
type HeapAllocator = zerogc_next_mimalloc_semisafe::heap::MimallocHeap;

const DROP_NEEDS_EXPLICIT_FREE: bool = cfg!(any(miri, feature = "debug-alloc"));

enum ObjectFreeCondition<'a, Id: CollectorId> {
    /// Free the object if it has not been marked.
    ///
    /// Used to sweep objects.
    Unmarked { state: &'a CollectorState<Id> },
    /// Unconditionally free the object.
    ///
    /// Used to destroy the
    Always,
}

pub struct OldGenerationSpace<Id: CollectorId> {
    // TODO: Add allocation count wrapper?
    heap: HeapAllocator,
    live_objects: UnsafeCell<Vec<Option<NonNull<GcHeader<Id>>>>>,
    collector_id: Id,
    allocated_bytes: Cell<usize>,
}
impl<Id: CollectorId> OldGenerationSpace<Id> {
    pub unsafe fn new(id: Id) -> Self {
        OldGenerationSpace {
            heap: HeapAllocator::new(),
            live_objects: UnsafeCell::new(Vec::new()),
            collector_id: id,
            allocated_bytes: Cell::new(0),
        }
    }

    pub unsafe fn sweep(&mut self, state: &CollectorState<Id>) {
        self.free_live_objects(ObjectFreeCondition::Unmarked { state });
    }

    unsafe fn free_live_objects(&mut self, cond: ObjectFreeCondition<'_, Id>) {
        let mut next_index: u32 = 0;
        self.live_objects.get_mut().retain(|func| {
            if func.is_none() {
                return false; // skip null objects, deallocated early
            }
            let header = &mut *func.unwrap().as_ptr();
            debug_assert_eq!(header.collector_id, self.collector_id);
            debug_assert_eq!(header.state_bits.get().generation(), GenerationId::Old);
            let should_free = match cond {
                ObjectFreeCondition::Unmarked { state } => {
                    let mark_bits = header.state_bits.get().raw_mark_bits().resolve(state);
                    match mark_bits {
                        GcMarkBits::White => true,  // should free
                        GcMarkBits::Black => false, // should not free
                    }
                }
                ObjectFreeCondition::Always => true, // always free
            };
            if should_free {
                // unmarked (should free)
                if cfg!(debug_assertions) {
                    header.alloc_info.live_object_index = u32::MAX;
                }
                let overall_layout = if header.state_bits.get().array() {
                    header.assume_array_header().layout_info().overall_layout()
                } else {
                    header.metadata.type_info.layout.overall_layout()
                };
                self.allocated_bytes.set(
                    self.allocated_bytes
                        .get()
                        .checked_sub(overall_layout.size())
                        .expect("allocated size underflow"),
                );
                self.heap
                    .deallocate(NonNull::from(header).cast(), overall_layout);
                false
            } else {
                // marked (should not free)
                header.alloc_info.live_object_index = next_index;
                next_index += 1;
                true
            }
        });
        assert_eq!(next_index as usize, self.live_objects.get_mut().len());
        if cfg!(debug_assertions) {
            // second pass to check indexes
            for (index, live) in self.live_objects.get_mut().iter().enumerate() {
                let live = live.expect("All `None` objects should be removed");
                assert_eq!(live.as_ref().alloc_info.live_object_index as usize, index);
            }
        }
    }

    /// Destroy an object whose value has not been initialized
    #[cold]
    pub(super) unsafe fn destroy_uninit_object(&self, header: NonNull<GcHeader<Id>>) {
        assert!(!header.as_ref().state_bits.get().value_initialized());
        let array = header.as_ref().state_bits.get().array();
        let overall_layout = if array {
            header
                .as_ref()
                .assume_array_header()
                .layout_info()
                .overall_layout()
        } else {
            header.as_ref().metadata.type_info.layout.overall_layout()
        };
        {
            let live_objects = &mut *self.live_objects.get();
            let live_object_index = header.as_ref().alloc_info.live_object_index as usize;
            let obj_ref = &mut live_objects[live_object_index];
            assert_eq!(*obj_ref, Some(header));
            *obj_ref = None; // null out remaining reference
        }
        self.heap.deallocate(header.cast(), overall_layout);
        self.allocated_bytes.set(
            self.allocated_bytes
                .get()
                .checked_sub(overall_layout.size())
                .expect("dealloc size overflow"),
        )
    }

    #[inline]
    pub unsafe fn alloc_raw<T: super::RawAllocTarget<Id>>(
        &self,
        target: &T,
    ) -> Result<NonNull<T::Header>, OldAllocError> {
        let overall_layout = target.overall_layout();
        let raw_ptr = match self.heap.allocate(overall_layout) {
            Ok(raw_ptr) => raw_ptr,
            Err(allocator_api2::alloc::AllocError) => return Err(OldAllocError::OutOfMemory),
        };
        self.allocated_bytes.set(
            self.allocated_bytes
                .get()
                .checked_add(overall_layout.size())
                .expect("allocated size overflow"),
        );
        let header_ptr = raw_ptr.cast::<T::Header>();
        let live_object_index: u32;
        {
            let live_objects = &mut *self.live_objects.get();
            live_object_index = u32::try_from(live_objects.len()).unwrap();
            live_objects.push(Some(header_ptr.cast::<GcHeader<Id>>()));
        }
        target.init_header(
            header_ptr,
            GcHeader {
                state_bits: Cell::new(target.init_state_bits(GenerationId::Old)),
                alloc_info: AllocInfo { live_object_index },
                metadata: target.header_metadata(),
                collector_id: self.collector_id,
            },
        );
        Ok(header_ptr)
    }

    #[inline]
    pub fn allocated_bytes(&self) -> usize {
        self.allocated_bytes.get()
    }
}
impl<Id: CollectorId> Drop for OldGenerationSpace<Id> {
    fn drop(&mut self) {
        if DROP_NEEDS_EXPLICIT_FREE {
            unsafe {
                self.free_live_objects(ObjectFreeCondition::Always);
            }
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum OldAllocError {
    #[error("Out of memory (old-gen)")]
    OutOfMemory,
}
