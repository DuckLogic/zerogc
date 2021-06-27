use crate::{CollectorId, GcSafe, GcDirectBarrier, GcRebrand, GcHandleSystem, GcErase, GcContextId, Trace, GcVisitor};
use std::cmp::Ordering;
use std::ptr::NonNull;
use std::hash::{Hasher, Hash};
use std::marker::PhantomData;
use std::ops::Deref;

/// A garbage collected pointer to a value.
///
/// The memory this pointer points to has been allocated from a garbage collector,
/// and will only be feed once the program is no longer using it.
///
/// Garbage collection can only happen at user-specified [safepoints](safepoint!).
///
/// Outside of these points, this smart pointer can be coerced to an ordinary reference,
/// which is bound to the lifetime of the corresponding [GcContext].
///
/// The objects can only survive garbage collection if they live in this smart-pointer.
///
/// ## Safety
/// Internally, this is just a pointer, regardless of the GC implementation.
///
/// Unsafe code should note that the implementation is permitted to use "copying" garbage collection,
/// and pointers can internally change locations between two safepoints.
///
/// However, in between safepoints the `Gc` pointer can be safely relied upon
/// to point to the same location.
///
/// A `Gc` pointer can be safely transmuted back and forth
/// to a raw pointer and reference (assuming the pointer remains valid).
#[repr(transparent)]
pub struct Gc<'gc, T: GcSafe + ?Sized + 'gc, Id: CollectorId> {
    ptr: NonNull<T>,
    /// Logically speaking, our pointer is `&'gc T`.
    ///
    /// This ensures that we can safely coerce to `&'gc T` in-between
    /// safepoints, making our `Deref` impl safe.
    /// NOTE: This is separate from our 'ContextId' marker
    ref_marker: PhantomData<&'gc T>,
    /// Marks that our '`gc` lifetime is invariant,
    /// and the borrow checker is not allowed to shrink or grow it.
    context_marker: GcContextId<'gc>,
    /// The marker type for our type of collectors.
    ///
    /// We hold a phantom  be zero-sized, so that [Gc] can be freely transmuted to/from a pointer.
    collector_id: PhantomData<Id>
}
impl<'gc, T: GcSafe + ?Sized + 'gc, Id: CollectorId> Gc<'gc, T, Id> {
    /// Create a GC pointer from a raw pointer
    ///
    /// ## Safety
    /// Undefined behavior if the underlying pointer is not valid
    /// and associated with the relevant collector id.
    ///
    /// Collectors may add their own additional invariants
    /// that callers need to maintain.
    #[inline]
    pub unsafe fn from_raw(id: Id, value: NonNull<T>) -> Self {
        let res = Gc {
            collector_id: PhantomData,
            ptr: value,
            context_marker: GcContextId(PhantomData),
            ref_marker: PhantomData
        };
        debug_assert_eq!(*Id::determine_from_ref(res), id);
        res
    }

    /// The value of the underlying pointer
    ///
    /// Guaranteed to live until the next garbage collection.
    #[inline(always)]
    pub fn value(self) -> &'gc T {
        unsafe { *(&self.ptr as *const NonNull<T> as *const &'gc T) }
    }

    /// Cast this reference to a raw pointer
    ///
    /// ## Safety
    /// It's undefined behavior to mutate the
    /// value. This is because some GCs are incremental,
    /// and may need write barriers.
    ///
    /// The pointer is not necessarily valid across safepoints.
    #[inline]
    pub unsafe fn as_raw_ptr(self) -> *mut T {
        self.ptr.as_ptr()
    }

    /// Create a handle to this object, which can be used without a context
    #[inline]
    pub fn create_handle<'a>(self) -> <Id::System as GcHandleSystem<'gc, 'a, T>>::Handle
        // TODO: These type bounds are a monstrosity
        where Id::System: GcHandleSystem<'gc, 'a, T>,
              Id::System: GcHandleSystem<'gc, 'a, T, Id=Id>,
              T: GcErase<'a, Id> + 'a,
              <T as GcErase<'a, Id>>::Erased: GcSafe + 'a {
        <Id::System as GcHandleSystem<'gc, 'a, T>>::create_handle(self)
    }

    /// Get a reference to the system
    ///
    /// ## Safety
    /// This is safe, because a [GCSystem] must outlive all the pointers it owns.
    #[inline]
    pub fn system(self) -> &'gc Id::System {
        unsafe { &*(self.id()).system() }
    }

    /// Get an [CollectorId] that references the owning collector
    ///
    /// ## Safety
    /// It is undefined behavior if the [CollectorRef] is used after its owning [GcSystem]
    /// has been destroyed
    #[inline]
    pub unsafe fn id(self) -> &'gc Id {
        Id::determine_from_ref(self)
    }
}
unsafe impl<'gc, T, Id> Trace for Gc<'gc, T, Id> where
    Id: CollectorId, T: GcSafe + ?Sized + 'gc {
    const NEEDS_TRACE: bool = true; // Were the whole point ;)

    #[inline]
    fn visit<V: GcVisitor>(&mut self, visitor: &mut V) -> Result<(), <V as GcVisitor>::Err> {
        unsafe { visitor.visit_gc(self) }
    }
}

