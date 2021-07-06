//! The implementation of [GcVec]
//!
//! A [GcVec] is simply a [RawGcVec] that also holds
//! an implicit reference to the owning [GcContext].

use core::marker::PhantomData;
use core::ops::{Deref, DerefMut};
use core::mem::ManuallyDrop;

use zerogc_derive::{unsafe_gc_impl};

use crate::{GcSimpleAlloc, CollectorId, Gc, GcSafe, GcRebrand, GcErase};
use crate::vec::repr::{GcVecRepr, ReallocFailedError};

pub mod repr;

/// A garbage collected array.
///
/// This is simply an alias for `Gc<[T]>`
#[repr(transparent)]
pub struct GcArray<'gc, T: GcSafe + 'gc, Id: CollectorId>(pub Gc<'gc, [T], Id>);
impl<'gc, T: GcSafe, Id: CollectorId> GcArray<'gc, T, Id> {
    /// The value of the array as a slice
    #[inline]
    pub fn value(self) -> &'gc [T] {
        self.0.value()
    }
}
impl<'gc, T: GcSafe, Id: CollectorId> Deref for GcArray<'gc, T, Id> {
    type Target = &'gc [T];

    #[inline]
    fn deref(&self) -> &Self::Target {
        &*self.0
    }
}
impl<'gc, T: GcSafe, Id: CollectorId> Copy for GcArray<'gc, T, Id> {}
impl<'gc, T: GcSafe, Id: CollectorId> Clone for GcArray<'gc, T, Id> {
    #[inline]
    fn clone(&self) -> Self {
        *self
    }
}
// Need to implement by hand, because [T] is not GcRebrand
unsafe_gc_impl!(
    target => GcArray<'gc, T, Id>,
    params => ['gc, T: GcSafe, Id: CollectorId],
    collector_id => Id,
    bounds => {
        TraceImmutable => never,
        GcRebrand => { where T: GcRebrand<'new_gc, Id>, <T as GcRebrand<'new_gc, Id>>::Branded: GcSafe },
        GcErase => { where T: GcErase<'min, Id>, <T as GcErase<'min, Id>>::Erased: GcSafe }
    },
    null_trace => never,
    branded_type => GcArray<'new_gc, <T as GcRebrand<'new_gc, Id>>::Branded, Id>,
    erased_type => GcArray<'min, <T as GcErase<'min, Id>>::Erased, Id>,
    NEEDS_TRACE => true,
    NEEDS_DROP => false,
    trace_mut => |self, visitor| {
        unsafe { visitor.visit_array(self) }
    },
);

/// A version of [Vec] for use with garbage collectors.
///
/// This is simply a thin wrapper around [RawGcVec]
/// that also contains a reference to the owning [GcContext].
pub struct GcVec<'gc, T: GcSafe, Ctx: GcSimpleAlloc> {
    /// A reference to the owning GcContext
    pub context: &'gc Ctx,
    /// The underlying [RawGcVec],
    /// which actually manages the memory
    pub raw: GcRawVec<'gc, T, Ctx::Id>
}
impl<'gc, T: GcSafe, Ctx: GcSimpleAlloc> GcVec<'gc, T, Ctx> {
    /// Convert this vector into its underlying [GcRawVec]
    #[inline]
    pub fn into_raw(self) -> GcRawVec<'gc, T, Ctx::Id> {
        self.raw
    }
    /// Reserve enough capacity for the specified number of additional elements.
    #[inline]
    pub fn reserve(&mut self, amount: usize) {
        let remaining = self.raw.capacity() - self.raw.len();
        if remaining < amount {
            self.grow(amount);
        }
    }
    /// Extend the vector with elements copied from the specified slice
    #[inline]
    pub fn extend_from_slice(&mut self, src: &[T])
        where T: Copy {
        self.reserve(src.len());
        // TODO: Write barriers?
        unsafe {
            (self.raw.as_repr_mut().ptr() as *mut T).add(self.len())
                .copy_from_nonoverlapping(src.as_ptr(), src.len())
        }
    }
    /// Push the specified value onto the vector
    #[inline]
    pub fn push(&mut self, val: T) {
        self.reserve(1);
        match self.raw.try_push(val) {
            Ok(()) => {},
            Err(InsufficientCapacityError { }) => {
                unsafe { std::hint::unreachable_unchecked(); }
            }
        }
    }
    #[cold]
    fn grow(&mut self, amount: usize) {
        let requested_capacity = self.len().checked_add(amount).unwrap();
        let new_capacity = self.raw.capacity().checked_mul(2).unwrap()
            .max(requested_capacity);
        if <Ctx::Id as CollectorId>::RawVecRepr::SUPPORTS_REALLOC {
            match self.raw.repr.value().realloc_in_place(new_capacity) {
                Ok(()) => {
                    return; // success
                }
                Err(ReallocFailedError::Unsupported) => unreachable!(),
                Err(ReallocFailedError::OutOfMemory) => panic!("Out of memory"),
                Err(ReallocFailedError::SizeUnsupported) => {} // fallthrough to realloc
            }
        }
        // Just allocate a new one, copying from the old
        let mut new_mem = self.context.alloc_vec_with_capacity(new_capacity).raw;
        // TODO: Write barriers
        unsafe {
            (new_mem.as_repr_mut().ptr() as *mut T).copy_from_nonoverlapping(
                self.raw.as_ptr() as *const T,
                self.raw.len()
            );
            new_mem.as_repr_mut().set_len(self.raw.len());
            let mut old_mem = std::mem::replace(&mut self.raw, new_mem);
            old_mem.as_repr_mut().set_len(0); // We don't want to drop the old elements
        }
    }
}
unsafe_gc_impl!(
    target => GcVec<'gc, T, Ctx>,
    params => ['gc, T: GcSafe, Ctx: GcSimpleAlloc],
    bounds => {
        TraceImmutable => never,
        Trace => { where T: GcSafe },
        GcRebrand => never,
        GcErase => never,
    },
    null_trace => never,
    NEEDS_TRACE => true,
    NEEDS_DROP => T::NEEDS_DROP /* if our inner type needs a drop */,
    trace_mut => |self, visitor| {
        unsafe { visitor.visit_vec::<T, _>(self.raw.as_repr_mut()) }
    },
    collector_id => Ctx::Id
);
impl<'gc, T: GcSafe, Ctx: GcSimpleAlloc> Deref for GcVec<'gc, T, Ctx> {
    type Target = GcRawVec<'gc, T, Ctx::Id>;

    #[inline]
    fn deref(&self) -> &Self::Target {
        &self.raw
    }
}

impl<'gc, T: GcSafe, Ctx: GcSimpleAlloc> DerefMut for GcVec<'gc, T, Ctx> {
    #[inline]
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.raw
    }
}

