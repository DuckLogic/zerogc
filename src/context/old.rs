use allocator_api2::alloc::Allocator;
use std::cell::{Cell, UnsafeCell};
use std::ptr::NonNull;
use zerogc_next_mimalloc_semisafe::heap::MimallocHeap;

use crate::context::layout::{AllocInfo, GcHeader, GcMarkBits};
use crate::context::{CollectStage, CollectStageTracker, CollectorState, GenerationId};
use crate::CollectorId;

pub struct OldGenerationSpace<Id: CollectorId> {
    heap: MimallocHeap,
    live_objects: UnsafeCell<Vec<NonNull<GcHeader<Id>>>>,
    collector_id: Id,
    stage: CollectStageTracker,
}
impl<Id: CollectorId> OldGenerationSpace<Id> {
    pub unsafe fn sweep(&mut self, state: &CollectorState<Id>) {
        self.stage
            .begin_stage(Some(CollectStage::Mark), CollectStage::Sweep);
        let mut next_index: u32 = 0;
        self.live_objects.get_mut().retain(|func| {
            let header = &mut *func.as_ptr();
            debug_assert_eq!(header.collector_id, self.collector_id);
            debug_assert_eq!(header.state_bits.get().generation(), GenerationId::Old);
            let mark_bits = header.state_bits.get().raw_mark_bits().resolve(state);
            match mark_bits {
                GcMarkBits::White => {
                    // unmarked
                    if cfg!(debug_assertions) {
                        header.alloc_info.live_object_index = u32::MAX;
                    }
                    false
                }
                GcMarkBits::Black => {
                    // marked
                    header.alloc_info.live_object_index = next_index;
                    next_index += 1;
                    true
                }
            }
        });
        assert_eq!(next_index as usize, self.live_objects.get_mut().len());
        if cfg!(debug_assertions) {
            // second pass to check indexes
            for (index, live) in self.live_objects.get_mut().iter().enumerate() {
                assert_eq!(live.as_ref().alloc_info.live_object_index as usize, index);
            }
        }
        self.stage.finish_stage(CollectStage::Sweep);
    }

    #[inline]
    pub unsafe fn alloc_raw<T: super::RawAllocTarget<Id>>(
        &self,
        target: T,
    ) -> Result<NonNull<T::Header>, OldAllocError> {
        let overall_layout = target.overall_layout();
        let raw_ptr = match self.heap.allocate(overall_layout) {
            Ok(raw_ptr) => raw_ptr,
            Err(allocator_api2::alloc::AllocError) => return Err(OldAllocError::OutOfMemory),
        };
        let header_ptr = raw_ptr.cast::<T::Header>();
        let live_object_index: u32;
        {
            let live_objects = &mut *self.live_objects.get();
            live_object_index = u32::try_from(live_objects.len()).unwrap();
            live_objects.push(header_ptr.cast::<GcHeader<Id>>());
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
}

#[derive(Debug, thiserror::Error)]
pub enum OldAllocError {
    #[error("Out of memory (oldgen)")]
    OutOfMemory,
}
