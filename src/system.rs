//! Defines the [`GcSystem`] API for collector backends.d

use crate::sealed::Sealed;
use core::fmt::Debug;
use core::hash::Hash;

use crate::trace::{Gc, GcSafe, NullTrace, TrustedDrop};

/// A garbage collector implementation,
/// conforming to the zerogc API.
///
/// ## Safety
/// For [`Gc`] references to be memory safe,
/// the implementation must be correct.
pub unsafe trait GcSystem {
    /// The type of the collector id.
    type Id: CollectorId<System = Self>;

    /// Return the collector's id.
    fn id(&self) -> Self::Id;
}

/// The collector-specific state of a [`GcContext`](crate::context::GcContext).
///
/// Depending on whether the collector is multithreaded,
/// there may be one context per thread or a single global context.
///
/// Owning a reference to the state prevents the [`GcSystem`] from being dropped.
///
/// ## Safety
/// A context must always be bound to a single thread.
///
/// This trait must be implemented correctly for the safety of other code.
pub unsafe trait GcContextState {
    /// The type of the collector id.
    type Id: CollectorId<ContextState = Self>;

    /// Return the collector's id.
    fn id(&self) -> Self::Id;

    /// Return a reference to the owning system.
    fn system(&self) -> &'_ <Self::Id as CollectorId>::System;

    /// Indicate that .
    unsafe fn safepoint(&mut self);

    /// Unconditionally force a garbage collection
    unsafe fn force_collect(&mut self);

    /// Allocate a regular garbage collected object.
    ///
    /// Initializes the object using the specified
    /// initialization function.
    ///
    /// ## Safety
    /// The lifetime of the returned [`Gc`] pointer is not statically checked.
    /// It is the caller's responsibility to ensure it is correct.
    unsafe fn alloc<'gc, T: GcSafe<'gc, Self::Id>, S: AllocInitSafety>(
        &self,
        init: impl FnOnce() -> T,
        safety: S,
    ) -> Gc<'gc, T, Self::Id>;
}

/// Whether an initialization function can be trusted to not panic.
///
/// ## Safety
/// The members of this trait must be implemented correctly
/// or the collector might trace uninitialized values.
pub unsafe trait AllocInitSafety: crate::sealed::Sealed {
    /// Whether the initialization function could possibly panic.
    ///
    /// If the function is trusted to never panic,
    /// this can avoid the need to handle initialization failures.
    ///
    /// ## Safety
    /// An incorrect implementation here can cause uninitialized
    /// values to be observed by the trace function,
    /// which is undefined behavior.
    const MIGHT_PANIC: bool;
}

/// An allocation initialization function that is trusted to never fail.
///
/// ## Safety
/// If the initialization function panics, then undefined behavior will occur.
pub(crate) struct TrustedAllocInit {
    _priv: (),
}
impl TrustedAllocInit {
    /// Creates a marker indicating that the allocation is guaranteed to never fail.
    ///
    /// ## Safety
    /// Undefined behavior if the corresponding initialization function ever fails.
    #[inline(always)]
    pub unsafe fn new_unchecked() -> Self {
        TrustedAllocInit { _priv: () }
    }
}
unsafe impl AllocInitSafety for TrustedAllocInit {
    const MIGHT_PANIC: bool = false;
}
impl Sealed for TrustedAllocInit {}

/// Indicates that the initialization function is untrusted,
/// and could possibly fail via panicking.
pub struct UntrustedAllocInit {
    _priv: (),
}
impl UntrustedAllocInit {
    /// Create a new marker value indicating the initialization function is untrusted,
    /// and could possibly fail.
    ///
    /// This is completely safe,
    /// since no assumptions are made.
    #[inline(always)]
    #[allow(clippy::new_without_default)]
    pub fn new() -> Self {
        UntrustedAllocInit { _priv: () }
    }
}
unsafe impl AllocInitSafety for UntrustedAllocInit {
    const MIGHT_PANIC: bool = true;
}
impl Sealed for UntrustedAllocInit {}

/// The header for a garbage-collected object.
///
/// The specific type of object header may not be known,
/// and could be either [array headers](GcArrayHeader) and [regular headers](GcRegularHeader).
///
/// ## Safety
/// Any access to an object's header is extremely unsafe.
///
/// It is usually done behind a [NonNull] pointer.
pub unsafe trait GcHeader {
    /// The id of the collector.
    type Id: CollectorId;

