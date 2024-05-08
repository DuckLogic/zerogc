#![feature(
    ptr_metadata, // RFC 2580 - Pointer meta
    coerce_unsized, // RFC 0982 - DST coercion
    unsize,
    trait_alias, // RFC 1733 - Trait aliases
    // Needed for epsilon collector:
    negative_impls, // More elegant than marker types
    alloc_layout_extra,
    const_mut_refs,
    const_option,
    slice_range, // Convenient for bounds checking :)
)]
#![cfg_attr(feature = "error", backtrace)]
#![cfg_attr(feature = "allocator-api", feature(allocator_api))]
#![feature(maybe_uninit_slice)]
#![feature(new_uninit)]
#![deny(missing_docs)]
#![allow(
    clippy::missing_safety_doc, // TODO: Add missing safety docs and make this #[deny(...)]
)]
#![cfg_attr(not(feature = "std"), no_std)]
//! Zero overhead tracing garbage collection for rust,
//! by abusing the borrow checker.
//!
//! ## Features
//! 1. Easy to use, since `Gc<T>` is `Copy` and coerces to a reference.
//! 2. Absolutely zero overhead when modifying pointers, since `Gc<T>` is `Copy`.
//! 3. Implementation agnostic API
//! 4. Unsafe code has complete freedom to manipulate garbage collected pointers, and it doesn't need to understand the distinction
//! 5. Uses rust's lifetime system to ensure all roots are known at explicit safepoints, without any runtime overhead.
//! 6. Collection can only happen with an explicit `safepoint` call and has no overhead between these calls,
//! 7. API supports moving objects (allowing copying/generational GCs)

#[cfg(feature = "alloc")]
extern crate alloc;
/*
 * Allows proc macros to access `::zeroc::$name`
 *
 * NOTE: I can't figure out a
 * way to emulate $crate in a way
 * that doesn't confuse integration
 * tests with the main crate
 */

extern crate self as zerogc;

/*
 * I want this library to use 'mostly' stable features,
 * unless there's good justification to use an unstable feature.
 */
#[cfg(all(not(feature = "std"), feature = "alloc"))]
use alloc::vec::Vec;
use core::cmp::Ordering;
use core::fmt::{self, Debug, Display, Formatter};
use core::hash::{Hash, Hasher};
use core::marker::{PhantomData, Unsize};
use core::ops::{CoerceUnsized, Deref, DerefMut};
use core::ptr::NonNull;

use zerogc_derive::unsafe_gc_impl;
pub use zerogc_derive::{NullTrace, Trace};

#[macro_use]
mod manually_traced;
#[macro_use]
mod macros;
pub mod cell;
pub mod prelude;

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

