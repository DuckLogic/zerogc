//! Internal code to abstract over the thread-safety of a collector.

use core::cell::Cell;
use core::num::NonZeroUsize;
use std::sync::Arc;

/// Internal abstraction over the thread-safety of a collector.
///
/// Publicly exposed via [`zerogc::system::GcThreadSafety`].
///
/// ## Safety
/// The correctness of this abstraction is relied upon for thread-safety.
pub unsafe trait ThreadSafetyInternal: crate::sealed::Sealed {
    /// Potentially thread-safe reference counting.
    type ReferenceCounter: ReferenceCounter;
}

/// A potentially thread-safe reference count.
///
/// ## Safety
/// The correctness of the reference-count is relied upon for memory safety.
pub unsafe trait ReferenceCounter: crate::sealed::Sealed {
    /// The maximum reference count.
    const MAX_COUNT: usize;
    /// Increment the reference count by one.
    ///
    /// For memory safety reasons,
    /// this method will implicitly abort on overflow
    /// just like standard-library reference counts.
    ///
    /// ## Safety
    /// Not technically unsafe, but incorrectly incrementing this
    /// could trigger memory leaks or aborts.
    ///
    /// Undefined behavior to incref a dead object.
    unsafe fn incref(&self) -> RefCountModification;
    /// Decrement the reference count by one.
    ///
    /// ## Safety
    /// Incorrectly decrementing the reference count
    /// may result in use-after-free.
    ///
    /// Undefined behavior to incref an already dead object.
    /// Undefined behavior on underflow.
    unsafe fn decref(&self) -> RefCountModification;
}

pub struct RefCountModification {
    /// The previous reference count.
    ///
    /// Guaranteed to be nonzero,
    /// since refcount operations can not be performed on dead objects.
    pub old_count: NonZeroUsize,
}
impl RefCountModification {
    #[inline]
    pub unsafe fn handle_incref(old_count: usize) -> Self {
        /*
         * Overflow is possible due to `std::mem::forget`.
         * If encountered, abort the program.
         * This is the same behavior as the stdlib.
         */
        if old_count >= Self::MAX_COUNT {
            libabort::abort(); // TODO: Switch to `trap` which is faster?
        }
        debug_assert_ne!(old_count, 0, "UB: cannot incref dead objects");
        RefCountModification {
            old_count: NonZeroUsize::new_unchecked(old_count),
        }
    }

    #[inline]
    pub unsafe fn handle_decref(old_count: usize) -> Self {
        debug_assert_ne!(old_count, 0, "UB: cannot decref already dead objects");
        RefCountModification {
            old_count: NonZeroUsize::new_unchecked(old_count),
        }
    }
}

/// A simple thread-unsafe reference count.
#[derive(Debug)]
pub struct NotSyncReferenceCounter {
    count: Cell<usize>,
}
unsafe impl ReferenceCounter for NotSyncReferenceCounter {
    const MAX_COUNT: usize = isize::MAX as usize;

    #[inline]
    unsafe fn incref(&self) -> RefCountModification {
        let old_count = self.count.get();
        let update = RefCountModification::handle_incref(old_count);
        self.count.set(old_count.unchecked_add(1));
        update
    }

    #[inline]
    unsafe fn decref(&self) -> RefCountModification {
        let old_count = self.count.get();
        let update = RefCountModification::handle_decref(old_count);
        self.count.set(old_count.unchecked_sub(1));
        update
    }
}
impl crate::sealed::Sealed for NotSyncReferenceCounter {}
// should be implied by Cell
static_assertions::assert_not_impl_any!(NotSyncReferenceCounter: Send, Sync);

#[cfg(feature = "sync")]
pub use self::sync::AtomicReferenceCounter;

#[cfg(feature = "sync")]
mod sync {
    use crate::system::threads::{RefCountModification, ReferenceCounter};
    use core::sync::atomic::{AtomicUsize, Ordering};

    /// An atomic reference-counter, similar to [`std::sync::Arc`].
    ///
    /// TODO: Implement biased reference counting (separate crate?)
    #[derive(Debug)]
    pub struct AtomicReferenceCounter {
        count: AtomicUsize,
    }
    unsafe impl ReferenceCounter for AtomicReferenceCounter {
        const MAX_COUNT: usize = isize::MAX as usize;

        #[inline]
        unsafe fn incref(&self) -> RefCountModification {
            // NOTE: The stdlib uses `Relaxed` ordering.
            let old_count = self.count.fetch_add(1, Ordering::Relaxed);
            RefCountModification::handle_incref(old_count)
        }

        #[inline]
        unsafe fn decref(&self) -> RefCountModification {
            // NTE: The stdlib uses `Release` ordering.
            let old_count = self.count.fetch_sub(1, Ordering::Release);
            RefCountModification::handle_decref(old_count)
        }
    }
    impl crate::sealed::Sealed for AtomicReferenceCounter {}
}
