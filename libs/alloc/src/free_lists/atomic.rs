//! An atomic linked list.
use std::ptr::NonNull;

use crossbeam_utils::atomic::AtomicCell;

use super::{FREE_SLOT_MARKER, MaybeFreeSlot};

/// The free list
///
/// This is a lock-free linked list
pub struct AtomicFreeList {
    next: AtomicCell<Option<NonNull<MaybeFreeSlot>>>,
    size: usize,
}
impl AtomicFreeList {
    /// Create a new free list
    #[inline]
    pub const fn new(size: usize) -> AtomicFreeList {
        assert!(size >= super::MINIMUM_SIZE, "Invalid size");
        AtomicFreeList { size, next: AtomicCell::new(None) }
    }
    /// The fixed size of entries in the free list
    #[inline]
    pub fn size(&self) -> usize {
        self.size
    }
    /// Peek at the next free slot
    #[inline]
    pub fn next_free(&self) -> Option<NonNull<MaybeFreeSlot>> {
        self.next.load()
    }
    /// Set the next free slot in the list
    #[inline]
    pub unsafe fn set_next_free(&self, next: Option<NonNull<MaybeFreeSlot>>) {
        self.next.store(next)
    }
    /// Take the next free slot
    #[inline]
    pub fn take_free(&self) -> Option<NonNull<u8>> {
        loop {
            let next_free = match self.next.load() {
                Some(free) => free,
                None => return None, // Out of free space
            };
            // Update free pointer
            unsafe {
                if self.next.compare_exchange(
                    Some(next_free),
                    next_free.as_ref().free.prev_free
                ).is_err() { continue /* retry */ }
                debug_assert_eq!(
                    next_free.as_ref().free.marker,
                    FREE_SLOT_MARKER
                );
                return Some(NonNull::from(&next_free.as_ref().memory).cast())
            }
        }
    }
}