/// A garbage collected pointer to a value.
///
/// This is the equivalent of a garbage collected smart-pointer.
/// It's so smart, you can even coerce it to a reference bound to the lifetime of the `GarbageCollectorRef`.
/// However, all those references are invalidated by the borrow checker as soon as
/// your reference to the collector reaches a safepoint.
/// The objects can only survive garbage collection if they live in this smart-pointer.
///
/// The smart pointer is simply a guarantee to the garbage collector
/// that this points to a garbage collected object with the correct header,
/// and not some arbitrary bits that you've decided to heap allocate.
///
/// ## Safety
/// A `Gc` can be safely transmuted back and forth from its corresponding pointer.
///
/// Unsafe code can rely on a pointer always dereferencing to the same value in between
/// safepoints. This is true even for copying/moving collectors.
///
/// ## Lifetime
/// The borrow does *not* refer to the value `&'gc T`.
/// Instead, it refers to the *context* `&'gc Id::Context`
///
/// This is necessary because `T` may have borrowed interior data
/// with a shorter lifetime `'a < 'gc`, making `&'gc T` invalid
/// (because that would imply 'gc: 'a, which is false).
///
/// This ownership can be thought of in terms of the following (simpler) system.
/// ```no_run
/// # trait GcSafe{}
/// # use core::marker::PhantomData;
/// struct GcContext {
///     values: Vec<Box<dyn GcSafe>>
/// }
/// struct Gc<'gc, T: GcSafe> {
///     index: usize,
///     marker: PhantomData<T>,
///     ctx: &'gc GcContext
/// }
/// ```
///
/// In this system, safepoints can be thought of mutations
/// that remove dead values from the `Vec`.
///
/// This ownership equivalency is also the justification for why
/// the `'gc` lifetime can be [covariant](https://doc.rust-lang.org/nomicon/subtyping.html#variance)
///
/// The only difference is that the real `Gc` structure
/// uses pointers instead of indices.
#[repr(transparent)]
pub struct Gc<'gc, T: ?Sized, Id: CollectorId> {
    /// The pointer to the garbage collected value.
    ///
    /// NOTE: The logical lifetime here is **not** `&'gc T`
    /// See the comments on 'Lifetime' for details.
    value: NonNull<T>,
    /// Marker struct used to statically identify the collector's type,
    /// and indicate that 'gc is a logical reference the system.
    ///
    /// The runtime instance of this value can be
    /// computed from the pointer itself: `NonNull<T>` -> `&CollectorId`
    collector_id: PhantomData<&'gc Id::System>,
}
impl<'gc, T: GcSafe<'gc, Id> + ?Sized, Id: CollectorId> Gc<'gc, T, Id> {
    /// Create a GC pointer from a raw pointer
    ///
    /// ## Safety
    /// Undefined behavior if the underlying pointer is not valid
    /// and doesn't correspond to the appropriate id.
    #[inline]
    pub unsafe fn from_raw(value: NonNull<T>) -> Self {
        Gc {
            collector_id: PhantomData,
            value,
        }
    }
    /// Create a [GcHandle] referencing this object,
    /// allowing it to be used without a context
    /// and referenced across safepoints.
    ///
    /// Requires that the collector [supports handles](`HandleCollectorId`)
    #[inline]
    pub fn create_handle(&self) -> Id::Handle<T::Branded>
    where
        Id: HandleCollectorId,
        T: GcRebrand<'static, Id>,
    {
        Id::create_handle(*self)
    }

    /// Get a reference to the system
    ///
    /// ## Safety
    /// This is based on the assumption that a [GcSystem] must outlive
    /// all of the pointers it owns.
    /// Although it could be restricted to the lifetime of the [CollectorId]
    /// (in theory that may have an internal pointer) it will still live for '&self'.
    #[inline]
    pub fn system(&self) -> &'_ Id::System {
        // This assumption is safe - see the docs
        unsafe { self.collector_id().assume_valid_system() }
    }
}
impl<'gc, T: ?Sized, Id: CollectorId> Gc<'gc, T, Id> {
    /// The value of the underlying pointer
    #[inline(always)]
    pub const fn value(&self) -> &'gc T {
        unsafe { *(&self.value as *const NonNull<T> as *const &'gc T) }
    }
    /// Cast this reference to a raw pointer
    ///
    /// ## Safety
    /// It's undefined behavior to mutate the
    /// value.
    /// The pointer is only valid as long as
    /// the reference is.
    #[inline]
    pub unsafe fn as_raw_ptr(&self) -> *mut T {
        self.value.as_ptr() as *const T as *mut T
    }

    /// Get a reference to the collector's id
    ///
    /// The underlying collector it points to is not necessarily always valid
    #[inline]
    pub fn collector_id(&self) -> &'_ Id {
        Id::from_gc_ptr(self)
    }
}

