#![feature(
    const_panic, // RFC 2345 - Const asserts
    ptr_metadata, // RFC 2580 - Pointer meta
    coerce_unsized, // RFC 0982 - DST coercion
    unsize,
)]
#![feature(maybe_uninit_slice)]
#![feature(new_uninit)]
#![deny(missing_docs)]
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
use core::mem::{self};
use core::ops::{Deref, DerefMut};
use core::ptr::{NonNull, Pointee, DynMetadata};
use core::marker::PhantomData;
use core::hash::{Hash, Hasher};
use core::fmt::{self, Debug, Formatter};

use zerogc_derive::unsafe_gc_impl;

use crate::vec::{GcVec};
pub use crate::vec::GcArray;
use std::ops::CoerceUnsized;
use std::marker::Unsize;

#[macro_use]
mod manually_traced;
#[macro_use]
mod macros;
pub mod cell;
pub mod prelude;
pub mod dummy_impl;
pub mod vec;

/// Invoke the closure with a temporary [GcContext],
/// then perform a safepoint afterwards.
///
/// Normally returns a tuple `($updated_root, $closure_result)`.
///
/// If a value is provided it is considered as a root of garbage collection
/// both for the safepoint and the duration of the entire context.
///
/// # Safety
/// This macro is completely safe, although it expands to unsafe code internally.
#[macro_export(local_inner_macros)]
macro_rules! safepoint_recurse {
    ($context:ident, |$sub_context:ident| $closure:expr) => {{
        let ((), result) = safepoint_recurse!($context, (), |$sub_context, new_root| {
            let () = new_root;
            $closure
        });
        result
    }};
    ($context:ident, $root:expr, |$sub_context:ident, $new_root:ident| $closure:expr) => {{
        let mut root = $root;
        let result = unsafe { __recurse_context!($context, &mut root, |$sub_context, $new_root| {
            $closure
        }) };
        /*
         * NOTE: We're assuming result is unmanaged here
         * The borrow checker will verify this is true (by marking this as a mutation).
         * If you need a manged result, use the @managed_result variant
         */
        let updated_root = safepoint!($context, $root);
        (updated_root, result)
    }};
    ($context:ident, $root:expr, @managed_result, |$sub_context:ident, $new_root:ident| $closure:expr) => {{
        use $crate::{GcContext};
        let mut root = $root;
        let erased_result = unsafe { __recurse_context!(
            $context, &mut root,
            |$sub_context, $new_root| {
                let result = $closure;
                $sub_context.rebrand_static(result)
            }
        ) };
        /*
         * Rebrand back to the current collector lifetime
         * It could have possibly been allocated from the sub-context inside the closure,
         * but as long as it was valid at the end of the closure it's valid now.
         * We trust that GcContext::recurse_context
         * did not perform a collection after calling the closure.
         */
        let result = unsafe { $context.rebrand_self(**erased_result) };
        safepoint!($context, (root, result))
    }};
}

/// Create a new sub-context for the duration of the closure
///
/// The specified `root` object will be appended to the shadow-stack
/// and is guarenteed to live for the entire lifetime of the closure
/// (and the created sub-context).
///
/// Unlike [safepoint_recurse!] this doesn't imply a safepoint anywhere.
///
/// # Safety
/// This doesn't actually mutate the original collector.
/// It is possible user code could trigger a collection in the closure
/// without the borrow checker noticing invalid pointers elsewhere.
/// (See docs for [GcContext::recurse_context])
///
/// It is not publicly exposed for this reason
#[macro_export]
#[doc(hidden)]
macro_rules! __recurse_context {
    ($context:ident, $root:expr, |$sub_context:ident, $new_root:ident| $closure:expr) => {{
        use $crate::{GcContext};
        // TODO: Panic safety
        $context.recurse_context(&mut $root, |mut $sub_context, erased_root| {
            /*
             * NOTE: Guarenteed to live for the lifetime of the entire closure.
             * However, it could be relocated if 'sub_collector' is collected
             */
            let $new_root = $sub_context.rebrand_self(*erased_root);
            $closure
        })
    }};
}

