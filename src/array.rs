//! Defines the interface to garbage collected arrays.
use core::cmp::Ordering;
use core::fmt::{self, Debug, Display, Formatter};
use core::hash::{Hash, Hasher};
use core::marker::PhantomData;
use core::ops::{Deref, Index};
use core::ptr::NonNull;
use core::slice::SliceIndex;
use core::str;

use crate::{CollectorId, Gc, GcRebrand, GcSafe};
use zerogc_derive::{unsafe_gc_impl, Trace};

use self::repr::GcArrayPtr;

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
    bytes: GcArray<'gc, u8, Id>,
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
    pub const unsafe fn from_utf8_unchecked(bytes: GcArray<'gc, u8, Id>) -> Self {
        GcString { bytes }
    }
    /// Retrieve this string as a raw array of bytes
    #[inline]
    pub const fn as_bytes(&self) -> GcArray<'gc, u8, Id> {
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
/// This is a 'repr(transparent)' wrapper arround
/// [GcArrayRepr].
#[repr(transparent)]
pub struct GcArray<'gc, T, Id: CollectorId> {
    ptr: Id::ArrayPtr,
    marker: PhantomData<Gc<'gc, [T], Id>>,
}
impl<'gc, T, Id: CollectorId> GcArray<'gc, T, Id> {
    /// Convert this array into a slice
    #[inline]
    pub fn as_slice(&self) -> &'gc [T] {
        unsafe { core::slice::from_raw_parts(self.as_raw_ptr(), self.len()) }
    }
    /// Load a raw pointer to the array's value
    #[inline]
    pub fn as_raw_ptr(&self) -> *mut T {
        self.ptr.as_raw_ptr() as *mut T
    }
    /// Get the underlying 'Id::ArrayPtr' for this array
    ///
    /// ## Safety
    /// Must not interpret the underlying pointer as the
    /// incorrect type.
    #[inline]
    pub const unsafe fn as_internal_ptr_repr(&self) -> &'_ Id::ArrayPtr {
        &self.ptr
    }
    /// Load the length of the array
    #[inline]
    pub fn len(&self) -> usize {
        match self.ptr.len() {
            Some(len) => len,
            None => Id::resolve_array_len(self),
        }
    }
    /// Check if the array is empty
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
    /// Resolve the [CollectorId]
    #[inline]
    pub fn collector_id(&self) -> &'_ Id {
        Id::resolve_array_id(self)
    }
    /// Create an array from the specified raw pointer and length
    ///
    /// ## Safety
    /// Pointer and length must be valid, and point to a garbage collected
    /// value allocated from the corresponding [CollectorId]
    #[inline]
    pub unsafe fn from_raw_ptr(ptr: NonNull<T>, len: usize) -> Self {
        GcArray {
            ptr: Id::ArrayPtr::from_raw_parts(ptr, len),
            marker: PhantomData,
        }
    }
}
/// If the underlying type is `Sync`, it's safe
/// to share garbage collected references between threads.
///
/// The safety of the collector itself depends on whether [CollectorId] is Sync.
/// If it is, the whole garbage collection implementation should be as well.
unsafe impl<'gc, T, Id> Sync for GcArray<'gc, T, Id>
where
    T: Sync,
    Id: CollectorId + Sync,
{
}
unsafe impl<'gc, T, Id> Send for GcArray<'gc, T, Id>
where
    T: Sync,
    Id: CollectorId + Sync,
{
}
impl<'gc, T, I, Id: CollectorId> Index<I> for GcArray<'gc, T, Id>
where
    I: SliceIndex<[T]>,
{
    type Output = I::Output;
    #[inline]
    fn index(&self, idx: I) -> &I::Output {
        &self.as_slice()[idx]
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
impl<'gc, T, Id: CollectorId> IntoIterator for GcArray<'gc, T, Id>
where
    T: 'gc,
{
    type Item = &'gc T;

    type IntoIter = core::slice::Iter<'gc, T>;

    #[inline]
    fn into_iter(self) -> Self::IntoIter {
        self.as_slice().iter()
    }
}
impl<'array, 'gc, T, Id: CollectorId> IntoIterator for &'array GcArray<'gc, T, Id>
where
    T: 'array,
{
    type Item = &'array T;

    type IntoIter = core::slice::Iter<'array, T>;

    #[inline]
    fn into_iter(self) -> Self::IntoIter {
        self.as_slice().iter()
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
        unsafe { visitor.trace_array(self) }
    },
    collector_id => Id,
    visit_inside_gc => |gc, visitor| {
        visitor.trace_gc(gc)
    }
);

#[cfg(test)]
mod test {
    use crate::epsilon::{self};
    use crate::{CollectorId, GcArray};
    #[test]
    fn test_covariance<'a>() {
        fn covariant<'a, T, Id: CollectorId>(s: GcArray<'static, T, Id>) -> GcArray<'a, T, Id> {
            s as _
        }
        const SRC: &[u32] = &[1, 2, 5];
        let s: epsilon::GcArray<'static, u32> = epsilon::gc_array(SRC);
        let k: epsilon::GcArray<'a, u32> = covariant(s);
        assert_eq!(k.as_slice(), SRC);
    }
}
