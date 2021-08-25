//! The underlying representation of a [GcVec]
//!
//! This is exposed only for use by collector implemetnations.
//! User code should avoid it.

use core::marker::PhantomData;
use core::ffi::c_void;
use core::alloc::Layout;

use crate::{GcSafe, CollectorId};

use zerogc_derive::unsafe_gc_impl;

/// A marker error to indicate in-place reallocation failed
#[derive(Debug)]
pub enum ReallocFailedError {
    /// Indicates that the operation is unsupported
    Unsupported,
    /// Indicates that the vector is too large to reallocate in-place
    SizeUnsupported,
    /// Indicates that the garbage collector is out of memory
    OutOfMemory,
}

/// The underlying representation of a [RawGcVec]
///
/// This varies from collector to collector.
///
/// ## Safety
/// This must be implemented consistent with the API of [RawGcVec].
///
/// It should only be used by [RawGcVec] and [GcVec].
pub unsafe trait GcVecRepr<'gc>: GcSafe<'gc, Self::Id> {
    /// The id of the collector
    type Id: CollectorId;
    /// Whether this vector supports in-place reallocation.
    const SUPPORTS_REALLOC: bool = false;
    /// The layout of the underlying element type
    fn element_layout(&self) -> Layout;
    /// The length of the vector.
    ///
    /// This is the number of elements that are actually
    /// initialized, as opposed to `capacity`, which is the number
    /// of elements that are available in total.
    fn len(&self) -> usize;
    /// Check if this vector is empty
    #[inline]
    fn is_empty(&self) -> bool {
        self.len() == 0
    }
    /// Set the length of the vector.
    ///
    /// ## Safety
    /// The underlying memory must be initialized up to the specified length,
    /// otherwise the vector's memory will be traced incorrectly.
    ///
    /// Undefined behavior if length is greater than capacity.
    unsafe fn set_len(&self, len: usize);
    /// The total number of elements that are available
    fn capacity(&self) -> usize;
    /// Attempt to reallocate the vector in-place,
    /// without moving the underlying pointer.
    fn realloc_in_place(&self, _new_capacity: usize) -> Result<(), ReallocFailedError> {
        assert!(!Self::SUPPORTS_REALLOC);
        Err(ReallocFailedError::Unsupported)
    }
    /// A pointer to the underlying memory
    ///
    /// ## Safety
    /// This is marked unsafe, because the type must be interpreted correctly.
    ///
    /// The returned memory must not be mutated, because it may have multiple owners.
    unsafe fn ptr(&self) -> *const c_void;
}
/// Dummy implementation of [GcVecRepr] for collectors which do not support [GcVec]
pub struct Unsupported<'gc, Id: CollectorId> {
    /// The marker `PhantomData` needed to construct this type
    pub marker: PhantomData<(Id, &'gc ())>,
    /// indicates this type should never exist at runtime
    // TODO: Replace with `!` once stabilized
    pub never: std::convert::Infallible
}
unsafe_gc_impl! {
    target => Unsupported<'gc, Id>,
    params => ['gc, Id: CollectorId],
    bounds => {
        GcSafe => always,
        GcRebrand => always,
    },
    null_trace => always, // TODO: Should we really `!NEEDS_TRACE`?
    branded_type => Unsupported<'new_gc, Id>,
    NEEDS_TRACE => false,
    NEEDS_DROP => false,
    collector_id => Id,
    visit => |self, visitor| { /* nop */ Ok(()) }
}
unsafe impl<'gc, Id: CollectorId> GcVecRepr<'gc> for Unsupported<'gc, Id> {
    type Id = Id;

    fn element_layout(&self) -> Layout {
        unimplemented!()
    }

    fn len(&self) -> usize {
        unimplemented!()
    }

    unsafe fn set_len(&self, _len: usize) {
        unimplemented!()
    }

    fn capacity(&self) -> usize {
        unimplemented!()
    }

    unsafe fn ptr(&self) -> *const c_void {
        unimplemented!()
    }
}