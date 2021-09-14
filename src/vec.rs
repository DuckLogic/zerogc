//! Garbage collected vectors.
use core::marker::PhantomData;
use core::ops::{Deref, DerefMut};
use core::convert::{AsRef, AsMut};
use core::cell::UnsafeCell;
use core::mem::ManuallyDrop;

use inherent::inherent;
use zerogc_derive::{unsafe_gc_impl};

use crate::{CollectorId, GcRebrand, GcSafe, GcSystem, Trace};
use crate::vec::raw::{IGcVec, ReallocFailedError};

pub mod cell;
pub mod raw;

pub use self::cell::GcVecCell;

/// A uniquely owned [Vec] for use with garbage collectors.
///
/// See the [module docs](`zerogc::vec`) for details
/// on why this can not be `Copy`.
///
/// On the other hand, `Clone` can be implemented
/// although this does a *deep copy* (just like `Vec::clone`).
pub struct GcVec<'gc, T: GcSafe<'gc, Id>, Id: CollectorId> {
    /*
     * NOTE: This must be `UnsafeCell`
     * so that we can implement `TraceImmutable`
     */
    raw: UnsafeCell<Id::RawVec<'gc, T>>
}
impl<'gc, T: GcSafe<'gc, Id>, Id: CollectorId> GcVec<'gc, T, Id> {
    /// Create a [GcVec] from a [GcRawVec].
    ///
    /// ## Safety
    /// There must be no other references to the specified raw vector.
    #[inline]
    pub unsafe fn from_raw(raw: Id::RawVec<'gc, T>) -> Self {
        GcVec { raw: UnsafeCell::new(raw) }
    }
    /// Convert this vector into its underlying [GcRawVec](`zerogc::vec::repr::GcRawVec`)
    ///
    /// ## Safety
    /// Because this consumes ownership,
    /// it is safe.
    #[inline]
    pub fn into_raw(self) -> Id::RawVec<'gc, T> {
        self.raw.into_inner()
    }
    /// Slice this vector's elements
    ///
    /// NOTE: The borrow is bound to the lifetime
    /// of this vector, and not `&'gc [T]`
    /// because of the possibility of calling `as_mut_slice`
    /// 
    /// ## Safety
    /// Because this vector is uniquely owned,
    /// its length cannot be mutated while it is still borrowed.
    ///
    /// This avoids the pitfalls of calling [IGcVec::as_slice_unchecked]
    /// on a vector with shared references.
    #[inline]
    pub fn as_slice(&self) -> &'_ [T] {
        // SAFETY: We are the only owner
        unsafe { self.as_raw().as_slice_unchecked() }
    }
    /// Get a mutable slice of this vector's elements.
    ///
    /// ## Safety
    /// Because this vector is uniquely owned,
    /// the underlying contents cannot be modified
    /// while another reference is in use (because there are no other references)
    #[inline]
    pub fn as_mut_slice(&mut self) -> &'_ mut [T] {
        unsafe {
            core::slice::from_raw_parts_mut(
                self.as_mut_raw().as_mut_ptr(),
                self.len()
            )
        }
    }
    /// Get a reference to the underlying [GcRawVec](`zerogc::vec::repr::GcRawVec`),
    /// bypassing restrictions on unique ownership.
    ///
    /// ## Safety
    /// It is undefined behavior to change the length of the 
    /// raw vector while this vector is in use.
    #[inline]
    pub unsafe fn as_raw(&self) -> &Id::RawVec<'gc, T> {
        &*self.raw.get()
    }
    /// Get a mutable reference to the underlying raw vector,
    /// bypassing restrictions on unique ownership.
    ///
    /// ## Safety
    /// It is undefined behavior to change the length of the
    /// raw vector while this vector is in use.
    ///
    /// Although calling this method requires unique ownership
    /// of this vector, the raw vector is `Copy` and could
    /// be duplicated.
    #[inline]
    pub unsafe fn as_mut_raw(&mut self) -> &mut Id::RawVec<'gc, T> {
        self.raw.get_mut()
    }
    /// Iterate over the vector's contents. 
    ///
    /// ## Safety
    /// This is safe for the same reason [GcVec::as_mut_slice] is.
    #[inline]
    pub fn iter(&self) -> core::slice::Iter<'_, T> {
        self.as_slice().iter()
    }

    /// Mutably iterate over the vector's contents. 
    ///
    /// ## Safety
    /// This is safe for the same reason [GcVec::as_mut_slice] is.
    #[inline]
    pub fn iter_mut(&mut self) -> core::slice::IterMut<'_, T> {
        self.as_mut_slice().iter_mut()
    }
}
/// Because `GcVec` is uniquely owned (`!Copy`),
/// it can safely dereference to a slice
/// without risk of another reference mutating it.
impl<'gc, T: GcSafe<'gc, Id>, Id: CollectorId> Deref for GcVec<'gc, T, Id> {
    type Target = [T];
    #[inline]
    fn deref(&self) -> &[T] {
        self.as_slice()
    }
}
/// Because `GcVec` is uniquely owned (`!Copy`),
/// it can safely de-reference to a mutable slice
/// without risk of another reference mutating its contents
impl<'gc, T: GcSafe<'gc, Id>, Id: CollectorId> DerefMut for GcVec<'gc, T, Id> {
    #[inline]
    fn deref_mut(&mut self) -> &mut [T] {
        self.as_mut_slice()
    }
}
impl<'gc, T: GcSafe<'gc, Id>, Id: CollectorId> AsRef<[T]> for GcVec<'gc, T, Id> {
    #[inline]
    fn as_ref(&self) -> &[T] {
        self.as_slice()
    }
}
impl<'gc, T: GcSafe<'gc, Id>, Id: CollectorId> AsMut<[T]> for GcVec<'gc, T, Id> {
    #[inline]
    fn as_mut(&mut self) -> &mut [T] {
        self.as_mut_slice()
    }
}
#[inherent]
unsafe impl<'gc, T: GcSafe<'gc, Id>, Id: CollectorId> IGcVec<'gc, T> for GcVec<'gc, T, Id> {
    type Id = Id;

