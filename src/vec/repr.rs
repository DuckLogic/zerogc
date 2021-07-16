//! The underlying representation of a [GcVec]
//!
//! This is exposed only for use by collector implemetnations.
//! User code should avoid it.

use core::ffi::c_void;
use core::alloc::Layout;

use crate::{GcSafe,};

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
pub unsafe trait GcVecRepr: GcSafe {
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
    fn realloc_in_place(&self, new_capacity: usize) -> Result<(), ReallocFailedError> {
        assert!(!Self::SUPPORTS_REALLOC);
        drop(new_capacity);
        Err(ReallocFailedError::Unsupported)
    }
    /// A pointer to the underlying memory
    ///
    /// ## Safety
    /// This is marked unsafe, because the type must be interpreted correctly.
    ///
    /// The returned memory must not be mutated, because it may have multiple owners.
    unsafe fn ptr(&self) -> *const c_void;
    /// Drop the values in the vector, assuming that
    ///
    /// The given generic parameter is the actual type of
    ///
    /// ## Safety
    /// Undefined behavior if vector is in use.
    /// Undefined behavior if the passed type is incorrect.
    unsafe fn unchecked_drop<T: GcSafe>(&mut self);
}
/// Dummy implementation of [GcVecRepr] for collectors which do not support [GcVec]
pub enum Unsupported {}
unsafe_trace_primitive!(Unsupported);
unsafe impl GcVecRepr for Unsupported {
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

    unsafe fn unchecked_drop<T: GcSafe>(&mut self) {
        unimplemented!()
    }
}