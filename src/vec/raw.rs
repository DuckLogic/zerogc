//! The underlying representation of a [GcVec](`crate::vec::GcVec`)

use core::iter::Iterator;
use core::marker::PhantomData;

use crate::{CollectorId, GcRebrand, GcSafe};

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

/// Basic methods shared across all garbage collected vectors.
///
/// All garbage collectors are implicitly
/// associated with their owning [GcContext](`crate::GcContext`).
///
/// This can be changed by calling `detatch`,
/// although this is currently unimplemented.
///
/// ## Safety
/// Undefined behavior if the methods
/// violate their contracts.
///
/// All of the methods in this trait
/// can be trusted to be implemented correctly.
pub unsafe trait IGcVec<'gc, T: GcSafe<'gc, Self::Id>>: Sized + Extend<T> {
    /// The id of the collector
    type Id: CollectorId;
    /// Copy the specified elements into a new vector,
    /// allocating it in the specified context
    ///
    /// This consumes ownership of the original vector.
    #[inline]
    fn from_vec(mut src: Vec<T>, ctx: &'gc <Self::Id as CollectorId>::Context) -> Self {
        let len = src.len();
        let mut res = Self::with_capacity_in(len, ctx);
        unsafe {
            res.as_mut_ptr().copy_from_nonoverlapping(src.as_ptr(), len);
            src.set_len(0);
            res.set_len(len);
            res
        }
    }
    /// Copy the specified elements into a new vector,
    /// allocating it in the specified context
    #[inline]
    fn copy_from_slice(src: &[T], ctx: &'gc <Self::Id as CollectorId>::Context) -> Self
    where
        T: Copy,
    {
        let len = src.len();
        let mut res = Self::with_capacity_in(len, ctx);
        unsafe {
            res.as_mut_ptr().copy_from_nonoverlapping(src.as_ptr(), len);
            res.set_len(len);
            res
        }
    }
    /// Allocate a new vector inside the specified context
    #[inline]
    fn new_in(ctx: &'gc <Self::Id as CollectorId>::Context) -> Self {
        Self::with_capacity_in(0, ctx)
    }
    /// Allocate a new vector with the specified capacity,
    /// using the specified context.
    fn with_capacity_in(capacity: usize, ctx: &'gc <Self::Id as CollectorId>::Context) -> Self;
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
    /// All the same restrictions apply as [Vec::set_len].
    ///
    /// By the time of the next garbage collection,
    /// The underlying memory must be initialized up to the specified length,
    /// otherwise the vector's memory will be traced incorrectly.
    ///
    /// Undefined behavior if length is greater than capacity.
    unsafe fn set_len(&mut self, len: usize);
    /// The total amount of space that is available
    /// without needing reallocation.
    fn capacity(&self) -> usize;
    /// Attempt to reallocate the vector in-place,
    /// without moving the underlying pointer.
    fn reserve_in_place(&mut self, additional: usize) -> Result<(), ReallocFailedError>;
    /// Reserves capacity for at least `additional`.
    ///
    /// If this type is a [GcVecCell](`crate::vec::cell::GcVecCell`) and there are outstanding borrows references,
    /// then this will panic as if calling [`RefCell::borrow_mut`](`core::cell::RefCell::borrow_mut`).
    ///
    /// ## Safety
    /// This method is safe.
    ///
    /// If this method finishes without panicking,
    /// it can be assumed that `new_capacity - old_capcity >= additional`
    #[inline]
    fn reserve(&mut self, additional: usize) {
        // See https://github.com/rust-lang/rust/blob/c7dbe7a8301/library/alloc/src/raw_vec.rs#L402
        if additional > self.capacity().wrapping_sub(self.len()) {
            grow_vec(self, additional);
        }
    }

    /// Push the specified element onto the end
    /// of this vector.
    ///
    /// If this type is a [GcVecCell](`crate::vec::GcVecCell`) and there are outstanding borrows references,
    /// then this will panic as if calling [`RefCell::borrow_mut`](`core::cell::RefCell::borrow_mut`).
    ///
    /// ## Safety
    /// This method is safe.
    #[inline]
    fn push(&mut self, val: T) {
        let old_len = self.len();
        // TODO: Write barriers
        self.reserve(1);
        unsafe {
            // NOTE: This implicitly calls `borrow_mut` if we are a `GcVecCell`
            self.as_mut_ptr().add(old_len).write(val);
            self.set_len(old_len + 1);
        }
    }
    /// Pop an element of the end of the vector,
    /// returning `None` if empty.
    ///
    /// This is analogous to [`Vec::pop`]
    ///
    /// If this type is a [GcVecCell](`crate::vec::GcVecCell`) and there are outstanding borrowed references,
    /// this will panic as if calling [`RefCell::borrow_mut`](`core::cell::RefCell::borrow_mut`).
    ///
    /// ## Safety
    /// This method is safe.
    #[inline]
    fn pop(&mut self) -> Option<T> {
        let old_len = self.len();
        if old_len > 0 {
            // TODO: Write barriers?
            // NOTE: This implicitly calls `borrow_mut` if we are a `GcVecCell`
            unsafe {
                self.set_len(old_len - 1);
                Some(self.as_ptr().add(old_len - 1).read())
            }
        } else {
            None
        }
    }
    /// Removes an element from the vector and returns it.
    ///
    /// The removed element is replaced by the last element in the vector.
    ///
    /// This doesn't preserve ordering, but it is `O(1)`
    #[inline]
    fn swap_remove(&mut self, index: usize) -> T {
        let len = self.len();
        assert!(index < len);
        unsafe {
            let last = core::ptr::read(self.as_ptr().add(len - 1));
            let hole = self.as_mut_ptr().add(index);
            self.set_len(len - 1);
            core::ptr::replace(hole, last)
        }
    }
    /// Extend the vector with elements copied from the specified slice
    #[inline]
    fn extend_from_slice(&mut self, src: &[T])
    where
        T: Copy,
    {
        let old_len = self.len();
        self.reserve(src.len());
        // TODO: Write barriers?
        unsafe {
            self.as_mut_ptr()
                .add(old_len)
                .copy_from_nonoverlapping(src.as_ptr(), src.len());
            self.set_len(old_len + src.len());
        }
    }
    /// Get the item at the specified index,
    /// or `None` if it is out of bounds.
    ///
    /// This returns a copy of the value.
    /// As such it requires `T: Copy`
    #[inline]
    fn get(&self, idx: usize) -> Option<T>
    where
        T: Copy,
    {
        unsafe { self.as_slice_unchecked().get(idx).copied() }
    }
    /// Set the item at the specified index
    ///
    /// Panics if the index is out of bounds.
    #[inline]
    fn set(&mut self, index: usize, val: T) {
        assert!(index < self.len());
        unsafe {
            *self.as_mut_ptr().add(index) = val;
        }
    }
    /// Replace the item at the specified index,
    /// returning the old value.
    ///
    /// For a traditional `Vec`,
    /// the equivalent would be `mem::replace(&mut v[index], new_value)`.
    #[inline]
    fn replace(&mut self, index: usize, val: T) -> T {
        assert!(index < self.len());
        unsafe { core::ptr::replace(self.as_mut_ptr(), val) }
    }
    /// Get a pointer to this vector's
    /// underling data.
    ///
    /// This is unsafe for the same
    ///
    /// ## Safety
    /// Undefined behavior if the pointer is used
    /// are used after the length changes.
    unsafe fn as_ptr(&self) -> *const T;
    /// Get a mutable pointer to the vector's underlying data.
    ///
    /// This is unsafe for the same reason [IGcVec::as_ptr]
    /// and
    ///
    /// ## Safety
    /// Undefined behavior if the pointer is used
    /// after the length changes.
    ///
    /// Undefined behavior if the elements
    /// are mutated while there are multiple outstanding references.
    #[inline]
    unsafe fn as_mut_ptr(&mut self) -> *mut T {
        self.as_ptr() as *mut T
    }
    /// Get a slice of this vector's elements.
    ///
    /// ## Safety
    /// If this type has shared references (multiple owners),
    /// then it is undefined behavior to mutate the length
    /// while the slice is still borrowed.
    ///
    /// In other words, the following is invalid:
    /// ```no_run
    /// # use zerogc::epsilon::{EpsilonCollectorId, EpsilonContext};
    /// # use zerogc::vec::{GcVecCell};
    /// # fn do_ub(context: &EpsilonContext) {
    /// let mut v = GcVecCell::<i32, EpsilonCollectorId>::new_in(context);
    /// v.push(23);
    /// let mut copy = v;
    /// let slice = unsafe { v.as_slice_unchecked() };
    /// copy.push(15);
    /// slice[0]; // UB
    /// # }
    /// ```
    #[inline]
    unsafe fn as_slice_unchecked(&self) -> &[T] {
        core::slice::from_raw_parts(self.as_ptr(), self.len())
    }
    /// Get the [GcContext](`crate::GcContext`) that this vector is associated with.
    ///
    /// Because each vector is implicitly associated with a [GcContext](`crate::GcContext`) (which is thread-local),
    /// vectors are `!Send` unless you call `detatch`.
    fn context(&self) -> &'gc <Self::Id as CollectorId>::Context;
}

/// Slow-case for `reserve`, when reallocation is actually needed
#[cold]
fn grow_vec<'gc, T, V>(vec: &mut V, amount: usize)
where
    V: IGcVec<'gc, T>,
    T: GcSafe<'gc, V::Id>,
{
    match vec.reserve_in_place(amount) {
        Ok(()) => {
            return; // success
        }
        Err(ReallocFailedError::OutOfMemory) => panic!("Out of memory"),
        Err(ReallocFailedError::Unsupported) | Err(ReallocFailedError::SizeUnsupported) => {
            // fallthrough to realloc
        }
    }
    let requested_capacity = vec.len().checked_add(amount).unwrap();
    let new_capacity = vec
        .capacity()
        .checked_mul(2)
        .unwrap()
        .max(requested_capacity);
    // Just allocate a new one, copying from the old
    let mut new_mem = V::with_capacity_in(new_capacity, vec.context());
    // TODO: Write barriers
    unsafe {
        new_mem
            .as_mut_ptr()
            .copy_from_nonoverlapping(vec.as_ptr(), vec.len());
        new_mem.set_len(vec.len());
        let mut old_mem = core::mem::replace(vec, new_mem);
        old_mem.set_len(0); // We don't want to drop the old elements
        assert!(vec.capacity() >= requested_capacity);
    }
}

/// A garbage collected vector
/// with unchecked interior mutability.
///
/// See the [module docs](`zerogc::vec`) for
/// more details on the distinctions between vector types.
///
/// ## Interior Mutability
/// TLDR: This is the [Cell](core::cell::Cell)
/// equivalent of [GcVecCell](zerogc::vec::GcVecCell).
///
/// It only gives/accepts owned values `T`, and cannot safely
/// give out references like `&T` or `&[T]`.
///
/// Interior mutability is necessary because garbage collection
/// allows shared references, and the length can be mutated
/// by one reference while another reference is in use.
///
/// Unlike `UnsafeCell`, not *all* accesses to this are `unsafe`.
/// Calls to `get`, `set`, and `push` are perfectly safe because they take/return `T`, not `&T`.
/// Only calls to `get_slice_unchecked` are `unsafe`.
///
/// NOTE: This is completely different from the distinction between
/// [Vec] and [RawVec](https://github.com/rust-lang/rust/blob/master/library/alloc/src/raw_vec.rs)
/// in the standard library.
/// In particular, this still contains a `len`.
///
/// ## Safety
/// This must be implemented consistent with the API of [GcRawVec].
///
/// It is undefined behavior to take a slice of the elements
/// while the length is being mutated.
///
/// This type is `!Send`,
/// because it is implicitly associated
/// with the [GcContext](`crate::GcContext`) it was allocated in.
///
/// Generally speaking,
/// [GcVec](`crate::vec::GcVec`) and [GcVecCell](`crate::vec::GcVecCell`) should be preferred.
pub unsafe trait GcRawVec<'gc, T: GcSafe<'gc, Self::Id>>: Copy + IGcVec<'gc, T> {
    /// Steal ownership of this vector, converting it into a [GcArray].
    ///
    /// Assumes that there are no other active references.
    ///
    /// This avoids copying memory if at all possible
    /// (although it might be necessary, depending on the implementation).
    ///
    /// ## Safety
    /// Undefined behavior if there are any other references
    /// to the vector or any uses after it has been stolen.
    ///
    /// This may or may not be debug-checked,
    /// depending on the implementation.
    ///
    /// This logically steals ownership of this vector and invalidates all other references to it,
    /// although the safety of it cannot be checked statically.
    unsafe fn steal_as_array_unchecked(self) -> crate::array::GcArray<'gc, T, Self::Id>;
    /// Iterate over the elements of the vectors
    ///
    /// Panics if the length changes.
    ///
    /// Because this copies the elements,
    /// it requires `T: Copy`
    #[inline]
    fn iter(&self) -> RawVecIter<'gc, T, Self>
    where
        T: Copy,
    {
        RawVecIter {
            target: *self,
            marker: PhantomData,
            index: 0,
            end: self.len(),
            original_len: self.len(),
        }
    }
}

/// An iterator over a [GcRawVec]
///
/// Because the length may change,
/// this iterates over the indices
/// and requires that `T: Copy`.
///
/// It is equivalent to the following code:
/// ```ignore
/// for idx in 0..vec.len() {
///     yield vec.get(idx);
/// }
/// ```
///
/// It will panic if the length changes,
/// either increasing or decreasing.
pub struct RawVecIter<'gc, T: GcSafe<'gc, V::Id> + Copy, V: GcRawVec<'gc, T>> {
    target: V,
    marker: PhantomData<crate::Gc<'gc, T, V::Id>>,
    index: usize,
    /// The end of the iterator
    ///
    /// Typically this will be `original_len`,
    /// although this can change if using `DoubleEndedIterator::next_back`
    end: usize,
    original_len: usize,
}
impl<'gc, T: GcSafe<'gc, V::Id> + Copy, V: GcRawVec<'gc, T>> RawVecIter<'gc, T, V> {
    /// Return the original length of the vector
    /// when iteration started.
    ///
    /// The iterator should panic if this changes.
    #[inline]
    pub fn original_len(&self) -> usize {
        self.original_len
    }
    /// Check that the vector's length is unmodified.
    #[inline]
    #[track_caller]
    fn check_unmodified_length(&self) {
        assert_eq!(
            self.target.len(),
            self.original_len,
            "Vector length mutated during iteration"
        );
    }
}
impl<'gc, T: GcSafe<'gc, V::Id> + Copy, V: GcRawVec<'gc, T>> Iterator for RawVecIter<'gc, T, V> {
    type Item = T;
    #[inline]
    #[track_caller]
    fn next(&mut self) -> Option<T> {
        self.check_unmodified_length();
        if self.index < self.end {
            self.index += 1;
            Some(unsafe { self.target.get(self.index - 1).unwrap_unchecked() })
        } else {
            None
        }
    }

