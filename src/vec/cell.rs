//! The implementation of [GcVecCell]
use core::cell::RefCell;

use zerogc_derive::unsafe_gc_impl;
use inherent::inherent;

use crate::SimpleAllocCollectorId;
use crate::vec::raw::{IGcVec, ReallocFailedError};
use crate::prelude::*;

struct VecCellInner<'gc, T: GcSafe<'gc, Id>, Id: CollectorId> {
    cell: RefCell<GcVec<'gc, T, Id>>
}
unsafe_gc_impl!(
    target => VecCellInner<'gc, T, Id>,
    params => ['gc, T: GcSafe<'gc, Id>, Id: CollectorId],
    bounds => {
        Trace => { where T: Trace },
        TraceImmutable => { where T: Trace },
        TrustedDrop => { where T: TrustedDrop },
        GcSafe => { where T: GcSafe<'gc, Id> },
        GcRebrand => { where T: GcRebrand<'new_gc, Id>, T::Branded: Sized }
    },
    null_trace => never,
    NEEDS_TRACE => true,
    NEEDS_DROP => GcVec::<'gc, T, Id>::NEEDS_DROP,
    branded_type => VecCellInner<'new_gc, T::Branded, Id>,
    trace_mut => |self, visitor| {
        visitor.trace::<GcVec<'gc, T, Id>>(self.cell.get_mut())
    },
    collector_id => Id,
    trace_immutable => |self, visitor| {
        visitor.trace::<GcVec<'gc, T, Id>>(unsafe { &mut *self.cell.as_ptr() })
    }
);

/// A garbage collected vector,
/// wrapped in a [RefCell] for interior mutability.
///
/// Essentially a `Gc<RefCell<GcVec<'gc, T, Id>>>`. However,
/// this can't be done directly because `RefCell<T>` normally requires
/// `T: NullTrace` (because of the possibility of write barriers).
#[derive(Trace)]
#[zerogc(collector_ids(Id), copy)]
pub struct GcVecCell<'gc, T: GcSafe<'gc, Id>, Id: CollectorId> {
    inner: Gc<'gc, VecCellInner<'gc, T, Id>, Id>,
}
impl<'gc, T: GcSafe<'gc, Id>, Id: CollectorId> GcVecCell<'gc, T, Id> {
    /// Immutably borrow the wrapped [GcVec].
    ///
    /// The returned borrow is dynamically tracked,
    /// and guarded by the returned
    /// [core::cell::Ref] object.
    ///
    /// All immutable accesses through the [IGcVec] interface
    /// implicitly call this method (and thus carry the same risk of panics).
    ///
    /// ## Panics
    /// Panics if this vector has an outstanding mutable borrow.
    #[inline]
    pub fn borrow(&self) -> core::cell::Ref<'_, GcVec<'gc, T, Id>> {
        self.inner.cell.borrow()
    }
    /// Mutably (and exclusively) borrow the wrapped [GcVec].
    ///
    /// The returned borrow is dynamically tracked,
    /// and guarded by the returned
    /// [core::cell::RefMut] object.
    ///
    /// All mutable accesses through the [IGcVec] interface
    /// implicitly call this method (and thus carry the same risk of panics).
    ///
    /// ## Panics
    /// Panics if this vector has any other outstanding borrows.
    #[inline]
    pub fn borrow_mut(&self) -> core::cell::RefMut<'_, GcVec<'gc, T, Id>> {
        self.inner.cell.borrow_mut()
    }
    /// Immutably borrow a slice of this vector's contents.
    ///
    /// Implicitly calls [GcVecCell::borrow],
    /// and caries the same risk of panics.
    #[inline]
    pub fn borrow_slice(&self) -> core::cell::Ref<'_, [T]> {
        core::cell::Ref::map(self.borrow(), |v| v.as_slice())
    }
    /// Mutably borrow a slice of this vector's contents.
    ///
    /// Implicitly calls [GcVecCell::borrow_mut],
    /// and caries the same risk of panics.
    #[inline]
    pub fn borrow_mut_slice(&self) -> core::cell::RefMut<'_, [T]> {
        core::cell::RefMut::map(self.borrow_mut(), |v| v.as_mut_slice())
    }
}
impl<'gc, T: GcSafe<'gc, Id>, Id: CollectorId> Copy for GcVecCell<'gc, T, Id> {}
impl<'gc, T: GcSafe<'gc, Id>, Id: CollectorId> Clone for GcVecCell<'gc, T, Id> {
    #[inline]
    fn clone(&self) -> Self {
       *self
    }
}
/// Because vectors are associated with a [GcContext](`crate::GcContext`),
/// they contain thread local data (and thus must be `!Send`
impl<'gc, T: GcSafe<'gc, Id>, Id: CollectorId> !Send for GcVecCell<'gc, T, Id> {}
#[inherent]
unsafe impl<'gc, T: GcSafe<'gc, Id>, Id: SimpleAllocCollectorId> IGcVec<'gc, T> for GcVecCell<'gc, T, Id> {
    type Id = Id;

    #[inline]
    pub fn with_capacity_in(capacity: usize, ctx: &'gc <Id as CollectorId>::Context) -> Self {
        GcVecCell { inner: ctx.alloc(VecCellInner {
            cell: RefCell::new(GcVec::with_capacity_in(capacity, ctx))
        }) }
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.inner.cell.borrow().len()
    }

    #[inline]
    pub unsafe fn set_len(&mut self, len: usize) {
        self.inner.cell.borrow_mut().set_len(len);
    }

    #[inline]
    pub fn capacity(&self) -> usize {
        self.inner.cell.borrow().capacity()
    }

    #[inline]
    pub fn reserve_in_place(&mut self, additional: usize) -> Result<(), ReallocFailedError> {
        self.inner.cell.borrow_mut().reserve_in_place(additional)
    }

    #[inline]
    pub unsafe fn as_ptr(&self) -> *const T {
        self.inner.cell.borrow().as_ptr()
    }

    #[inline]
    pub fn context(&self) -> &'gc <Id as CollectorId>::Context {
        self.inner.cell.borrow().context()
    }

    // Default methods:
    pub unsafe fn as_mut_ptr(&mut self) -> *mut T;
    pub fn replace(&mut self, index: usize, val: T) -> T;
    pub fn set(&mut self, index: usize, val: T);
    pub fn extend_from_slice(&mut self, src: &[T])
        where T: Copy;
    pub fn push(&mut self, val: T);
    pub fn pop(&mut self) -> Option<T>;
    pub fn swap_remove(&mut self, index: usize) -> T;
    pub fn reserve(&mut self, additional: usize);
    pub fn is_empty(&self) -> bool;
    pub fn new_in(ctx: &'gc <Id as CollectorId>::Context) -> Self;
    pub fn copy_from_slice(src: &[T], ctx: &'gc <Id as CollectorId>::Context) -> Self
        where T: Copy;
    pub fn from_vec(src: Vec<T>, ctx: &'gc <Id as CollectorId>::Context) -> Self;
    pub fn get(&mut self, index: usize) -> Option<T>
        where T: Copy;
    pub unsafe fn as_slice_unchecked(&self) -> &[T];
}
impl<'gc, T: GcSafe<'gc, Id>, Id: CollectorId> Extend<T> for GcVecCell<'gc, T, Id> {
    fn extend<A: IntoIterator<Item=T>>(&mut self, iter: A) {
        self.inner.cell.borrow_mut().extend(iter);
    }
}