/// Double-indirection is completely safe
unsafe impl<'gc, T: ?Sized + GcSafe<'gc, Id>, Id: CollectorId> TrustedDrop for Gc<'gc, T, Id> {}
unsafe impl<'gc, T: ?Sized + GcSafe<'gc, Id>, Id: CollectorId> GcSafe<'gc, Id> for Gc<'gc, T, Id> {
    #[inline]
    unsafe fn trace_inside_gc<V>(gc: &mut Gc<'gc, Self, Id>, visitor: &mut V) -> Result<(), V::Err>
    where
        V: GcVisitor,
    {
        // Double indirection is fine. It's just a `Sized` type
        visitor.trace_gc(gc)
    }
}
/// Rebrand
unsafe impl<'gc, 'new_gc, T, Id> GcRebrand<'new_gc, Id> for Gc<'gc, T, Id>
where
    T: GcSafe<'gc, Id> + ?Sized + GcRebrand<'new_gc, Id>,
    Id: CollectorId,
    Self: Trace,
{
    type Branded = Gc<'new_gc, <T as GcRebrand<'new_gc, Id>>::Branded, Id>;
}
unsafe impl<'gc, T: ?Sized + GcSafe<'gc, Id>, Id: CollectorId> Trace for Gc<'gc, T, Id> {
    // We always need tracing....
    const NEEDS_TRACE: bool = true;
    // we never need to be dropped because we are `Copy`
    const NEEDS_DROP: bool = false;

    #[inline]
    fn trace<V: GcVisitor>(&mut self, visitor: &mut V) -> Result<(), V::Err> {
        unsafe {
            // We're delegating with a valid pointer.
            <T as GcSafe<'gc, Id>>::trace_inside_gc(self, visitor)
        }
    }
}
impl<'gc, T: GcSafe<'gc, Id> + ?Sized, Id: CollectorId> Deref for Gc<'gc, T, Id> {
    type Target = T;

    #[inline(always)]
    fn deref(&self) -> &Self::Target {
        self.value()
    }
}
unsafe impl<'gc, O, V, Id> GcDirectBarrier<'gc, Gc<'gc, O, Id>> for Gc<'gc, V, Id>
where
    O: GcSafe<'gc, Id> + 'gc,
    V: GcSafe<'gc, Id> + 'gc,
    Id: CollectorId,
{
    #[inline(always)]
    unsafe fn write_barrier(&self, owner: &Gc<'gc, O, Id>, field_offset: usize) {
        Id::gc_write_barrier(owner, self, field_offset)
    }
}
// We can be copied freely :)
impl<'gc, T: ?Sized, Id: CollectorId> Copy for Gc<'gc, T, Id> {}
impl<'gc, T: ?Sized, Id: CollectorId> Clone for Gc<'gc, T, Id> {
    #[inline(always)]
    fn clone(&self) -> Self {
        *self
    }
}
// Delegating impls
impl<'gc, T: GcSafe<'gc, Id> + Hash, Id: CollectorId> Hash for Gc<'gc, T, Id> {
    #[inline]
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.value().hash(state)
    }
}
impl<'gc, T: GcSafe<'gc, Id> + PartialEq, Id: CollectorId> PartialEq for Gc<'gc, T, Id> {
    #[inline]
    fn eq(&self, other: &Self) -> bool {
        // NOTE: We compare by value, not identity
        self.value() == other.value()
    }
}
impl<'gc, T: GcSafe<'gc, Id> + Eq, Id: CollectorId> Eq for Gc<'gc, T, Id> {}
impl<'gc, T: GcSafe<'gc, Id> + PartialEq, Id: CollectorId> PartialEq<T> for Gc<'gc, T, Id> {
    #[inline]
    fn eq(&self, other: &T) -> bool {
        self.value() == other
    }
}
impl<'gc, T: GcSafe<'gc, Id> + PartialOrd, Id: CollectorId> PartialOrd for Gc<'gc, T, Id> {
    #[inline]
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        self.value().partial_cmp(other.value())
    }
}
impl<'gc, T: GcSafe<'gc, Id> + PartialOrd, Id: CollectorId> PartialOrd<T> for Gc<'gc, T, Id> {
    #[inline]
    fn partial_cmp(&self, other: &T) -> Option<Ordering> {
        self.value().partial_cmp(other)
    }
}
impl<'gc, T: GcSafe<'gc, Id> + Ord, Id: CollectorId> Ord for Gc<'gc, T, Id> {
    #[inline]
    fn cmp(&self, other: &Self) -> Ordering {
        self.value().cmp(other)
    }
}
impl<'gc, T: ?Sized + GcSafe<'gc, Id> + Debug, Id: CollectorId> Debug for Gc<'gc, T, Id> {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        if !f.alternate() {
            // Pretend we're a newtype by default
            f.debug_tuple("Gc").field(&self.value()).finish()
        } else {
            // Alternate spec reveals `collector_id`
            f.debug_struct("Gc")
                .field("collector_id", &self.collector_id)
                .field("value", &self.value())
                .finish()
        }
    }
}
impl<'gc, T: ?Sized + GcSafe<'gc, Id> + Display, Id: CollectorId> Display for Gc<'gc, T, Id> {
    fn fmt(&self, f: &mut Formatter) -> fmt::Result {
        Display::fmt(&self.value(), f)
    }
}

