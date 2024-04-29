use std::alloc::Layout;
use std::cell::Cell;
use std::marker::PhantomData;
use std::ptr::NonNull;

use bumpalo::ChunkRawIter;

use crate::context::{AllocInfo, GcHeader, GcStateBits, GcTypeInfo, GenerationId, HeaderMetadata};
use crate::utils::bumpalo_raw::{BumpAllocRaw, BumpAllocRawConfig};
use crate::utils::{Alignment, LayoutExt};
use crate::CollectorId;

/// A young-generation object-space
///
/// If copying is in progress,
/// there may be two young generations for a single collector.
///
/// The design of the allocator is heavily based on [`bumpalo`](https://crates.io/crates/bumpalo)
pub struct YoungGenerationSpace<Id: CollectorId> {
    bump: BumpAllocRaw<BumpConfig<Id>>,
    collector_id: Id,
}
impl<Id: CollectorId> YoungGenerationSpace<Id> {
    /// The maximum size to allocate in the young generation.
    ///
    /// Anything larger than this is immediately sent to the old generation.
    pub const SIZE_LIMIT: usize = 1024;

    #[inline(always)]
    pub unsafe fn alloc_uninit(
        &self,
        type_info: &'static GcTypeInfo<Id>,
    ) -> Result<NonNull<GcHeader<Id>>, YoungAllocError> {
        let overall_size = type_info.layout.overall_size;
        if overall_size > Self::SIZE_LIMIT {
            return Err(YoungAllocError::SizeExceedsLimit);
        }
        let Ok(raw_ptr) = self
            .bump
            .try_alloc_layout(Layout::from_size_align_unchecked(
                overall_size,
                GcHeader::<Id>::FIXED_ALIGNMENT,
            ))
        else {
            return Err(YoungAllocError::OutOfMemory);
        };
        let header_ptr = raw_ptr.cast::<GcHeader<Id>>();
        header_ptr.as_ptr().write(GcHeader {
            state_bits: Cell::new(
                GcStateBits::builder()
                    .with_forwarded(false)
                    .with_generation(GenerationId::Young)
                    .with_array(false)
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

    #[inline]
    pub unsafe fn iter_raw_allocations(&self) -> IterRawAllocations<'_, Id> {
        IterRawAllocations {
            chunk_iter: self.bump.iter_allocated_chunks_raw(),
            remaining_chunk_info: None,
            marker: PhantomData,
        }
    }
}
#[derive(Debug, thiserror::Error)]
enum YoungAllocError {
    #[error("Out of memory")]
    OutOfMemory,
    #[error("Size exceeds young-alloc limit")]
    SizeExceedsLimit,
}

struct IterRawAllocations<'bump, Id: CollectorId> {
    chunk_iter: ChunkRawIter<'bump>,
    remaining_chunk_info: Option<(NonNull<u8>, usize)>,
    marker: PhantomData<Id>,
}
impl<Id: CollectorId> Iterator for IterRawAllocations<'_, Id> {
    type Item = NonNull<GcHeader<Id>>;

    #[inline]
    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if let Some((ref mut remaining_chunk_ptr, ref mut remaining_chunk_size)) =
                self.remaining_chunk_info
            {
                if *remaining_chunk_size == 0 {
                    continue;
                }
                debug_assert!(
                    *remaining_chunk_size >= GcHeader::<Id>::REGULAR_HEADER_LAYOUT.size()
                );
                debug_assert_eq!(
                    // TODO: Use `is_aligned_to` once stabilized
                    remaining_chunk_ptr
                        .as_ptr()
                        .align_offset(GcHeader::<Id>::FIXED_ALIGNMENT),
                    0
                );
                unsafe {
                    let header = &*remaining_chunk_ptr.as_ptr().cast::<GcHeader<Id>>();
                    debug_assert_eq!(header.state_bits.get().generation(), GenerationId::Young);
                    let overall_object_size = header.alloc_info.this_object_overall_size as usize;
                    *remaining_chunk_ptr = NonNull::new_unchecked(
                        remaining_chunk_ptr.as_ptr().add(overall_object_size),
                    );
                    *remaining_chunk_size = remaining_chunk_size.unchecked_sub(overall_object_size);
                    return Some(NonNull::from(header));
                }
            } else {
                match self.chunk_iter.next() {
                    None => return None,
                    Some((remaining_chunk_ptr, remaining_chunk_size)) => {
                        self.remaining_chunk_info = Some((
                            unsafe { NonNull::new_unchecked(remaining_chunk_ptr) },
                            remaining_chunk_size,
                        ));
                    }
                }
            }
        }
    }
}

struct BumpConfig<Id: CollectorId>(PhantomData<&'static Id>);
impl<Id: CollectorId> BumpAllocRawConfig for BumpConfig<Id> {
    const FIXED_ALIGNMENT: Alignment = match Alignment::new(GcHeader::<Id>::FIXED_ALIGNMENT) {
        Ok(alignment) => alignment,
        Err(_) => unreachable!("GcHeader alignment must be valid"),
    };
}