    #[inline]
    fn size_hint(&self) -> (usize, Option<usize>) {
        let len = self.end - self.index;
        (len, Some(len))
    }

    #[inline]
    fn count(self) -> usize
    where
        Self: Sized,
    {
        self.len()
    }
}
impl<'gc, T: GcSafe<'gc, V::Id> + Copy, V: GcRawVec<'gc, T>> DoubleEndedIterator
    for RawVecIter<'gc, T, V>
{
    #[inline]
    fn next_back(&mut self) -> Option<Self::Item> {
        self.check_unmodified_length();
        if self.end > self.index {
            self.end -= 1;
            Some(unsafe { self.target.get(self.end).unwrap_unchecked() })
        } else {
            None
        }
    }
}
impl<'gc, T: GcSafe<'gc, V::Id> + Copy, V: GcRawVec<'gc, T>> ExactSizeIterator
    for RawVecIter<'gc, T, V>
{
}
impl<'gc, T: GcSafe<'gc, V::Id> + Copy, V: GcRawVec<'gc, T>> core::iter::FusedIterator
    for RawVecIter<'gc, T, V>
{
}
/// Dummy implementation of [GcRawVec] for collectors
/// which do not support [GcVec](`crate::vec::GcVec`)
pub struct Unsupported<'gc, T, Id: CollectorId> {
    /// The marker `PhantomData` needed to construct this type
    pub marker: PhantomData<crate::Gc<'gc, [T], Id>>,
    /// indicates this type should never exist at runtime
    // TODO: Replace with `!` once stabilized
    pub never: core::convert::Infallible,
}
unsafe_gc_impl! {
    target => Unsupported<'gc, T, Id>,
    params => ['gc, T: GcSafe<'gc, Id>, Id: CollectorId],
    bounds => {
        GcSafe => always,
        GcRebrand => { where T: GcRebrand<'new_gc, Id>, T::Branded: Sized },
    },
    /*
     * NOTE: We are *not* NullTrace,
     * because we don't want users to rely
     * on that and then have trouble switching away later
     */
    null_trace => never,
    branded_type => Unsupported<'new_gc, T::Branded, Id>,
    NEEDS_TRACE => false,
    NEEDS_DROP => false,
    collector_id => Id,
    trace_template => |self, visitor| { /* nop */ Ok(()) }
}
impl<'gc, T: GcSafe<'gc, Id>, Id: CollectorId> Extend<T> for Unsupported<'gc, T, Id> {
    fn extend<I: IntoIterator<Item = T>>(&mut self, _iter: I) {
        unimplemented!()
    }
}
impl<'gc, T: GcSafe<'gc, Id>, Id: CollectorId> Copy for Unsupported<'gc, T, Id> {}
impl<'gc, T: GcSafe<'gc, Id>, Id: CollectorId> Clone for Unsupported<'gc, T, Id> {
    fn clone(&self) -> Self {
        *self
    }
}