    /// Return a reference to the object's collector id.
    fn id(&self) -> Self::Id;

    /// Determine the specific kind of header.
    fn kind(&self) -> GcHeaderKind<'_, Self::Id>;
}

/// Indicates the specific type of header,
/// which can be used for casts.
pub enum GcHeaderKind<'a, Id: CollectorId> {
    /// An [array header](GcArrayHeader).
    Array(&'a Id::ArrayHeader),
    /// A [regular header](GcRegularHeader)
    Regular(&'a Id::RegularHeader),
}
impl<'a, Id: CollectorId> GcHeaderKind<'a, Id> {
    /// Attempt to cast this header into an [array header](GcArrayHeader),
    /// returning `None` if it fails.
    #[inline]
    pub fn as_array(&self) -> Option<&'_ Id::ArrayHeader> {
        match *self {
            Self::Array(arr) => Some(arr),
            _ => None,
        }
    }

    /// Attempt to cast this header into an [regular object header](GcRegularHeader),
    /// returning `None` if it fails.
    #[inline]
    pub fn as_regular(&self) -> Option<&'_ Id::RegularHeader> {
        match *self {
            Self::Regular(header) => Some(header),
            _ => None,
        }
    }

    /// Unsafely assume this header is a [regular object header](GcRegularHeader).
    ///
    /// ## Safety
    /// Triggers undefined behavior if the cast is incorrect.
    #[inline]
    pub unsafe fn assume_regular(&self) -> &'_ Id::RegularHeader {
        self.as_regular().unwrap_unchecked()
    }

    /// Unsafely assume this header is a [array header](GcArrayHeader).
    ///
    /// ## Safety
    /// Triggers undefined behavior if the cast is incorrect.
    #[inline]
    pub unsafe fn assume_array(&self) -> &'_ Id::ArrayHeader {
        self.as_array().unwrap_unchecked()
    }
}

/// The header for a regular garbage-collected object.
pub unsafe trait GcRegularHeader: GcHeader {
    /// Convert a reference to an object header into a typed [`Gc`] pointer.
    ///
    /// ## Safety
    /// Undefined behavior if the type or lifetime is incorrect.
    unsafe fn to_gcptr<'gc, T: GcSafe<'gc, Self::Id>>(&self) -> Gc<'gc, T, Self::Id>;
}

/// The header for a garbage-collected array.
pub unsafe trait GcArrayHeader: GcHeader {
    /// Return the length of the array,
    /// in terms of the elements.
    fn len(&self) -> usize;
    /// Check if the array is empty.
    #[inline]
    fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Uniquely identifies the collector in case there are
/// multiple collectors.
///
/// This can be seen as a typed pointer to a [`GcSystem`].
///
/// ## Safety
/// To simply the typing, this contains no references to the
/// lifetime of the associated [GcSystem].
///
/// A reference to the system is implicitly held and is unsafe to access.
/// As long as the collector is valid,
/// this id should be too.
///
/// It should be safe to assume that a collector exists
/// if any of its [`Gc`] pointers still do!
pub unsafe trait CollectorId:
    Copy + Eq + Hash + Debug + NullTrace + TrustedDrop + 'static + for<'gc> GcSafe<'gc, Self>
{
    /// The type of the garbage collector system
    type System: GcSystem<Id = Self>;
    /// The [context-specific](`GcContext`) state for a collector.
    type ContextState: GcContextState<Id = Self>;
    /// The header for a garbage-collected array.
    type ArrayHeader: GcArrayHeader<Id = Self>;
    /// The header for a regulalar [`Gc`] object
    type RegularHeader: GcRegularHeader<Id = Self>;

    /// Determine the [regular header](GcRegularHeader) for the specified [Gc] object.
    ///
    /// ## Safety
    /// Undefined behavior if the header is used incorrectly.
    ///
    /// The header is normally guaranteed to live for the `'gc` lifetime.
    /// However, during a collection, it may not be valid for quite as long,
    /// so the lifetime of the returned header is tied to a transient borrow.
    /// User code should not be able to access a GC pointer during a collection,
    /// so only unsafe code needs to worry about this.
    unsafe fn determine_header<'a, 'gc, T>(gc: &'a Gc<'gc, T, Self>) -> &'a Self::RegularHeader
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
    ///
    /// The lifetime of the returned system must be correct,
    /// and not used after the collector no longer exists.
    unsafe fn assume_valid_system<'a>(self) -> &'a Self::System;
}
