//! A simpler implementation of [::zerogc::CollectorContext]
//! that doesn't support multiple threads/contexts.
//!
//! In exchange, there is no locking :)

use std::cell::{Cell, UnsafeCell, RefCell};
use std::mem::ManuallyDrop;
use std::fmt::{self, Debug, Formatter};
use std::marker::PhantomData;

use slog::{Logger, FnValue, trace, o};

use crate::{CollectorRef, ShadowStack, ContextState};
use crate::collector::RawCollectorImpl;

/// Manages coordination of garbage collections
///
/// This is factored out of the main code mainly due to
/// differences from single-threaded collection
pub struct CollectionManager<C: RawCollectorImpl> {
    /// Implicit collector ref
    _marker: PhantomData<CollectorRef<C>>,
    /// Access to the internal state
    state: RefCell<CollectorState>,
    /// Whether a collection is currently in progress
    ///
    /// Used for debugging only
    collecting: Cell<bool>,
    /// Sanity check to ensure there's only one context
    has_existing_context: Cell<bool>,
}
impl<C: RawCollectorImpl> CollectionManager<C> {
    pub fn new() -> Self {
        CollectionManager {
            _marker: PhantomData,
            state: RefCell::new(CollectorState::new()),
            collecting: Cell::new(false),
            has_existing_context: Cell::new(false)
        }
    }
    #[inline]
    pub fn is_collecting(&self) -> bool {
        self.collecting.get()
    }
    #[inline]
    pub fn should_trigger_collection(&self) -> bool {
        /*
         * Unlike the sync context manager, we can assume
         * there is only a single thread.
         * Therefore we don't need to check for other threads
         * having a collection in progress when we're at a safepoint.
         *
         * If we were having a collection, control flow is already
         * taken over by the collector ;)
         */
        false
    }
    pub(super) unsafe fn freeze_context(&self, context: &RawContext<C>) {
        debug_assert_eq!(context.state.get(), ContextState::Active);
        unimplemented!("Freezing single-threaded contexts")
    }
    pub(super) unsafe fn unfreeze_context(&self, _context: &RawContext<C>) {
        // We can't freeze, so we sure can't unfreeze
        unreachable!("Can't unfreeze a single-threaded context")
    }
}
pub struct RawContext<C: RawCollectorImpl> {
    /// Since we're the only context, we should (logically)
    /// be the only owner.
    ///
    /// This is still an Arc for easier use alongside the
    /// thread-safe implementation
    pub(crate) collector: CollectorRef<C>,
    // NOTE: We are Send, not Sync
    pub(super) shadow_stack: UnsafeCell<ShadowStack<C>>,
    // TODO: Does the collector access this async?
    pub(super) state: Cell<ContextState>,
    logger: Logger
}
impl<C: RawCollectorImpl> Debug for RawContext<C> {
    fn fmt(&self, f: &mut Formatter) -> fmt::Result {
        f.debug_struct("RawContext")
            .field(
                "collector",
                &format_args!("{:p}", &self.collector)
            )
            .field(
                "shadow_stacks",
                // We're assuming this is valid....
                unsafe { &*self.shadow_stack.get() }
            )
            .field("state", &self.state.get())
            .finish()
    }
}
impl<C: RawCollectorImpl> RawContext<C> {
    pub(crate) unsafe fn from_collector(collector: CollectorRef<C>) -> ManuallyDrop<Box<Self>> {
        assert!(
            !collector.as_raw().manager().has_existing_context
                .replace(true),
            "Already created a context for the collector!"
        );
        let logger = collector.as_raw().logger().new(o!());
        let context = ManuallyDrop::new(Box::new(RawContext {
            logger: logger.clone(), collector,
            shadow_stack: UnsafeCell::new(ShadowStack {
                last: std::ptr::null_mut(),
            }),
            state: Cell::new(ContextState::Active)
        }));
        trace!(
            logger, "Initializing context";
            "ptr" => format_args!("{:p}", &**context),
        );
        context
    }
    /// Trigger a safepoint for this context.
    ///
    /// This implicitly attempts a collection,
    /// potentially blocking until completion..
    ///
    /// Undefined behavior if mutated during collection
    #[cold]
    #[inline(never)]
    pub unsafe fn trigger_safepoint(&self) {
        /*
         * Begin a collection.
         *
         * Since we are the only collector we don't have
         * to worry about awaiting other threads stopping
         * at a safepoint.
         * This simplifies the implementation considerably.
         */
        assert!(!self.collector.as_raw().manager().collecting.get());
        self.collector.as_raw().manager().collecting.set(true);
        let collection_id = self.collector.as_raw().manager().state.borrow_mut()
            .next_pending_id();
        trace!(
            self.logger,
            "Creating collector";
            "id" => collection_id,
            "ctx_ptr" => format_args!("{:?}", self)
        );
        let shadow_stack = &*self.shadow_stack.get();
        let ptr = self as *const RawContext<C> as *mut RawContext<C>;
        // Change our state to mark we are now waiting at a safepoint
        assert_eq!(self.state.replace(ContextState::SafePoint {
            collection_id,
        }), ContextState::Active);
        trace!(
            self.logger, "Beginning collection";
            "ptr" => ?ptr,
            "shadow_stack" => FnValue(|_| format!("{:?}", shadow_stack.as_vec())),
            "state" => ?self.state,
            "collection_id" => collection_id,
            "original_size" => %self.collector.as_raw().allocated_size(),
        );
        self.collector.as_raw().perform_raw_collection(&[ptr]);
        assert_eq!(
            self.state.replace(ContextState::Active),
            ContextState::SafePoint { collection_id }
        );
        assert!(self.collector.as_raw().manager().collecting.replace(false));
    }
    /// Borrow a reference to the shadow stack,
    /// assuming this context is valid (not active).
    ///
    /// A context is valid if it is either frozen
    /// or paused at a safepont.
    pub unsafe fn assume_valid_shadow_stack(&self) -> &ShadowStack<C> {
        match self.state.get() {
            ContextState::Active => unreachable!("active context: {:?}", self),
            ContextState::SafePoint { .. } | ContextState::Frozen { .. } => {},
        }
        &*self.shadow_stack.get()
    }
}

// Pending collections

/// Keeps track of a pending collection (if any)
///
/// This must be held under a write lock for a collection to happen.
/// This must be held under a read lock to prevent collections.
pub struct CollectorState {
    next_pending_id: u64
}
impl CollectorState {
    pub fn new() -> Self {
        CollectorState {
            next_pending_id: 0
        }
    }
    fn next_pending_id(&mut self) -> u64 {
        let id = self.next_pending_id;
        self.next_pending_id = id.checked_add(1)
            .expect("Overflow");
        id
    }
}

/// Methods to control collector state.
///
/// Because we're not defined in the same crate,
/// we must use an extension trait.
pub(crate) trait NoSyncCollectorImpl: RawCollectorImpl {
    #[inline]
    unsafe fn free_context(&self, _raw: *mut RawContext<Self>) {
        // No extra work to do - automatic Drop handles everything
    }
}
/// Blanket implementation
impl<C: RawCollectorImpl> NoSyncCollectorImpl for C {}