/// Indicate it's safe to begin a garbage collection,
/// while keeping the specified root alive.
///
/// All other garbage collected pointers that aren't reachable
/// from the root are invalidated.
/// They have a lifetime that references the [GcContext]
/// and the borrow checker considers the safepoint a 'mutation'.
///
/// The root is exempted from the "mutation" and rebound to the new lifetime.
///
/// ## Example
/// ```
/// # use ::zerogc::safepoint;
/// # let mut context = zerogc::dummy_impl::DummySystem::new().new_context();
/// # // TODO: Can we please get support for non-Sized types like `String`?!?!?!
/// let root = zerogc::dummy_impl::leaked(String::from("potato"));
/// let root = safepoint!(context, root);
/// assert_eq!(**root, "potato");
/// ```
///
/// ## Safety
/// This macro is completely safe, although it expands to unsafe code internally.
#[macro_export]
macro_rules! safepoint {
    ($context:ident, $value:expr) => {unsafe {
        use $crate::{GcContext};
        // TODO: What happens if we panic during a collection
        /*
         * Some collectors support multiple running instances
         * with different ids, handing out different GC pointers.
         * TODO: Should we be checking somehow that the ids match?
         */
        let mut erased = $context.rebrand_static($value);
        $context.basic_safepoint(&mut &mut erased);
        $context.rebrand_self(erased)
    }};
}

/// Indicate its safe to begin a garbage collection (like [safepoint!])
/// and then "freeze" the specified context.
///
/// Until it's unfrozen, the context can't be used for allocation.
/// Its roots are marked invalid, since the collector could be relocating them.
/// However, the roots of any parent contexts are still considered valid.
///
/// This allows other threads to perform collections *without blocking this thread*.
#[macro_export]
macro_rules! freeze_context {
    ($context:ident) => {unsafe {
        use $crate::{GcContext, FrozenContext};
        let mut context = $context;
        context.freeze();
        FrozenContext::new(context)
    }};
}

/// Unfreeze the context, allowing it to be used again
///
/// Returns a [GcContext] struct.
#[macro_export]
macro_rules! unfreeze_context {
    ($frozen:ident) => {unsafe {
        use $crate::{FrozenContext, GcContext};
        let mut context = FrozenContext::into_context($frozen);
        context.unfreeze();
        context
    }};
}

/// A garbage collector implementation.
///
/// These implementations should be completely safe and zero-overhead.
pub unsafe trait GcSystem {
    /// The type of collector IDs given by this system
    type Id: CollectorId;
    /// The type of contexts used in this sytem
    type Context: GcContext<Id=Self::Id>;
}


/// A system which supports creating handles to [Gc] references.
///
/// This type-system hackery is needed because
/// we need to place bounds on `T as GcBrand`
// TODO: Remove when we get more powerful types
pub unsafe trait GcHandleSystem<'gc, 'a, T: GcSafe + ?Sized + 'gc>: GcSystem
    where T: GcErase<'a, Self::Id>,
          <T as GcErase<'a, Self::Id>>::Erased: GcSafe {
    /// The type of handles to this object.
    type Handle: GcHandle<<T as GcErase<'a, Self::Id>>::Erased, System=Self>;

    /// Create a handle to the specified GC pointer,
    /// which can be used without a context
    ///
    /// NOTE: Users should only use from [Gc::create_handle].
    ///
    /// The system is implicit in the [Gc]
    #[doc(hidden)]
    fn create_handle(gc: Gc<'gc, T, Self::Id>) -> Self::Handle;
}

/// The context of garbage collection,
/// which can be frozen at a safepoint.
///
/// This is essentially used to maintain a shadow-stack to a set of roots,
/// which are guarenteed not to be collected until a safepoint.
///
/// This context doesn't necessarily support allocation (see [GcSimpleAlloc] for that).
pub unsafe trait GcContext: Sized {
    /// The system used with this context
    type System: GcSystem<Context=Self, Id=Self::Id>;
    /// The type of ids used in the system
    type Id: CollectorId;

    /// Potentially perform a garbage collection, freeing
    /// all objects that aren't reachable from the specified root.
    ///
    /// This is a less safe version of [GcContext::basic_safepoint]
    /// that doesn't explicitly mark this context as "mutated".
    /// However, just like any other safepoint,
    /// it still logically invalidates all unreachable objects
    /// because the collector might free them.
    ///
    /// ## Safety
    /// All objects that are potentially in-use must be reachable from the specified root.
    /// Otherwise, they may be accidentally freed during garbage collection.
    ///
    /// It is the user's responsibility to check this,
    /// since this doesn't statically invalidate (or mutate)
    /// the outstanding references.
    unsafe fn unchecked_safepoint<T: Trace>(&self, root: &mut &mut T);

