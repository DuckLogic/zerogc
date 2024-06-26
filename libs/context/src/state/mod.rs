use crate::collector::RawCollectorImpl;
use crate::{CollectorRef, ContextState, ShadowStack};

use core::fmt::Debug;
use core::mem::ManuallyDrop;

use alloc::boxed::Box;

pub mod nosync;
/// The internal state of the collector
///
/// Has a thread-safe and thread-unsafe implementation.

#[cfg(feature = "sync")]
pub mod sync;

/// Manages coordination of garbage collections
pub unsafe trait CollectionManager<C>: self::sealed::Sealed
where
    C: RawCollectorImpl<Manager = Self, RawContext = Self::Context>,
{
    type Context: RawContext<C>;
    fn new() -> Self;
    fn is_collecting(&self) -> bool;
    fn should_trigger_collection(&self) -> bool;
    /// Freeze this context
    ///
    /// ## Safety
    /// See [GcContext::free_context]
    unsafe fn freeze_context(&self, context: &Self::Context);
    /// Unfreeze the context
    ///
    /// ## Safety
    /// See [GcContext::unfreeze_context]
    unsafe fn unfreeze_context(&self, context: &Self::Context);

    //
    // Extension methods on collector
    //

    /// Attempt to prevent garbage collection for the duration of the closure
    ///
    /// This method is **OPTIONAL** and will panic if unimplemented.
    fn prevent_collection<R>(collector: &C, func: impl FnOnce() -> R) -> R;

    /// Free the specified context
    ///
    /// ## Safety
    /// - Assumes the specified pointer is valid
    /// - Assumes there are no more outstanding borrows to values in the context
    unsafe fn free_context(collector: &C, context: *mut Self::Context);
}

/// The underlying state of a context
///
/// Each context is bound to one and only one thread,
/// even if the collector supports multi-threading.
pub unsafe trait RawContext<C>: Debug + self::sealed::Sealed
where
    C: RawCollectorImpl<RawContext = Self>,
{
    unsafe fn register_new(collector: &CollectorRef<C>) -> ManuallyDrop<Box<Self>>;
    /// Trigger a safepoint for this context.
    ///
    /// This implicitly attempts a collection,
    /// potentially blocking until completion..
    ///
    /// Undefined behavior if mutated during collection
    ///
    /// ## Safety
    /// See [GcContext::unchecked_safepoint]
    unsafe fn trigger_safepoint(&self);
    /// Borrow a reference to the shadow stack,
    /// assuming this context is valid (not active).
    ///
    /// A context is valid if it is either frozen
    /// or paused at a safepoint.
    ///
    /// ## Safety
    /// The context must be "inactive",
    /// either frozen or paused at a safepoint.
    #[inline]
    unsafe fn assume_valid_shadow_stack(&self) -> &ShadowStack<C> {
        match self.state() {
            ContextState::Active => unreachable!("active context: {:?}", self),
            ContextState::SafePoint { .. } | ContextState::Frozen { .. } => {}
        }
        &*self.shadow_stack_ptr()
    }
    /// Get a pointer to the shadow stack
    fn shadow_stack_ptr(&self) -> *mut ShadowStack<C>;
    /// Get a reference to the collector as a [CollectorRef]
    ///
    /// ## Safety
    /// Assumes the underlying collector is still valid.
    unsafe fn collector_ref(&self) -> &'_ CollectorRef<C>;
    /// Get a reference to the collector,
    /// assuming that it's valid
    ///
    /// ## Safety
    /// Assumes that the underlying collector
    /// is still valid.
    #[inline]
    unsafe fn collector(&self) -> &C {
        self.collector_ref().as_raw()
    }
    fn state(&self) -> ContextState;
}

mod sealed {
    pub trait Sealed {}
}
