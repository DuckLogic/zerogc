use std::sync::{Arc};
use std::cell::{Cell, UnsafeCell};
use std::sync::atomic::{Ordering};

use slog::{Logger, FnValue, trace, Drain, o};

use zerogc::{GcContext, Trace};
use crate::{SimpleCollector, RawSimpleCollector, DynTrace};
use crate::utils::ThreadId;
use std::mem::ManuallyDrop;
use std::fmt::{Debug, Formatter};
use std::{fmt, mem};
use std::collections::HashSet;
use parking_lot::{Mutex, RwLockReadGuard, RwLockUpgradableReadGuard, RwLockWriteGuard};

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum ContextState {
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
    Frozen,
}
impl ContextState {
    fn is_frozen(&self) -> bool {
        match *self {
            ContextState::Frozen => true,
            _ => false
        }
    }
}

pub struct RawContext {
    pub(crate) collector: Arc<RawSimpleCollector>,
    // NOTE: We are Send, not Sync
    shadow_stack: UnsafeCell<ShadowStack>,
    // TODO: Does the collector access this async?
    state: Cell<ContextState>,
    logger: Logger
}
impl Debug for RawContext {
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
impl RawContext {
    pub(crate) unsafe fn register_new(collector: &Arc<RawSimpleCollector>) -> ManuallyDrop<Box<Self>> {
        let original_thread = if collector.logger.is_trace_enabled() {
            ThreadId::current()
        } else {
            ThreadId::Nop
        };
        let mut context = ManuallyDrop::new(Box::new(RawContext {
            collector: collector.clone(),
            logger: collector.logger.new(o!(
                "original_thread" => original_thread.clone()
            )),
            shadow_stack: UnsafeCell::new(ShadowStack {
                elements: Vec::with_capacity(4)
            }),
            state: Cell::new(ContextState::Active)
        }));
        let old_num_total = collector.add_context(&mut **context);
        trace!(
            collector.logger, "Creating new context";
            "ptr" => format_args!("{:p}", &**context),
            "old_num_total" => old_num_total,
            "current_thread" => &original_thread
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
         * Collecting requires a *write* lock.
         * We are higher priority than
         *
         * This assumes that parking_lot priorities pending writes
         * over pending reads. The standard library doesn't guarantee this.
         */
        let mut guard = self.collector.state.write();
        let state = &mut *guard;
        // If there is not a active `PendingCollection` - create one
        if state.pending.is_none() {
            assert!(!self.collector.collecting.compare_and_swap(
                false, true, Ordering::SeqCst
            ));
            let id = state.next_pending_id();
            let known_contexts = state.known_contexts.get_mut();
            state.pending = Some(PendingCollection::new(
                id,
                known_contexts.iter()
                    .cloned()
                    .filter(|ctx| (**ctx).state.get().is_frozen())
                    .collect(),
                known_contexts.len(),
            ));
            trace!(
                self.logger,
                "Creating collector";
                "id" => id,
                "ctx_ptr" => format_args!("{:?}", self),
                "initially_valid_contexts" => ?state.pending.as_ref()
                    .unwrap().valid_contexts,
                "known_contexts" => ?*known_contexts
            );
        }
        let shadow_stack = &*self.shadow_stack.get();
        let ptr = self as *const RawContext as *mut RawContext;
        debug_assert!(state.known_contexts.get_mut().contains(&ptr));
        let mut pending = state.pending.as_mut().unwrap();
        // Change our state to mark we are now waiting at a safepoint
        assert_eq!(self.state.replace(ContextState::SafePoint {
            collection_id: pending.id
        }), ContextState::Active);
        debug_assert_eq!(
            state.known_contexts.get_mut().len(),
            pending.total_contexts
        );
        pending.push_pending_context(ptr);
        let expected_id = pending.id;
        pending.waiting_contexts += 1;
        trace!(
            self.logger, "Awaiting collection";
            "ptr" => ?ptr,
            "current_thread" => FnValue(|_| ThreadId::current()),
            "shadow_stack" => ?shadow_stack.elements,
            "total_contexts" => pending.total_contexts,
            "waiting_contexts" => pending.waiting_contexts,
            "state" => ?pending.state,
            "collector_id" => expected_id
        );
        self.collector.await_collection(
            expected_id, ptr, guard,
            |state, contexts| {
                let pending = state.pending.as_mut().unwrap();
                /*
                 * NOTE: We keep this trace since it actually dumps contents
                 * of stacks
                 */
                trace!(
                    self.logger, "Beginning simple collection";
                    "current_thread" => FnValue(|_| ThreadId::current()),
                    "original_size" => self.collector.heap.allocator.allocated_size(),
                    "contexts" => ?contexts,
                    "total_contexts" => pending.total_contexts,
                    "state" => ?pending.state,
                    "collector_id" => pending.id,
                );
                self.collector.perform_raw_collection(&contexts);
                assert_eq!(
                    pending.state,
                    PendingState::InProgress
                );
                pending.state = PendingState::Finished;
                // Now acknowledge that we're finished
                self.collector.acknowledge_finished_collection(
                    &mut state.pending, ptr
                );
            }
        );
        trace!(
            self.logger, "Finished waiting for collection";
            "current_thread" => FnValue(|_| ThreadId::current()),
            "collector_id" => expected_id,
        );
    }
    /// Borrow a reference to the shadow stack,
    /// assuming this context is valid (not active).
    ///
    /// A context is valid if it is either frozen
    /// or paused at a safepont.
    pub unsafe fn assume_valid_shadow_stack(&self) -> &ShadowStack {
        match self.state.get() {
            ContextState::Active => unreachable!("active context: {:?}", self),
            ContextState::SafePoint { .. } | ContextState::Frozen { .. } => {},
        }
        &*self.shadow_stack.get()
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
    raw: ManuallyDrop<Box<RawContext>>,
    /// Whether we are the root context
    ///
    /// Only the root actually owns the `Arc`
    /// and is responsible for dropping it
    root: bool
}
impl SimpleCollectorContext {
    pub(crate) unsafe fn register_root(collector: &SimpleCollector) -> Self {
        SimpleCollectorContext {
            raw: RawContext::register_new(&collector.0),
            root: true, // We are responsible for unregistering
        }
    }
    #[inline]
    pub(crate) fn collector(&self) -> &RawSimpleCollector {
        &*self.raw.collector
    }
}
impl Drop for SimpleCollectorContext {
    #[inline]
    fn drop(&mut self) {
        if self.root {
            let collector = self.collector();
            unsafe {
                trace!(
                    collector.logger, "Freeing context";
                    "ptr" => format_args!("{:p}", &**self.raw),
                    "current_thread" => FnValue(|_| ThreadId::current()),
                    "state" => ?self.raw.state.get()
                );
                collector.free_context(
                    std::ptr::read(&self.raw)
                )
            }
        }
    }
}
unsafe impl GcContext for SimpleCollectorContext {
    type System = SimpleCollector;

    #[inline]
    unsafe fn basic_safepoint<T: Trace>(&mut self, value: &mut &mut T) {
        debug_assert_eq!(self.raw.state.get(), ContextState::Active);
        let dyn_ptr = (*self.raw.shadow_stack.get())
            .push(value);
        if self.raw.collector.should_collect() {
            self.raw.trigger_safepoint();
        }
        debug_assert_eq!(self.raw.state.get(), ContextState::Active);
        let popped = (*self.raw.shadow_stack.get())
            .pop_unchecked();
        debug_assert_eq!(popped, dyn_ptr);
    }

    unsafe fn freeze(&mut self) {
        assert_eq!(self.raw.state.get(), ContextState::Active);
        self.raw.state.set(ContextState::Frozen);
        // We may need to notify others that we are frozen
        if self.raw.collector.collecting.load(Ordering::Acquire) {
            self.raw.collector.notify_waiting_safepoints();
        }
    }

    unsafe fn unfreeze(&mut self) {
        /*
         * A pending collection might be relying in the validity of this
         * context's shadow stack, so unfreezing it while in progress
         * could trigger undefined behavior!!!!!
         */
        self.collector().prevent_collection(|_| {
            assert_eq!(self.raw.state.get(), ContextState::Frozen);
            self.raw.state.set(ContextState::Active);
        })
    }

    #[inline]
    unsafe fn recurse_context<T, F, R>(&self, value: &mut &mut T, func: F) -> R
        where T: Trace, F: for<'gc> FnOnce(&'gc mut Self, &'gc mut T) -> R {
        debug_assert_eq!(self.raw.state.get(), ContextState::Active);
        let dyn_ptr = (*self.raw.shadow_stack.get())
            .push(value);
        let mut sub_context = SimpleCollectorContext {
            /*
             * safe to copy because we wont drop it
             * Lifetime is guarenteed to be restricted to
             * the closure.
             */
            raw: std::ptr::read(&self.raw),
            root: false /* don't drop our pointer!!! */
        };
        let result = func(&mut sub_context, value);
        debug_assert!(!sub_context.root);
        // Since we're not the root, no need to run drop code!
        std::mem::forget(sub_context);
        debug_assert_eq!(self.raw.state.get(), ContextState::Active);
        let actual_ptr = (*self.raw.shadow_stack.get())
            .pop_unchecked();
        debug_assert_eq!(actual_ptr, dyn_ptr);
        result
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

#[derive(Clone, Debug)]
pub struct ShadowStack {
    pub(crate) elements: Vec<*mut dyn DynTrace>
}
impl ShadowStack {
    #[inline]
    pub(crate) unsafe fn push<'a, T: Trace + 'a>(&mut self, value: &'a mut T) -> *mut dyn DynTrace {
        let short_ptr = value as &mut (dyn DynTrace + 'a)
            as *mut (dyn DynTrace + 'a);
        let long_ptr = std::mem::transmute::<
            *mut (dyn DynTrace + 'a),
            *mut (dyn DynTrace + 'static)
        >(short_ptr);
        self.elements.push(long_ptr);
        long_ptr
    }
    #[inline]
    pub(crate) unsafe fn pop_unchecked(&mut self) -> *mut dyn DynTrace {
        match self.elements.pop() {
            Some(value) => value,
            None => {
                #[cfg(debug_assertions)] {
                    /*
                     * In debug mode we'd rather panic than
                     * trigger undefined behavior.
                     */
                    unreachable!()
                }
                #[cfg(not(debug_assertions))] {
                    // Hint to optimizer we consider this impossible
                    std::hint::unreachable_unchecked()
                }
            }
        }
    }
}

// Frozen contexts

// Pending collections

/// Keeps track of a pending collection (if any)
///
/// This must be held under a write lock for a collection to happen.
/// This must be held under a read lock to prevent collections.
pub struct CollectorState {
    /// A pointer to the currently pending collection (if any)
    ///
    /// Once the number of the known roots in the pending collection
    /// is equal to the number of `total_contexts`,
    /// collection can safely begin.
    pending: Option<PendingCollection>,
    /// A list of all the known contexts
    ///
    /// This persists across collections. Once a context
    /// is freed it's assumed to be unused.
    ///
    /// NOTE: We need another layer of locking since "readers"
    /// like add_context may want
    known_contexts: Mutex<HashSet<*mut RawContext>>,
    next_pending_id: u64
}
impl CollectorState {
    pub fn new() -> Self {
        CollectorState {
            pending: None,
            known_contexts: Mutex::new(HashSet::new()),
            next_pending_id: 0
        }
    }
    fn next_pending_id(&mut self) -> u64 {
        let id = self.next_pending_id;
        self.next_pending_id = id.checked_add(1).expect("Overflow");
        id
    }
}
/*
 * Implementing methods to control collector state.
 *
 * Because we're defined in the same crate
 * we can use an inhernent impl instead of an extension trait.
 */
impl RawSimpleCollector {
    pub fn prevent_collection<R>(&self, func: impl FnOnce(&CollectorState) -> R) -> R {
        // Acquire the lock to ensure there's no collection in progress
        let mut state = self.state.read();
        while state.pending.is_some() {
            RwLockReadGuard::unlocked(&mut state, || {
                let mut lock = self.safepoint_lock.lock();
                self.safepoint_wait.wait(&mut lock);
            })
        }
        assert!(!self.collecting.load(Ordering::SeqCst));
        func(&*state) // NOTE: Lock will be released by RAII
    }
    pub unsafe fn add_context(&self, raw: *mut RawContext) -> usize {
        /*
         * It's unsafe to create a context
         * while a collection is in progress.
         */
        self.prevent_collection(|state| {
            // Lock the list of contexts :p
            let mut known_contexts = state.known_contexts.lock();
            let old_num_known = known_contexts.len();
            assert!(known_contexts.insert(raw), "Already inserted {:p}", raw);
            old_num_known
        })
    }
    pub unsafe fn free_context(&self, raw: ManuallyDrop<Box<RawContext>>) {
        /*
         * TODO: Consider using regular read instead of `read_upgradable`
         * This is only prevented because of the fact that we may
         * need to mutate `valid_contexts` and `total_contexts`
         */
        let ptr = &**raw as *const RawContext as *mut RawContext;
        let guard = self.state.upgradable_read();
        match &guard.pending {
            None => { // No collection
                // Still need to remove from list of known_contexts
                assert!(guard.known_contexts.lock().remove(&ptr));
                drop(guard);
            },
            Some(pending @ PendingCollection { state: PendingState::Finished, .. }) => {
                /*
                 * We're assuming here that the finished collection
                 * has already been removed from the list
                 * of `pending_contexts` and has left the safepoint.
                 * Verify this assumption
                 */
                assert!(!pending.valid_contexts.contains(&ptr));
                assert!(guard.known_contexts.lock().remove(&ptr));
                drop(guard);
            },
            Some(PendingCollection { state: PendingState::Waiting, .. }) => {
                let mut guard = RwLockUpgradableReadGuard::upgrade(guard);
                /*
                 * Freeing a context could actually benefit
                 * a waiting collection by allowing it to
                 * proceed.
                 * Verify that we're not actually in the `valid_contexts`
                 * list. A context in that list must not be freed.
                 *
                 * We need to upgrade the lock before we can mutate the state
                 */
                let pending = guard.pending.as_mut().unwrap();
                assert_eq!(
                    pending.valid_contexts.remove_item(&ptr),
                    None, "state = {:?}", raw.state.get()
                );
                pending.total_contexts -= 1;
                assert!(guard.known_contexts.get_mut().remove(&ptr));
                drop(guard);
            },
            Some(PendingCollection { state: PendingState::InProgress, .. }) => {
                unreachable!("cant free while collection is in progress")
            },
        }
        // Now drop the Box
        drop(ManuallyDrop::into_inner(raw));
        /*
         * Notify all
         * TODO: I think this is really only useful if we're waiting....
         */
        self.notify_waiting_safepoints();
    }
    pub fn notify_waiting_safepoints(&self) {
        self.safepoint_wait.notify_all();
    }
    /// Wait until the specified collection is finished
    ///
    /// This will implicitly begin collection as soon
    /// as its ready
    unsafe fn await_collection(
        &self,
        expected_id: u64,
        context: *mut RawContext,
        mut lock: RwLockWriteGuard<CollectorState>,
        perform_collection: impl FnOnce(&mut CollectorState, Vec<*mut RawContext>)
    ) {
        loop {
            match &mut lock.pending {
                Some(ref mut pending) => {
                    /*
                     * We should never move onto a new collection
                     * till we're finished waiting....
                     */
                    assert_eq!(expected_id, pending.id);
                    // Since we're Some, this should be true
                    debug_assert!(self.collecting.load(Ordering::SeqCst));
                    match pending.state {
                        PendingState::Finished => {
                            self.acknowledge_finished_collection(
                                &mut lock.pending, context
                            );
                            drop(lock);
                            return
                        },
                        PendingState::Waiting if pending.valid_contexts.len() ==
                            pending.total_contexts => {
                            /*
                             * We have all the roots. Trigger a collection
                             * Here we're assuming all the shadow stacks we've
                             * accumulated actually correspond to the shadow stacks
                             * of all the live contexts.
                             * We also assume that the shadow stacks correspond to
                             * the program's roots.
                             */
                            assert_eq!(
                                std::mem::replace(
                                    &mut pending.state,
                                    PendingState::InProgress
                                ),
                                PendingState::Waiting
                            );
                            /*
                             * In debug mode we keep using `valid_contexts`
                             * for sanity checks later on
                             */
                            let contexts = if cfg!(debug_assertions) {
                                pending.valid_contexts.clone()
                            } else {
                                // Otherwise we might as well be done with it
                                mem::replace(
                                    &mut pending.valid_contexts,
                                    Vec::new()
                                )
                            };
                            perform_collection(&mut *lock, contexts);
                            drop(lock);
                            /*
                             * Notify all blocked threads we're finished.
                             * We can proceed immediately and everyone else
                             * will slowly begin to wakeup;
                             */
                            self.safepoint_wait.notify_all();
                            return
                        }
                        PendingState::InProgress | PendingState::Waiting => {
                            /* still need to wait */
                        }
                    }
                }
                None => {
                    panic!(
                        "Unexpectedly finished collection: {}",
                        expected_id
                    )
                }
            }
            RwLockWriteGuard::unlocked(&mut lock, || {
                let mut lock = self.safepoint_lock.lock();
                /*
                 * Block until collection finishes
                 * Parking lot says there shouldn't be any "spurious"
                 * (accidental) wakeups. However I guess it's possible
                 * we're woken up somehow in the middle of collection.
                 * I'm going to loop just in case :)
                 */
                self.safepoint_wait.wait(&mut lock);
            })
        }
    }
    unsafe fn acknowledge_finished_collection(
        &self,
        pending_ref: &mut Option<PendingCollection>,
        context: *mut RawContext
    ) {
        let waiting_contexts = {
            let pending = pending_ref.as_mut().unwrap();
            assert_eq!(pending.state, PendingState::Finished);
            if cfg!(debug_assertions) {
                // Remove ourselves to mark the fact we're done
                assert_eq!(
                    pending.valid_contexts.remove_item(&context),
                    Some(context)
                );
            }
            // Mark ourselves as officially finished with the safepoint
            assert_eq!(
                (*context).state.replace(ContextState::Active),
                ContextState::SafePoint {
                    collection_id: pending.id
                }
            );
            pending.waiting_contexts -= 1;
            pending.waiting_contexts
        };
        if waiting_contexts == 0 {
            *pending_ref = None;
            assert!(self.collecting.compare_and_swap(
                true, false,
                Ordering::SeqCst
            ));
            // Someone may be waiting for us to become `None`
            self.safepoint_wait.notify_all();
        }
    }
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum PendingState {
    /// The desired collection was finished
    Finished,
    /// The collection is in progress
    InProgress,
    /// Waiting for all the threads to stop at a safepoint
    ///
    /// We need every thread's shadow stack to safely proceed.
    Waiting
}

/// The state of a collector waiting for all its contexts
/// to reach a safepoint
#[derive(Debug)]
struct PendingCollection {
    /// The state of the current collection
    state: PendingState,
    /// The total number of known contexts
    ///
    /// While a collection is in progress,
    /// no new contexts will be added.
    /// Freeing a context will update this as
    /// appropriate.
    total_contexts: usize,
    /// The number of contexts that are waiting
    waiting_contexts: usize,
    /// The contexts that are ready to be collected.
    valid_contexts: Vec<*mut RawContext>,
    /// The unique id of this safepoint
    ///
    /// 64-bit integers pretty much never overflow (for like 100 years)
    id: u64
}
impl PendingCollection {
    pub fn new(
        id: u64,
        valid_contexts: Vec<*mut RawContext>,
        total_contexts: usize,
    ) -> Self {
        PendingCollection {
            state: PendingState::Waiting,
            total_contexts,
            waiting_contexts: 0,
            valid_contexts,
            id
        }
    }
    /// Push a context that's pending collection
    ///
    /// Undefined behavior if the context roots are invalid in any way.
    pub unsafe fn push_pending_context(
        &mut self,
        context: *mut RawContext,
    ) {
        debug_assert_ne!((*context).state.get(), ContextState::Active);
        assert_eq!(self.state, PendingState::Waiting);
        debug_assert!(!self.valid_contexts.contains(&context));
        self.valid_contexts.push(context);
        // We be waiting
        assert_eq!(self.state, PendingState::Waiting);
    }
}