    /// Potentially perform a garbage collection, freeing
    /// all objects that aren't reachable from the specified root.
    ///
    /// This is what the [safepoint!] macro expands to internally.
    /// For example
    /// ```no_run
    /// # use zerogc::prelude::*;
    /// # use zerogc_derive::Trace;
    /// # use zerogc::dummy_impl::{Gc, DummyCollectorId, DummyContext as ExampleContext};
    /// # #[derive(Trace)]
    /// # struct Obj { val: u32 }
    /// fn example<'gc>(ctx: &'gc mut ExampleContext, root: Gc<'gc, Obj>) {
    ///     let temp = ctx.alloc(Obj { val: 12 }); // Lives until the call to 'basic_safepoint'
    ///     assert_eq!(temp.val, 12); // VALID
    ///     // This is what `let root = safepoint!(ctx, root)` would expand to:
    ///     let root = unsafe {
    ///         /*
    ///          * Change the lifetime of root from 'gc -> 'static
    ///          * This is necessary so the mutation of 'basic_safepoint'
    ///          * wont invalidate it.
    ///          */
    ///         let mut preserved_root: Gc<'static, Obj> = ctx.rebrand_static(root);
    ///         /*
    ///          * This invalidates 'temp' (because it mutates the owning context)
    ///          * However, preserved_root is fine because it has been changed the 'static
    ///          * lifetime.
    ///          */
    ///         ctx.basic_safepoint(&mut &mut preserved_root);
    ///         /*
    ///          *  It would be unsafe for the user to access
    ///          * preserved_root, because its lifetime of 'static is too large.
    ///          * The call to 'rebrand_self' changes the lifetime of preserved_root
    ///          * back to the (new) lifetime of the context.
    ///          *
    ///          * The safe result is returned as the result of the block.
    ///          * The user has chosen to re-assign the result to the `root` variable
    ///          * via `let root = safepoint!(ctx, root)`.
    ///          */
    ///         ctx.rebrand_self(preserved_root)
    ///     };
    ///     // assert_eq!(temp.val, 12); // INVALID. `temp` was mutated and bound to the old lifetime
    ///     assert_eq!(root.val, 4); // VALID - The lifetime has been updated
    /// }
    /// ```
    #[inline]
    unsafe fn basic_safepoint<T: Trace>(&mut self, root: &mut &mut T) {
        self.unchecked_safepoint(root)
    }

    /// Inform the garbage collection system we are at a safepoint
    /// and are ready for a potential garbage collection.
    ///
    /// Unlike a `basic_safepoint`, the collector continues to
    /// stay at the safepoint instead of returning immediately.
    /// The context can't be used for anything (including allocations),
    /// until it is unfrozen.
    ///
    /// This allows other threads to perform collections while this
    /// thread does other work (without using the GC).
    ///
    /// The current contexts roots are considered invalid
    /// for the duration of the collection,
    /// since the collector could potentially relocate them.
    ///
    /// Any parent contexts are fine and their roots will be
    /// preserved by collections.
    ///
    /// ## Safety
    /// Assumes this context is valid and not already frozen.
    ///
    /// Don't invoke this directly
    unsafe fn freeze(&mut self);

    /// Unfreeze this context, allowing it to be used again.
    ///
    /// ## Safety
    /// Must be a valid context!
    /// Must be currently frozen!
    ///
    /// Don't invoke this directly
    unsafe fn unfreeze(&mut self);

