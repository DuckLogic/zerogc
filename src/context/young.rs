use crate::context::{GcHeader, GcLayout, GcTypeInfo};
use crate::utils::LayoutExt;
use crate::CollectorId;
use std::alloc::Layout;
use std::cell::Cell;
use std::ptr::NonNull;

/// A yong-generation object-space
///
/// If copying is in progress,
/// there may be two young generations for a single collector.
///
/// The design of the allocator is heavily based on [`bumpalo`](https://crates.io/crates/bumpalo)
pub struct YoungGenerationSpace<Id> {
    current_chunk: Cell<NonNull<ChunkHeader>>,
    current_chunk_next_ptr: Cell<NonNull<u8>>,
    current_chunk_end_ptr: Cell<NonNull<u8>>,
    collector_id: Id,
}
impl<Id: CollectorId> YoungGenerationSpace<Id> {
    #[inline(always)]
    pub fn alloc_uninit(
        &self,
        layout_info: GcLayout,
        type_info: &'static GcTypeInfo<Id>,
    ) -> (*mut GcHeader<Id>, NonNull<u8>) {
        debug_assert_eq!(layout_info, type_info.layout_info);
    }
    fn ensure_chunk(&self, layout: Layout) -> Layout {
        x
    }
}

#[repr(C, align(16))]
pub struct ChunkHeader {
    initial_object: Option<NonNull<u8>>,
    final_allocation_ptr: Option<NonNull<u8>>,
    prev_chunk: Option<NonNull<ChunkHeader>>,
    data_size: usize,
}
impl ChunkHeader {
    #[inline]
    fn data_start_ptr(&self) -> NonNull<u8> {
        unsafe {
            NonNull::new_unchecked(
                (self as *const Self as *const u8).add(Self::DATA_OFFSET) as *mut u8
            )
        }
    }
    #[inline]
    fn data_end_ptr(&self) -> NonNull<u8> {
        unsafe { NonNull::new_unchecked(self.start_ptr().as_ptr().add(self.data_size)) }
    }
    #[inline]
    fn overall_layout(&self) -> Layout {
        assert_eq!(Self::DATA_OFFSET, Self::HEADER_LAYOUT.size());
        unsafe {
            Layout::from_size_align_unchecked(
                Self::DATA_OFFSET.unchecked_add(self.data_size),
                Self::DATA_ALIGNMENT,
            )
            .unwrap()
        }
    }
    #[inline]
    fn data_layout(&self) -> Layout {
        unsafe { Layout::from_size_align_unchecked(self.data_size, Self::DATA_ALIGNMENT) }
    }
    const HEADER_LAYOUT: Layout = Layout::new::<Self>();
    pub const DATA_OFFSET: usize = {
        let this_layout = Self::HEADER_LAYOUT;
        assert!(this_layout.align() <= Self::DATA_ALIGNMENT);
        let padding = LayoutExt(this_layout).padding_needed_for(Self::DATA_ALIGNMENT);
        this_layout.size() + padding
    };
    /// This over-alignment should be good enough for everything...
    pub const DATA_ALIGNMENT: usize = std::mem::align_of::<Self>();
}
