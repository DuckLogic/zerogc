//! Defines the core [Trace] trait.

use core::fmt::Debug;
use core::ops::{Deref, DerefMut};

use zerogc_derive::unsafe_gc_impl;

use crate::system::CollectorId;

pub mod barrier;
mod gcptr;

pub use self::gcptr::Gc;

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
