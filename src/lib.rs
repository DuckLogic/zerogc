#![feature(
    const_panic, // RFC 2345 - Const asserts
    ptr_metadata, // RFC 2580 - Pointer meta
    coerce_unsized, // RFC 0982 - DST coercion
    unsize,
    trait_alias, // RFC 1733 - Trait aliases
    generic_associated_types, // RFC 1598 - Generic associated types
    const_raw_ptr_deref, // Needed for const Gc::value
    option_result_unwrap_unchecked, // Only used by the 'serde' implementation...
    // Needed for epsilon collector:
    once_cell, // RFC 2788 (Probably will be accepted)
    negative_impls, // More elegant than marker types
    alloc_layout_extra,
    const_fn_fn_ptr_basics,
    const_option,
    const_fn_trait_bound, // NOTE: Needed for the `epsilon_static_array` macro
    const_trait_impl, // EXPERIMENTAL: const Deref
    const_slice_from_raw_parts,
    const_transmute_copy,
    slice_range, // Convenient for bounds checking :)
)]
#![cfg_attr(feature="error", backtrace)]
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
use core::ops::{Deref, DerefMut, CoerceUnsized};
use core::ptr::{NonNull, Pointee, DynMetadata};
use core::marker::{PhantomData, Unsize};
use core::hash::{Hash, Hasher};
use core::fmt::{self, Debug, Formatter, Display};
use core::cmp::Ordering;

use vec::raw::{GcRawVec};
use zerogc_derive::unsafe_gc_impl;
pub use zerogc_derive::{Trace, NullTrace};

use crate::vec::{GcVec};
pub use crate::array::GcArray;