    #[inline]
    pub fn with_capacity_in(capacity: usize, ctx: &'gc <Id::System as GcSystem>::Context) -> Self {
        unsafe {
            Self::from_raw(Id::RawVec::<'gc, T>::with_capacity_in(capacity, ctx))
        }
    }

    #[inline]
    pub fn len(&self) -> usize {
        unsafe { self.as_raw() }.len()
    }

    #[inline]
    pub unsafe fn set_len(&mut self, len: usize) {
        self.as_mut_raw().set_len(len)
    }

    #[inline]
    pub fn capacity(&self) -> usize {
        unsafe { self.as_raw().capacity() }
    }

    #[inline]
    pub fn reserve_in_place(&mut self, additional: usize) -> Result<(), ReallocFailedError> {
        unsafe { self.as_mut_raw().reserve_in_place(additional) }
    }

    #[inline]
    pub unsafe fn as_ptr(&self) -> *const T {
        self.as_raw().as_ptr()
    }

    #[inline]
    pub fn context(&self) -> &'gc <Id::System as GcSystem>::Context {
        unsafe { self.as_raw().context() }
    }

    // Default methods:
    pub fn replace(&mut self, index: usize, val: T) -> T;
    pub fn set(&mut self, index: usize, val: T);
    pub fn extend_from_slice(&mut self, src: &[T])
        where T: Copy;
    pub fn push(&mut self, val: T);
    pub fn reserve(&mut self, additional: usize);
    pub fn is_empty(&self) -> bool;
    pub fn new_in(ctx: &'gc <Id::System as GcSystem>::Context) -> Self;
    pub fn copy_from_slice(src: &[T], ctx: &'gc <Id::System as GcSystem>::Context) -> Self
        where T: Copy;
    pub fn from_vec(src: Vec<T>, ctx: &'gc <Id::System as GcSystem>::Context) -> Self;
    /*
     * Intentionally hidden:
     * 1. as_slice_unchecked (just use as_slice)
     * 2. get (just use slice::get)
     * 3. as_mut_ptr (just use slice::as_mut_ptr)
     */
}
impl<'gc, T: GcSafe<'gc, Id>, Id: CollectorId> Extend<T> for GcVec<'gc, T, Id> {
    #[inline]
    fn extend<I: IntoIterator<Item = T>>(&mut self, iter: I) {
        let iter = iter.into_iter();
        self.reserve(iter.size_hint().1.unwrap_or(0));
        for val in iter {
            self.push(val);
        }
    }
}
impl<'gc, T: GcSafe<'gc, Id>, Id: CollectorId> IntoIterator for GcVec<'gc, T, Id> {
    type Item = T;

    type IntoIter = IntoIter<'gc, T, Id>;

    #[inline]
    fn into_iter(mut self) -> Self::IntoIter {
        let len = self.len();
        unsafe {
            let start = self.as_ptr();
            let end = start.add(len);
            self.set_len(0);
            IntoIter { start, end, raw: ManuallyDrop::new(self), marker: PhantomData }
        }
    }

}

/// The [GcVec] analogue of [std::vec::IntoIter]
pub struct IntoIter<'gc, T: GcSafe<'gc, Id>, Id: CollectorId> {
    start: *const T,
    end: *const T,
    raw: ManuallyDrop<GcVec<'gc, T, Id>>,
    marker: PhantomData<GcVec<'gc, T, Id>>
}
impl<'gc, T: GcSafe<'gc, Id>, Id: CollectorId> Iterator for IntoIter<'gc, T, Id> {
    type Item = T;
    #[inline]
    fn next(&mut self) -> Option<T> {
        if self.start < self.end {
            let val = self.start;
            unsafe {
                self.start = self.start.add(1);
                Some(val.read())
            }
        } else {
            None
        }
    }
    #[inline]
    fn size_hint(&self) -> (usize, Option<usize>) {
        let len = unsafe { self.end.offset_from(self.start) as usize };
        (len, Some(len))
    }
}
impl<'gc, T: GcSafe<'gc, Id>, Id: CollectorId> DoubleEndedIterator for IntoIter<'gc, T, Id> {
    #[inline]
    fn next_back(&mut self) -> Option<Self::Item> {
        if self.end > self.start {
            unsafe {
                self.end = self.end.sub(1);
                Some(self.end.read())
            }
        } else {
            None
        }
    }
}
impl<'gc, T: GcSafe<'gc, Id>, Id: CollectorId> core::iter::ExactSizeIterator for IntoIter<'gc, T, Id> {}
impl<'gc, T: GcSafe<'gc, Id>, Id: CollectorId> core::iter::FusedIterator for IntoIter<'gc, T, Id> {}
impl<'gc, T: GcSafe<'gc, Id>, Id: CollectorId> Drop for IntoIter<'gc, T, Id> {
    fn drop(&mut self) {
        for val in self.by_ref() {
            drop(val);
        }
        unsafe { ManuallyDrop::drop(&mut self.raw) };
    }
}