/// In order to send *references* between threads,
/// the underlying type must be sync.
///
/// This is the same reason that `Arc<T>: Send` requires `T: Sync`
unsafe impl<'gc, T, Id> Send for Gc<'gc, T, Id>
where
    T: GcSafe<'gc, Id> + ?Sized + Sync,
    Id: CollectorId + Sync,
{
}

/// If the underlying type is `Sync`, it's safe
/// to share garbage collected references between threads.
///
/// The safety of the collector itself depends on whether [CollectorId] is Sync.
/// If it is, the whole garbage collection implementation should be as well.
unsafe impl<'gc, T, Id> Sync for Gc<'gc, T, Id>
where
    T: GcSafe<'gc, Id> + ?Sized + Sync,
    Id: CollectorId + Sync,
{
}

/// Indicates that a mutable reference to a type
/// is safe to use without triggering a write barrier.
///
/// This means one of either two things:
/// 1. This type doesn't need any write barriers
/// 2. Mutating this type implicitly triggers a write barrier.
///
/// This is the bound for `RefCell<T>`. Since a RefCell doesn't explicitly trigger write barriers,
/// a `RefCell` can only be used with `T` if either:
/// 1. `T` doesn't need any write barriers or
/// 2. `T` implicitly triggers write barriers on any mutation
pub unsafe trait ImplicitWriteBarrier {}
unsafe impl<T: NullTrace> ImplicitWriteBarrier for T {}

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

/// Safely trigger a write barrier before
/// writing to a garbage collected value.
///
/// The value must be in managed memory,
/// a *direct* part of a garbage collected object.
/// Write barriers (and writes) must include a reference
/// to its owning object.
///
/// ## Safety
/// It is undefined behavior to forget to trigger a write barrier.
///
/// Field offsets are unchecked. They must refer to the correct
/// offset (in bytes).
///
/// ### Indirection
/// This trait only support "direct" writes,
/// where the destination field is inline with the source object.
///
/// For example it's correct to implement `GcDirectWrite<Value=A> for (A, B)`,
/// since since `A` is inline with the owning tuple.
///
/// It is **incorrect** to implement `GcDirectWrite<Value=T> for Vec<T>`,
/// since it `T` is indirectly referred to by the vector.
/// There's no "field offset" we can use to get from `*mut Vec` -> `*mut T`.
///
/// The only exception to this rule is [Gc] itself.
/// GcRef can freely implement [GcDirectBarrier] for any (and all values),
/// even though it's just a pointer.
/// It's the final destination of all write barriers and is expected
/// to internally handle the indirection.
pub unsafe trait GcDirectBarrier<'gc, OwningRef>: Trace {
    /// Trigger a write barrier,
    /// before writing to one of the owning object's managed fields
    ///
    /// It is undefined behavior to mutate a garbage collected field
    /// without inserting a write barrier before it.
    ///
    /// Generational, concurrent and incremental GCs need this to maintain
    /// the tricolor invariant.
    ///
    /// ## Safety
    /// The specified field offset must point to a valid field
    /// in the source object.
    ///
    /// The type of this value must match the appropriate field
    unsafe fn write_barrier(&self, owner: &OwningRef, field_offset: usize);
}

