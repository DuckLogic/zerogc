//! Defines the [`GcContext`] API,
//!
//! This is the primary safe interface with the collector.

use crate::system::{GcContextState, TrustedAllocInit, UntrustedAllocInit};
use crate::{CollectorId, Gc, GcSafe};

pub mod handle;

/// A single context for interfacing with a garbage collector.
///
/// For any given collector, there should be only one of these per thread.
///
/// ## Safepoint
/// A collection can only occur at a [`safepoint`](GcContext::safepoint).
/// This semantically "mutates" the context,
/// invalidating all outstanding [`Gc`] references.
pub struct GcContext<Id: CollectorId> {
    state: Id::ContextState,
}
impl<Id: CollectorId> GcContext<Id> {
    /// Return the id of the collector.
    #[inline]
    pub fn id(&self) -> Id {
        self.state.id()
    }

    /// Indicate that the thread is ready for a potential collection,
    /// invaliding all outstanding [`Gc`] references.
    ///
    /// If other threads have active contexts,
    /// this will need to wait for them before a collection can occur.
    #[inline]
    pub fn safepoint(&mut self) {
        unsafe {
            self.state.safepoint();
        }
    }

    /// Test if a safepoint needs to be invoked.
    ///
    /// This should return true if [`GcContext::is_gc_needed`] does,
    /// but also should return true if other threads are
    /// already blocked at a safepoint.
    ///
    /// In a single-threaded scenario, the two tests should be equivalent.
    /// See docs for [`GcContext::is_gc_needed`] for why the two tests are seperate.
    ///
    /// This can be used to avoid the overhead of mutation.
    /// For example, consider the following code:
    /// ```
    /// # use zerogc::prelude::*;
    ///
    /// fn run<Id: CollectorId>(ctx: &GcContext<Id>) {
    ///     let mut index = 0;
    ///     const LIMIT: usize = 100_000;
    ///     let total =
    ///     while index < LIMIT {
    ///         {
    ///             let cached = expensive_pure_computation(ctx);
    ///             while index < LIMIT && !ctx.is_safepoint_needed() {
    ///                 index % cached.len()
    ///             }
    ///         }
    ///         // a safepoint was deemed necessary, which would invalidate the `cached` object.
    ///     }
    /// }
    ///
    /// fn expensive_pure_computation<Id: CollectorId>(ctx: &GcContext<Id>) -> Gc<'_, String, Id>
    /// # { unreachable!() }
    /// ```
    #[inline]
    pub fn is_safepoint_needed(&self) -> bool {
        self.state.is_safepoint_needed()
    }

    /// Test if a garbage collection is needed.
    ///
    /// This is a weaker test than [`GcContext::is_safepoint_needed`],
    /// which this does not check if other threads are blocked.
    ///
    /// This function is distinct from [`GcContext::is_safepoint_needed`] because user code
    /// may already have ways to atomically signal other executing threads.
    /// In that case, there is no need to check if other threads are blocked.
    ///
    /// In a single-threaded context, the two tests are equivalent.
    #[inline]
    pub fn is_gc_needed(&self) -> bool {
        self.state.is_gc_needed()
    }

    /// Trigger a [`safepoint`](Self::safepoint) and force a garbage collection.
    ///
    /// This will block until all threads reach a safepoint.
    pub fn force_collect(&mut self) {
        unsafe {
            self.state.force_collect();
        }
    }

    /// Allocate garbage collected memory with the specified value.
    ///
    /// In some cases [`Self::alloc_with`] could be more efficient,
    /// since it can avoid moving a value.
    #[inline(always)]
    pub fn alloc<'gc, T>(&'gc self, value: T) -> Gc<'gc, T, Id>
    where
        T: GcSafe<'gc, Id>,
    {
        // SAFETY: Lifetime is correct
        // SAFETY: Initialization will never fail.
        unsafe {
            self.state.alloc(
                #[inline(always)]
                || value,
                TrustedAllocInit::new_unchecked(),
            )
        }
    }

    /// Allocate a garbage collected value,
    /// initializing it using the specified callback.
    ///
    /// The initialization function is permitted to panic.
    #[inline(always)]
    pub fn alloc_with<'gc, T>(&'gc self, func: impl FnOnce() -> T) -> Gc<'gc, T, Id>
    where
        T: GcSafe<'gc, Id>,
    {
        // SAFETY: Lifetime is correct
        unsafe { self.state.alloc(func, UntrustedAllocInit::new()) }
    }
}
