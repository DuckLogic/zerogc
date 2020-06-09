// TODO: Use stable rust
#![feature(
    alloc_layout_extra, // Used for GcObject::from_raw
    never_type, // Used for errors (which are currently impossible)
    negative_impls, // impl !Send is much cleaner than PhantomData<Rc<()>>
    exhaustive_patterns, // Allow exhaustive matching against never
    const_alloc_layout, // Used for StaticType
    const_fn, // We statically create type info
    const_if_match, // Used for StaticType
    const_panic, // Const panic should be stable
    const_transmute, // This can already be acheived with unions...
    untagged_unions, // Why isn't this stable?
    new_uninit, // Until Rust has const generics, this is how we init arrays..
    specialization, // Used for specialization (effectively required by GcRef)
)]
use zerogc::{GcSystem, GcSafe, Trace, GcContext, GcVisitor, GcSimpleAlloc, GcRef, GcBrand, GcDirectBarrier, FrozenContext};
use std::alloc::Layout;
use std::cell::{RefCell, Cell};
use std::ptr::NonNull;
use std::os::raw::c_void;
use std::mem::{transmute, ManuallyDrop};
use crate::alloc::{SmallArenaList, small_object_size};
use std::ops::Deref;
use std::hash::{Hash, Hasher};
use std::fmt::{Debug, Formatter};
use std::fmt;
use std::marker::PhantomData;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, AtomicBool, Ordering};
use crossbeam::atomic::AtomicCell;
use parking_lot::{Mutex, Condvar, MutexGuard, MappedMutexGuard};
use std::collections::HashSet;

use slog::{Logger, FnValue, o, debug, trace};

#[derive(Clone)]
struct ThreadId {
    id: std::thread::ThreadId,
    name: Option<String>
}
impl ThreadId {
    fn current() -> ThreadId {
        let thread = std::thread::current();
        ThreadId {
            id: thread.id(),
            name: thread.name().map(String::from)
        }
    }
}
impl slog::Value for ThreadId {
    fn serialize(
        &self, _record: &slog::Record,
        key: &'static str,
        serializer: &mut dyn slog::Serializer
    ) -> slog::Result<()> {
        let id = self.id;
        match self.name {
            Some(ref name) => {
                serializer.emit_arguments(key, &format_args!(
                    "{}: {:?}", *name, id
                ))
            },
            None => {
                serializer.emit_arguments(key, &format_args!(
                    "{:?}", id
                ))
            },
        }
    }
}

#[cfg(feature = "small-object-arenas")]
mod alloc;
#[cfg(not(feature = "small-object-arenas"))]
mod alloc {
    pub const fn is_small_object<T>() -> bool {
        false
    }
    pub const fn small_object_size<T>() -> usize {
        unimplemented!()
    }
    pub struct FakeArena;
    impl FakeArena {
        pub(crate) fn alloc(&self) -> std::ptr::NonNull<super::GcHeader> {
            unimplemented!()
        }
    }
    pub struct SmallArenaList;
    impl SmallArenaList {
        // Create dummy
        pub fn new() -> Self { SmallArenaList }
        pub fn find<T>(&self) -> Option<FakeArena> { None }
    }
}

/// A garbage collected pointer
///
/// See docs for [zerogc::GcRef]
pub struct Gc<'gc, T: GcSafe + 'gc> {
    /// Used to uniquely identify the collector,
    /// to ensure we aren't modifying another collector's pointers
    ///
    /// As long as our memory is valid,
    /// it implies this pointer is too..
    collector_ptr: NonNull<RawSimpleCollector>,
    value: NonNull<T>,
    marker: PhantomData<&'gc T>
}
impl<'gc, T: GcSafe + 'gc> Gc<'gc, T> {
    #[inline]
    pub(crate) unsafe fn from_raw(
        collector_ptr: NonNull<RawSimpleCollector>,
        value: NonNull<T>
    ) -> Self {
        Gc { collector_ptr, value, marker: PhantomData }
    }
}
impl<'gc, T: GcSafe + 'gc> GcRef<'gc, T> for Gc<'gc, T> {
    type System = SimpleCollector;

