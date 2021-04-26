//! API and implementation for fixed-size free lists
//!
//! Implemented on top of linked lists

use std::ptr::NonNull;

pub mod atomic;
pub mod simple;

/// The minimum size of supported memory (in words)
///
/// Since the header takes at least one word,
/// its not really worth ever allocating less than this
pub const MINIMUM_WORDS: usize = 2;
/// The minimum size of supported memory (in bytes)
pub const MINIMUM_SIZE: usize = MINIMUM_WORDS * std::mem::size_of::<usize>();

/// We use zero as our marker value.
///
/// This is based on the assumption that the first word
/// of the object is never zero. This is guaranteed by
/// the [FreeList] API contract.
/// 
/// ## Safety
/// The first field of any allocated memory block must never be zero,
/// as that is used to mark freed memory internally. This avoids
/// any unnecessary space overhead.
///
/// For garbage collectors and language implementors
/// this should not be a problem
/// 
/// Having `field1 = 0usize` is **undefined behavior**!
///
/// If client code can innocently writes `0usize` to
/// the first word of the allocated memory,
/// the allocator will assume the slot is freed,
/// even though the client is still using it.
/// Then we will hand it
/// off to other codes, allowing multiple-ownership (UB).
///
///
/// ### Original reasoning (for simple collector)
/// GcHeader::type_info` is the first field of the header
/// and it will never be null (its a reference).
/// Therefore this marker will never conflict with a valid header.
pub const FREE_SLOT_MARKER: usize = 0;
/// A slot in the free list
///
/// This gives the slot's metadata
///
/// May or may not be marked with a [FreeSlotMarker]
#[repr(C)]
pub struct FreeSlot {
    /// Marker for the slot, initialized to `FREE_SLOT_MARKER`
    pub marker: usize,
    /// Pointer to the previous free slot
    pub(crate) prev_free: Option<NonNull<MaybeFreeSlot>>,
}
#[doc(hidden)]
#[repr(C)]
pub union MaybeFreeSlot {
    pub free: FreeSlot,
    pub memory: [u8; MINIMUM_SIZE],
}

impl MaybeFreeSlot {
    /// Check if this slot is free
    #[inline]
    pub unsafe fn is_free(&self) -> bool {
        self.free.marker == FREE_SLOT_MARKER
    }
    /// Mark the slot as free, linking it to the specified previous free slot
    #[inline]
    pub unsafe fn mark_free(&mut self, prev: Option<NonNull<MaybeFreeSlot>>) {
        debug_assert!(!self.is_free());
        self.free = FreeSlot {
            marker: FREE_SLOT_MARKER,
            prev_free: prev
        };
    }
}