#![feature(
    negative_impls, // !Send is much cleaner than `PhantomData<Rc>`
    untagged_unions, // I want to avoid ManuallyDrop in unions
    const_fn_trait_bound, // So generics + const fn are unstable, huh?
    generic_associated_types, // Finally!
)]
#![allow(
    clippy::missing_safety_doc, // Entirely internal code
)]
#![cfg_attr(not(feature = "std"), no_std)]
//! The implementation of (GcContext)[`::zerogc::GcContext`] that is
//! shared among both thread-safe and thread-unsafe code.

/*
 * NOTE: Allocation is still needed for internals
 *
 * Uses:
 * 1. `Box` for each handle
 * 2. `Vec` for listing buckets of handles
 * 3. `Arc` and `Box` for boxing context state
 * 
 * TODO: Should we drop these uses entirely?
 */
extern crate alloc;

use core::mem::ManuallyDrop;
use core::fmt::{self, Debug, Formatter};

use alloc::boxed::Box;
use alloc::vec::Vec;

use zerogc::prelude::*;

pub mod state;

#[macro_use]
pub mod utils;
pub mod collector;
pub mod handle;

use crate::collector::{RawCollectorImpl};

pub use crate::collector::{WeakCollectorRef, CollectorRef, CollectorId};
pub use crate::state::{CollectionManager, RawContext};

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum ContextState {
    /// The context is active.
    ///
    /// Its contents are potentially being mutated,
    /// so the `shadow_stack` doesn't necessarily
    /// reflect the actual set of thread roots.
    ///
    /// New objects could be allocated that are not
    /// actually being tracked in the `shadow_stack`.
    Active,
    /// The context is waiting at a safepoint
    /// for a collection to complete.
    ///
    /// The mutating thread is blocked for the
    /// duration of the safepoint (until collection completes).
    ///
    /// Therefore, its `shadow_stack` is guarenteed to reflect
    /// the actual set of thread roots.
    SafePoint {
        /// The id of the collection we are waiting for
        collection_id: u64
    },
    /// The context is frozen.
    /// Allocation or mutation can't happen
    /// but the mutator thread isn't actually blocked.
    ///
    /// Unlike a safepoint, this is explicitly unfrozen at the
    /// user's discretion.
    ///
    /// Because no allocation or mutation can happen,
    /// its shadow_stack stack is guarenteed to
    /// accurately reflect the roots of the context.
    #[cfg_attr(not(feature = "sync"), allow(unused))] // TODO: Implement frozen for simple contexts?
    Frozen,
}
impl ContextState {
    #[cfg_attr(not(feature = "sync"), allow(unused))] // TODO: Implement frozen for simple contexts?
    fn is_frozen(&self) -> bool {
        matches!(*self, ContextState::Frozen)
    }
}

/*
 * These form a stack of contexts,
 * which all share owns a pointer to the RawContext,
 * The raw context is implicitly bound to a single thread
 * and manages the state of all the contexts.
 *
 * https://llvm.org/docs/GarbageCollection.html#the-shadow-stack-gc
 * Essentially these objects maintain a shadow stack
 *
 * The pointer to the RawContext must be Arc, since the
 * collector maintains a weak reference to it.
 * I use double indirection with a `Rc` because I want
 * `recurse_context` to avoid the cost of atomic operations.
 *
 * SimpleCollectorContexts mirror the application stack.
 * They can be stack allocated inside `recurse_context`.
 * All we would need to do is internally track ownership of the original
 * context. The sub-collector in `recurse_context` is very clearly
 * restricted to the lifetime of the closure
 * which is a subset of the parent's lifetime.
 *
 * We still couldn't be Send, since we use interior mutablity
 * inside of RawContext that is not thread-safe.
 */