unsafe impl<'gc, T: GcSafe<'gc, Id>, Id: CollectorId> GcRawVec<'gc, T> for Unsupported<'gc, T, Id> {
    unsafe fn steal_as_array_unchecked(self) -> crate::GcArray<'gc, T, Id> {
        unimplemented!()
    }
}
#[allow(unused_variables)]
unsafe impl<'gc, T: GcSafe<'gc, Id>, Id: CollectorId> IGcVec<'gc, T> for Unsupported<'gc, T, Id> {
    type Id = Id;

    fn len(&self) -> usize {
        unimplemented!()
    }

    unsafe fn set_len(&mut self, _len: usize) {
        unimplemented!()
    }

    fn capacity(&self) -> usize {
        unimplemented!()
    }

    fn with_capacity_in(capacity: usize, ctx: &'gc <Id as CollectorId>::Context) -> Self {
        unimplemented!()
    }

    fn reserve_in_place(&mut self, additional: usize) -> Result<(), ReallocFailedError> {
        unimplemented!()
    }

    fn reserve(&mut self, additional: usize) {
        unimplemented!()
    }

    unsafe fn as_ptr(&self) -> *const T {
        unimplemented!()
    }

    fn context(&self) -> &'gc Id::Context {
        unimplemented!()
    }
}