    /// Rebrand to the specified root so that it
    /// lives for the `'static` lifetime.
    ///
    /// ## Safety
    /// This function is unsafe to use directly.
    ///
    /// It should only be used as part of the [safepoint!] macro.
    #[inline(always)]
    unsafe fn rebrand_static<'a, T>(&self, value: T) -> T::Erased
        where T: GcErase<'a, Self::Id>, T::Erased: Sized {
        let branded = mem::transmute_copy(&value);
        mem::forget(value);
        branded
    }
    /// Rebrand the specified root so that it
    /// lives for the lifetime of this context.
    ///
    /// This effectively undoes the effect of [GcContext::rebrand_static].
    ///
    /// ## Safety
    /// This function is unsafe to use directly.
    ///
    /// It should only be used as part of the [safepoint!] macro.
    #[inline(always)]
    unsafe fn rebrand_self<'gc, T>(&'gc self, value: T) -> T::Branded
        where T: GcRebrand<'gc, Self::Id>, T::Branded: Sized {
        let branded = mem::transmute_copy(&value);
        mem::forget(value);
        branded
    }

    /// Invoke the closure with a temporary [GcContext],
    /// preserving the specified object as a root of garbage collection.
    ///
    /// This will add the specified root to the shadow stack
    /// before invoking the closure. Therefore, any safepoints
    /// invoked inside the closure/sub-function will implicitly preserve all
    /// objects reachable from the root in the parent function.
    ///
    /// This doesn't nessicarrily imply a safepoint,
    /// although its wrapper macro ([safepoint_recurse!]) typically does.
    ///
    /// ## Safety
    /// All objects that are in use must be reachable from the specified root.
    ///
    /// The specified closure could trigger a collection in the sub-context,
    /// so all live objects in the parent function need to be reachable from
    /// the specified root.
    ///
    /// See the [safepoint_recurse!] macro for a safe wrapper
    unsafe fn recurse_context<T, F, R>(&self, root: &mut &mut T, func: F) -> R
        where T: Trace, F: for <'gc> FnOnce(&'gc mut Self, &'gc mut T) -> R;
}
/// A simple interface to allocating from a [GcContext]. 
///
/// Some garbage collectors implement more complex interfaces,
/// so implementing this is optional
pub unsafe trait GcSimpleAlloc: GcContext {
    /// Allocate room for a object in, but don't finish initializing it.
    ///
    /// ## Safety
    /// The object **must** be initialized by the time the next collection
    /// rolls around, so that the collector can properly trace it
    unsafe fn alloc_uninit<'gc, T>(&'gc self) -> (Self::Id, *mut T)
        where T: GcSafe + 'gc;
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
    fn alloc<'gc, T>(&'gc self, value: T) -> Gc<'gc, T, Self::Id>
        where T: GcSafe + 'gc {
        unsafe {
            let (id, ptr) = self.alloc_uninit::<T>();
            ptr.write(value);
            Gc::from_raw(id, NonNull::new_unchecked(ptr))
        }
    }
    /// Allocate a slice with the specified length,
    /// whose memory is uninitialized
    ///
    /// ## Safety
    /// The slice **MUST** be initialized by the next safepoint.
    /// By the time the next collection rolls around,
    /// the collector will assume its entire contents are initialized.
    ///
    /// In particular, the traditional way to implement a [Vec] is wrong.
    /// It is unsafe to leave the range of memory `[len, capacity)` uninitialized.
    unsafe fn alloc_uninit_slice<'gc, T>(&'gc self, len: usize) -> (Self::Id, *mut T)
        where T: GcSafe + 'gc;
    /// Allocate a slice, copied from the specified input
    fn alloc_slice_copy<'gc, T>(&'gc self, src: &[T]) -> GcArray<'gc, T, Self::Id>
        where T: GcSafe + Copy + 'gc {
        unsafe {
            let (_id, res_ptr) = self.alloc_uninit_slice::<T>(src.len());
            res_ptr.copy_from_nonoverlapping(src.as_ptr(), src.len());
            GcArray::from_raw_ptr(
                NonNull::new_unchecked(res_ptr),
                src.len()
            )
        }
    }
    /// Allocate a slice by filling it with results from the specified closure.
    ///
    /// The closure receives the target index as its only argument.
    ///
    /// ## Safety
    /// The closure must always succeed and never panic.
    ///
    /// Otherwise, the gc may end up tracing the values even though they are uninitialized.
    #[inline]
    unsafe fn alloc_slice_fill_with<'gc, T, F>(&'gc self, len: usize, mut func: F) -> GcArray<'gc, T, Self::Id>
        where T: GcSafe + 'gc, F: FnMut(usize) -> T {
        let (_id, res_ptr) = self.alloc_uninit_slice::<T>(len);
        for i in 0..len {
            res_ptr.add(i).write(func(i));
        }
        GcArray::from_raw_ptr(
            NonNull::new_unchecked(res_ptr),
            len
        )
    }
    /// Allocate a slice of the specified length,
    /// initializing everything to `None`
    #[inline]
    fn alloc_slice_none<'gc, T>(&'gc self, len: usize) -> GcArray<'gc, Option<T>, Self::Id>
        where T: GcSafe + 'gc {
        unsafe {
            self.alloc_slice_fill_with(len, |_idx| None)
        }
    }
    /// Allocate a slice by repeatedly copying a single value.
    #[inline]
    fn alloc_slice_fill_copy<'gc, T>(&'gc self, len: usize, val: T) -> GcArray<'gc, T, Self::Id>
        where T: GcSafe + Copy + 'gc {
        unsafe {
            self.alloc_slice_fill_with(len, |_idx| val)
        }
    }
    /// Create a new [GcVec] with zero initial length,
    /// with an implicit reference to this [GcContext].
    fn alloc_vec<'gc, T>(&'gc self) -> GcVec<'gc, T, Self>
        where T: GcSafe + 'gc;
    /// Allocate a new [GcVec] with the specified capacity
    /// and an implcit reference to this [GcContext]
    fn alloc_vec_with_capacity<'gc, T>(&'gc self, capacity: usize) -> GcVec<'gc, T, Self>
        where T: GcSafe + 'gc;
}
/// The internal representation of a frozen context
///
/// ## Safety
/// Don't use this directly!!!
#[doc(hidden)]
#[must_use]
pub struct FrozenContext<C: GcContext> {
    /// The frozen context
    context: C,
}
impl<C: GcContext> FrozenContext<C> {
    #[doc(hidden)]
    #[inline]
    pub unsafe fn new(context: C) -> Self {
        FrozenContext { context }
    }
    #[doc(hidden)]
    #[inline]
    pub unsafe fn into_context(self) -> C {
        self.context
    }
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
pub unsafe trait CollectorId: Copy + Eq + Debug + NullTrace + 'static {
    /// The type of the garbage collector system
    type System: GcSystem<Id=Self>;
    /// The raw representation of vectors in this collector.
    ///
    /// May be [crate::vec::repr::VecUnsupported] if vectors are unsupported.
    type RawVecRepr: crate::vec::repr::GcVecRepr;