/// Indicates that a type's [Drop](core::ops::Drop) implementation is trusted
/// not to resurrect any garbage collected object.
///
/// This is a requirement for a type to be allocated in [GcSimpleAlloc],
/// and also for a type to be `GcSafe`.
///
/// Unlike java finalizers, these trusted destructors
/// avoids a second pass to check for resurrected objects.
/// This is important giving the frequency of destructors
/// when interoperating with native code.
///
/// The collector is of course free to implement support java-style finalizers in addition
/// to supporting these "trusted" destructors.
///
/// ## Safety
/// To implement this trait, the type's destructor
/// must never reference garbage collected pointers that may already be dead
/// and must never resurrect dead objects.
/// The garbage collector may have already freed the other objects
/// before calling this type's drop function.
///
pub unsafe trait TrustedDrop: Trace {}

/// A marker trait correlating all garbage collected pointers
/// corresponding to the specified `Id` are valid for the `'gc` lifetime.
///
/// If this type is implemented for a specific [CollectorId] `Id`,
/// it indicates the possibility of containing pointers belonging to that collector.
///
/// If a type is `NullTrace, it should implement `GcSafe` for all possible collectors.
/// However, if a type `NEEDS_TRACE`, it will usually only implement GcSafe for the specific
/// [CollectorId]s it happens to contain (although this is not guarenteed).
///
/// ## Mixing with other lifetimes
/// Note that `T: GcSafe<'gc, T>` does *not* necessarily imply `T: 'gc`.
/// This allows a garbage collected lifetime to contain shorter lifetimes
///
/// For example,
/// ```
/// # use zerogc::epsilon::{Gc, EpsilonContext as GcContext, EpsilonCollectorId as CollectorId};
/// # use zerogc::prelude::*;
/// #[derive(Trace)]
/// #[zerogc(ignore_lifetimes("'a"), collector_ids(CollectorId))]
/// struct TempLifetime<'gc, 'a> {
///     temp: &'a i32,
///     gc: Gc<'gc, i32>
/// }
/// fn alloc_ref_temp<'gc>(ctx: &'gc GcContext, long_lived: Gc<'gc, i32>) {
///     let temp = 5; // Lives for 'a (shorter than 'gc)
///     let temp_ref = ctx.alloc(TempLifetime {
///         temp: &temp, gc: long_lived
///     });
///     assert_eq!(&temp as *const _, temp_ref.temp as *const _)
/// }
/// ```
///
/// ## Mixing collectors
/// The `Id` parameter allows mixing and matching pointers from different collectors,
/// each with their own 'gc lifetime.
/// For example,
/// ```ignore // TODO: Support this. See issue #33
/// # use zerogc::{Gc, CollectorId, GcSafe};
/// # use zerogc_derive::Trace;
/// # type JsGcId = zerogc::epsilon::EpsilonCollectorId;
/// # type OtherGcId = zerogc::epsilon::EpsilonCollectorId;
/// #[derive(Trace)]
/// struct MixedGc<'gc, 'js> {
///     internal_ptr: Gc<'gc, i32, OtherGcId>,
///     js_ptr: Gc<'js, i32, JsGcId>
/// }
/// impl<'gc, 'js> MixedGc<'gc, 'js> {
///     fn verify(&self) {
///         assert!(<Self as GcSafe<'gc, OtherGcId>>::assert_gc_safe());
///         assert!(<Self as GcSafe<'js, JsGcId>>::assert_gc_safe());
///         // NOT implemented: <Self as GcSafe<'gc, ThirdId>>
///     }
/// }
/// ```
///
/// ## Safety
/// In addition to the guarantees of [Trace] and [TrustedDrop],
/// implementing this type requires that all [Gc] pointers of
/// the specified `Id` have the `'gc` lifetime (if there are any at all).
pub unsafe trait GcSafe<'gc, Id: CollectorId>: Trace + TrustedDrop {
    /// Assert this type is GC safe
    ///
    /// Only used by procedural derive
    #[doc(hidden)]
    fn assert_gc_safe() -> bool
    where
        Self: Sized,
    {
        true
    }
    /// Trace this object behind a [Gc] pointer.
    ///
    /// This is **required** to delegate to one of the following methods on [GcVisitor]:
    /// 1. [GcVisitor::trace_gc] - For regular, `Sized` types
    /// 2. [GcVisitor::trace_array] - For slices and arrays
    /// 3. [GcVisitor::trace_trait_object] - For trait objects
    ///
    /// ## Safety
    /// This must delegate to the appropriate method on [GcVisitor],
    /// or undefined behavior will result.
    ///
    /// The user is required to supply an appropriate [Gc] pointer.
    unsafe fn trace_inside_gc<V>(gc: &mut Gc<'gc, Self, Id>, visitor: &mut V) -> Result<(), V::Err>
    where
        V: GcVisitor;
}

