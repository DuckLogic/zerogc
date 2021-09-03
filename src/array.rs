//! Defines the interface to garbage collected arrays.
use core::ops::Deref;
use core::ptr::NonNull;

use crate::{CollectorId, GcSafe, GcRebrand};
use zerogc_derive::unsafe_gc_impl;

use self::repr::GcArrayRepr;

pub mod repr;

/// A garbage collected array.
///
/// The length is immutable and cannot change
/// once it has been allocated.
///
/// ## Safety
/// This is a `#[repr(transparent)]` wrapper around
/// [GcArrayRepr].
#[repr(transparent)]
pub struct GcArray<'gc, T: 'gc, Id: CollectorId> {
    repr: Id::ArrayRepr<'gc, T>
}
impl<'gc, T: GcSafe<'gc, Id>, Id: CollectorId> GcArray<'gc, T, Id> {
    /// Create an array from the specified raw pointer and length
    ///
    /// ## Safety
    /// Pointer and length must be valid, and point to a garbage collected
    /// value allocated from the corresponding [CollectorId]
    #[inline]
    pub unsafe fn from_raw_ptr(ptr: NonNull<T>, len: usize) -> Self {
        GcArray { repr: Id::ArrayRepr::<'gc, T>::from_raw_parts(ptr, len) }
    }
}
// Relax T: GcSafe bound
impl<'gc, T, Id: CollectorId> GcArray<'gc, T, Id> {
    /// The value of the array as a slice
    #[inline]
    pub fn as_slice(self) -> &'gc [T] {
        self.repr.as_slice()
    }
    /// Load a raw pointer to the array's value
    #[inline]
    pub fn as_raw_ptr(self) -> *mut T {
        self.repr.as_raw_ptr()
    }
    /// Load the length of the array
    #[inline]
    pub fn len(&self) -> usize {
        self.repr.len()
    }
    /// Check if the array is empty
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.repr.is_empty()
    }
    /// Resolve the [CollectorId]
    #[inline]
    pub fn collector_id(&self) -> &'_ Id {
        Id::resolve_array_id(&self.repr)
    }
    /// Get access to the array's underlying representation. 
    #[inline]
    pub fn as_raw_repr(&self) -> &Id::ArrayRepr<'gc, T> {
        &self.repr
    }
}
impl<'gc, T: GcSafe<'gc, Id>, Id: CollectorId> Deref for GcArray<'gc, T, Id> {
    type Target = [T];

    #[inline]
    fn deref(&self) -> &Self::Target {
        self.as_slice()
    }
}
impl<'gc, T, Id: CollectorId> Copy for GcArray<'gc, T, Id> {}
impl<'gc, T, Id: CollectorId> Clone for GcArray<'gc, T, Id> {
    #[inline]
    fn clone(&self) -> Self {
        *self
    }
}
// Need to implement by hand, because [T] is not GcRebrand
unsafe_gc_impl!(
    target => GcArray<'gc, T, Id>,
    params => ['gc, T: GcSafe<'gc, Id>, Id: CollectorId],
    bounds => {
        TraceImmutable => never,
        GcRebrand => { where T: GcRebrand<'new_gc, Id>, <T as GcRebrand<'new_gc, Id>>::Branded: Sized + GcSafe<'new_gc, Id> },
    },
    null_trace => never,
    branded_type => GcArray<'new_gc, <T as GcRebrand<'new_gc, Id>>::Branded, Id>,
    NEEDS_TRACE => true,
    NEEDS_DROP => false,
    trace_mut => |self, visitor| {
        unsafe { visitor.visit_array(self) }
    },
    collector_id => Id,
    visit_inside_gc => |gc, visitor| {
        visitor.visit_gc(gc)
    }
);