    /// Get the runtime id of the collector that allocated the [Gc]
    fn from_gc_ptr<'a, 'gc, T>(gc: &'a Gc<'gc, T, Self>) -> &'a Self
        where T: GcSafe + ?Sized + 'gc, 'gc: 'a;

    /// Resolve the length of the specified [GcArray]
    fn resolve_array_len<'gc, T>(array: GcArray<'gc, T, Self>) -> usize
        where T: GcSafe + 'gc;

    /// Resolve the CollectorId for the specified [GcArray]
    ///
    /// This is the [GcArray] counterpart of `from_gc_ptr`
    fn resolve_array_id<'a, 'gc, T>(gc: &'a GcArray<'gc, T, Self>) -> &'a Self
        where T: GcSafe + 'gc, 'gc: 'a;

    /// Perform a write barrier before writing to a garbage collected field
    ///
    /// ## Safety
    /// Similar to the [GcDirectBarrier] trait, it can be assumed that
    /// the field offset is correct and the types match.
    unsafe fn gc_write_barrier<'gc, O: GcSafe + ?Sized + 'gc, V: GcSafe + ?Sized + 'gc>(
        owner: &Gc<'gc, O, Self>,
        value: &Gc<'gc, V, Self>,
        field_offset: usize
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
#[repr(transparent)]
pub struct Gc<'gc, T: GcSafe + ?Sized + 'gc, Id: CollectorId> {
    value: NonNull<T>,
    /// Marker struct used to statically identify the collector's type
    ///
    /// The runtime instance of this value can be computed from the pointer itself: `NonNull<T>`
    collector_id: PhantomData<Id>,
    /*
     * TODO: I think this lifetime variance is safe
     * Better add some tests and an explanation.
     */
    marker: PhantomData<&'gc T>,
}
impl<'gc, T: GcSafe + ?Sized + 'gc, Id: CollectorId> Gc<'gc, T, Id> {
    /// Create a GC pointer from a raw ID/pointer pair
    ///
    /// ## Safety
    /// Undefined behavior if the underlying pointer is not valid
    /// and associated with the collector corresponding to the id.
    #[inline]
    pub unsafe fn from_raw(id: Id, value: NonNull<T>) -> Self {
        let res = Gc { collector_id: PhantomData, value, marker: PhantomData };
        #[cfg(debug_assertions)] {
            debug_assert_eq!(id, *Id::from_gc_ptr(&res));
        }
        res
    }

    /// The value of the underlying pointer
    #[inline(always)]
    pub fn value(&self) -> &'gc T {
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

    /// Create a handle to this object, which can be used without a context
    #[inline]
    pub fn create_handle<'a>(&self) -> <Id::System as GcHandleSystem<'gc, 'a, T>>::Handle
        where Id::System: GcHandleSystem<'gc, 'a, T>,
              T: GcErase<'a, Id> + 'a,
              <T as GcErase<'a, Id>>::Erased: GcSafe + 'a {
        <Id::System as GcHandleSystem<'gc, 'a, T>>::create_handle(*self)
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

    /// Get a reference to the collector's id
    ///
    /// The underlying collector it points to is not necessarily always valid
    #[inline]
    pub fn collector_id(&self) -> &'_ Id {
        Id::from_gc_ptr(self)
    }
}

