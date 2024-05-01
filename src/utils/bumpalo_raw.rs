use bumpalo::{AllocErr, Bump};
use std::alloc::Layout;
use std::marker::PhantomData;
use std::ptr::NonNull;

use crate::utils::Alignment;

pub struct BumpAllocRaw<Config: BumpAllocRawConfig> {
    inner: Bump,
    marker: PhantomData<Config>,
}
impl<Config: BumpAllocRawConfig> BumpAllocRaw<Config> {
    #[inline(always)]
    pub fn try_alloc_layout(&self, layout: Layout) -> Result<NonNull<u8>, AllocErr> {
        assert_eq!(layout.align(), Config::FIXED_ALIGNMENT.value());
        self.inner.try_alloc_layout(layout)
    }

    #[inline]
    pub unsafe fn iter_allocated_chunks_raw(&self) -> bumpalo::ChunkRawIter<'_> {
        self.inner.iter_allocated_chunks_raw()
    }
}

pub trait BumpAllocRawConfig {
    const FIXED_ALIGNMENT: Alignment;
}