    #[inline]
    fn value(&self) -> &'gc T {
        unsafe { &mut *self.value.as_ptr() }
    }
}
/// Double-indirection is completely safe
unsafe impl<'gc, T: GcSafe + 'gc> GcSafe for Gc<'gc, T> {
    const NEEDS_DROP: bool = true; // We are Copy
}
/// Rebrand
unsafe impl<'gc, 'new_gc, T> GcBrand<'new_gc, SimpleCollector> for Gc<'gc, T>
    where T: GcSafe + GcBrand<'new_gc, SimpleCollector>,
          T::Branded: GcSafe {
    type Branded = Gc<'new_gc, <T as GcBrand<'new_gc, SimpleCollector>>::Branded>;
}
unsafe impl<'gc, T: GcSafe + 'gc> Trace for Gc<'gc, T> {
    // We always need tracing....
    const NEEDS_TRACE: bool = true;

    #[inline]
    fn visit<V: GcVisitor>(&mut self, visitor: &mut V) -> Result<(), V::Err> {
        <V as GcVisit>::visit_gc(visitor, self);
        Ok(())
    }
}
impl<'gc, T: GcSafe + 'gc> Deref for Gc<'gc, T> {
    type Target = &'gc T;

    #[inline(always)]
    fn deref(&self) -> &Self::Target {
        unsafe { &*(&self.value as *const NonNull<T> as *const &'gc T) }
    }
}
/// Simple GC doesn't need write barriers :)
///
/// This implementation is just for writing `Gc<T> -> Gc<T>`
unsafe impl<'gc, T, V> GcDirectBarrier<'gc, Gc<'gc, T>> for Gc<'gc, V>
    where T: GcSafe + 'gc, V: GcSafe + 'gc {
    #[inline(always)]  // NOP
    unsafe fn write_barrier(&self, _owner: &Gc<'gc, T>, _field_offset: usize) {}
}
// We can be copied freely :)
impl<'gc, T: GcSafe + 'gc> Copy for Gc<'gc, T> {}
impl<'gc, T: GcSafe + 'gc> Clone for Gc<'gc, T> {
    #[inline]
    fn clone(&self) -> Self {
        *self
    }
}
// Delegating impls
impl<'gc, T: GcSafe + Hash + 'gc> Hash for Gc<'gc, T> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.value().hash(state)
    }
}
impl<'gc, T: GcSafe + PartialEq + 'gc> PartialEq for Gc<'gc, T> {
    fn eq(&self, other: &Self) -> bool {
        // NOTE: We compare by value, not identity
        self.value() == other.value()
    }
}
impl<'gc, T: GcSafe + Eq + 'gc> Eq for Gc<'gc, T> {}
impl<'gc, T: GcSafe + PartialEq + 'gc> PartialEq<T> for Gc<'gc, T> {
    fn eq(&self, other: &T) -> bool {
        self.value() == other
    }
}
impl<'gc, T: GcSafe + Debug + 'gc> Debug for Gc<'gc, T> {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        if !f.alternate() {
            // Pretend we're a newtype by default
            f.debug_tuple("Gc").field(self.value()).finish()
        } else {
            // Alternate spec reveals `collector_ptr`
            f.debug_struct("Gc")
                .field("collector_ptr", &self.collector_ptr)
                .field("value", self.value())
                .finish()
        }
    }
}
// Visitor (specialized trait)
trait GcVisit: GcVisitor {
    fn visit_gc<'gc, T: GcSafe + 'gc>(&mut self, gc: &mut Gc<'gc, T>);
}
impl<V: GcVisitor> GcVisit for V {
    #[inline]
    default fn visit_gc<'gc, T: GcSafe + 'gc>(&mut self, _gc: &mut Gc<'gc, T>) {}
}
/// In order to send *references* between threads,
/// the underlying type must be sync.
///
/// This is the same reason that `Arc<T>: Send` requires `T: Sync`
unsafe impl<'gc, T: GcSafe + Sync> Send for Gc<'gc, T> {}
/// If the underlying type is `Sync`, it's safe
/// to share garbage collected references between threads.
///
/// The collector itself is always safe :D
unsafe impl<'gc, T: GcSafe + Sync> Sync for Gc<'gc, T> {}

/// Persistent roots of the collector,
/// that are known to be valid longer than the lifetime
/// of a single call to `safepoint!`
///
/// These are not nessicarrily eternal.
#[cfg(not(feature = "sync"))]
struct PersistentRoots {
    /// Pointers to the currently used shadow stacks
    ///
    /// I guess we could use a faster hash function but
    /// I don't really think freezing collectors is performance
    /// critical.
    frozen_shadow_stacks: Mutex<HashSet<NonNull<ShadowStack>>>
}
impl PersistentRoots {
    fn new() -> Self {
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

pub struct SimpleCollector(Arc<RawSimpleCollector>);
impl SimpleCollector {
    pub fn create() -> Self {
        SimpleCollector::with_logger(Logger::root(
            slog::Discard,
            o!()
        ))
    }
    pub fn with_logger(logger: Logger) -> Self {
        let mut collector = Arc::new(RawSimpleCollector {
            logger,
            pending: PendingCollectionTracker::new(),
            heap: GcHeap::new(INITIAL_COLLECTION_THRESHOLD)
        });
        let collector_ptr = &*collector
            as *const _
            as *mut RawSimpleCollector;
        Arc::get_mut(&mut collector).unwrap()
            .heap.allocator.collector_ptr = collector_ptr;
        SimpleCollector(collector)
    }
    /// Create a new context bound to this collector
    ///
    /// Warning: Only one collector should be created per thread.
    /// Doing otherwise can cause deadlocks/panics.
    pub fn create_context(&self) -> SimpleCollectorContext {
        let id = unsafe { self.0.pending.add_context() };
        // TODO: Avoid calling if logger isn't being used
        let original_thread = ThreadId::current();
        trace!(
            self.0.logger, "Creating new context";
            "id" => id, "current_thread" => &original_thread
        );
        let shadow_stack = RefCell::new(ShadowStack {
            elements: Vec::with_capacity(4), id
        });
        SimpleCollectorContext(Arc::new(RawContext {
            logger: self.0.logger.new(o!(
                "original_id" => id,
                "original_thread" => original_thread
            )),
            shadow_stack, collector: self.0.clone(),
            frozen_ptr: Cell::new(None)
        }))
    }
}

unsafe impl GcSystem for SimpleCollector {}

trait DynTrace {
    fn trace(&mut self, visitor: &mut MarkVisitor);
}
impl<T: Trace + ?Sized> DynTrace for T {
    fn trace(&mut self, visitor: &mut MarkVisitor) {
        let Ok(()) = self.visit(visitor);
    }
}
#[derive(Clone)]
struct ShadowStack {
    id: usize,
    elements: Vec<*mut dyn DynTrace>
}
impl ShadowStack {
    #[inline]
    pub unsafe fn push<'a, T: Trace + 'a>(&mut self, value: &'a mut T) -> *mut dyn DynTrace {
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
    pub fn pop(&mut self) -> Option<*mut dyn DynTrace> {
        self.elements.pop()
    }
}


/// The initial memory usage to start a collection
const INITIAL_COLLECTION_THRESHOLD: usize = 2048;

struct GcHeap {
    threshold: AtomicUsize,
    allocator: SimpleAlloc
}
impl GcHeap {
    fn new(threshold: usize) -> GcHeap {
        GcHeap {
            threshold: AtomicUsize::new(threshold),
            allocator: SimpleAlloc::new()
        }
    }
    #[inline]
    fn should_collect(&self) -> bool {
        /*
         * Going with relaxed ordering because it's not essential
         * that we see updates immediately.
         * Eventual consistency should be enough to eventually
         * trigger a collection.
         */
        self.allocator.allocated_size.load(Ordering::Relaxed)
            >= self.threshold.load(Ordering::Relaxed)
    }
}

/// A link in the chain of `BigGcObject`s
type BigObjectLinkItem = Option<NonNull<BigGcObject<DynamicObj>>>;
/// An atomic link in the linked-list of BigObjects
///
/// This is thread-safe
#[derive(Default)]
struct BigObjectLink(AtomicCell<BigObjectLinkItem>);
impl BigObjectLink {
    #[inline]
    pub fn new(item: BigObjectLinkItem) -> Self {
        BigObjectLink(AtomicCell::new(item))
    }
    #[inline]
    fn item(&self) -> BigObjectLinkItem {
        self.0.load()
    }
    #[inline]
    unsafe fn set_item_forced(&self, val: BigObjectLinkItem) {
        self.0.store(val)
    }
    #[inline]
    fn append_item(&self, big_obj: Box<BigGcObject>) {
        // Must use CAS loop in case another thread updates
        let mut expected_prev = big_obj.prev.item();
        let mut updated_item = unsafe {
            NonNull::new_unchecked(Box::into_raw(big_obj))
        };
        loop {
            match self.0.compare_exchange(
                expected_prev, Some(updated_item)
            ) {
                Ok(_) => break,
                Err(actual_prev) => {
                    unsafe {
                        /*
                         * We have exclusive access to `updated_item`
                         * here so we don't need to worry about CAS.
                         * We just need to update its `prev`
                         * link to point to the new value.
                         */
                        updated_item.as_mut().prev.0.store(actual_prev);
                        expected_prev = actual_prev;
                    }
                }
            }
        }
    }
}

/// The thread-safe implementation of an allocator
///
/// Most allocations should avoid locking.
pub(crate) struct SimpleAlloc {
    collector_ptr: *mut RawSimpleCollector,
    small_arenas: SmallArenaList,
    big_object_link: BigObjectLink,
    /// Whether the meaning of the mark bit is currently inverted.
    ///
    /// This flips every collection
    mark_inverted: AtomicBool,
    allocated_size: AtomicUsize
}
impl SimpleAlloc {
    fn new() -> SimpleAlloc {
        SimpleAlloc {
            collector_ptr: std::ptr::null_mut(),
            allocated_size: AtomicUsize::new(0),
            small_arenas: SmallArenaList::new(),
            big_object_link: BigObjectLink::new(None),
            mark_inverted: AtomicBool::new(false)
        }
    }
    #[inline]
    fn allocated_size(&self) -> usize {
        self.allocated_size.load(Ordering::Acquire)
    }
    #[inline]
    fn add_allocated_size(&self, amount: usize) {
        self.allocated_size.fetch_add(amount, Ordering::AcqRel);
    }
    #[inline]
    fn set_allocated_size(&self, amount: usize) {
        self.allocated_size.store(amount, Ordering::Release)
    }
    #[inline]
    fn mark_inverted(&self) -> bool {
        self.mark_inverted.load(Ordering::Acquire)
    }
    #[inline]
    fn set_mark_inverted(&self, b: bool) {
        self.mark_inverted.store(b, Ordering::Release)
    }

    #[inline]
    fn alloc<T: GcSafe>(&self, value: T) -> Gc<'_, T> {
        if let Some(arena) = self.small_arenas.find::<T>() {
            let header = arena.alloc();
            unsafe {
                header.as_ptr().write(GcHeader::new(
                    T::STATIC_TYPE,
                    MarkState::White.to_raw(self.mark_inverted())
                ));
                let value_ptr = header.as_ref().value().cast::<T>();
                value_ptr.write(value);
                self.add_allocated_size(small_object_size::<T>());
                Gc::from_raw(
                    NonNull::new_unchecked(self.collector_ptr),
                    NonNull::new_unchecked(value_ptr),
                )
            }
        } else {
            self.alloc_big(value)
        }
    }
    fn alloc_big<T: GcSafe>(&self, value: T) -> Gc<'_, T> {
        let mut object = Box::new(BigGcObject {
            header: GcHeader::new(
                T::STATIC_TYPE,
                MarkState::White.to_raw(self.mark_inverted())
            ),
            static_value: ManuallyDrop::new(value),
            prev: BigObjectLink::new(self.big_object_link.item()),
        });
        let gc = unsafe { Gc::from_raw(
            NonNull::new_unchecked(self.collector_ptr),
            NonNull::new_unchecked(&mut *object.static_value),
        ) };
        {
            let size = std::mem::size_of::<BigGcObject<T>>();
            self.big_object_link.append_item(unsafe {
                BigGcObject::into_dynamic_box(object)
            });
            self.add_allocated_size(size);
        }
        gc
    }
    unsafe fn sweep<'a>(&self) {
        let mut expected_size = self.allocated_size();
        let mut actual_size = 0;
        // Clear small arenas
        #[cfg(feature = "small-object-arenas")]
        for arena in self.small_arenas.iter() {
            let mut last_free = arena.free.next_free();
            let mark_inverted = self.mark_inverted.load(Ordering::SeqCst);
            arena.for_each(|slot| {
                if (*slot).is_free() {
                    /*
                     * Just ignore this. It's already part of our linked list
                     * of allocated objects.
                     */
                } else {
                    match (*slot).header.raw_state().resolve(mark_inverted) {
                        MarkState::White => {
                            // Free the object, dropping if necessary
                            expected_size -= (*slot).header.type_info.total_size();
                            if let Some(drop) = (*slot).header
                                .type_info.drop_func {
                                drop((*slot).header.value());
                            }
                            // Add to free list
                            (*slot).mark_free(last_free);
                            last_free = Some(NonNull::new_unchecked(slot));
                        },
                        MarkState::Grey => panic!("All grey objects should've been processed"),
                        MarkState::Black => {
                            /*
                             * Retain the object
                             * State will be implicitly set to white
                             * by inverting mark the meaning of the mark bits.
                             */
                            actual_size += (*slot).header.type_info.total_size();
                        },
                    }
                }
            });
            arena.free.set_next_free(last_free);
        }
        // Clear large objects
        let mut last_linked = None;
        let mut link_item = self.big_object_link.item();
        let was_mark_inverted = self.mark_inverted();
        while let Some(big_link) = link_item {
            let obj = &mut *big_link.as_ptr();
            link_item = obj.prev.item();
            match obj.header.raw_state().resolve(was_mark_inverted) {
                MarkState::White => {
                    // Free the object
                    expected_size -= obj.header.type_info.total_size();
                    drop(Box::from_raw(obj));
                },
                MarkState::Grey => panic!("All gray objects should've been processed"),
                MarkState::Black => {
                    /*
                     * Retain the object
                     * State will be implicitly set to white
                     * by inverting mark the meaning of the mark bits.
                     */
                    actual_size += obj.header.type_info.total_size();
                    obj.prev.set_item_forced(last_linked);
                    last_linked = Some(NonNull::from(&mut *obj));
                }
            }
        }
        /*
         * Flip the meaning of the mark bit,
         * implicitly resetting all Black (reachable) objects
         * to White.
         */
        self.set_mark_inverted(!was_mark_inverted);
        self.big_object_link.set_item_forced(last_linked);
        assert_eq!(expected_size, actual_size);
        self.set_allocated_size(actual_size);
    }
}
unsafe impl Send for SimpleAlloc {}
/// We're careful to be thread safe here
///
/// This isn't auto implemented because of the
/// raw pointer to the collector (we only use it as an id)
unsafe impl Sync for SimpleAlloc {}

/// Keeps track of a pending collection (if any)
struct PendingCollectionTracker {
    persistent_roots: PersistentRoots,
    /// Simple flag on whether we're currently collecting
    ///
    /// This should be true whenever `self.pending` is `Some`.
    /// However if you don't hold the lock you may not always
    /// see a fully consistent state.
    collecting: AtomicBool,
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
    fn new() -> Self {
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
    unsafe fn add_context(&self) -> usize {
        /*
         * NOTE: We need to lock to ensure no collection is
         * happening at this time. It's unsafe to create a context
         * while a collection is in progress.
         */
        let mut pending = self.pending.lock();
        while pending.is_some() {
            self.safepoint_wait.wait(&mut pending);
        }
        let mut old = self.total_contexts.load(Ordering::Acquire);
        loop {
            let updated = old.checked_add(1).unwrap();
            match self.total_contexts.compare_exchange_weak(
                old, updated,
                Ordering::AcqRel,
                Ordering::Acquire, // This is what crossbeam does
            ) {
                Ok(_) => break,
                Err(new_total) => {
                    old = new_total;
                }
            }
        }
        old
    }
    unsafe fn free_context(&self) {
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
    fn num_total_contexts(&self) -> usize {
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
    fn new(id: u64) -> Self {
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
    unsafe fn push_pending_context(
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
enum PendingStatus {
    ShouldCollect {
        temp_stacks: Vec<NonNull<ShadowStack>>,
        frozen_stacks: Vec<NonNull<ShadowStack>>
    },
    NeedToWait
}

/// We're careful - I swear :D
unsafe impl Send for RawSimpleCollector {}
unsafe impl Sync for RawSimpleCollector {}

/// The internal data for a simple collector
struct RawSimpleCollector {
    logger: Logger,
    pending: PendingCollectionTracker,
    heap: GcHeap,
}
impl RawSimpleCollector {
    #[inline]
    fn should_collect(&self) -> bool {
        /*
         * All these loads are relaxed. Its okay if we see
         * delayed updates as long as we see them eventually.
         * This is based on the assumption that safepoints are
         * frequent but need to be cheap.
         *
         * If this is false, we should be using `Acquire`
         * ordering.
         */
        self.heap.should_collect() || self.pending
            .collecting.load(Ordering::Relaxed)
    }
    #[cold]
    #[inline(never)]
    unsafe fn perform_raw_collection(
        &self, temp_stacks: &[NonNull<ShadowStack>],
        frozen_stacks: &[NonNull<ShadowStack>]
    ) {
        let roots: Vec<*mut dyn DynTrace> = frozen_stacks.iter()
            .chain(&*temp_stacks)
            .flat_map(|stack| (*stack.as_ptr()).elements.iter())
            .cloned()
            .collect();
        let num_roots = roots.len();
        let mut task = CollectionTask {
            expected_collector: self as *const Self as *mut Self,
            roots, heap: &self.heap,
            grey_stack: if cfg!(feature = "implicit-grey-stack") {
                Vec::new()
            } else {
                Vec::with_capacity(64)
            }
        };
        let original_size = self.heap.allocator.allocated_size();
        task.run();
        let updated_size = self.heap.allocator.allocated_size();
        debug!(
            self.logger, "Finished simple GC";
            "current_thread" => FnValue(|_| ThreadId::current()),
            "num_roots" => num_roots,
            "original_size" => original_size,
            "memory_freed" => original_size - updated_size
        );
    }
}
struct CollectionTask<'a> {
    expected_collector: *mut RawSimpleCollector,
    roots: Vec<*mut dyn DynTrace>,
    heap: &'a GcHeap,
    #[cfg_attr(feature = "implicit-grey-stack", allow(dead_code))]
    grey_stack: Vec<*mut GcHeader>
}
impl<'a> CollectionTask<'a> {
    fn run(&mut self) {
        // Mark
        for &root in &self.roots {
            let mut visitor = MarkVisitor {
                expected_collector: self.expected_collector,
                grey_stack: &mut self.grey_stack,
                inverted_mark: self.heap.allocator.mark_inverted()
            };
            // Dynamically dispatched
            unsafe { (*root).trace(&mut visitor); }
        }
        let was_inverted_mark = self.heap.allocator.mark_inverted();
        #[cfg(not(feature = "implicit-grey-stack"))] unsafe {
            while let Some(obj) = self.grey_stack.pop() {
                debug_assert_eq!(
                    (*obj).raw_state().resolve(was_inverted_mark),
                    MarkState::Grey
                );
                let mut visitor = MarkVisitor {
                    expected_collector: self.expected_collector,
                    grey_stack: &mut self.grey_stack,
                    inverted_mark: was_inverted_mark
                };
                ((*obj).type_info.trace_func)(
                    &mut *(*obj).value(),
                    &mut visitor
                );
                // Mark the object black now it's innards have been traced
                (*obj).update_raw_state(MarkState::Black.to_raw(was_inverted_mark));
            }
        }
        // Sweep
        unsafe { self.heap.allocator.sweep() };
        let updated_size = self.heap.allocator.allocated_size();
        // Update the threshold to be 150% of currently used size
        self.heap.threshold.store(
            updated_size + (updated_size / 2),
            Ordering::SeqCst
        );
    }
}

struct MarkVisitor<'a> {
    expected_collector: *mut RawSimpleCollector,
    #[cfg_attr(feature = "implicit-grey-stack", allow(dead_code))]
    grey_stack: &'a mut Vec<*mut GcHeader>,
    /// If this meaning of the mark bit is currently inverted
    ///
    /// This flips every collection
    inverted_mark: bool
}
unsafe impl GcVisitor for MarkVisitor<'_> {
    type Err = !;
}
impl GcVisit for MarkVisitor<'_> {
    fn visit_gc<'gc, T: GcSafe + 'gc>(&mut self, gc: &mut Gc<'gc, T>) {
        /*
         * Check the collectors match. Otherwise we're mutating
         * other people's data.
         */
        assert_eq!(gc.collector_ptr.as_ptr(), self.expected_collector);
        let obj = unsafe { &mut *GcHeader::from_value_ptr(gc.as_raw_ptr()) };
        match obj.raw_state().resolve(self.inverted_mark) {
            MarkState::White => {
                if !T::NEEDS_TRACE {
                    /*
                     * We don't need to mark this grey
                     * It has no internals that need to be traced.
                     * We can directly move it directly to the black set
                     */
                    obj.update_raw_state(MarkState::Black.to_raw(self.inverted_mark));
                } else {
                    /*
                     * We need to mark this object grey and push it onto the grey stack.
                     * It will be processed later
                     */
                    (*obj).update_raw_state(MarkState::Grey.to_raw(self.inverted_mark));
                    #[cfg(not(feature = "implicit-grey-stack"))] {
                        self.grey_stack.push(obj as *mut GcHeader);
                    }
                    #[cfg(feature = "implicit-grey-stack")] unsafe {
                        /*
                         * The user wants an implicit grey stack using
                         * recursion. This risks stack overflow but can
                         * boost performance (See 9a9634d68a4933d).
                         * On some workloads this is fine.
                         */
                        T::trace(
                            &mut *((*obj).value() as *mut T),
                            &mut *self
                        );
                        /*
                         * Mark the object black now it's innards have been traced
                         * NOTE: We do **not** do this with an implicit stack.
                         */
                        (*obj).update_raw_state(MarkState::Black);
                    }
                }
            },
            MarkState::Grey => {
                /*
                 * We've already pushed this object onto the gray stack
                 * It will be traversed eventually, so we don't need to do anything.
                 */
            },
            MarkState::Black => {
                // We've already traversed this object. It's already known to be reachable
            },
        }
    }
}

struct RawContext {
    collector: Arc<RawSimpleCollector>,
    // NOTE: We are Send, not Sync
    shadow_stack: RefCell<ShadowStack>,
    frozen_ptr: Cell<Option<*mut dyn DynTrace>>,
    logger: Logger
}
impl RawContext {
    /// Attempt a collection,
    /// potentially blocking on other threads
    ///
    /// Undefined behavior if mutated during collection
    #[cold]
    #[inline(never)]
    unsafe fn maybe_collect(&self) {
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
                    "stack_id" => shadow_stack.id,
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
                    "stack_id" => shadow_stack.id,
                    "collector_id" => expected_id
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
pub struct SimpleCollectorContext(Arc<RawContext>);
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
unsafe impl<'gc, T: GcSafe + 'gc> GcSimpleAlloc<'gc, T> for &'gc SimpleCollectorContext {
    type Ref = Gc<'gc, T>;

    #[inline]
    fn alloc(&self, value: T) -> Gc<'gc, T> {
        self.0.collector.heap.allocator.alloc(value)
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

struct GcType {
    value_size: usize,
    value_offset: usize,
    #[cfg_attr(feature = "implicit-grey-stack", allow(unused))]
    trace_func: unsafe fn(*mut c_void, &mut MarkVisitor),
    drop_func: Option<unsafe fn(*mut c_void)>,
}
impl GcType {
    #[inline]
    const fn total_size(&self) -> usize {
        self.value_offset + self.value_size
    }
}
trait StaticGcType {
    const VALUE_OFFSET: usize;
    const STATIC_TYPE: &'static GcType;
}
impl<T: GcSafe> StaticGcType for T {
    const VALUE_OFFSET: usize = {
        if alloc::is_small_object::<T>() {
            // Small object
            let layout = Layout::new::<GcHeader>();
            layout.size() + layout.padding_needed_for(std::mem::align_of::<T>())
        } else {
            // Big object
            let layout = Layout::new::<BigGcObject<()>>();
            layout.size() + layout.padding_needed_for(std::mem::align_of::<T>())
        }
    };
    const STATIC_TYPE: &'static GcType = &GcType {
        value_size: std::mem::size_of::<T>(),
        value_offset: Self::VALUE_OFFSET,
        trace_func: unsafe { transmute::<_, unsafe fn(*mut c_void, &mut MarkVisitor)>(
            <T as DynTrace>::trace as fn(&mut T, &mut MarkVisitor),
        ) },
        drop_func: if <T as GcSafe>::NEEDS_DROP {
            unsafe { Some(transmute::<_, unsafe fn(*mut c_void)>(
                std::ptr::drop_in_place::<T> as unsafe fn(*mut T)
            )) }
        } else { None }
    };
}

/// A header for a GC object
///
/// This is shared between both small arenas
/// and fallback alloc vis `BigGcObject`
#[repr(C)]
struct GcHeader {
    type_info: &'static GcType,
    /*
     * NOTE: State byte should come last
     * If the value is small `(u32)`, we could reduce
     * the padding to a 3 bytes and fit everything in a word.
     *
     * Do we really need to use atomic stores?
     */
    raw_state: AtomicCell<RawMarkState>,
}
impl GcHeader {
    #[inline]
    pub fn new(type_info: &'static GcType, raw_state: RawMarkState) -> Self {
        GcHeader { type_info, raw_state: AtomicCell::new(raw_state) }
    }
    #[inline]
    pub fn value(&self) -> *mut c_void {
        unsafe {
            (self as *const GcHeader as *mut GcHeader as *mut u8)
                // NOTE: This takes into account the possibility of `BigGcObject`
                .add(self.type_info.value_offset)
                .cast::<c_void>()
        }
    }
    #[inline]
    pub unsafe fn from_value_ptr<T: GcSafe>(ptr: *mut T) -> *mut GcHeader {
        (ptr as *mut u8).sub(T::STATIC_TYPE.value_offset) as *mut GcHeader
    }
    #[inline]
    fn raw_state(&self) -> RawMarkState {
        self.raw_state.load()
    }
    #[inline]
    fn update_raw_state(&self, raw_state: RawMarkState) {
        self.raw_state.store(raw_state);
    }
}

/// Marker for an unknown GC object
struct DynamicObj;

#[repr(C)]
struct BigGcObject<T = DynamicObj> {
    header: GcHeader,
    /// The previous object in the linked list of allocated objects,
    /// or null if its the end
    prev: BigObjectLink,
    /// This is dropped using dynamic type info
    static_value: ManuallyDrop<T>
}
impl<T> BigGcObject<T> {
    #[inline]
    unsafe fn into_dynamic_box(val: Box<Self>) -> Box<BigGcObject<DynamicObj>> {
        std::mem::transmute::<Box<BigGcObject<T>>, Box<BigGcObject<DynamicObj>>>(val)
    }
}
impl<T> Drop for BigGcObject<T> {
    fn drop(&mut self) {
        unsafe {
            if let Some(drop) = self.header.type_info.drop_func {
                drop(&mut *self.static_value as *mut T as *mut c_void);
            }
        }
    }
}

/// The raw mark state of an object
///
/// Every cycle the meaning of the white/black states
/// flips. This allows us to implicitly mark objects
/// without actually touching their bits :)
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum RawMarkState {
    /// Normally this marks the white state
    ///
    /// If we're inverted, this marks black
    Red,
    /// This always marks the grey state
    ///
    /// Inverting the mark bit doesn't affect the
    /// grey state
    Grey,
    /// Normally this marks the blue state
    ///
    /// If we're inverted, this marks white
    Blue
}
impl RawMarkState {
    #[inline]
    fn resolve(self, inverted_mark: bool) -> MarkState {
        match (self, inverted_mark) {
            (RawMarkState::Red, false) => MarkState::White,
            (RawMarkState::Red, true) => MarkState::Black,
            (RawMarkState::Grey, _) => MarkState::Grey,
            (RawMarkState::Blue, false) => MarkState::Black,
            (RawMarkState::Blue, true) => MarkState::White
        }
    }
}

/// The current mark state of the object
///
/// See [Tri Color Marking](https://en.wikipedia.org/wiki/Tracing_garbage_collection#Tri-color_marking)
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
#[repr(u8)]
enum MarkState {
    /// The object is in the "white set" and is a candidate for having its memory freed.
    ///
    /// Once all the objects have been marked,
    /// all remaining white objects will be freed.
    White,
    /// The object is in the gray set and needs to be traversed to look for reachable memory
    ///
    /// After being scanned this object will end up in the black set.
    Grey,
    /// The object is in the black set and is reachable from the roots.
    ///
    /// This object cannot be freed.
    Black
}
impl MarkState {
    #[inline]
    fn to_raw(self, inverted_mark: bool) -> RawMarkState {
        match (self, inverted_mark) {
            (MarkState::White, false) => RawMarkState::Red,
            (MarkState::White, true) => RawMarkState::Blue,
            (MarkState::Grey, _) => RawMarkState::Grey,
            (MarkState::Black, false) => RawMarkState::Blue,
            (MarkState::Black, true) => RawMarkState::Red,
        }
    }
}
