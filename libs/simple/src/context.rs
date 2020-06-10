use std::sync::Arc;
use std::cell::{RefCell, Cell};
use std::sync::atomic::{Ordering, AtomicBool, AtomicUsize};
use std::ptr::NonNull;
use std::collections::HashSet;

use slog::{Logger, FnValue, trace};
use parking_lot::{MutexGuard, Mutex, Condvar, MappedMutexGuard};
use crossbeam::atomic::AtomicCell;

use zerogc::{GcContext, Trace, FrozenContext};
use crate::{SimpleCollector, RawSimpleCollector, DynTrace};
use crate::utils::ThreadId;

pub struct RawContext {
    /*
     * TODO: Encapsulate construction (create_context),
     * making these fields private
     */
    pub(crate) collector: Arc<RawSimpleCollector>,
    // NOTE: We are Send, not Sync
    pub shadow_stack: RefCell<ShadowStack>,
    pub(crate) frozen_ptr: Cell<Option<*mut dyn DynTrace>>,
    pub logger: Logger
}
impl RawContext {
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
        let shadow_stack = self.shadow_stack.borrow();
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
impl Drop for RawContext {
    fn drop(&mut self) {
        if self.frozen_ptr.get().is_some() {
            todo!("Undefined behavior to leak/drop frozen collector")
        }
        // TODO: Is it a good idea to log in Drop?
        trace!(self.logger, "Freeing context");
        unsafe { self.collector.pending.free_context() }
    }
}
// TODO: Private constructor and encapsulate
pub struct SimpleCollectorContext(pub(crate) Arc<RawContext>);
impl SimpleCollectorContext {
    #[inline]
    pub(crate) fn collector(&self) -> &RawSimpleCollector {
        &*self.0.collector
    }
}
unsafe impl GcContext for SimpleCollectorContext {
    type System = SimpleCollector;

    unsafe fn basic_safepoint<T: Trace>(&mut self, value: &mut &mut T) {
        let dyn_ptr = self.0.shadow_stack.borrow_mut().push(value);
        if self.0.collector.should_collect() {
            self.0.maybe_collect();
        }
        assert_eq!(
            self.0.shadow_stack.borrow_mut().pop(),
            Some(dyn_ptr)
        );
    }

    unsafe fn frozen_safepoint<T: Trace>(&mut self, value: &mut &mut T)
                                         -> FrozenContext<'_, Self> {
        assert!(self.0.frozen_ptr.get().is_none());
        let dyn_ptr = self.0.shadow_stack.borrow_mut().push(value);
        if self.0.collector.should_collect() {
            self.0.maybe_collect();
        }
        self.0.frozen_ptr.set(Some(dyn_ptr));
        /*
         * The guard (FrozenContext) ensures that we wont be mutated.
         * This is still extremely unsafe since we're assuming the pointer
         * will remain valid.
         * TODO: This is use after free if the user **doesn't** call unfreeze
         * We should probably fix that.....
         */
        self.0.collector.pending.persistent_roots
            .add_frozen_stack(NonNull::from(&*self.0.shadow_stack.borrow()));
        FrozenContext::new(self)
    }