/// A [GcSafe] type with all garbage-collected pointers
/// erased to the `'static`  type.
///
/// This should generally only be used by internal code.
///
/// ## Safety
/// This type is incredibly unsafe. It eliminates all the safety invariants
/// of lifetimes.
pub trait GcSafeErased<Id: CollectorId> = GcSafe<'static, Id>;

/// Assert that a type implements Copy
///
/// Used by the derive code
#[doc(hidden)]
pub fn assert_copy<T: Copy>() {}

/// A wrapper type that assumes its contents don't need to be traced
#[repr(transparent)]
#[derive(Copy, Clone, Debug)]
pub struct AssumeNotTraced<T>(T);
impl<T> AssumeNotTraced<T> {
    /// Assume the specified value doesn't need to be traced
    ///
    /// ## Safety
    /// Undefined behavior if the value contains anything that need to be traced
    /// by a garbage collector.
    #[inline]
    pub unsafe fn new(value: T) -> Self {
        AssumeNotTraced(value)
    }
    /// Unwrap the inner value of this wrapper
    #[inline]
    pub fn into_inner(self) -> T {
        self.0
    }
}
impl<T> Deref for AssumeNotTraced<T> {
    type Target = T;

    #[inline]
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
impl<T> DerefMut for AssumeNotTraced<T> {
    #[inline]
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}
unsafe_gc_impl! {
    target => AssumeNotTraced<T>,
    params => [T],
    bounds => {
        // Unconditionally implement all traits
        Trace => always,
        TraceImmutable => always,
        TrustedDrop => always,
        GcSafe => always,
        GcRebrand => always,
    },
    null_trace => always,
    branded_type => Self,
    NEEDS_TRACE => false,
    NEEDS_DROP => core::mem::needs_drop::<T>(),
    trace_template => |self, visitor| { /* nop */ Ok(()) }
}

/// Allows changing the lifetime of all [`Gc`] references.
///
/// Any other lifetimes should be unaffected by the 'branding'.
/// Since we control the only lifetime,
/// we don't have to worry about interior references.
///
/// ## Safety
/// Assuming the `'new_gc` lifetime is correct,
/// It must be safe to transmute back and forth to `Self::Branded`,
/// switching all garbage collected references from `Gc<'old_gc, T>` to `Gc<'new_gc, T>`
pub unsafe trait GcRebrand<'new_gc, Id: CollectorId>: Trace {
    /// This type with all garbage collected lifetimes
    /// changed to `'new_gc`
    ///
    /// This must have the same in-memory repr as `Self`,
    /// so that it's safe to transmute.
    type Branded: GcSafe<'new_gc, Id> + ?Sized;

