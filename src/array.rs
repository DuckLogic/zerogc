//! Defines the interface to garbage collected arrays.
use core::ops::Deref;
use core::ptr::NonNull;
use core::cmp::Ordering;
use core::str;
use core::fmt::{self, Formatter, Debug, Display};
use core::hash::{Hash, Hasher};

use crate::{CollectorId, GcSafe, GcRebrand};
use zerogc_derive::{Trace, unsafe_gc_impl};

use self::repr::GcArrayRepr;

pub mod repr;

/// A garbage collected string.
///
/// This is a transparent wrapper around `GcArray<u8>`,
/// with the additional invariant that it's utf8 encoded.
///
/// ## Safety
/// The bytes can be assumed to be UTF8 encoded,
/// just like with a `str`.
///
/// Assuming the bytes are utf8 encoded,
/// this can be transmuted back and forth from `GcArray<u8, Id>`
#[repr(transparent)]
#[derive(Trace, Eq, PartialEq, Hash, Clone, Copy)]
#[zerogc(copy, collector_ids(Id))]
pub struct GcString<'gc, Id: CollectorId> {
    bytes: GcArray<'gc, u8, Id>    
}
impl<'gc, Id: CollectorId> GcString<'gc, Id> {
    /// Convert an array of UTF8 bytes into a string.
    ///
    /// Returns an error if the bytes aren't valid UTF8,
    /// just like [core::str::from_utf8].
    #[inline]
    pub fn from_utf8(bytes: GcArray<'gc, u8, Id>) -> Result<Self, core::str::Utf8Error> {
        core::str::from_utf8(bytes.as_slice())?;
        // SAFETY: Validated with from_utf8 call
        Ok(unsafe { Self::from_utf8_unchecked(bytes) })
    }
    /// Convert an array of UTF8 bytes into a string,
    /// without checking for validity.
    ///
    /// ## Safety
    /// Undefined behavior if the bytes aren't valid
    /// UTF8, just like with [core::str::from_utf8_unchecked]
    #[inline]
    pub unsafe fn from_utf8_unchecked(bytes: GcArray<'gc, u8, Id>) -> Self {
        GcString { bytes }
    }
    /// Retrieve this string as a raw array of bytes
    #[inline]
    pub fn as_bytes(&self) -> GcArray<'gc, u8, Id> {
        self.bytes
    }
    /// Convert this string into a slice of bytes
    #[inline]
    pub fn as_str(&self) -> &'gc str {
        unsafe { str::from_utf8_unchecked(self.as_bytes().as_slice()) }
    }
}
impl<'gc, Id: CollectorId> Deref for GcString<'gc, Id> {
    type Target = str;
    #[inline]
    fn deref(&self) -> &'_ str {
        self.as_str()
    }
}
impl<'gc, Id: CollectorId> Debug for GcString<'gc, Id> {
    fn fmt(&self, f: &mut Formatter) -> fmt::Result {
        Debug::fmt(self.as_str(), f)
    }
}
impl<'gc, Id: CollectorId> Display for GcString<'gc, Id> {
    fn fmt(&self, f: &mut Formatter) -> fmt::Result {
        Display::fmt(self.as_str(), f)
    }  
}

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
impl<'gc, T, Id: CollectorId> Deref for GcArray<'gc, T, Id> {
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
impl<'gc, T: Debug, Id: CollectorId> Debug for GcArray<'gc, T, Id> {
    fn fmt(&self, f: &mut Formatter) -> fmt::Result {
        f.debug_list().entries(self.iter()).finish()
    }
}
impl<'gc, T: PartialEq, Id: CollectorId> PartialEq for GcArray<'gc, T, Id> {
    #[inline]
    fn eq(&self, other: &Self) -> bool {
        self.as_slice() == other.as_slice()
    }
}
impl<'gc, T: PartialEq, Id: CollectorId> PartialEq<[T]> for GcArray<'gc, T, Id> {
    #[inline]
    fn eq(&self, other: &[T]) -> bool {
        self.as_slice() == other
    }
}
impl<'gc, T: PartialOrd, Id: CollectorId> PartialOrd for GcArray<'gc, T, Id> {
    #[inline]
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        self.as_slice().partial_cmp(other.as_slice())
    }
}
impl<'gc, T: PartialOrd, Id: CollectorId> PartialOrd<[T]> for GcArray<'gc, T, Id> {
    #[inline]
    fn partial_cmp(&self, other: &[T]) -> Option<Ordering> {
        self.as_slice().partial_cmp(other)
    }
}
impl<'gc, T: Ord, Id: CollectorId> Ord for GcArray<'gc, T, Id> {
    #[inline]
    fn cmp(&self, other: &Self) -> Ordering {
        self.as_slice().cmp(other)
    }
}
impl<'gc, T: Eq, Id: CollectorId> Eq for GcArray<'gc, T, Id> {}
impl<'gc, T: Hash, Id: CollectorId> Hash for GcArray<'gc, T, Id> {
    #[inline]
    fn hash<H: Hasher>(&self, hasher: &mut H) {
        T::hash_slice(self.as_slice(), hasher)
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