    unsafe fn unfreeze(frozen: FrozenContext<'_, Self>) -> &'_ mut Self {
        let ctx = FrozenContext::into_inner(frozen);
        let dyn_ptr = ctx.0.frozen_ptr.replace(None).unwrap();
        ctx.0.collector.pending.persistent_roots
            .remove_frozen_stack(NonNull::from(&*ctx.0.shadow_stack.borrow()));
        assert_eq!(
            ctx.0.shadow_stack.borrow_mut().pop(),
            Some(dyn_ptr)
        );
        ctx
    }


    unsafe fn recurse_context<T, F, R>(&self, value: &mut &mut T, func: F) -> R
        where T: Trace, F: for<'gc> FnOnce(&'gc mut Self, &'gc mut T) -> R {
        let dyn_ptr = self.0.shadow_stack.borrow_mut().push(value);
        let mut sub_context = SimpleCollectorContext(self.0.clone());
        let result = func(&mut sub_context, value);
        drop(sub_context);
        assert_eq!(
            self.0.shadow_stack.borrow_mut().pop(),
            Some(dyn_ptr)
        );
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

#[derive(Clone)]
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
}

// Frozen contexts

/// Persistent roots of the collector,
/// that are known to be valid longer than the lifetime
/// of a single call to `safepoint!`
///
/// These are not nessicarrily eternal.
pub struct PersistentRoots {
    /// Pointers to the currently used shadow stacks
    ///
    /// I guess we could use a faster hash function but
    /// I don't really think freezing collectors is performance
    /// critical.
    // TODO: This could lead to use after free
    frozen_shadow_stacks: Mutex<HashSet<NonNull<ShadowStack>>>
}
impl PersistentRoots {
    pub fn new() -> Self {
        PersistentRoots {
            frozen_shadow_stacks: Mutex::new(HashSet::new())
        }
    }
    unsafe fn add_frozen_stack(&self, ptr: NonNull<ShadowStack>) {
        assert!(self.frozen_shadow_stacks.lock().insert(ptr));
    }
    unsafe fn remove_frozen_stack(&self, ptr: NonNull<ShadowStack>) {
        assert!(self.frozen_shadow_stacks.lock().remove(&ptr));
    }
    fn num_frozen_stacks(&self) -> usize {
        self.frozen_shadow_stacks.lock().len()
    }
    fn frozen_stacks(&self) -> Vec<NonNull<ShadowStack>> {
        self.frozen_shadow_stacks.lock().iter().cloned().collect()
    }
}
/// We're careful with our pointers
unsafe impl Send for PersistentRoots {}
/// This is only safe since we use a mutex
unsafe impl Sync for PersistentRoots {}

// Pending collections

/// Keeps track of a pending collection (if any)
pub struct PendingCollectionTracker {
    persistent_roots: PersistentRoots,
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
    /// The number of active contexts
    ///
    /// Leaking a context is "safe" in the sense
    /// it wont cause undefined behavior,
    /// but it will cause deadlock,
    total_contexts: AtomicUsize,
    next_pending_id: AtomicCell<u64>
}
impl PendingCollectionTracker {
    pub fn new() -> Self {
        PendingCollectionTracker {
            persistent_roots: PersistentRoots::new(),
            total_contexts: AtomicUsize::new(0),
            collecting: AtomicBool::new(false),
            pending: Mutex::new(None),
            safepoint_wait: Condvar::new(),
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
    pub unsafe fn add_context(&self) -> usize {
        /*
         * NOTE: We need to lock to ensure no collection is
         * happening at this time. It's unsafe to create a context
         * while a collection is in progress.
         */
        let mut pending = self.pending.lock();
        while pending.is_some() {
            self.safepoint_wait.wait(&mut pending);
        }
        let mut total = self.total_contexts.load(Ordering::Acquire);
        loop {
            let updated = total.checked_add(1).unwrap();
            match self.total_contexts.compare_exchange_weak(
                total, updated,
                Ordering::AcqRel,
                Ordering::Acquire, // This is what crossbeam does
            ) {
                Ok(new_total) => {
                    total = new_total;
                    break
                },
                Err(new_total) => {
                    total = new_total;
                }
            }
        }
        total
    }
    pub unsafe fn free_context(&self) {
        /*
         * Ensure we don't have a collection in progress
         */
        let mut pending = self.pending.lock();
        while pending.is_some() {
            self.safepoint_wait.wait(&mut pending);
        }
        let mut total = self.total_contexts.load(Ordering::Acquire);
        loop {
            assert_ne!(total, 0);
            match self.total_contexts.compare_exchange(
                total, total - 1,
                Ordering::AcqRel,
                Ordering::Acquire, // This is what crossbeam does
            ) {
                Ok(_) => break,
                Err(new_total) => {
                    total = new_total;
                }
            }
        }
        /*
         * It's possible that other threads are waiting for a pending
         * collection. Freeing this context could possibly allow them
         * to start collecting. Thus we need to notify them to avoid
         * deadlock.
         */
        self.safepoint_wait.notify_all();
    }
    pub fn num_total_contexts(&self) -> usize {
        self.total_contexts
            .load(std::sync::atomic::Ordering::SeqCst)
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
                            dbg!(std::thread::current(), expected_id, pending.num_waiters);
                            pending.num_waiters -= 1;
                            if pending.num_waiters == 0 {
                                // We're done :)
                                dbg!("Finished all waiting", pending.id, std::thread::current());
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
                    dbg!(std::thread::current());
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
            dbg!("Freeing collection", guard.as_ref().unwrap().id, std::thread::current());
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
    /// are currently waiting.
    ///
    /// This does not contain any of the frozen context stacks.
    /// Those are handled by [PersistentRoots].
    shadow_stacks: Vec<NonNull<ShadowStack>>,
    /// Number of threads currently waiting for completion
    ///
    /// Once this reaches zero we can free this collection
    num_waiters: usize,
    /// The unique id of this safepoint
    ///
    /// 64-bit integers pretty much never overflow (for like 100 years)
    id: u64
}
impl PendingCollection {
    pub fn new(id: u64) -> Self {
        PendingCollection {
            state: PendingState::Waiting,
            shadow_stacks: Vec::new(),
            num_waiters: 0,
            id
        }
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
pub enum PendingStatus {
    ShouldCollect {
        temp_stacks: Vec<NonNull<ShadowStack>>,
        frozen_stacks: Vec<NonNull<ShadowStack>>
    },
    NeedToWait
}
