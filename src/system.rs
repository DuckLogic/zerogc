//! Defines the [`GcSystem`] API for collector backends.d

use core::fmt::Debug;
use core::hash::Hash;

use crate::trace::{Gc, GcSafe, NullTrace, TrustedDrop};

/// A garbage collector implementation,
/// conforming to the zerogc API.
pub unsafe trait GcSystem {
    /// The type of collector IDs given by this system
    type Id: CollectorId;
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
