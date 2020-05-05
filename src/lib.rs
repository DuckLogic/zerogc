//! Zero overhead tracing garbage collection for rust, by abusing the borrow checker.
//!
//!
//! The idea behind this collector that by making all potential for collections explicit,
//! we can take advantage of the borrow checker to statically guarantee everything's valid.
//! We call these potential collections 'safepoints' and the borrow checker can statically prove all uses are valid in between.
//! There is no chance for collection to happen in between these safepoints,
//! and you have unrestricted use of garbage collected pointers until you reach one.
//!
//! ## Major Features
//! 1. Easy to use, since `Gc<T>` is `Copy` and coerces to a reference.
//! 2. Absolutely zero overhead when modifying pointers, since `Gc<T>` is `Copy`.
//! 3. Support for important libraries builtin to the collector
//! 4. Unsafe code has complete freedom to manipulate garbage collected pointers, and it doesn't need to understand the distinction
//! 5. Uses rust's lifetime system to ensure all roots are known at explicit safepoints, without any runtime overhead.
//! 6. Collection can only happen with an explicit `safepoint` call and has no overhead between these calls,
//! 7. Optional graceful handling of allocation failures.
//!
//! ## Usage
//! ````rust
//! let collector = GarbageCollector::default();
//! let retained: Gc<Vec<u32>> = collector.alloc(vec![1, 2, 3]);
//! assert_eq!(retained[0], 1) // Garbage collected references deref directly to slices
//! let other =
//! /*
//!  * Garbage collect `retained`
//!  * This will explicitly trace the retained object `retained`,
//!  * and sweep the rest (`sweeped`) away.
//!  */
//! safepoint!(collector, retained);
//! retained.; // This borrow checker will allow this since `retained` was retained
//! ````

/*
 * I want this library to use 'mostly' stable features,
 * unless there's good justification to use an unstable feature.
 */
#![feature(
    const_fn, // I refuse to break encapsulation
    optin_builtin_traits, // These are much clearer to use.
    trace_macros // This is a godsend for debugging
)]
extern crate unreachable;
extern crate num_traits;
extern crate core;


use std::mem;
use std::ptr::NonNull;
use std::ops::Deref;
use std::cell::{UnsafeCell};
use std::fmt::{Debug};

#[macro_use]
mod manually_traced;
pub mod cell;


pub use self::cell::{GcCell, GcRefCell};

mod utils;

use std::any::TypeId;

/// Indicate it's safe to begin a garbage collection,
/// while keeping the specified root alive.
///
/// All other garbage collected pointers that aren't reachable from the root are invalidated.
/// They have a lifetime that references the [GarbageCollectorRef]
/// and the borrow checker considers the safepoint a 'mutation'.
///
/// The root is exempted from the "mutation" and rebound to the new lifetime.
///
/// ## Example
/// ```
/// let root = safepoint!(collector, root);
/// ```
///
/// ## Safety
/// This macro is completely safe, although it expands to unsafe code internally.
#[macro_export]
macro_rules! safepoint {
    ($collector:ident, $value:expr) => {{
        use std::mem::ManuallyDropped;
        use $crate::{GarbageCollectorRef};
        // TODO: What happens if we panic during a collection
        let mut erased = GarbageCollectorRef::rebrand_static(&$collector, $value);
        GarbageCollectorRef::basic_safepoint(&mut $collector, &mut erased);
        GarbageCollectorRef::rebrand_self(&collector, erased)
    }};
}

/// The globally unique id of this garbage collector,
/// which is used to brand all garbage collected objects.
///
/// Some collectors may have multiple instances at runtime
/// and this ensures that their objects don't get mixed up.
/// Other collectors have a single global instance so they can just use a zero-sized type.
///
/// This also allows garbage collectors to perform call
pub unsafe trait CollectorId: Copy + Eq + Debug {
    #[inline(always)]
    fn matches<T: CollectorId>(self, other: T) -> bool {
        // NOTE: Type comparison will be constant after monomorphization
        if TypeId::of::<Self>() == TypeId::of::<T> {
            self == unsafe { mem::transmute::<T, Self>(other) }
        } else {
            false
        }
    }
}

/// A garbage collector implementation.
///
/// This is completely safe and zero-overhead.
pub unsafe trait GarbageCollectionSystem {
    type Id: CollectorId;

    /// Give the globally unique id of the collector.
    ///
    /// This must be unique for two different collectors in the same process,
    /// but not necessarily unique between different processes.
    fn id(&self) -> Self::Id;
}