/// Indicates there is insufficient capacity for an operation on a [RawGcVec]
#[derive(Debug)]
pub struct InsufficientCapacityError;

/// A version of [Vec] for use with garbage collection.
///
/// Despite using garbage collected memory, this type may only have a single owner
/// in order to ensure unique `&mut` references.
///
/// If this were the case, one reference could create a `&[T]` slice
/// while another slice references it.
///
/// Unlike [GcVec], this doesn't contain a reference to the owning [GcContext].
///
/// NOTE: This is completely different from the distinction between
/// [Vec] and [RawVec](https://github.com/rust-lang/rust/blob/master/library/alloc/src/raw_vec.rs)
/// in the standard library.
/// In particular, this still contains a `len`.
///
/// In fact, most [GcVec] methods just delegate here with an additional context.
///
/// ## Safety
/// To avoid undefined behavior, there can only be a single reference
/// to a [RawGcVec], despite [Gc] implementing `Copy`.
#[repr(transparent)]
pub struct GcRawVec<'gc, T: GcSafe + 'gc, Id: CollectorId> {
    repr: Gc<'gc, Id::RawVecRepr, Id>,
    marker: PhantomData<&'gc [T]>
}
impl<'gc, T: GcSafe, Id: CollectorId> GcRawVec<'gc, T, Id> {
    /// Create a [RawGcVec] from the specified repr
    ///
    /// ## Safety
    /// The specified representation must be valid and actually
    /// be intended for this type `T`.
    #[inline]
    pub unsafe fn from_repr(repr: Gc<'gc, Id::RawVecRepr, Id>) -> Self {
        GcRawVec { repr, marker: PhantomData }
    }
    /// View this [RawGcVec] as a [GcVec] for the duration of the specified closure,
    /// with an implicit reference to the specified [GcContext]
    #[inline]
    pub fn with_context_mut<Ctx, F, R>(&mut self, ctx: &'gc Ctx, func: F) -> R
        where Ctx: GcSimpleAlloc<Id=Id>, F: FnOnce(&mut GcVec<'gc, T, Ctx>) -> R {
        let vec = ManuallyDrop::new(GcVec {
            // NOTE: This is okay, because we will restore any changes via `scopeguard`
            raw: unsafe { std::ptr::read(self) },
            context: ctx
        });
        let mut guard = scopeguard::guard(vec, |vec| {
            // Restore any changes
            *self = ManuallyDrop::into_inner(vec).raw;
        });
        func(&mut *guard)
    }
    /// View this [RawGcVec] as a [GcVec] for the duration of the specified closure
    #[inline]
    pub fn with_context<Ctx, F, R>(&self, ctx: &'gc Ctx, func: F) -> R
        where Ctx: GcSimpleAlloc<Id=Id>, F: FnOnce(&GcVec<'gc, T, Ctx>) -> R {
        let vec = ManuallyDrop::new(GcVec {
            // NOTE: This is okay, because we will forget it later
            raw: unsafe { std::ptr::read(self) },
            context: ctx
        });
        let guard = scopeguard::guard(vec, std::mem::forget);
        func(&*guard)
    }
    /// The length of this vector
    #[inline]
    pub fn len(&self) -> usize {
        self.repr.len()
    }
    /// The capacity of this vector.
    #[inline]
    pub fn capacity(&self) -> usize {
        self.repr.capacity()
    }
    /// Clear the vector, setting its length to zero
    #[inline]
    pub fn clear(&mut self) {
        unsafe {
            let elements = self.as_ptr() as *mut T;
            let old_len = self.len();
            // Drop after we set the length, in case it panics
            self.repr.set_len(0);
            std::ptr::drop_in_place::<[T]>(std::ptr::slice_from_raw_parts_mut(
                elements, old_len
            ));
        }
    }
    /// Push a value onto this vector,
    /// returning an error if there is insufficient space.
    #[inline]
    pub fn try_push(&mut self, val: T) -> Result<(), InsufficientCapacityError> {
        let old_len = self.len();
        if old_len < self.capacity() {
            unsafe {
                // TODO: Write barriers....
                (self.as_ptr() as *mut T).add(old_len).write(val);
                self.repr.set_len(old_len + 1);
            }
            Ok(())
        } else {
            Err(InsufficientCapacityError)
        }
    }
    /// Get the raw representation of this vector as a [GcVecRepr]
    ///
    /// ## Safety
    /// The user must not violate the invariants of the repr.
    #[inline]
    pub unsafe fn as_repr(&self) -> Gc<'gc, Id::RawVecRepr, Id> {
        self.repr
    }
    /// Get a mutable reference to the raw represnetation of this vector
    ///
    /// ## Safety
    /// The user must preserve the validity of the underlying representation.
    #[inline]
    pub unsafe fn as_repr_mut(&mut self) -> &mut Gc<'gc, Id::RawVecRepr, Id> {
        &mut self.repr
    }
    /// Interpret this vector as a slice
    ///
    /// Because the length may change,
    /// this is bound to the lifetime of the current value.
    #[inline]
    pub fn as_slice(&self) -> &[T] {
        unsafe { std::slice::from_raw_parts(
            self.as_ptr(),
            self.len()
        ) }
    }
    /// Give a pointer to the underlying slice of memory
    ///
    /// ## Safety
    /// The returned memory must not be mutated.
    #[inline]
    pub unsafe fn as_ptr(&self) -> *const T {
        self.repr.ptr() as *const T
    }
}
impl<'gc, T: GcSafe, Id: CollectorId> Deref for GcRawVec<'gc, T, Id> {
    type Target = [T];

    #[inline]
    fn deref(&self) -> &Self::Target {
        self.as_slice() // &'gc > &'self
    }
}
unsafe_gc_impl!(
    target => GcRawVec<'gc, T, Id>,
    params => ['gc, T: GcSafe, Id: CollectorId],
    collector_id => Id,
    bounds => {
        TraceImmutable => never,
        GcRebrand => {
            where T: GcSafe + GcRebrand<'new_gc, Id>,
                <T as GcRebrand<'new_gc, Id>>::Branded: GcSafe
        },
        GcErase => {
            where T: GcSafe + GcErase<'min, Id>,
                <T as GcErase<'min, Id>>::Erased: GcSafe
        },
    },
    branded_type => GcRawVec<'new_gc, <T as GcRebrand<'new_gc, Id>>::Branded, Id>,
    erased_type => GcRawVec<'min, <T as GcErase<'min, Id>>::Erased, Id>,
    null_trace => never,
    NEEDS_TRACE => true,
    NEEDS_DROP => false, // GcVecRepr is responsible for Drop
    trace_mut => |self, visitor| {
        unsafe { visitor.visit_vec::<T, Id>(self.as_repr_mut()) }
    },
);