    /// Assert this type can be rebranded
    ///
    /// Only used by procedural derive
    #[doc(hidden)]
    fn assert_rebrand() {}
}

/// Indicates that a type can be traced by a garbage collector.
///
/// This doesn't necessarily mean that the type is safe to allocate in a garbage collector ([GcSafe]).
///
/// ## Safety
/// See the documentation of the `trace` method for more info.
/// Essentially, this object must faithfully trace anything that
/// could contain garbage collected pointers or other `Trace` items.
pub unsafe trait Trace {
    /// Whether this type needs to be traced by the garbage collector.
    ///
    /// Some primitive types don't need to be traced at all,
    /// and can be simply ignored by the garbage collector.
    ///
    /// Collections should usually delegate this decision to their element type,
    /// claiming the need for tracing only if their elements do.
    /// For example, to decide `Vec<u32>::NEEDS_TRACE` you'd check whether `u32::NEEDS_TRACE` (false),
    /// and so then `Vec<u32>` doesn't need to be traced.
    /// By the same logic, `Vec<Gc<u32>>` does need to be traced,
    /// since it contains a garbage collected pointer.
    ///
    /// If there are multiple types involved, you should check if any of them need tracing.
    /// One perfect example of this is structure/tuple types which check
    /// `field1::NEEDS_TRACE || field2::NEEDS_TRACE || field3::needs_trace`.
    /// The fields which don't need tracing will always ignored by `GarbageCollector::trace`,
    /// while the fields that do will be properly traced.
    ///
    /// False negatives will always result in completely undefined behavior.
    /// False positives could result in un-necessary tracing, but are perfectly safe otherwise.
    /// Therefore, when in doubt you always assume this is true.
    ///
    /// If this is true `NullTrace` should (but doesn't have to) be implemented.
    /*
     * TODO: Should we move this to `GcSafe`?
     * Needing tracing for `Id1` doesn't nessicitate
     * needing tracing for `Id2`
     */
    const NEEDS_TRACE: bool;
    /// If this type needs a destructor run.
    ///
    /// This is usually equivalent to [core::mem::needs_drop].
    /// However, procedurally derived code can sometimes provide
    /// a no-op drop implementation (for safety)
    /// which would lead to a false positive with `core::mem::needs_drop()`
    const NEEDS_DROP: bool;
    /// Trace each field in this type.
    ///
    /// Structures should trace each of their fields,
    /// and collections should trace each of their elements.
    ///
    /// ### Safety
    /// Some types (like `Gc`) need special actions taken when they're traced,
    /// but those are somewhat rare and are usually already provided by the garbage collector.
    ///
    /// Behavior is restricted during tracing:
    /// ## Permitted Behavior
    /// - Reading your own memory (includes iteration)
    ///   - Interior mutation is undefined behavior, even if you use `GcCell`
    /// - Calling `GcVisitor::trace` with the specified collector
    ///   - `GcVisitor::trace` already verifies that the ids match, so you don't need to do that
    /// - Panicking on unrecoverable errors
    ///   - This should be reserved for cases where you are seriously screwed up,
    ///       and can't fulfill your contract to trace your interior properly.
    ///     - One example is `Gc<T>` which panics if the garbage collectors are mismatched
    ///   - Garbage collectors may chose to [abort](std::process::abort) if they encounter a panic,
    ///     so you should avoid doing it if possible.
    /// ## Never Permitted Behavior
    /// - Forgetting a element of a collection, or field of a structure
    ///   - If you forget an element undefined behavior will result
    ///   - This is why you should always prefer automatically derived implementations where possible.
    ///     - With an automatically derived implementation you will never miss a field
    /// - It is undefined behavior to mutate any of your own data.
    ///   - The mutable `&mut self` is just so copying collectors can relocate GC pointers
    /// - Calling other operations on the garbage collector (including allocations)
    fn trace<V: GcVisitor>(&mut self, visitor: &mut V) -> Result<(), V::Err>;
}

