use std::alloc::Layout;
use std::ptr::NonNull;

use bumpalo::Bump;

use super::EpsilonAlloc;

pub struct BumpEpsilonAlloc(Bump);
impl EpsilonAlloc for BumpEpsilonAlloc {
    #[inline]
    fn new() -> Self {
        BumpEpsilonAlloc(Bump::new())
    }
    #[inline]
    fn alloc_layout(&self, layout: Layout) -> NonNull<u8> {
        self.0.alloc_layout(layout)
    }
    #[inline]
    unsafe fn free_alloc(&self, _target: NonNull<u8>, _layout: Layout) {}
    const NEEDS_EXPLICIT_FREE: bool = false;
}