/// Drains a [`GcRawVec`] by repeatedly calling `IGcVec::swap_remove`
///
/// Panics if the length is modified while iteration is in progress.
pub struct DrainRaw<'a, 'gc, T: GcSafe<'gc, V::Id>, V: GcRawVec<'gc, T>> {
    start: usize,
    expected_len: usize,
    target: &'a mut V,
    marker: PhantomData<crate::Gc<'gc, T, V::Id>>,
}
impl<'a, 'gc, T: GcSafe<'gc, V::Id>, V: GcRawVec<'gc, T>> Iterator for DrainRaw<'a, 'gc, T, V> {
    type Item = T;
    #[inline]
    fn next(&mut self) -> Option<T> {
        assert_eq!(
            self.expected_len,
            self.target.len(),
            "Length changed while iteration was in progress"
        );
        if self.start < self.expected_len {
            self.expected_len -= 1;
            Some(self.target.swap_remove(self.start))
        } else {
            None
        }
    }
    #[inline]
    fn size_hint(&self) -> (usize, Option<usize>) {
        let len = self.expected_len - self.start;
        (len, Some(len))
    }
}
impl<'a, 'gc, T: GcSafe<'gc, V::Id>, V: GcRawVec<'gc, T>> DoubleEndedIterator
    for DrainRaw<'a, 'gc, T, V>
{
    fn next_back(&mut self) -> Option<Self::Item> {
        if self.start < self.expected_len {
            self.expected_len -= 1;
            Some(self.target.pop().unwrap())
        } else {
            None
        }
    }
}
impl<'a, 'gc, T: GcSafe<'gc, V::Id>, V: GcRawVec<'gc, T>> Drop for DrainRaw<'a, 'gc, T, V> {
    fn drop(&mut self) {
        for val in self.by_ref() {
            drop(val);
        }
    }
}
