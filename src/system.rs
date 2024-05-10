//! Defines the [`GcSystem`] API for collector backends.d

use core::fmt::Debug;
use core::hash::Hash;

use crate::trace::{Gc, GcRebrand, GcSafe, NullTrace, TrustedDrop};

/// A garbage collector implementation,
/// conforming to the zerogc API.
pub unsafe trait GcSystem {
    /// The type of collector IDs given by this system
    type Id: CollectorId;
}

/// A [CollectorId] that supports allocating [GcHandle]s
///
/// Not all collectors necessarily support handles.
pub unsafe trait HandleCollectorId: CollectorId {
    /// The type of [GcHandle] for this collector.
    ///
    /// This is parameterized by the *erased* type,
    /// not by the original type.
    type Handle<T>: GcHandle<T, System = Self::System, Id = Self>
    where
        T: GcSafe<'static, Self> + ?Sized;

    /// Create a handle to the specified GC pointer,
    /// which can be used without a context
    ///
    /// NOTE: Users should only use from [Gc::create_handle].
    ///
    /// The system is implicit in the [Gc]
    #[doc(hidden)]
    fn create_handle<'gc, T>(gc: Gc<'gc, T, Self>) -> Self::Handle<T::Branded>
    where
        T: GcSafe<'gc, Self> + GcRebrand<'static, Self> + ?Sized;
}

/// Uniquely identifies the collector in case there are
/// multiple collectors.
///
/// ## Safety
/// To simply the typing, this contains no references to the
/// lifetime of the associated [GcSystem].
///
/// It's implicitly held and is unsafe to access.
/// As long as the collector is valid,
/// this id should be too.
///
/// It should be safe to assume that a collector exists
/// if any of its pointers still do!
pub unsafe trait CollectorId:
    Copy + Eq + Hash + Debug + NullTrace + TrustedDrop + 'static + for<'gc> GcSafe<'gc, Self>
{
    /// The type of the garbage collector system
    type System: GcSystem<Id = Self>;

    /// Get the runtime id of the collector that allocated the [Gc]
    ///
    /// Assumes that `T: GcSafe<'gc, Self>`, although that can't be
    /// proven at compile time.
    fn from_gc_ptr<'a, 'gc, T>(gc: &'a Gc<'gc, T, Self>) -> &'a Self
    where
        T: ?Sized,
        'gc: 'a;

    /// Perform a write barrier before writing to a garbage collected field
    ///
    /// ## Safety
    /// Similar to the [GcDirectBarrier] trait, it can be assumed that
    /// the field offset is correct and the types match.
    unsafe fn gc_write_barrier<'gc, O: GcSafe<'gc, Self> + ?Sized, V: GcSafe<'gc, Self> + ?Sized>(
        owner: &Gc<'gc, O, Self>,
        value: &Gc<'gc, V, Self>,
        field_offset: usize,
    );

    /// Assume the ID is valid and use it to access the [GcSystem]
    ///
    /// NOTE: The system is bound to the lifetime of *THIS* id.
    /// A CollectorId may have an internal pointer to the system
    /// and the pointer may not have a stable address. In other words,
    /// it may be difficult to reliably take a pointer to a pointer.
    ///
    /// ## Safety
    /// Undefined behavior if the associated collector no longer exists.
    unsafe fn assume_valid_system(&self) -> &Self::System;
}

/// A owned handle which points to a garbage collected object.
///
/// This is considered a root by the garbage collector that is independent
/// of any specific [GcContext]. Safepoints
/// don't need to be informed of this object for collection to start.
/// The root is manually managed by user-code, much like a [Box] or
/// a reference counted pointer.
///
/// This can be cloned and stored independently from a context,
/// bridging the gap between native memory and managed memory.
/// These are useful to pass to C APIs or any other code
/// that doesn't cooperate with zerogc.
///
/// ## Tracing
/// The object behind this handle is already considered a root of the collection.
/// It should always be considered reachable by the garbage collector.
///
/// Validity is tracked by this smart-pointer and not by tracing.
/// Therefore it is safe to implement [NullTrace] for handles.
/*
 * TODO: Should we drop the Clone requirement?
 */
pub unsafe trait GcHandle<T: GcSafe<'static, Self::Id> + ?Sized>:
    Sized + Clone + NullTrace + for<'gc> GcSafe<'gc, Self::Id>
{
    /// The type of the system used with this handle
    type System: GcSystem<Id = Self::Id>;
    /// The type of [CollectorId] used with this sytem
    type Id: CollectorId;

    /// Access this handle inside the closure,
    /// possibly associating it with the specified
    ///
    /// This is accesses the object within "critical section"
    /// that will **block collections**
    /// for as long as the closure is in use.
    ///
    /// These calls cannot be invoked recursively or they
    /// may cause a deadlock.
    ///
    /// This is similar in purpose to JNI's [GetPrimitiveArrayCritical](https://docs.oracle.com/javase/8/docs/technotes/guides/jni/spec/functions.html#GetPrimitiveArrayCritical_ReleasePrimitiveArrayCritical).
    /// However it never performs a copy, it is just guarenteed to block any collections.
    /*
     * TODO: Should we require this of all collectors?
     * How much does it limit flexibility?
     */
    fn use_critical<R>(&self, func: impl FnOnce(&T) -> R) -> R;
}
