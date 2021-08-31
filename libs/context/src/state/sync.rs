//! Thread safe state
//!
//! Note that this module has full permission to use
//! the standard library in all its glory.
use std::cell::{Cell, UnsafeCell};
use std::sync::atomic::{Ordering, AtomicBool};
use std::mem::ManuallyDrop;
use std::fmt::{Debug, Formatter};
use std::{fmt, mem};
use std::collections::HashSet;
use parking_lot::{Mutex, RwLockReadGuard, RwLockUpgradableReadGuard, RwLockWriteGuard, Condvar, RwLock};

use slog::{Logger, FnValue, trace, Drain, o};

use super::{ShadowStack, ContextState};
use crate::{RawCollectorImpl, CollectorRef};
use crate::utils::ThreadId;
use crate::collector::SyncCollector;

/// Manages coordination of garbage collections
///
/// This is factored out of the main code mainly due to
/// differences from single-threaded collection
pub struct CollectionManager<C: RawCollectorImpl> {
    /// Lock on the internal state
    state: RwLock<CollectorState<C>>,
    /// Simple flag on whether we're currently collecting
    ///
    /// This should be true whenever `self.state.pending` is `Some`.
    /// However if you don't hold the lock you may not always
    /// see a fully consistent state.
    collecting: AtomicBool,
    /// The condition variable for all contexts to be valid
    ///
    /// In order to be valid, a context must be either frozen
    /// or paused at a safepoint.
    ///
    /// After a collection is marked as pending, threads must wait until
    /// all contexts are valid before the actual work can begin.
    valid_contexts_wait: Condvar,
    /// Wait until a garbage collection is over.
    ///
    /// This must be separate from `valid_contexts_wait`.
    /// A garbage collection can only begin once all contexts are valid,
    /// so this condition logically depends on `valid_contexts_wait`.
    collection_wait: Condvar,
    /// The mutex used alongside `valid_contexts_wait`
    ///
    /// This doesn't actually protect any data. It's just
    /// used because [parking_lot::RwLock] doesn't support condition vars.
    /// This is the [officially suggested solution](https://github.com/Amanieu/parking_lot/issues/165)
    // TODO: Should we replace this with `known_collections`?
    valid_contexts_lock: Mutex<()>,
    /// The mutex used alongside `collection_wait`
    ///
    /// Like `collection_wait`, his doesn't actually protect any data.
    collection_wait_lock: Mutex<()>,
}
impl<C: SyncCollector> super::sealed::Sealed for CollectionManager<C> {}
unsafe impl<C> super::CollectionManager<C> for CollectionManager<C>
    where C: SyncCollector,
          C: RawCollectorImpl<Manager=Self, RawContext=RawContext<C>> {
    type Context = RawContext<C>;

    fn new() -> Self {
        assert!(C::SYNC);
        CollectionManager {
            state: RwLock::new(CollectorState::new()),
            valid_contexts_wait: Condvar::new(),
            collection_wait: Condvar::new(),
            valid_contexts_lock: Mutex::new(()),
            collection_wait_lock: Mutex::new(()),
            collecting: AtomicBool::new(false),
        }
    }
    #[inline]
    fn is_collecting(&self) -> bool {
        self.collecting.load(Ordering::SeqCst)
    }
    #[inline]
    fn should_trigger_collection(&self) -> bool {
        /*
         * Use relaxed ordering. Eventually consistency is correct
         * enough for our use cases. It may delay some threads reaching
         * a safepoint for a little bit, but it avoids an expensive
         * memory fence on ARM and weakly ordered architectures.
         */
        self.collecting.load(Ordering::Relaxed)
    }
    unsafe fn freeze_context(&self, context: &RawContext<C>) {
        assert_eq!(context.state.get(), ContextState::Active);
        // TODO: Isn't this state read concurrently?
        context.state.set(ContextState::Frozen);
        /*
         * We may need to notify others that we are frozen
         * This means we are now "valid" for the purposes of
         * collection ^_^
         */
        self.valid_contexts_wait.notify_all();
    }
    unsafe fn unfreeze_context(&self, context: &RawContext<C>) {
        /*
         * A pending collection might be relying in the validity of this
         * context's shadow stack, so unfreezing it while in progress
         * could trigger undefined behavior!!!!!
         */
        context.collector.as_raw().prevent_collection(|_| {
            assert_eq!(context.state.get(), ContextState::Frozen);
            context.state.set(ContextState::Active);
        })
    }

    #[inline]
    fn prevent_collection<R>(collector: &C, func: impl FnOnce() -> R) -> R {
        collector.prevent_collection(|_state| func())
    }

    #[inline]
    unsafe fn free_context(collector: &C, context: *mut Self::Context) {
        collector.free_context(context)
    }
}