#[macro_use] // We have macros in here!
#[cfg(feature = "serde1")]
pub mod serde;
#[macro_use]
mod manually_traced;
#[macro_use]
mod macros;
pub mod cell;
pub mod prelude;
pub mod epsilon;
pub mod array;
pub mod vec;
pub mod internals;
#[cfg(feature = "errors")]
pub mod errors;

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
/// # let mut context = zerogc::epsilon::EpsilonSystem::leak().new_context();
/// # // TODO: Can we please get support for non-Sized types like `String`?!?!?!
/// let root = zerogc::epsilon::leaked(String::from("potato"));
/// let root = safepoint!(context, root);
/// assert_eq!(*root, "potato");
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
    /// # use zerogc::epsilon::{Gc, EpsilonCollectorId, EpsilonContext as ExampleContext};
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
    ///
    /// ## Safety
    /// Any garbage collected objects not reachable from the roots
    /// are invalidated after this method.
    ///
    /// The user must ensure that invalidated objects are not used
    /// after the safepoint,
    /// although the (logical) mutation of the context
    /// should significantly assist with that.
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
    unsafe fn rebrand_static<T>(&self, value: T) -> T::Branded
        where T: GcRebrand<'static, Self::Id>, T::Branded: Sized {
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

    /// Get the [GcSystem] associated with this context
    fn system(&self) -> &'_ Self::System;

    /// Get the id of this context
    fn id(&self) -> Self::Id;
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
    unsafe fn alloc_uninit<'gc, T>(&'gc self) -> *mut T
        where T: GcSafe<'gc, Self::Id>;
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
        where T: GcSafe<'gc, Self::Id> {
        unsafe {
            let ptr = self.alloc_uninit::<T>();
            ptr.write(value);
            Gc::from_raw(NonNull::new_unchecked(ptr))
        }
    }
    /// Allocate a [GcString](`crate::array::GcString`), copied from the specified source
    #[inline]
    fn alloc_str<'gc>(&'gc self, src: &str) -> array::GcString<'gc, Self::Id> {
        let bytes = self.alloc_slice_copy(src.as_bytes());
        // SAFETY: Guaranteed by the original `src`
        unsafe { array::GcString::from_utf8_unchecked(bytes) }
    }

    /// Wrap the specified error in a dynamically dispatched [GcError](`self::errors::GcError`)
    ///
    /// Uses a [GcHandle] to ensure the result lives for `'static`,
    /// and thus can implement `std::error::Error` and conversion into `anyhow::Error`.
    #[cfg(feature = "errors")]
    #[cold]
    fn alloc_error<'gc, T>(&'gc self, src: T) -> crate::errors::GcError<Self::Id>
        where T: crate::errors::GcErrorType<'gc, Self::Id>,
            <T as GcRebrand<'static, Self::Id>>::Branded: 'static,
            Self::Id: HandleCollectorId {
        crate::errors::GcError::from_gc_allocated(self.alloc(src))

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
    unsafe fn alloc_uninit_slice<'gc, T>(&'gc self, len: usize) -> *mut T
        where T: GcSafe<'gc, Self::Id>;
    /// Allocate a slice, copied from the specified input
    fn alloc_slice_copy<'gc, T>(&'gc self, src: &[T]) -> GcArray<'gc, T, Self::Id>
        where T: GcSafe<'gc, Self::Id> + Copy {
        unsafe {
            let res_ptr = self.alloc_uninit_slice::<T>(src.len());
            res_ptr.copy_from_nonoverlapping(src.as_ptr(), src.len());
            GcArray::from_raw_ptr(
                NonNull::new_unchecked(res_ptr),
                src.len()
            )
        }
    }
    /// Allocate an array, taking ownership of the values in
    /// the specified vec.
    #[inline]
    #[cfg(feature = "alloc")]
    fn alloc_array_from_vec<'gc, T>(&'gc self, mut src: Vec<T>) -> GcArray<'gc, T, Self::Id> 
        where T: GcSafe<'gc, Self::Id> {
        unsafe {
            let ptr = src.as_ptr();
            let len = src.len();
            /*
             * NOTE: Don't steal ownership of the
             * source Vec until *after* we allocate.
             *
             * It is possible allocation panics in
             * which case we want to free the source elements.
             */
            let dest = self.alloc_uninit_slice::<T>(len);
            /*
             * NOTE: From here to the end,
             * we should be infallible.
             */
            src.set_len(0);
            dest.copy_from_nonoverlapping(ptr, len);
            let res = GcArray::from_raw_ptr(
                NonNull::new_unchecked(ptr as *mut _),
                len
            );
            drop(src);
            res
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
        where T: GcSafe<'gc, Self::Id>, F: FnMut(usize) -> T {
        let res_ptr = self.alloc_uninit_slice::<T>(len);
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
        where T: GcSafe<'gc, Self::Id> {
        unsafe {
            self.alloc_slice_fill_with(len, |_idx| None)
        }
    }
    /// Allocate a slice by repeatedly copying a single value.
    #[inline]
    fn alloc_slice_fill_copy<'gc, T>(&'gc self, len: usize, val: T) -> GcArray<'gc, T, Self::Id>
        where T: GcSafe<'gc, Self::Id> + Copy {
        unsafe {
            self.alloc_slice_fill_with(len, |_idx| val)
        }
    }
    /// Create a new [GcRawVec] with the specified capacity
    /// and an implicit reference to this [GcContext].
    fn alloc_raw_vec_with_capacity<'gc, T>(&'gc self, capacity: usize) -> <Self::Id as CollectorId>::RawVec<'gc, T>
        where T: GcSafe<'gc, Self::Id>;
    /// Create a new [GcVec] with zero initial length,
    /// with an implicit reference to this [GcContext].
    #[inline]
    fn alloc_vec<'gc, T>(&'gc self) -> GcVec<'gc, T, Self::Id>
        where T: GcSafe<'gc, Self::Id> {
        unsafe {
            crate::vec::GcVec::from_raw(self.alloc_raw_vec_with_capacity(0))
        }
    }
    /// Allocate a new [GcVec] with the specified capacity
    /// and an implicit reference to this [GcContext]
    #[inline]
    fn alloc_vec_with_capacity<'gc, T>(&'gc self, capacity: usize) -> GcVec<'gc, T, Self::Id>
        where T: GcSafe<'gc, Self::Id> {
        unsafe {
            crate::vec::GcVec::from_raw(self.alloc_raw_vec_with_capacity(capacity))
        }
    }
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

/// A trait alias for [CollectorId]s that support [GcSimpleAlloc]
pub trait SimpleAllocCollectorId = CollectorId where <<Self as CollectorId>::System as GcSystem>::Context: GcSimpleAlloc;