unsafe impl<'gc, T, Id> GcSafe for Gc<'gc, T, Id> where
    Id: CollectorId, T: GcSafe + ?Sized + 'gc {
    const NEEDS_DROP: bool = false; // We are copy
}

unsafe impl<'gc, 'min, T, Id> GcErase<'min, Id> for Gc<'gc, T, Id> where
    Id: CollectorId, T: GcSafe + ?Sized + 'gc + GcErase<'min, Id>, T::Erased: GcSafe + 'min {
    type Erased = Gc<'min, T::Erased, Id>;
}
unsafe impl<'gc, 'new_gc, T, Id> GcRebrand<'new_gc, Id> for Gc<'gc, T, Id> where
    Id: CollectorId, T: GcSafe + ?Sized + 'gc + GcRebrand<'new_gc, Id>, T::Branded: GcSafe + 'new_gc {
    type Branded = Gc<'new_gc, T::Branded, Id>;
}
unsafe impl<'gc, O, V, Id: CollectorId> GcDirectBarrier<'gc, Gc<'gc, O, Id>> for Gc<'gc, V, Id>
    where O: GcSafe + ?Sized + 'gc, V: GcSafe + ?Sized + 'gc {
    #[inline(always)]
    unsafe fn write_barrier(&self, owner: &Gc<'gc, O, Id>, field_offset: usize) {
        self.id().write_barrier(*self, *owner, field_offset)
    }
}
impl<'gc, T: GcSafe + ?Sized + 'gc, Id: CollectorId> Copy for Gc<'gc, T, Id> {}
impl<'gc, T: GcSafe + ?Sized + 'gc, Id: CollectorId> Clone for Gc<'gc, T, Id> {
    #[inline]
    fn clone(&self) -> Self {
        *self
    }
}
impl<'gc, T, Id> PartialEq for Gc<'gc, T, Id>
    where T: GcSafe + ?Sized + 'gc, Id: CollectorId, T: PartialEq {
    #[inline]
    fn eq(&self, other: &Self) -> bool {
        (**self).eq(**other)
    }

    #[inline]
    fn ne(&self, other: &Self) -> bool {
        (**self).ne(**other)
    }
}
impl<'gc, T, Id> Eq for Gc<'gc, T, Id>
    where T: GcSafe + ?Sized + 'gc, Id: CollectorId, T: Eq {}
impl<'gc, T, Id> Hash for Gc<'gc, T, Id>
    where T: GcSafe + ?Sized + 'gc, Id: CollectorId, T: Hash {
    #[inline]
    fn hash<H: Hasher>(&self, state: &mut H) {
        (**self).hash(state);
    }
    /*
     * NOTE: We can't override 'hash_slice' meaningfully, because
     * [Gc<'gc, T>] transmutes to [*ptr T] not [T]
     */
}
impl<'gc, T, Id> PartialOrd for Gc<'gc, T, Id>
    where T: GcSafe + ?Sized + 'gc, Id: CollectorId, T: PartialOrd {
    #[inline]
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        (**self).partial_cmp(other)
    }
}
impl<'gc, T, Id> Ord for Gc<'gc, T, Id>
    where T: GcSafe + ?Sized + 'gc, Id: CollectorId, T: Ord + PartialOrd {
    #[inline]
    fn cmp(&self, other: &Self) -> Ordering {
        (**self).cmp(**other)
    }
}
impl<'gc, T: GcSafe + ?Sized + 'gc, Id: CollectorId> Deref for Gc<'gc, T, Id> {
    type Target = &'gc T;

    #[inline(always)]
    fn deref(&self) -> &'_ &'gc T {
        unsafe { &*(&self.ptr as *const NonNull<T> as *const &'gc T) }
    }
}



/// In order to send *references* between threads,
/// the underlying type must be sync.
///
/// This is the same reason that `Arc<T>: Send` requires `T: Sync`
unsafe impl<'gc, T, Id> Send for Gc<'gc, T, Id>
    where T: GcSafe + Sync, Id: CollectorId + Sync {}

/// If the underlying type is `Sync`, it's safe
/// to share garbage collected references between threads.
///
/// The safety of the collector itself depends on the flag `cfg!(feature = "sync")`
/// If it is, the whole garbage collection implenentation should be as well.
unsafe impl<'gc, T, Id> Sync for Gc<'gc, T, Id>
    where T: GcSafe + Sync, Id: CollectorId + Sync {}