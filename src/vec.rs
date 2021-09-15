//! Garbage collected vectors.
//!
//! There are three different vector types in zerogc
//! 1. `GcVec` - Requires unique ownership `!Copy`,
//!      but coerces directly to a slice and implements `Index`
//! 2. `GcVecCell` - Is `Copy`, but uses a [RefCell] to guard coercion to a slice along with all other accesses.
//!     - Since every operation implicitly calls `borrow`, *any* operation may panic.
//! 3. `GcRawVec` - Is `Copy`, but coercion to slice is `unsafe`.
//!    - Iteration implicitly panics if the length changes.
//!
//! Here's a table comparing them:
//!
//! | Supported Features   | [`GcVec`]    | [`GcRawVec`]      | [`GcVecCell`]  |
//! |----------------------|--------------|-------------------|----------------|
//! | Shared ownership?    | No - `!Copy` | Yes, is `Copy`    | Yes, is `Copy` |
//! | Can coerce to slice? | Easiy `Deref` | No (is `unsafe`)  | Needs `borrow()` -> `cell::Ref<[T]>` |
//! | impl `std::ops::Index` | Yes.       | No.               | No (but can `borrow` as `GcVec` which does) |
//! | Runtime borrow checks? | No.        | Only for `iter`    | Yes            |
//! | Best stdlib analogy  | `Vec<T>`     | `Rc<Vec<Cell<T>>>` | `Rc<RefCell<Vec<T>>>` |
//!
//! All vector types are `!Send`, because all garbage collected vectors
//! have an [implicit reference to the GcContext](`crate::vec::IGcVec::context`).
//!
//! ## The shared mutability problem
//! Because a vector's length is mutable, garbage collected vectors are significantly more complicated
//! than [garbage collected arrays](`crate::array::GcArray`).
//! Rust's memory safety is based around the distinction between mutable and immutable references
//! `&mut T` and `&T`.Mutable references `&mut T` must be uniquely owned and cannot be duplicated,
//! while immutable references `&T` can be freely duplicated and shared,
//! but cannot be duplicated. This has been called "aliasing XOR mutability".
//!
//! This becomes a significant problem with garbage collected references like [`Gc`](`crate::Gc`),
//! because they can be copied freely (they implement `Copy`).
//! Just like [`Rc`](`std::rc::Rc`), this means they can only give out immutable references `&T`
//! by difference. With vectors this makes it impossible to mutate the length (ie. call `push`)
//! without interior mutability like RefCell.
//!
//! This is a problem for languages beyond just rust, because of issues like iterator invalidation.
//! If we have two references to a vector, a `push` on one vector may mutate the length while an iteration
//! on the other vector is still in progress.
//!
//! To put it another way, it's impossible for a garbage collected vector/list to implement all of the following properties:
//! 1. Coerce the vector directly into a slice or pointer to the underlying memory.
//! 2. Allow multiple references to the vector (implement ~Copy`)
//! 3. The ability to mutate the length and potentially reallocate.
//!
//! ### What Java/Python do
//! Most languages sidestep the problem by forbidding option 1
//! and refusing to give direct pointer-access to garbage-collected arrays.
//! Usually, forbidding option 1 is not a problem, since arrays in Java
//! and lists in Python are builtin into the language and replace most of the users of pointers.
//!
//! There are some exceptions though. Java's JNI allows raw access to an arrays's memory with `GetPrimitiveArrayCritical`.
//! However, there is no need to worry about mutating length because java arrays have a fixed
//! size (although it does temporarily prevent garbage collection).
//! Python's native CAPI exposes access to a list's raw memory with `PyListObject->ob_items`,
//! although native code must be careful of any user code changing the length.
//!
//! This problem can also come up in unexpected places.
//! For example, the C implementation of Python's `list.sort` coerces the list into a raw pointer,
//! then sorts the memory directly using the pointers. The original code for that function
//! continued to use the original pointer even if a user comparison, leading to bugs.
//! The current implementation temporarily sets the length to zero and carefully guards against any mutations.
//!
//! ### What Rust/C++ do (in their stdlib)
//! Rust and C++ tend to restrict the second option, allowing only a single
//! reference to a vector by default.
//!
//! Rust's ownership system and borrow checker ensures that `Vec<T>` only has a single owner.
//! Changing the length requires a a uniquely owned `&mut T` reference.
//! Mutating the length while iterating is statically impossible
//!
//! In C++, you *should* only have one owner, but this can't be statically verified.
//! Mutating the length while iterating is undefined behavior.
//!
//! In Rust, you can have duplicate ownership by wrapping in a `Rc`,
//! and you can allow runtime-checked mutation by wrapping in a `RefCell`.
//!
//! ### The zerogc solution
//! `zerogc` allows the user to pick their guarantees.
//!
//! If you are fine with unique ownership, you can use [`GcVec`].
//! Unlike most garbage collected types, this allows mutable access
//! to the contents. It can even coerce to a `&mut [T]` reference.
//!
//! If you want multiple ownership, you are left with two options:
//! 1. `GcVecCell` which is essentially `Gc<RefCell<Vec<T>>>`. It has some runtime overhead,
//!    but allows coercion to `&[T]` (and even `&mut [T]`)
//! 2. `GcRawVec` which *doesn't* allow coercion to `&[T]`, but avoids runtime borrow checks.
//!
//! `GcRawVec` is probably the most niche of all the vector types and it doesn't really
//! have a direct analogue with any standard library types. It is probably most similar to
//! a `Cell< Gc< Vec<Cell<T>> >>`.
//!
//! Calls to `get` yield `T` instead of `&T` require `T: Copy` (just like [`Cell`](`core::cell::Cell`)),
//! because there is no way to guarantee the length wont be mutated or the element won't be `set`
//! while the reference is in use.
//!
//! A `GcRawVec` may never give out references or slices directly to its contents,
//! because other references may trigger reallocation at any time (via a `push`).
//!
//! NOTE: A similar problem occurs with asynchronous, concurrent collectors.
//! It has been called the [stretchy vector problem](https://www.ravenbrook.com/project/mps/master/manual/html/guide/vector.html)
//! by some. This is less of a problem for `zerogc`, because collections can only
//! happen at explicit safepoints.
use core::marker::PhantomData;
use core::ops::{Deref, DerefMut, Index, IndexMut, RangeBounds};
use core::slice::SliceIndex;
use core::convert::{AsRef, AsMut};
use core::cell::UnsafeCell;
use core::mem::ManuallyDrop;
use core::fmt::{self, Debug, Formatter};
use core::ptr::NonNull;