pub struct RawContext<C: RawCollectorImpl> {
    pub(crate) collector: CollectorRef<C>,
    original_thread: ThreadId,
    // NOTE: We are Send, not Sync
    pub(super) shadow_stack: UnsafeCell<ShadowStack<C>>,
    // TODO: Does the collector access this async?
    pub(super) state: Cell<ContextState>,
    pub(super) logger: Logger
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
/// Dummy impl
impl<C: SyncCollector> super::sealed::Sealed for RawContext<C> {}
unsafe impl<C> super::RawContext<C> for RawContext<C>
    where C: SyncCollector<RawContext=Self, Manager=CollectionManager<C>> {
    unsafe fn register_new(collector: &CollectorRef<C>) -> ManuallyDrop<Box<Self>> {
        let original_thread = if collector.as_raw().logger().is_trace_enabled() {
            ThreadId::current()
        } else {
            ThreadId::Nop
        };
        let mut context = ManuallyDrop::new(Box::new(RawContext {
            collector: collector.clone_internal(),
            original_thread: original_thread.clone(),
            logger: collector.as_raw().logger().new(o!(
                "original_thread" => original_thread.clone()
            )),
            shadow_stack: UnsafeCell::new(ShadowStack {
                last: std::ptr::null_mut(),
            }),
            state: Cell::new(ContextState::Active)
        }));
        let old_num_total = collector.as_raw().add_context(&mut **context);
        trace!(
            collector.as_raw().logger(), "Creating new context";
            "ptr" => format_args!("{:p}", &**context),
            "old_num_total" => old_num_total,
            "current_thread" => &original_thread
        );
        context
    }
    #[cold]
    #[inline(never)]
    unsafe fn trigger_safepoint(&self) {
        /*
         * Collecting requires a *write* lock.
         * We are higher priority than
         *
         * This assumes that parking_lot priorities pending writes
         * over pending reads. The standard library doesn't guarantee this.
         */
        let collector = self.collector.as_raw();
        let mut guard = collector.manager().state.write();
        let state = &mut *guard;
        // If there is not a active `PendingCollection` - create one
        if state.pending.is_none() {
            assert_eq!(collector.manager().collecting.compare_exchange(
                false, true,
                Ordering::SeqCst,
                Ordering::Relaxed, // Failure is a panic either way -_-
            ), Ok(false));
            let id = state.next_pending_id();
            #[allow(clippy::mutable_key_type)] // Used for debugging (see below)
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
                "known_contexts" => FnValue(|_| {
                    // TODO: Use nested-values/serde?!?!
                    #[allow(clippy::mutable_key_type)] // We only use this for debugging
                    let mut map = std::collections::HashMap::new();
                    for &context in &*known_contexts {
                        map.insert(context, format!("{:?} @ {:?}: {:?}",
                            (*context).state.get(),
                            (*context).original_thread,
                            &*(*context).shadow_stack.get()
                        ));
                    }
                    format!("{:?}", map)
                })
            );
        }
        let shadow_stack = &*self.shadow_stack.get();
        let ptr = self as *const RawContext<C> as *mut RawContext<C>;
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
            "shadow_stack" => FnValue(|_| format!("{:?}", shadow_stack.as_vec())),
            "total_contexts" => pending.total_contexts,
            "waiting_contexts" => pending.waiting_contexts,
            "state" => ?pending.state,
            "collector_id" => expected_id
        );
        collector.await_collection(
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
                    "original_size" => %collector.allocated_size(),
                    "contexts" => ?contexts,
                    "total_contexts" => pending.total_contexts,
                    "state" => ?pending.state,
                    "collector_id" => pending.id,
                );
                collector.perform_raw_collection(&contexts);
                assert_eq!(
                    pending.state,
                    PendingState::InProgress
                );
                pending.state = PendingState::Finished;
                // Now acknowledge that we're finished
                collector.acknowledge_finished_collection(
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

    #[inline]
    fn shadow_stack_ptr(&self) -> *mut ShadowStack<C> {
        self.shadow_stack.get()
    }

    #[inline]
    unsafe fn collector_ref(&self) -> &CollectorRef<C> {
        &self.collector
    }

    #[inline]
    fn state(&self) -> ContextState {
        self.state.get()
    }
}
// Pending collections

/// Keeps track of a pending collection (if any)
///
/// This must be held under a write lock for a collection to happen.
/// This must be held under a read lock to prevent collections.
pub struct CollectorState<C: RawCollectorImpl> {
    /// A pointer to the currently pending collection (if any)
    ///
    /// Once the number of the known roots in the pending collection
    /// is equal to the number of `total_contexts`,
    /// collection can safely begin.
    pending: Option<PendingCollection<C>>,
    /// A list of all the known contexts
    ///
    /// This persists across collections. Once a context
    /// is freed it's assumed to be unused.
    ///
    /// NOTE: We need another layer of locking since "readers"
    /// like add_context may want
    known_contexts: Mutex<HashSet<*mut RawContext<C>>>,
    next_pending_id: u64
}
#[allow(clippy::new_without_default)]
impl<C: RawCollectorImpl> CollectorState<C> {
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

/// Methods to control collector state.
///
/// Because we're not defined in the same crate,
/// we must use an extension trait.
pub(crate) trait SyncCollectorImpl: RawCollectorImpl<Manager=CollectionManager<Self>> {
    fn prevent_collection<R>(&self, func: impl FnOnce(&CollectorState<Self>) -> R) -> R {
        // Acquire the lock to ensure there's no collection in progress
        let mut state = self.manager().state.read();
        while state.pending.is_some() {
            RwLockReadGuard::unlocked(&mut state, || {
                let mut lock = self.manager().collection_wait_lock.lock();
                self.manager().collection_wait.wait(&mut lock);
            })
        }
        assert!(!self.manager().collecting.load(Ordering::SeqCst));
        func(&*state) // NOTE: Lock will be released by RAII
    }
    unsafe fn add_context(&self, raw: *mut RawContext<Self>) -> usize {
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
    unsafe fn free_context(&self, raw: *mut RawContext<Self>) {
        trace!(
            self.logger(), "Freeing context";
            "ptr" => format_args!("{:p}", raw),
            "state" => ?(*raw).state.get()
        );
        /*
         * TODO: Consider using regular read instead of `read_upgradable`
         * This is only prevented because of the fact that we may
         * need to mutate `valid_contexts` and `total_contexts`
         */
        let ptr = raw; // TODO - blegh
        let guard = self.manager().state.upgradable_read();
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
                // We shouldn't be in the list of valid contexts!
                assert_eq!(
                    pending.valid_contexts.iter()
                        .find(|&&ctx| std::ptr::eq(ctx, ptr)),
                    None, "state = {:?}", (*raw).state.get()
                );
                pending.total_contexts -= 1;
                assert!(guard.known_contexts.get_mut().remove(&ptr));
                drop(guard);
            },
            Some(PendingCollection { state: PendingState::InProgress, .. }) => {
                unreachable!("cant free while collection is in progress")
            },
        }
        /*
         * Notify all threads waiting for contexts to be valid.
         * TODO: I think this is really only useful if we're waiting....
         */
        self.manager().valid_contexts_wait.notify_all();
        // Now drop the Box
        drop(Box::from_raw(raw));
    }
    /// Wait until the specified collection is finished
    ///
    /// This will implicitly begin collection as soon
    /// as its ready
    unsafe fn await_collection(
        &self,
        expected_id: u64,
        context: *mut RawContext<Self>,
        mut lock: RwLockWriteGuard<CollectorState<Self>>,
        perform_collection: impl FnOnce(
            &mut CollectorState<Self>,
            Vec<*mut RawContext<Self>>
        )
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
                    debug_assert!(self.manager().collecting.load(Ordering::SeqCst));
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
                                mem::take(&mut pending.valid_contexts)
                            };
                            perform_collection(&mut *lock, contexts);
                            drop(lock);
                            /*
                             * Notify all blocked threads we're finished.
                             * We can proceed immediately and everyone else
                             * will slowly begin to wakeup;
                             */
                            self.manager().collection_wait.notify_all();
                            return
                        }
                        PendingState::Waiting => {
                            RwLockWriteGuard::unlocked(&mut lock, || {
                                let mut lock = self.manager().valid_contexts_lock.lock();
                                /*
                                 * Wait for all contexts to be valid.
                                 *
                                 * Typically we're waiting for them to reach
                                 * a safepoint.
                                 */
                                self.manager().valid_contexts_wait.wait(&mut lock);
                            })
                        }
                        PendingState::InProgress => {
                            RwLockWriteGuard::unlocked(&mut lock, || {
                                let mut lock = self.manager().collection_wait_lock.lock();
                                /*
                                 * Another thread has already started collecting.
                                 * Wait for them to finish.
                                 *
                                 * Block until collection finishes
                                 * Parking lot says there shouldn't be any "spurious"
                                 * (accidental) wakeups. However I guess it's possible
                                 * we're woken up somehow in the middle of collection.
                                 */
                                self.manager().collection_wait.wait(&mut lock);
                            })
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
        }
    }
    unsafe fn acknowledge_finished_collection(
        &self,
        pending_ref: &mut Option<PendingCollection<Self>>,
        context: *mut RawContext<Self>
    ) {
        let waiting_contexts = {
            let pending = pending_ref.as_mut().unwrap();
            assert_eq!(pending.state, PendingState::Finished);
            if cfg!(debug_assertions) {
                // Remove ourselves to mark the fact we're done
                match pending.valid_contexts.iter()
                    .position(|&ptr| std::ptr::eq(ptr, context)) {
                    Some(index) => {
                        pending.valid_contexts.remove(index);
                    },
                    None => panic!("Unable to find context: {:p}", context)
                }
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
            assert_eq!(self.manager().collecting.compare_exchange(
                true, false,
                Ordering::SeqCst,
                Ordering::Relaxed, // Failure is catastrophic
            ), Ok(true));
            // Someone may be waiting for us to become `None`
            self.manager().collection_wait.notify_all();
        }
    }
}
/// Blanket implementation
impl<C> SyncCollectorImpl for C
    where C: crate::collector::SyncCollector,
          C: RawCollectorImpl<Manager=CollectionManager<Self>> {}

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
pub(crate) struct PendingCollection<C: RawCollectorImpl> {
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
    valid_contexts: Vec<*mut RawContext<C>>,
    /// The unique id of this safepoint
    ///
    /// 64-bit integers pretty much never overflow (for like 100 years)
    id: u64
}
impl<C: RawCollectorImpl> PendingCollection<C> {
    pub fn new(
        id: u64,
        valid_contexts: Vec<*mut RawContext<C>>,
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
    #[inline]
    pub unsafe fn push_pending_context(
        &mut self,
        context: *mut RawContext<C>,
    ) {
        debug_assert_ne!((*context).state.get(), ContextState::Active);
        assert_eq!(self.state, PendingState::Waiting);
        debug_assert!(!self.valid_contexts.contains(&context));
        self.valid_contexts.push(context);
        // We be waiting
        assert_eq!(self.state, PendingState::Waiting);
    }
}

