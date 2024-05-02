use allocator_api2::alloc::Allocator;
use std::alloc::Layout;
use std::cell::Cell;
use std::ptr::NonNull;
use zerogc_next_mimalloc_semisafe::heap::MimallocHeap;

use crate::context::young::YoungGenerationSpace;
use crate::context::{
    AllocInfo, GcHeader, GcMarkBits, GcStateBits, GcTypeInfo, GenerationId, HeaderMetadata,
};
use crate::CollectorId;

pub struct OldGenerationSpace<Id: CollectorId> {
    heap: MimallocHeap,
    live_objects: Vec<NonNull<GcHeader<Id>>>,
    collector_id: Id,
    mark_bits_inverted: bool,
}
impl<Id: CollectorId> OldGenerationSpace<Id> {
    #[inline]
    pub fn mark_bits_inverted(&self) -> bool {
        self.mark_bits_inverted
    }

    #[inline(always)]
    pub unsafe fn alloc_uninit(
        &self,
        type_info: &'static GcTypeInfo<Id>,
    ) -> Result<NonNull<GcHeader<Id>>, OldAllocError> {
        let overall_size = type_info.layout.overall_size;
        let raw_ptr = match self.heap.allocate(Layout::from_size_align_unchecked(
            overall_size,
            GcHeader::<Id>::FIXED_ALIGNMENT,
        )) {
            Ok(raw_ptr) => raw_ptr,
            Err(allocator_api2::alloc::AllocError) => return Err(OldAllocError::OutOfMemory),
        };
        let header_ptr = raw_ptr.cast::<GcHeader<Id>>();
        header_ptr.as_ptr().write(GcHeader {
            state_bits: Cell::new(
                GcStateBits::builder()
                    .with_forwarded(false)
                    .with_generation(GenerationId::Old)
                    .with_array(false)
                    .with_raw_mark_bits(GcMarkBits::White.to_raw(self))
                    .build(),
            ),
            alloc_info: AllocInfo {
                this_object_overall_size: overall_size as u32,
            },
            metadata: HeaderMetadata { type_info },
            collector_id: self.collector_id,
        });
        Ok(header_ptr)
    }
}

#[derive(Debug, thiserror::Error)]
enum OldAllocError {
    #[error("Out of memory (oldgen)")]
    OutOfMemory,
}

unsafe impl<Id: CollectorId> super::Generation<Id> for OldGenerationSpace<Id> {
    const ID: GenerationId = GenerationId::Old;

    #[inline]
    fn cast_young(&self) -> Option<&'_ YoungGenerationSpace<Id>> {
        None
    }

    #[inline]
    fn cast_old(&self) -> Option<&'_ OldGenerationSpace<Id>> {
        Some(self)
    }
}