// TODO: Rename to remove 'Simple' from name
pub struct CollectorContext<C: RawCollectorImpl> {
    raw: *mut C::RawContext,
    /// Whether we are the root context
    ///
    /// Only the root actually owns the `Arc`
    /// and is responsible for dropping it
    root: bool
}
impl<C: RawCollectorImpl> CollectorContext<C> {
    pub(crate) unsafe fn register_root(collector: &CollectorRef<C>) -> Self {
        CollectorContext {
            raw: Box::into_raw(ManuallyDrop::into_inner(
                C::RawContext::register_new(collector)
            )),
            root: true, // We are responsible for unregistering
        }
    }
    #[inline]
    pub fn collector(&self) -> &C {
        unsafe { (*self.raw).collector() }
    }
    #[inline(always)]
    unsafe fn with_shadow_stack<R, T: Trace>(
        &self, value: *mut &mut T, func: impl FnOnce() -> R
    ) -> R {
        let old_link = (*(*self.raw).shadow_stack_ptr()).last;
        let new_link = ShadowStackLink {
            element: C::as_dyn_trace_pointer(value),
            prev: old_link
        };
        (*(*self.raw).shadow_stack_ptr()).last = &new_link;
        let result = func();
        debug_assert_eq!(
            (*(*self.raw).shadow_stack_ptr()).last,
            &new_link
        );
        (*(*self.raw).shadow_stack_ptr()).last = new_link.prev;
        result
    }
    #[cold]
    unsafe fn trigger_basic_safepoint<T: Trace>(&self, element: &mut &mut T) {
        self.with_shadow_stack(element, || {
            (*self.raw).trigger_safepoint();
        })
    }
}
impl<C: RawCollectorImpl> Drop for CollectorContext<C> {
    #[inline]
    fn drop(&mut self) {
        if self.root {
            unsafe {
                C::Manager::free_context(self.collector(), self.raw);
            }
        }
    }
}
unsafe impl<C: RawCollectorImpl> GcContext for CollectorContext<C> {
    type System = CollectorRef<C>;
    type Id = CollectorId<C>;

    #[inline]
    unsafe fn unchecked_safepoint<T: Trace>(&self, value: &mut &mut T) {
        debug_assert_eq!((*self.raw).state(), ContextState::Active);
        if (*self.raw).collector().should_collect() {
            self.trigger_basic_safepoint(value);
        }
        debug_assert_eq!((*self.raw).state(), ContextState::Active);
    }

    unsafe fn freeze(&mut self) {
        (*self.raw).collector().manager().freeze_context(&*self.raw);
    }

    unsafe fn unfreeze(&mut self) {
        (*self.raw).collector().manager().unfreeze_context(&*self.raw);
    }

    #[inline]
    unsafe fn recurse_context<T, F, R>(&self, value: &mut &mut T, func: F) -> R
        where T: Trace, F: for<'gc> FnOnce(&'gc mut Self, &'gc mut T) -> R {
        debug_assert_eq!((*self.raw).state(), ContextState::Active);
        self.with_shadow_stack(value, || {
            let mut sub_context = ManuallyDrop::new(CollectorContext {
                /*
                 * safe to copy because we wont drop it
                 * Lifetime is guarenteed to be restricted to
                 * the closure.
                 */
                raw: self.raw,
                root: false /* don't drop our pointer!!! */
            });
            let result = func(&mut *sub_context, value);
            debug_assert!(!sub_context.root);
            // No need to run drop code on context.....
            core::mem::forget(sub_context);
            debug_assert_eq!((*self.raw).state(), ContextState::Active);
            result
        })
    }

    #[inline]
    fn system(&self) -> &'_ Self::System {
        unsafe { (&*self.raw).collector_ref() }
    }

    #[inline]
    fn id(&self) -> Self::Id {
        unsafe { (&*self.raw).collector() }.id()
    }
}

/// It's not safe for a context to be sent across threads.
///
/// We use (thread-unsafe) interior mutability to maintain the
/// shadow stack. Since we could potentially be cloned via `safepoint_recurse!`,
/// implementing `Send` would allow another thread to obtain a
/// reference to our internal `&RefCell`. Further mutation/access
/// would be undefined.....
impl<C: RawCollectorImpl> !Send for CollectorContext<C> {}

//
// Root tracking
//

#[repr(C)]
#[derive(Debug)]
pub(crate) struct ShadowStackLink<T> {
    pub element: T,
    /// The previous link in the chain,
    /// or NULL if there isn't any
    pub prev: *const ShadowStackLink<T>
}

impl<C: RawCollectorImpl> Debug for ShadowStack<C> {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.debug_struct("ShadowStack")
            .field("last", &format_args!("{:p}", self.last))
            .finish()
    }
}
#[derive(Clone)]
pub struct ShadowStack<C: RawCollectorImpl> {
    /// The last element in the shadow stack,
    /// or NULL if it's empty
    pub(crate) last: *const ShadowStackLink<C::DynTracePtr>
}
impl<C: RawCollectorImpl> ShadowStack<C> {
    unsafe fn as_vec(&self) -> Vec<C::DynTracePtr> {
        let mut result: Vec<_> = self.reverse_iter().collect();
        result.reverse();
        result
    }
    #[inline]
    pub unsafe fn reverse_iter(&self) -> impl Iterator<Item=C::DynTracePtr> + '_ {
        core::iter::successors(
            self.last.as_ref(),
            |link| link.prev.as_ref()
        ).map(|link| link.element)
    }
}
