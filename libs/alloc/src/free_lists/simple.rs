//! A simple free list

use std::ptr::NonNull;
use std::cell::Cell;

use super::{MaybeFreeSlot, FREE_SLOT_MARKER};

/// A simple free list
pub struct SimpleFreeList {
    next: Cell<Option<NonNull<MaybeFreeSlot>>>,
    size: usize
}
impl SimpleFreeList {
    /// Create a new free list
    #[inline]
    pub const fn new(size: usize) -> SimpleFreeList {
        assert!(size >= super::MINIMUM_SIZE);
        SimpleFreeList { next: Cell::new(None), size }
    }
    /// The fixed size of entries in the free list
    #[inline]
    pub fn size(&self) -> usize {
        self.size
    }
    /// Peek at the next free slot
    #[inline]
    pub fn next_free(&self) -> Option<NonNull<MaybeFreeSlot>> {
        self.next.get()
    }
    /// Set the next free slot in the list
    #[inline]
    pub unsafe fn set_next_free(&self, next: Option<NonNull<MaybeFreeSlot>>) {
        self.next.set(next)
    }
    /// Take the next free slot
    #[inline]
    pub fn take_free(&self) -> Option<NonNull<u8>> {
        let next_free = match self.next.get() {
            Some(free) => free,
            None => return None, // Out of free space
        };
        unsafe {
            let prev_free = next_free.as_ref().free.prev_free;
            self.next.set(prev_free);
            debug_assert_eq!(
                next_free.as_ref().free.marker,
                FREE_SLOT_MARKER
            );
            return Some(NonNull::from(&next_free.as_ref().memory).cast());
        }
    }
}