use inherent::inherent;
use zerogc_derive::{unsafe_gc_impl};

use crate::{CollectorId, GcRebrand, GcSafe, GcSystem, Trace};
use crate::vec::raw::{ReallocFailedError};

pub mod cell;
pub mod raw;

pub use self::raw::{IGcVec, GcRawVec};
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
    /// Convert this vector into its underlying [GcRawVec](`zerogc::vec::raw::GcRawVec`)
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
    /// Get a reference to the underlying [GcRawVec](`zerogc::vec::raw::GcRawVec`),
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
impl<'gc, T: GcSafe<'gc, Id>, I, Id: CollectorId> Index<I> for GcVec<'gc, T, Id>
    where I: SliceIndex<[T]> {
    type Output = I::Output;
    #[inline]
    fn index(&self, idx: I) -> &I::Output {
        &self.as_slice()[idx]
    }
}
impl<'gc, T: GcSafe<'gc, Id>, I, Id: CollectorId> IndexMut<I> for GcVec<'gc, T, Id>
    where I: SliceIndex<[T]> {
    #[inline]
    fn index_mut(&mut self, index: I) -> &mut Self::Output {
        &mut self.as_mut_slice()[index]
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
impl<'gc, T: GcSafe<'gc, Id>, Id: CollectorId> Debug for GcVec<'gc, T, Id>
    where T: Debug {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.debug_list()
            .entries(self.iter())
            .finish()
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

    type Drain<'a> where T: 'a, 'gc: 'a = Drain<'a, 'gc, T, Id>;

    pub fn drain(&mut self, range: impl RangeBounds<usize>) -> Drain<'_, 'gc, T, Id> {
        /*
         * See `Vec::drain`
         */
        let old_len = self.len();
        let range = core::slice::range(range, ..old_len);
        /*
         * Preemptively set length to `range.start` in case the resulting `Drain`
         * is leaked.
         */
        unsafe {
            self.set_len(range.start);
            let r = core::slice::from_raw_parts(
                self.as_ptr().add(range.start),
                range.len()
            );
            Drain {
                tail_start: range.end,
                tail_len: old_len - range.end,
                iter: r.iter(),
                vec: NonNull::from(self)
            }
        }
    }

    // Default methods:
    pub fn replace(&mut self, index: usize, val: T) -> T;
    pub fn set(&mut self, index: usize, val: T);
    pub fn extend_from_slice(&mut self, src: &[T])
        where T: Copy;
    pub fn push(&mut self, val: T);
    pub fn pop(&mut self) -> Option<T>;
    pub fn swap_remove(&mut self, index: usize) -> T;
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

/// The garbage collected analogue of [`std::vec::Drain`]
pub struct Drain<'a, 'gc, T: GcSafe<'gc, Id>, Id: CollectorId> {
    /// Index of tail to preserve
    tail_start: usize,
    /// The length of tail to preserve
    tail_len: usize,
    iter: core::slice::Iter<'a, T>,
    vec: NonNull<GcVec<'gc, T, Id>>,
}
impl<'a, 'gc, T: GcSafe<'gc, Id>, Id: CollectorId> Drain<'a, 'gc, T, Id> {
    unsafe fn cleanup(&mut self) {
        if self.tail_len == 0 { return }
        /*
         * Copy `tail` back to vec.
         */
        let v = self.vec.as_mut();
        let old_len = v.len();
        debug_assert!(old_len <= self.tail_start);
        if old_len != self.tail_start {
            v.as_ptr().add(self.tail_start)
                .copy_to(v.as_mut_ptr().add(old_len), self.tail_len);
        }
        v.set_len(old_len + self.tail_len);
    }
}
impl<'a, 'gc, T: GcSafe<'gc, Id>, Id: CollectorId> Iterator for Drain<'a, 'gc, T, Id> {
    type Item = T;
    #[inline]
    fn next(&mut self) -> Option<T> {
        self.iter.next().map(|e| unsafe { core::ptr::read(e) })
    }

    #[inline]
    fn size_hint(&self) -> (usize, Option<usize>) {
        self.iter.size_hint()
    }
}
impl<'a, 'gc, T: GcSafe<'gc, Id>, Id: CollectorId> Drop for Drain<'a, 'gc, T, Id> {
    fn drop(&mut self) {
        while let Some(val) = self.iter.next() {
            let _guard = scopeguard::guard(self, |s| unsafe { s.cleanup() });
            unsafe { core::ptr::drop_in_place(val as *const T as *mut T); }
            core::mem::forget(_guard);
        }
        unsafe { self.cleanup() };
    }
}
impl<'a, 'gc, T: GcSafe<'gc, Id>, Id: CollectorId> DoubleEndedIterator for Drain<'a, 'gc, T, Id> {
    #[inline]
    fn next_back(&mut self) -> Option<Self::Item> {
        self.iter.next_back().map(|e| unsafe { core::ptr::read(e) })
    }
}
impl<'a, 'gc, T: GcSafe<'gc, Id>, Id: CollectorId> core::iter::ExactSizeIterator for Drain<'a, 'gc, T, Id> {}
impl<'a, 'gc, T: GcSafe<'gc, Id>, Id: CollectorId> core::iter::FusedIterator for Drain<'a, 'gc, T, Id> {}

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
/// Because it is implicitly associated with a [GcContext](`crate::GcContext`) (which is thread-local),
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