impl<'gc, T: GcSafe<'gc, Id> + Clone, Id: CollectorId> Clone for GcVec<'gc, T, Id> {
    #[inline]
    fn clone(&self) -> Self {
        let mut res = Self::with_capacity_in(self.len(), self.context());
        res.extend(self.iter().cloned());
        res
    }
}
/// Because it is implicitly associated with a [GcContext] (which is thread-local),
/// this must be `!Send`
impl<'gc, T, Id: CollectorId> !Send for GcVec<'gc, T, Id> {}
unsafe_gc_impl!(
    target => GcVec<'gc, T, Id>,
    params => ['gc, T: GcSafe<'gc, Id>, Id: CollectorId],
    bounds => {
        TraceImmutable => { where T: Trace },
        Trace => { where T: GcSafe<'gc, Id> + Trace },
        GcRebrand => { where T: GcRebrand<'new_gc, Id>, T::Branded: Sized },
    },
    branded_type => GcVec<'new_gc, T::Branded, Id>,
    null_trace => never,
    NEEDS_TRACE => true,
    NEEDS_DROP => <T as Trace>::NEEDS_DROP /* if our inner type needs a drop */,
    trace_mut => |self, visitor| {
        unsafe {
            visitor.trace_vec(self.as_mut_raw())
        }
    },
    trace_immutable => |self, visitor| {
        unsafe {
            visitor.trace_vec(&mut *self.raw.get())
        }
    },
    collector_id => Id
);

/// Indicates there is insufficient capacity for an operation on a [GcRawVec]
#[derive(Debug)]
pub struct InsufficientCapacityError;

