use std::sync::{Arc, Weak as WeakArc};
use std::cell::{Cell, UnsafeCell};
use std::sync::atomic::{Ordering, AtomicBool};
use std::ptr::NonNull;

use slog::{Logger, FnValue, trace, Drain, o};
use parking_lot::{MutexGuard, Mutex, Condvar, MappedMutexGuard};
use crossbeam::atomic::AtomicCell;

use zerogc::{GcContext, Trace, FrozenContext};
use crate::{SimpleCollector, RawSimpleCollector, DynTrace};
use crate::utils::ThreadId;
use std::mem::ManuallyDrop;
use std::fmt::{Debug, Formatter};
use std::fmt;
use std::collections::HashSet;

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
    Frozen {
        /// The pointer to the last element in the
        /// frozen shadow stack.
        ///
        /// Used for internal sanity checking.
        last_ptr: *mut dyn DynTrace
    }
}

pub struct RawContext {
    pub(crate) collector: Arc<RawSimpleCollector>,
    // NOTE: We are Send, not Sync
    shadow_stack: UnsafeCell<ShadowStack>,
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
    pub unsafe fn register_new(collector: &Arc<RawSimpleCollector>) -> ManuallyDrop<Box<Self>> {
        let original_thread = if collector.logger.is_trace_enabled() {
            ThreadId::current()
        } else {
            ThreadId::Nop
        };
        let mut context = ManuallyDrop::new(Box::new(RawContext {
            collector: collector.clone(),
            logger: collector.logger.new(o!(
                "original_thread" => original_thread
            )),
            shadow_stack: UnsafeCell::new(ShadowStack {
                elements: Vec::with_capacity(4)
            }),
            state: Cell::new(ContextState::Active)
        }));
        let old_num_total = collector.pending.add_context(&mut **context);
        trace!(
            collector.logger, "Creating new context";
            "old_num_total" => old_num_total,
            "current_thread" => &original_thread
        );
        context
    }
    /// Attempt a collection,
    /// potentially blocking on other threads
    ///
    /// Undefined behavior if mutated during collection
    #[cold]
    #[inline(never)]
    pub unsafe fn maybe_collect(&self) {
        let mut pending = self.collector.pending.pending.lock();
        if pending.is_none() {
            assert!(!self.collector.pending.collecting.compare_and_swap(
                false, true, Ordering::SeqCst
            ));
            let id = self.collector.pending.next_pending_id();
            *pending = Some(PendingCollection::new(id));
        }
        assert_eq!(self.state.get(), ContextState::Active);
        let shadow_stack = unsafe { &*self.shadow_stack.get() };
        let status = pending.as_mut().unwrap().push_pending_context(
            &*shadow_stack, &self.collector.pending.persistent_roots,
            self.collector.pending.num_total_contexts()
        );
        match status {
            PendingStatus::ShouldCollect { temp_stacks, frozen_stacks } => {
                // NOTE: We keep this trace since it actually dumps contets of stacks
                trace!(
                    self.logger, "Beginning simple collection";
                    "current_thread" => FnValue(|_| ThreadId::current()),
                    "original_size" => self.collector.heap.allocator.allocated_size(),
                    "temp_stacks" => FnValue(|_| {
                        format!("{:?}", temp_stacks.iter()
                            .map(|ptr| (*ptr.as_ptr()).elements.clone())
                            .collect::<Vec<_>>())
                    }),
                    "frozen_stacks" => FnValue(|_| {
                        format!("{:?}", frozen_stacks.iter()
                            .map(|ptr| (*ptr.as_ptr()).elements.clone())
                            .collect::<Vec<_>>())
                    }),
                    "total_contexts" => FnValue(|_| self.collector.pending.num_total_contexts()),
                    "state" => ?pending.as_ref().map(|pending| pending.state),
                    "collector_id" => ?pending.as_ref().map(|pending| pending.id),
                );
                self.collector.perform_raw_collection(
                    &temp_stacks, &frozen_stacks
                );
                self.collector.pending.finish_collection(pending);
            },
            PendingStatus::NeedToWait => {
                let pending = MutexGuard::map(pending, |p| p.as_mut().unwrap());
                trace!(
                    self.logger, "Awaiting collection";
                    "current_thread" => FnValue(|_| ThreadId::current()),
                    "shadow_stack" => ?shadow_stack.elements,
                    "total_contexts" => FnValue(|_| self.collector.pending.num_total_contexts()),
                    "known_temp_stacks" => FnValue(|_| pending.shadow_stacks.len()),
                    "known_frozen_stacks" => FnValue(|_| self.collector.pending.persistent_roots.num_frozen_stacks()),
                    "state" => ?pending.state,
                    "collector_id" => pending.id
                );
                let expected_id = pending.id;
                self.collector.pending.await_collection(pending);
                trace!(
                    self.logger, "Finished waiting for collection";
                    "current_thread" => FnValue(|_| ThreadId::current()),
                    "collector_id" => expected_id,
                );
            }
        }
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
            raw: RawContext::register(collector.0.clone()),
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
            collector.pending.add_context()
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
            self.raw.maybe_collect();
        }
        debug_assert_eq!(self.raw.state.get(), ContextState::Active);
        let popped = (*self.raw.shadow_stack.get())
            .pop_unchecked();
        debug_assert_eq!(popped, dyn_ptr);
    }

    unsafe fn frozen_safepoint<T: Trace>(&mut self, value: &mut &mut T)
                                         -> FrozenContext<'_, Self> {
        assert_eq!(self.raw.state.get(), ContextState::Active);
        let dyn_ptr = (*self.raw.shadow_stack.get())
            .push(value);
        assert_eq!(self.raw.state.get(), ContextState::Active);
        self.raw.state.set(ContextState::Frozen {
            last_ptr: dyn_ptr
        });
        if self.raw.collector.should_collect() {
            /*
             * NOTE: This should implicitly notify threads
             * in case someone was waiting on us.
             */
            self.raw.maybe_collect();
        }
        FrozenContext::new(self)
    }

    unsafe fn unfreeze(frozen: FrozenContext<'_, Self>) -> &'_ mut Self {
        let ctx = FrozenContext::into_inner(frozen);
        /*
         * TODO: Ensure there is no collection in progess.
         * A pending collection might be relying in the validity of this
         * context's shadow stack, so unfreezing it while in progress
         * could trigger undefined behavior!!!!!
         */
        let dyn_ptr = match ctx.raw.state.get() {
            ContextState::Frozen { last_ptr } => last_ptr,
            state => unreachable!("{:?}", state)
        };
        let actual_ptr = unsafe { (*ctx.raw.shadow_stack.get()).pop() };
        debug_assert_eq!(actual_ptr, Some(dyn_ptr));
        ctx.raw.state.set(ContextState::Active);
        ctx
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
    pub(crate) fn pop(&mut self) -> Option<*mut dyn DynTrace> {
        self.elements.pop()
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
pub struct PendingCollectionTracker {
    /// Simple flag on whether we're currently collecting
    ///
    /// This should be true whenever `self.pending` is `Some`.
    /// However if you don't hold the lock you may not always
    /// see a fully consistent state.
    // TODO: Privacy
    pub collecting: AtomicBool,
    /// A pointer to the currently pending collection (if any)
    ///
    /// Once the number of the known roots in the pending collection
    /// is equal to the number of `total_contexts`,
    /// collection can safely begin.
    pending: Mutex<Option<PendingCollection>>,
    /// The condition variable other threads wait on
    /// at a safepoint.
    ///
    /// This is also used to notify threads when a collection is over.
    safepoint_wait: Condvar,
    /// A list of all the known contexts
    ///
    /// This persists across collections. Once a context
    /// is freed it's assumed to be unused.
    // TODO: Use a faster hash function like fxhasher?
    known_contexts: Mutex<HashSet<*mut RawContext>>,
    next_pending_id: AtomicCell<u64>
}
impl PendingCollectionTracker {
    pub fn new() -> Self {
        PendingCollectionTracker {
            collecting: AtomicBool::new(false),
            pending: Mutex::new(None),
            safepoint_wait: Condvar::new(),
            known_contexts: Mutex::new(Vec::new()),
            next_pending_id: AtomicCell::new(0)
        }
    }
    fn next_pending_id(&self) -> u64 {
        let mut id = self.next_pending_id.load();
        loop {
            let next = id.checked_add(1).expect("Overflow");
            match self.next_pending_id.compare_exchange(id, next) {
                Ok(_) => return id,
                Err(actual) => {
                    id = actual;
                }
            }
        }
    }
    pub fn prevent_collection<R, F>(&self, func: F) -> R
        where F: FnOnce() -> R {
        // Acquire the lock to ensure there's no collection in progress
        let mut pending = self.pending.lock();
        while pending.is_some() {
            self.safepoint_wait.wait(&mut pending);
        }
        assert!(!self.collecting.load(Ordering::SeqCst));
        func() // NOTE: Lock will be released by RAII
    }
    pub unsafe fn add_context(&self, raw: *mut RawContext) -> usize {
        /*
         * It's unsafe to create a context
         * while a collection is in progress.
         */
        self.prevent_collection(|| {
            // Lock the list of contexts :p
            let mut known_contexts = self.known_contexts.lock();
            let old_num_known = known_contexts.len();
            assert!(known_contexts.insert(raw));
            old_num_known
        })
    }
    pub unsafe fn free_context(&self, raw: ManuallyDrop<Box<RawContext>>) {
        let mut pending = self.pending.lock();
        match pending.map(|pending| pending.state) {
            None | Some(PendingState::Finished) => {}, // No collection -> no problem
            Some(PendingState::Waiting) => {
                /*
                 * Freeing a context could actually benefit
                 * a waiting collection by allowing it to
                 * proceed. We notify all waiters at the end
                 * for that specific reason.
                 */
                todo!("Remove from list of pending collections")
            },
            Some(PendingState::InProgress) => unreachable!(),
        }
        let ptr = &**raw as *const RawContext as *mut RawContext;
        assert!(self.known_contexts.lock().remove(&ptr));
        // Now drop the Box
        drop(ManuallyDrop::into_inner(raw));
    }
    pub fn notify_waiting_safepoints(&self) {
        self.safepoint_wait.notify_all();
    }
    /// Wait until the specified collection is finished
    unsafe fn await_collection(
        &self, mut expected_collection: MappedMutexGuard<PendingCollection>
    ) {
        let expected_id = expected_collection.id;
        expected_collection.num_waiters += 1;
        drop(expected_collection);
        loop {
            let mut lock = self.pending.lock();
            match *lock {
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
                            if pending.num_waiters == 0 {
                                // We're done :)
                                *lock = None;
                                assert!(self.collecting.compare_and_swap(
                                    true, false,
                                    Ordering::SeqCst
                                ));
                            }
                            return
                        },
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
            /*
             * Block until collection finishes
             * Parking lot says there shouldn't be any "spurious"
             * (accidental) wakeups. However I guess it's possible
             * we're woken up somehow in the middle of collection.
             * I'm going to loop just in case :)
             */
            self.safepoint_wait.wait(&mut lock);
        }
    }
    unsafe fn finish_collection(&self, mut guard: MutexGuard<Option<PendingCollection>>) {
        let waiting = {
            let pending = guard.as_mut().unwrap();
            assert_eq!(pending.state, PendingState::InProgress);
            pending.state = PendingState::Finished;
            pending.num_waiters
        };
        if waiting == 0 {
            *guard = None;
            assert!(self.collecting.compare_and_swap(
                true, false,
                Ordering::SeqCst
            ));
        }
        drop(guard);
        // Notify all blocked threads
        self.safepoint_wait.notify_all();
    }

    /// Push a context that's pending collection
    ///
    /// Undefined behavior if the context roots are invalid in any way.
    pub unsafe fn push_pending_context(
        &mut self, shadow_stack: &ShadowStack,
        persistent_roots: &PersistentRoots,
        total_contexts: usize
    ) -> PendingStatus {
        assert_eq!(self.state, PendingState::Waiting);
        self.shadow_stacks.push(
            NonNull::from(shadow_stack)
        );
        let temp_shadow_stacks = self.shadow_stacks.len();
        // Add in shadow stacks from frozen collectors
        let known_shadow_stacks = temp_shadow_stacks
            + persistent_roots.num_frozen_stacks();
        use std::cmp::Ordering;
        match known_shadow_stacks.cmp(&total_contexts) {
            Ordering::Less => {
                // We should already be waiting
                assert_eq!(self.state, PendingState::Waiting);
                /*
                 * We're still waiting on some other contexts
                 * Not really sure what it means to 'wait' in
                 * a single threaded context, but this method isn't
                 * responsible for actually blocking so I don't care.
                 */
                PendingStatus::NeedToWait
            },
            Ordering::Equal => {
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
                        &mut self.state,
                        PendingState::InProgress
                    ),
                    PendingState::Waiting
                );
                PendingStatus::ShouldCollect {
                    temp_stacks: std::mem::replace(
                        &mut self.shadow_stacks,
                        Vec::new()
                    ),
                    frozen_stacks: persistent_roots.frozen_stacks()
                }
            },
            Ordering::Greater => {
                /*
                 * len(shadow_stacks) => len(total_contexts)
                 * This is nonsense in highly unsafe code.
                 * We'll just panic
                 */
                unreachable!()
            }
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
struct PendingCollection {
    /// The state of the current collection
    state: PendingState,
    /// The shadow stacks for all of the collectors that
    /// still active and haven't been paused at a safepoint.
    ///
    /// We're waiting on this to be empty before we can
    /// begin our collection....
    pending_contexts: Vec<*mut RawContext>,
    /// The contexts that are ready to be collected.
    valid_contexts: Vec<*mut RawContext>,
    /// The unique id of this safepoint
    ///
    /// 64-bit integers pretty much never overflow (for like 100 years)
    id: u64
}
impl PendingCollection {
    pub fn new(id: u64, pending_contexts: Vec<WeakArc<RawContext>>) -> Self {
        PendingCollection {
            state: PendingState::Waiting,
            pending_contexts,
            valid_contexts: Vec::new(),
            id
        }
    }
}
pub enum PendingStatus<'a> {
    ShouldCollect {
        contexts: Vec<&'a RawContext>
    },
    NeedToWait
}