/// Double-indirection is completely safe
unsafe impl<'gc, T: ?Sized + GcSafe + 'gc, Id: CollectorId> GcSafe for Gc<'gc, T, Id> {}
/// Rebrand
unsafe impl<'gc, 'new_gc, T, Id> GcRebrand<'new_gc, Id> for Gc<'gc, T, Id>
    where T: GcSafe + ?Sized + GcRebrand<'new_gc, Id>,
          <T as GcRebrand<'new_gc, Id>>::Branded: GcSafe,
          Id: CollectorId, Self: Trace {
    type Branded = Gc<'new_gc, <T as GcRebrand<'new_gc, Id>>::Branded, Id>;
}
unsafe impl<'gc, 'a, T, Id> GcErase<'a, Id> for Gc<'gc, T, Id>
    where T: GcSafe + ?Sized + GcErase<'a, Id>,
          <T as GcErase<'a, Id>>::Erased: GcSafe,
          Id: CollectorId, Self: Trace {
    type Erased = Gc<'a, <T as GcErase<'a, Id>>::Erased, Id>;
}
unsafe impl<'gc, T: ?Sized + GcSafe + 'gc, Id: CollectorId> Trace for Gc<'gc, T, Id> {
    // We always need tracing....
    const NEEDS_TRACE: bool = true;
    // we never need to be dropped because we are `Copy`
    const NEEDS_DROP: bool = false;

    #[inline]
    fn visit<V: GcVisitor>(&mut self, visitor: &mut V) -> Result<(), V::Err> {
        unsafe {
            // We're delegating with a valid pointer.
            <T as Trace>::visit_inside_gc(self, visitor)
        }
    }

    #[inline]
    unsafe fn visit_inside_gc<'actual_gc, V, ActualId>(gc: &mut Gc<'actual_gc, Self, ActualId>, visitor: &mut V) -> Result<(), V::Err>
        where V: GcVisitor, ActualId: CollectorId, Self: GcSafe + 'actual_gc {
        // Double indirection is fine. It's just a `Sized` type
        visitor.visit_gc(gc)
    }
}
impl<'gc, T: GcSafe + ?Sized + 'gc, Id: CollectorId> Deref for Gc<'gc, T, Id> {
    type Target = &'gc T;

    #[inline(always)]
    fn deref(&self) -> &Self::Target {
        unsafe { &*(&self.value as *const NonNull<T> as *const &'gc T) }
    }
}
unsafe impl<'gc, O, V, Id> GcDirectBarrier<'gc, Gc<'gc, O, Id>> for Gc<'gc, V,Id>
    where O: GcSafe + 'gc, V: GcSafe + 'gc, Id: CollectorId {
    #[inline(always)]
    unsafe fn write_barrier(&self, owner: &Gc<'gc, O, Id>, field_offset: usize) {
        Id::gc_write_barrier(owner, self, field_offset)
    }
}
// We can be copied freely :)
impl<'gc, T: GcSafe + ?Sized + 'gc, Id: CollectorId> Copy for Gc<'gc, T, Id> {}
impl<'gc, T: GcSafe + ?Sized + 'gc, Id: CollectorId> Clone for Gc<'gc, T, Id> {
    #[inline(always)]
    fn clone(&self) -> Self {
        *self
    }
}
// Delegating impls
impl<'gc, T: GcSafe + Hash + 'gc, Id: CollectorId> Hash for Gc<'gc, T, Id> {
    #[inline]
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.value().hash(state)
    }
}
impl<'gc, T: GcSafe + PartialEq + 'gc, Id: CollectorId> PartialEq for Gc<'gc, T, Id> {
    #[inline]
    fn eq(&self, other: &Self) -> bool {
        // NOTE: We compare by value, not identity
        self.value() == other.value()
    }
}
impl<'gc, T: GcSafe + Eq + 'gc, Id: CollectorId> Eq for Gc<'gc, T, Id> {}
impl<'gc, T: GcSafe + PartialEq + 'gc, Id: CollectorId> PartialEq<T> for Gc<'gc, T, Id> {
    #[inline]
    fn eq(&self, other: &T) -> bool {
        self.value() == other
    }
}
impl<'gc, T: GcSafe + Debug + 'gc, Id: CollectorId> Debug for Gc<'gc, T, Id> {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        if !f.alternate() {
            // Pretend we're a newtype by default
            f.debug_tuple("Gc").field(self.value()).finish()
        } else {
            // Alternate spec reveals `collector_id`
            f.debug_struct("Gc")
                .field("collector_id", &self.collector_id)
                .field("value", self.value())
                .finish()
        }
    }
}