/// The context of garbage collection,
/// which can be frozen at a safepoint.
///
/// This is essentially used to maintain a shadow-stack to a set of roots,
/// which are guarenteed not to be collected until a safepoint.
///
/// This context doesn't necessarily support allocation (see [GcAllocContext] for that).
pub trait GcContext {
    type Id: CollectorId;
    /// Inform the garbage collection system we are at a safepoint
    /// and are ready for a potential garbage collection.
    ///
    /// This method is unsafe, see the `safepoint!` macro for a safe wrapper.
    unsafe fn basic_safepoint<T: Trace>(&mut self, value: &T);

    #[inline(always)]
    #[doc(hidden)]
    unsafe fn rebrand_static<T: GcBrand<'static, Self::Id>>(&self, value: T) -> T::Branded {
        mem::transmute(value)
    }
    #[inline(always)]
    #[doc(hidden)]
    unsafe fn rebrand_self<'a, T: GcBrand<'a, Self::Id>>(&'a self, value: T) -> T::Branded {
        mem::transmute(self)
    }

    /// Stop the collector at a safepoint,
    /// then invoke the closure with a temporary [GcContext].
    ///
    /// The specified value is used both as a root for the initial safepoint
    /// and is guarenteed to live throughout the created context for the closure.
    fn recurse_context<T, F, R>(&mut self, value: T, func: F) -> R
        where T: Trace,
              F: for<'a, 'b> FnOnce(&'a mut Self, &'b <T as GcBrand<'b, Self::Id>>::Branded) -> R;
}
pub trait GcAllocContext: GcContext {
    type MemoryErr: Debug;
    /// Allocate the specified object in this garbage collector,
    /// binding it to the lifetime of this collector.
    ///
    /// The object will never be collected until the next safepoint,
    /// which is considered a mutation by the borrow checker and will be statically checked.
    /// Therefore, we can statically guarantee the pointers will survive until the next safepoint.
    ///
    /// See `safepoint!` docs on how to properly invoke a safepoint
    /// and transfer values across it.
    ///
    /// This gives a immutable reference to the resulting object.
    /// Once allocated, the object can only be correctly modified with a `GcCell`
    #[inline]
    fn alloc<T: GcSafe>(&self, value: T) -> Gc<'_, T, Self::Id> {
        self.try_alloc(value)
            .unwrap_or_else(|cause| {
                panic!("Failed to allocate: {:?}", cause)
            })
    }

    /// Same as `alloc`, but returns an error instead of panicing on failure
    fn try_alloc<T: GcSafe>(&self, value: T) -> Result<Gc<'_, T, Self::Id>, Self::MemoryErr>;
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
#[derive(PartialEq, Eq, PartialOrd, Ord, )]
pub struct Gc<'gc, T: GcSafe + ?Sized + 'gc, Id: CollectorId> {
    id: Id,
    ptr: UnsafeCell<NonNull<T>>
}
impl<'gc, T: ?Sized + GcSafe, Id: CollectorId> Gc<'gc, T, Id> {
    /// Create a new garbage collected pointer to the specified value.
    ///
    /// ## Safety
    /// Not only are you assuming the specified pointer is valid for the `'gc` liftime,
    /// you're also assuming that it points to a garbage collected object.
    ///
    /// Specifically, since you're assuming the value was allocated from part of a valid `GcObject`,
    /// and included .
    /// Dark magic and evil pointer casts are performed to turn the pointer back into a `GcObject`,
    /// so you're assuming that this is valid to perform.
    #[inline(always)]
    pub unsafe fn new(id: Id, ptr: NonNull<T>) -> Self {
        Gc { id, ptr: UnsafeCell::new(ptr) }
    }
    #[inline]
    pub fn value(self) -> &'gc T {
        unsafe { self.ptr.into_inner().as_ref() }
    }
}
impl<'gc, T: ?Sized + GcSafe, Id: CollectorId> Deref for Gc<'gc, T, Id> {
    type Target = &'gc T;

    #[inline(always)]
    fn deref(&self) -> &&'gc T {
        &self.value()
    }
}
impl<'gc, T: GcSafe + ?Sized, Id: CollectorId> Clone for Gc<'gc, T, Id> {
    #[inline(always)]
    fn clone(&self) -> Self {
        *self
    }
}
impl<'gc, T: GcSafe + ?Sized, Id: CollectorId> Copy for Gc<'gc, T, Id> {}
unsafe impl<'gc, 'new_gc, T, Id> GcBrand<'new_gc, Id> for Gc<'gc, T, Id>
    where T: GcSafe + ?Sized + GcBrand<'new_gc, Id>,
          Id: CollectorId {
    type Branded = Gc<'new_gc, T::Branded, Id>;
}