/// A [CollectorId] that supports allocating [GcHandle]s
///
/// Not all collectors necessarily support handles.
pub unsafe trait HandleCollectorId: CollectorId {
    /// The type of [GcHandle] for this collector.
    ///
    /// This is parameterized by the *erased* type,
    /// not by the original type.
    type Handle<T>: GcHandle<T, System=Self::System, Id=Self>
        where T: GcSafe<'static, Self> + ?Sized;

    /// Create a handle to the specified GC pointer,
    /// which can be used without a context
    ///
    /// NOTE: Users should only use from [Gc::create_handle].
    ///
    /// The system is implicit in the [Gc]
    #[doc(hidden)]
    fn create_handle<'gc, T>(gc: Gc<'gc, T, Self>) -> Self::Handle<T::Branded>
        where T: GcSafe<'gc, Self> + GcRebrand<'static, Self> + ?Sized;
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
pub unsafe trait CollectorId: Copy + Eq + Hash + Debug + NullTrace + TrustedDrop + 'static + for<'gc> GcSafe<'gc, Self> {
    /// The type of the garbage collector system
    type System: GcSystem<Id=Self>;
    /// The implementation of [GcRawVec] for this type.
    ///
    /// May be [crate::vec::raw::Unsupported] if vectors are unsupported.
    type RawVec<'gc, T: GcSafe<'gc, Self>>: crate::vec::raw::GcRawVec<'gc, T, Id=Self>;
    /// The raw representation of `GcArray` pointers
    /// in this collector.
    type ArrayRepr<'gc, T>: ~const crate::array::repr::GcArrayRepr<'gc, T, Id=Self>;

    /// Get the runtime id of the collector that allocated the [Gc]
    ///
    /// Assumes that `T: GcSafe<'gc, Self>`, although that can't be
    /// proven at compile time.
    fn from_gc_ptr<'a, 'gc, T>(gc: &'a Gc<'gc, T, Self>) -> &'a Self
        where T: ?Sized, 'gc: 'a;

    /// Resolve the CollectorId for the specified [GcArray]'s representation.
    ///
    /// This is the [GcArray] counterpart of `from_gc_ptr`
    fn resolve_array_id<'a, 'gc, T>(repr: &'a Self::ArrayRepr<'gc, T>) -> &'a Self
        where 'gc: 'a;

    /// Resolve the length of the specified array
    fn resolve_array_len<T>(repr: &Self::ArrayRepr<'_, T>) -> usize;

    /// Perform a write barrier before writing to a garbage collected field
    ///
    /// ## Safety
    /// Similar to the [GcDirectBarrier] trait, it can be assumed that
    /// the field offset is correct and the types match.
    unsafe fn gc_write_barrier<'gc, O: GcSafe<'gc, Self> + ?Sized, V: GcSafe<'gc, Self> + ?Sized>(
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
///
/// ## Lifetime
/// The borrow does *not* refer to the value `&'gc T`.
/// Instead, it refers to the *system* `&'gc Id::System`
///
/// This is necessary because `T` may have borrowed interior data
/// with a shorter lifetime `'a < 'gc`, making `&'gc T` invalid
/// (because that would imply 'gc: 'a, which is false).
///
/// This ownership can be thought of in terms of the following (simpler) system.
/// ```no_run
/// # trait GcSafe{}
/// # use core::marker::PhantomData;
/// struct GcSystem {
///     values: Vec<Box<dyn GcSafe>>
/// }
/// struct Gc<'gc, T: GcSafe> {
///     index: usize,
///     marker: PhantomData<T>,
///     system: &'gc GcSystem
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
        Gc { collector_id: PhantomData, value }
    }
    /// Create a [GcHandle] referencing this object,
    /// allowing it to be used without a context
    /// and referenced across safepoints.
    ///
    /// Requires that the collector [supports handles](`HandleCollectorId`)
    #[inline]
    pub fn create_handle(&self) -> Id::Handle<T::Branded>
        where Id: HandleCollectorId, T: GcRebrand<'static, Id>  {
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
        where V: GcVisitor {
        // Double indirection is fine. It's just a `Sized` type
        visitor.trace_gc(gc)
    }
}
/// Rebrand
unsafe impl<'gc, 'new_gc, T, Id> GcRebrand<'new_gc, Id> for Gc<'gc, T, Id>
    where T: GcSafe<'gc, Id> + ?Sized + GcRebrand<'new_gc, Id>,
          Id: CollectorId, Self: Trace {
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
impl<'gc, T: GcSafe<'gc, Id> + ?Sized, Id: CollectorId> const Deref for Gc<'gc, T, Id> {
    type Target = T;

    #[inline(always)]
    fn deref(&self) -> &Self::Target {
        self.value()
    }
}
unsafe impl<'gc, O, V, Id> GcDirectBarrier<'gc, Gc<'gc, O, Id>> for Gc<'gc, V,Id>
    where O: GcSafe<'gc, Id> + 'gc, V: GcSafe<'gc, Id> + 'gc, Id: CollectorId {
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
    where T: GcSafe<'gc, Id> + ?Sized + Sync, Id: CollectorId + Sync {}

/// If the underlying type is `Sync`, it's safe
/// to share garbage collected references between threads.
///
/// The safety of the collector itself depends on whether [CollectorId] is Sync.
/// If it is, the whole garbage collection implementation should be as well.
unsafe impl<'gc, T, Id> Sync for Gc<'gc, T, Id>
    where T: GcSafe<'gc, Id> + ?Sized + Sync, Id: CollectorId + Sync {}

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
pub unsafe trait GcHandle<T: GcSafe<'static, Self::Id> + ?Sized>: Sized + Clone + NullTrace
     + for<'gc> GcSafe<'gc, Self::Id> {
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

    /// Associate this handle with the specified context,
    /// allowing its underlying object to be accessed
    /// as long as the context is valid.
    ///
    /// The underlying object can be accessed just like any
    /// other object that would be allocated from the context.
    /// It'll be properly collected and can even be used as a root
    /// at the next safepoint.
    fn bind_to<'new_gc>(
        &self,
        context: &'new_gc <Self::System as GcSystem>::Context
    ) -> Gc<'new_gc, <T as GcRebrand<'new_gc, Self::Id>>::Branded, Self::Id>
        where T: GcRebrand<'new_gc, Self::Id>;
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
    fn assert_gc_safe() -> bool where Self: Sized { true }
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
        where V: GcVisitor;
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
    where T: ?Sized + GcSafe<'gc, Id> + Unsize<U>, U: ?Sized + GcSafe<'gc, Id>, Id: CollectorId {

}

/// Marker types for types that don't need to be traced
///
/// If this trait is implemented `Trace::NEEDS_TRACE` must be false
pub unsafe trait NullTrace: Trace + TraceImmutable {
    /// Dummy method for macros to verify that a type actually implements `NullTrace`
    #[doc(hidden)]
    #[inline]
    fn verify_null_trace() where Self: Sized {
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
    unsafe fn trace_gc<'gc, T, Id>(
        &mut self, gc: &mut Gc<'gc, T, Id>
    ) -> Result<(), Self::Err>
        where T: GcSafe<'gc, Id>, Id: CollectorId;

    /// Visit a garbage collected trait object.
    ///
    /// ## Safety
    /// The trait object must point to a garbage collected object.
    unsafe fn trace_trait_object<'gc, T, Id>(
        &mut self, gc: &mut Gc<'gc, T, Id>
    ) -> Result<(), Self::Err>
        where T: ?Sized + GcSafe<'gc, Id> + Pointee<Metadata=DynMetadata<T>> + DynTrace<'gc, Id>,
              Id: CollectorId;

    /// Visit a garbage collected vector.
    ///
    /// ## Safety
    /// Undefined behavior if the vector is invalid.
    unsafe fn trace_vec<'gc, T, V>(
        &mut self, raw: &mut V
    ) -> Result<(), Self::Err>
        where T: GcSafe<'gc, V::Id>, V: GcRawVec<'gc, T>;

    /// Visit a garbage collected array.
    ///
    /// ## Safety
    /// Undefined behavior if the array is invalid.
    unsafe fn trace_array<'gc, T, Id>(
        &mut self, array: &mut GcArray<'gc, T, Id>
    ) -> Result<(), Self::Err>
        where T: GcSafe<'gc, Id>, Id: CollectorId;
}