/// In order to send *references* between threads,
/// the underlying type must be sync.
///
/// This is the same reason that `Arc<T>: Send` requires `T: Sync`
unsafe impl<'gc, T, Id> Send for Gc<'gc, T, Id>
    where T: GcSafe + ?Sized + Sync, Id: CollectorId + Sync {}

/// If the underlying type is `Sync`, it's safe
/// to share garbage collected references between threads.
///
/// The safety of the collector itself depends on whether [CollectorId] is Sync.
/// If it is, the whole garbage collection implenentation should be as well.
unsafe impl<'gc, T, Id> Sync for Gc<'gc, T, Id>
    where T: GcSafe + ?Sized + Sync, Id: CollectorId + Sync {}

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
pub unsafe trait GcHandle<T: GcSafe + ?Sized>: Clone + NullTrace {
    /// The type of the system used with this handle
    type System: GcSystem<Id=Self::Id>;
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
/// Trait for binding [GcHandle]s to contexts
/// using [GcBindHandle::bind_to]
///
/// This is separate from the [GcHandle] trait
/// because Rust doesn't have Generic Associated Types
///
/// TODO: Remove when we get more powerful types
pub unsafe trait GcBindHandle<'new_gc, T: GcSafe + ?Sized>: GcHandle<T>
    where T: GcRebrand<'new_gc, Self::Id>,
          <T as GcRebrand<'new_gc, Self::Id>>::Branded: GcSafe {
    /// Associate this handle with the specified context,
    /// allowing its underlying object to be accessed
    /// as long as the context is valid.
    ///
    /// The underlying object can be accessed just like any
    /// other object that would be allocated from the context.
    /// It'll be properly collected and can even be used as a root
    /// at the next safepoint.
    fn bind_to(&self, context: &'new_gc <Self::System as GcSystem>::Context) -> Gc<
        'new_gc,
        <T as GcRebrand<'new_gc, Self::Id>>::Branded,
        Self::Id
    >;
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

/// A marker type, indicating that a type can be safely allocated by a garbage collector.
///
/// ## Safety
/// Custom destructors must never reference garbage collected pointers.
/// The garbage collector may have already freed the other objects
/// before calling this type's drop function.
///
/// Unlike java finalizers, this allows us to deallocate objects normally
/// and avoids a second pass over the objects
/// to check for resurrected objects.
pub unsafe trait GcSafe: Trace {
    /// Assert this type is GC safe
    ///
    /// Only used by procedural derive
    #[doc(hidden)]
    fn assert_gc_safe() where Self: Sized {}
}
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
        GcSafe => always,
        GcRebrand => { where T: 'new_gc },
        GcErase => { where T: 'min }
    },
    null_trace => always,
    branded_type => AssumeNotTraced<T>,
    erased_type => AssumeNotTraced<T>,
    NEEDS_TRACE => false,
    NEEDS_DROP => core::mem::needs_drop::<T>(),
    visit => |self, visitor| { /* nop */ Ok(()) }
}

/// Changes all references to garbage collected
/// objects to match a specific lifetime.
///
/// This indicates that its safe to transmute to the new `Branded` type
/// and all that will change is the lifetimes.
// TODO: Can we support lifetimes that are smaller than 'new_gc
pub unsafe trait GcRebrand<'new_gc, Id: CollectorId>: Trace {
    /// This type with all garbage collected lifetimes
    /// changed to `'new_gc`
    ///
    /// This must have the same in-memory repr as `Self`,
    /// so that it's safe to transmute.
    type Branded: ?Sized + 'new_gc;

    /// Assert this type can be rebranded
    ///
    /// Only used by procedural derive
    #[doc(hidden)]
    fn assert_rebrand() {}
}
/// Indicates that it's safe to erase all GC lifetimes
/// and change them to 'static (logically an 'unsafe)
///
/// This trait is the opposite of [GcRebrand]
///
/// The lifetime '`a` is the minimum lifetime of all other non-gc references.
pub unsafe trait GcErase<'a, Id: CollectorId>: Trace {
    /// This type with all garbage collected lifetimes
    /// changed to `'static` (or erased)
    ///
    /// This must have the same in-memory repr as `Self`,
    /// so that it's safe to transmute.
    type Erased: ?Sized + 'a;

