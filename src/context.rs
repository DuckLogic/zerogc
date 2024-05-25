//! Defines the [`GcContext`] API,
//!
//! This is the primary safe interface with the collector.

use crate::system::{GcContextState, TrustedAllocInit, UntrustedAllocInit};
use crate::{CollectorId, Gc, GcSafe};

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
