use std::ptr::NonNull;
use std::alloc::Layout;

#[cfg(feature = "epsilon-arena-alloc")]
mod arena;

pub trait EpsilonAlloc {
    fn new() -> Self;
    fn alloc_layout(&self, layout: Layout) -> NonNull<u8>;
    unsafe fn free_alloc(&self, target: NonNull<u8>, layout: Layout);
    const NEEDS_EXPLICIT_FREE: bool;
}

#[cfg(feature = "epsilon-arena-alloc")]
pub type Default = arena::BumpEpsilonAlloc;
#[cfg(not(feature = "epsilon-arena-alloc"))]
pub type Default = StdEpsilonAlloc;

pub struct StdEpsilonAlloc;
impl EpsilonAlloc for StdEpsilonAlloc {
    #[inline]
    fn new() -> Self {
        StdEpsilonAlloc
    }

    #[inline]
    fn alloc_layout(&self, layout: Layout) -> NonNull<u8> {
        const EMPTY: &[u8] = b"";
        if layout.size() == 0 {
            return NonNull::from(EMPTY).cast();
        }
        // SAFETY: We checked for layout.size() == 0
        NonNull::new(unsafe { std::alloc::alloc(layout) })
            .unwrap_or_else(|| std::alloc::handle_alloc_error(layout))
    }

    #[inline]
    unsafe fn free_alloc(&self, target: NonNull<u8>, layout: Layout) {
        if layout.size() == 0 {
            return; // We returned our dummy empty alloc
        }
        std::alloc::dealloc(target.as_ptr(), layout)
    }

    const NEEDS_EXPLICIT_FREE: bool = true;
}