    /// Assert this type can be erased
    ///
    /// Only used by procedural derive
    #[doc(hidden)]
    fn assert_erase() {}
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
    const NEEDS_TRACE: bool;
    /// If this type needs a destructor run.
    ///
    /// This is usually equivalent to [core::mem::needs_drop].
    /// However, procedurally derived code can sometimes provide
    /// a no-op drop implementation (for safety)
    /// which would lead to a false positive with `core::mem::needs_drop()`
    const NEEDS_DROP: bool;
    /// Visit each field in this type
    ///
    /// Users should never invoke this method, and always call the `V::visit` instead.
    /// Only the collector itself is premitted to call this method,
    /// and **it is undefined behavior for the user to invoke this**.
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
    /// - Calling `GcVisitor::visit` with the specified collector
    ///   - `GarbageCollector::trace` already verifies that it owns the data, so you don't need to do that
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
    fn visit<V: GcVisitor>(&mut self, visitor: &mut V) -> Result<(), V::Err>;
    /// Visit this object, assuming its already inside a GC pointer.
    ///
    /// This is **required** to delegate to one of the following methods on [GcVisitor]:
    /// 1. [GcVisitor::visit_gc] - For regular, `Sized` types
    /// 2. [GcVisitor::visit_array] - For slices and arrays
    /// 3. [GcVisitor::visit_trait_object] - For trait objects
    ///
    /// ## Safety
    /// This must delegate to the appropriate method on [GcVisitor],
    /// or undefined behavior will result.
    ///
    /// The user is required to supply an appropriate [Gc] pointer.
    unsafe fn visit_inside_gc<'gc, V, Id>(gc: &mut Gc<'gc, Self, Id>, visitor: &mut V) -> Result<(), V::Err>
        where V: GcVisitor, Id: CollectorId, Self: GcSafe + 'gc;
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
    /// Visit an immutable reference to this type
    ///
    /// The visitor may want to relocate garbage collected pointers,
    /// which this type must support.
    fn visit_immutable<V: GcVisitor>(&self, visitor: &mut V) -> Result<(), V::Err>;
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
pub unsafe trait DynTrace {}
unsafe impl<T: ?Sized + Trace + GcSafe> DynTrace for T {}

impl<'gc, T, U, Id> CoerceUnsized<Gc<'gc, U, Id>> for Gc<'gc, T, Id>
    where T: ?Sized + GcSafe + Unsize<U>, U: ?Sized + GcSafe, Id: CollectorId {

}

/// Marker types for types that don't need to be traced
///
/// If this trait is implemented `Trace::NEEDS_TRACE` must be false
pub unsafe trait NullTrace: Trace + TraceImmutable {}

/// Visits garbage collected objects
///
/// This should only be used by a [GcSystem]
pub unsafe trait GcVisitor: Sized {
    /// The type of errors returned by this visitor
    type Err: Debug;

    /// Visit a reference to the specified value
    #[inline(always)]
    fn visit<T: Trace + ?Sized>(&mut self, value: &mut T) -> Result<(), Self::Err> {
        value.visit(self)
    }
    /// Visit a reference to the specified value
    #[inline(always)]
    fn visit_immutable<T: TraceImmutable + ?Sized>(&mut self, value: &T) -> Result<(), Self::Err> {
        value.visit_immutable(self)
    }

    /// Visit a garbage collected pointer
    ///
    /// ## Safety
    /// Undefined behavior if the GC pointer isn't properly visited.
    unsafe fn visit_gc<'gc, T, Id>(
        &mut self, gc: &mut Gc<'gc, T, Id>
    ) -> Result<(), Self::Err>
        where T: GcSafe + 'gc, Id: CollectorId;

    /// Visit a garbage collected trait object.
    ///
    /// ## Safety
    /// The trait object must point to a garbage collected object.
    unsafe fn visit_trait_object<'gc, T, Id>(
        &mut self, gc: &mut Gc<'gc, T, Id>
    ) -> Result<(), Self::Err>
        where T: ?Sized + GcSafe + 'gc + Pointee<Metadata=DynMetadata<T>> + DynTrace,
              Id: CollectorId;

    /// Visit a garbage collected vector.
    ///
    /// ## Safety
    /// Undefined behavior if the vector is invalid.
    unsafe fn visit_vec<'gc, T, Id>(
        &mut self, raw: &mut Gc<'gc, Id::RawVecRepr, Id>
    ) -> Result<(), Self::Err>
        where T: GcSafe + 'gc, Id: CollectorId;

    /// Visit a garbage collected array.
    ///
    /// ## Safety
    /// Undefined behavior if the array is invalid.
    unsafe fn visit_array<'gc, T, Id>(
        &mut self, array: &mut GcArray<'gc, T, Id>
    ) -> Result<(), Self::Err>
        where T: GcSafe + 'gc, Id: CollectorId;
}