/// A type that can be safely traced/relocated
/// without having to use a mutable reference
///
/// Types with interior mutability (like `RefCell` or `Cell<Gc<T>>`)
/// can safely implement this, since they allow safely relocating the pointer
/// without a mutable reference.
/// Likewise primitives (with new garbage collected data) can also
/// implement this (since they have nothing to trace).
pub unsafe trait TraceImmutable: Trace {
    /// Trace an immutable reference to this type
    ///
    /// The visitor may want to relocate garbage collected pointers,
    /// so any `Gc` pointers must be behind interior mutability.
    fn trace_immutable<V: GcVisitor>(&self, visitor: &mut V) -> Result<(), V::Err>;
}

/// A type that can be traced via dynamic dispatch,
/// specialized for a particular [CollectorId].
///
/// This indicates that the underlying type implements both [Trace]
/// and [GcSafe],
/// even though the specifics may not be known at compile time.
/// If the type is allocated inside a [Gc] pointer,
/// collectors can usually use their own runtime type information
/// to dispatch to the correct tracing code.
///
/// This is useful for use in trait objects,
/// because this marker type is object safe (unlike the regular [Trace] trait).
///
/// ## Safety
/// This type should never be implemented directly.
/// It is automatically implemented for all types that are `Trace + GcSafe`.
///
/// If an object implements this trait, then it the underlying value
/// **must** implement [Trace] and [GcSafe] at runtime,
/// even though that can't be proved at compile time.
///
/// The garbage collector will be able to use its runtime type information
/// to find the appropriate implementation at runtime,
/// even though its not known at compile tme.
pub unsafe trait DynTrace<'gc, Id: CollectorId> {}
unsafe impl<'gc, Id: CollectorId, T: ?Sized + Trace + GcSafe<'gc, Id>> DynTrace<'gc, Id> for T {}

impl<'gc, T, U, Id> CoerceUnsized<Gc<'gc, U, Id>> for Gc<'gc, T, Id>
where
    T: ?Sized + GcSafe<'gc, Id> + Unsize<U>,
    U: ?Sized + GcSafe<'gc, Id>,
    Id: CollectorId,
{
}

/// Marker types for types that don't need to be traced
///
/// If this trait is implemented `Trace::NEEDS_TRACE` must be false
pub unsafe trait NullTrace: Trace + TraceImmutable {
    /// Dummy method for macros to verify that a type actually implements `NullTrace`
    #[doc(hidden)]
    #[inline]
    fn verify_null_trace()
    where
        Self: Sized,
    {
    }
}

/// Visits garbage collected objects
///
/// This should only be used by a [GcSystem]
pub unsafe trait GcVisitor: Sized {
    /// The type of errors returned by this visitor
    type Err: Debug;

    /// Trace a reference to the specified value
    #[inline]
    fn trace<T: Trace + ?Sized>(&mut self, value: &mut T) -> Result<(), Self::Err> {
        value.trace(self)
    }
    /// Trace an immutable reference to the specified value
    #[inline]
    fn trace_immutable<T: TraceImmutable + ?Sized>(&mut self, value: &T) -> Result<(), Self::Err> {
        value.trace_immutable(self)
    }

    /// Visit a garbage collected pointer
    ///
    /// ## Safety
    /// Undefined behavior if the GC pointer isn't properly visited.
    unsafe fn trace_gc<'gc, T, Id>(&mut self, gc: &mut Gc<'gc, T, Id>) -> Result<(), Self::Err>
    where
        T: GcSafe<'gc, Id>,
        Id: CollectorId;
}