/// Indicates that a type can be safely allocated by a garbage collector.
///
/// ## Safety
/// Custom destructors must never reference garbage collected pointers.
/// The garbage collector may have already freed the other objects
/// before calling this type's drop function.
///
/// Unlike java finalizers, this allows us to deallocate objects normally
/// and avoids a second pass over the objects
/// to check for resurrected objects.
pub unsafe trait GcSafe: Trace {}

/// Changes all references to garbage
/// collected objects to match `'new_gc`.
///
/// This indicates that its safe to transmute to the new `Branded` type
/// and all that will change is the lifetimes.
///
/// Only pointers with the collector id type `Id` will have their lifetime changed.
pub unsafe trait GcBrand<'new_gc, Id: CollectorId> {
    type Branded: ?Sized + 'new_gc;
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
    /// False positives could result in unessicarry tracing, but are perfectly safe otherwise.
    /// Therefore, when in doubt you always assume this is true.
    const NEEDS_TRACE: bool;
    /// Visit each
    ///
    /// Users should never invoke this method, and always use `GarbageCollector::trace` instead.
    /// Only the collector itself is premited to call this method,
    /// and **it is undefined behavior for the user to invoke this**.
    ///
    /// Structures should trace each of their fields,
    /// and collections should trace each of their elements.
    ///
    ///
    /// ### Safety
    /// Some types (like `Gc`) need special actions taken when they're traced,
    /// but those are somewhat rare and are usually already provided by the garbage collector.
    ///
    /// Unless I explicitly document actions as legal I may decide to change i.
    /// I am only bound by the constraints of [semantic versioning](http://semver.org/) in the trace function
    /// if I explicitly document it as safe behavior in this method's documentation.
    /// If you try something that isn't explicitly documented here as permitted behavior,
    /// the collector may choose to override your memory with `0xDEADBEEF`.
    /// ## Always Permitted
    /// - Reading your own memory (includes iteration)
    ///   - Interior mutation is undefined behavior, even if you use `GcCell`
    /// - Calling `GcVisitor::visit` with the specified collector
    ///   - `GarbageCollector::trace` already verifies that it owns the data, so you don't need to do that
    /// - Panicking
    ///   - This should be reserved for cases where you are seriously screwed up,
    ///       and can't fulfill your contract to trace your interior properly.
    ///     - One example is `Gc<T>` which panics if the garbage collectors are mismatched
    ///   - This rule may change in future versions, depending on how we deal with multi-threading.
    /// ## Never Permitted Behavior
    /// - Forgetting a element of a collection, or field of a structure
    ///   - If you forget an element undefined behavior will result
    ///   - This is why we always prefer automatically derived implementations where possible.
    ///     - You will never trigger undefined behavior with an automatic implementation,
    ///       and it'll always be completely sufficient for safe code (aside from destructors).
    ///     - With an automatically derived implementation you will never miss a field,
    /// - Invoking this function directly, without delegating to `GcVisitor`
    unsafe fn visit<V: GcVisitor>(&self, visitor: &mut V) -> Result<(), V::Err>;
}

/// Visits garbage collected objects
///
/// This should only be used by a [GarbageCollectionSystem]
pub unsafe trait GcVisitor {
    type Err;
    fn visit<T: Trace>(&mut self, value: &T) -> Result<(), Self::Err>;
    /// Visit a garbage collected pointer
    fn visit_gc<T, Id>(&mut self, gc: &Gc<'_, T, Id>) -> Result<(), Self::Err>
        where T: GcSafe + ?Sized, Id: CollectorId;
}



//
// Fundamental implementations
//

/// Double indirection is safe (but usually stupid)
unsafe impl<'gc, T: GcSafe + ?Sized, Id: CollectorId> GcSafe for Gc<'gc, T, Id> {}

unsafe impl<'gc, T: GcSafe + ?Sized, Id: CollectorId> Trace for Gc<'gc, T, Id> {
    const NEEDS_TRACE: bool = true;

    #[inline(always)]
    unsafe fn visit<V: GcVisitor>(&self, visitor: &mut V) -> Result<(), V::Err> {
        visitor.visit_gc(self)
    }
}