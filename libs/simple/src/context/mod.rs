//! The implementation of [::zerogc::CollectorContext] that is
//! shared among both thread-safe and thread-unsafe code.

#[cfg(feature = "sync")]
mod sync;
#[cfg(not(feature = "sync"))]
mod simple;
#[cfg(feature = "sync")]
pub use self::sync::*;
#[cfg(not(feature = "sync"))]
pub use self::simple::*;

use zerogc::prelude::*;
use super::{SimpleCollector, RawSimpleCollector, DynTrace};
use std::mem::ManuallyDrop;
use std::ptr::NonNull;


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
pub struct SimpleCollectorContext {
    raw: *mut RawContext,
    /// Whether we are the root context
    ///
    /// Only the root actually owns the `Arc`
    /// and is responsible for dropping it
    root: bool
}
impl SimpleCollectorContext {
    #[cfg(not(feature = "sync"))]
    pub(crate) unsafe fn from_collector(collector: &SimpleCollector) -> Self {
        SimpleCollectorContext {
            raw: Box::into_raw(ManuallyDrop::into_inner(
                RawContext::from_collector(collector.internal_clone())
            )),
            root: true // We are the exclusive owner
        }
    }
    #[cfg(feature = "sync")]
    pub(crate) unsafe fn register_root(collector: &SimpleCollector) -> Self {
        SimpleCollectorContext {
            raw: Box::into_raw(ManuallyDrop::into_inner(
                RawContext::register_new(&collector)
            )),
            root: true, // We are responsible for unregistering
        }
    }
    #[inline]
    pub(crate) fn collector(&self) -> &RawSimpleCollector {
        unsafe { &(*self.raw).collector.as_raw() }
    }
    #[inline(always)]
    unsafe fn with_shadow_stack<R, T: Trace>(
        &self, value: *mut &mut T, func: impl FnOnce() -> R
    ) -> R {
        let old_link = (*(*self.raw).shadow_stack.get()).last;
        let new_link = ShadowStackLink {
            element: NonNull::new_unchecked(
                std::mem::transmute::<
                    *mut dyn DynTrace,
                    *mut (dyn DynTrace + 'static)
                >(value as *mut dyn DynTrace)
            ),
            prev: old_link
        };
        (*(*self.raw).shadow_stack.get()).last = &new_link;
        let result = func();
        debug_assert_eq!(
            (*(*self.raw).shadow_stack.get()).last,
            &new_link
        );
        (*(*self.raw).shadow_stack.get()).last = new_link.prev;
        result
    }
    #[cold]
    unsafe fn trigger_basic_safepoint<T: Trace>(&self, element: &mut &mut T) {
        self.with_shadow_stack(element, || {
            (*self.raw).trigger_safepoint();
        })
    }
}
impl Drop for SimpleCollectorContext {
    #[inline]
    fn drop(&mut self) {
        if self.root {
            unsafe {
                self.collector().free_context(self.raw);
            }
        }
    }
}
unsafe impl GcContext for SimpleCollectorContext {
    type System = SimpleCollector;

    #[inline]
    unsafe fn basic_safepoint<T: Trace>(&mut self, value: &mut &mut T) {
        debug_assert_eq!((*self.raw).state.get(), ContextState::Active);
        if (*self.raw).collector.as_raw().should_collect() {
            self.trigger_basic_safepoint(value);
        }
        debug_assert_eq!((*self.raw).state.get(), ContextState::Active);
    }

    unsafe fn freeze(&mut self) {
        (*self.raw).collector.as_raw().manager.freeze_context(&*self.raw);
    }

    unsafe fn unfreeze(&mut self) {
        (*self.raw).collector.as_raw().manager.unfreeze_context(&*self.raw);
    }

    #[inline]
    unsafe fn recurse_context<T, F, R>(&self, value: &mut &mut T, func: F) -> R
        where T: Trace, F: for<'gc> FnOnce(&'gc mut Self, &'gc mut T) -> R {
        debug_assert_eq!((*self.raw).state.get(), ContextState::Active);
        self.with_shadow_stack(value, || {
            let mut sub_context = ManuallyDrop::new(SimpleCollectorContext {
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
            std::mem::forget(sub_context);
            debug_assert_eq!((*self.raw).state.get(), ContextState::Active);
            result
        })
    }
}

/// It's not safe for a context to be sent across threads.
///
/// We use (thread-unsafe) interior mutability to maintain the
/// shadow stack. Since we could potentially be cloned via `safepoint_recurse!`,
/// implementing `Send` would allow another thread to obtain a
/// reference to our internal `&RefCell`. Further mutation/access
/// would be undefined.....
impl !Send for SimpleCollectorContext {}

//
// Root tracking
//

#[repr(C)]
#[derive(Debug)]
pub(crate) struct ShadowStackLink {
    pub element: NonNull<dyn DynTrace>,
    /// The previous link in the chain,
    /// or NULL if there isn't any
    pub prev: *const ShadowStackLink
}

#[derive(Clone, Debug)]
pub struct ShadowStack {
    /// The last element in the shadow stack,
    /// or NULL if it's empty
    pub(crate) last: *const ShadowStackLink
}
impl ShadowStack {
    unsafe fn as_vec(&self) -> Vec<*mut dyn DynTrace> {
        let mut result: Vec<_> = self.reverse_iter().collect();
        result.reverse();
        result
    }
    #[inline]
    pub(crate) unsafe fn reverse_iter(&self) -> impl Iterator<Item=*mut dyn DynTrace> + '_ {
        std::iter::successors(
            self.last.as_ref(),
            |link| link.prev.as_ref()
        ).map(|link| link.element.as_ptr())
    }
}