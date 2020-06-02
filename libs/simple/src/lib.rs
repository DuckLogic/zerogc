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
use zerogc::{GcSystem, GcSafe, Trace, GcContext, GcVisitor, GcSimpleAlloc, GcRef, GcBrand, GcDirectBarrier};
use std::alloc::Layout;
use std::cell::{RefCell, Cell};
use std::ptr::NonNull;
#[cfg(not(feature = "sync"))]
use std::rc::{Rc};
use std::os::raw::c_void;
use std::mem::{transmute, ManuallyDrop};
use crate::alloc::{SmallArenaList, small_object_size};
use std::ops::Deref;
use std::hash::{Hash, Hasher};
use std::fmt::{Debug, Formatter};
use std::fmt;
use std::marker::PhantomData;
#[cfg(feature = "sync")]
use std::sync::atomic::{AtomicUsize, AtomicBool, Ordering};
#[cfg(feature = "sync")]
use crossbeam::atomic::AtomicCell;
#[cfg(feature = "sync")]
use std::sync::{Arc};
#[cfg(feature = "sync")]
use parking_lot::{Mutex, Condvar};

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
#[cfg(feature = "sync")]
unsafe impl<'gc, T: GcSafe + Sync> Send for Gc<'gc, T> {}
/// As long as the collector is thread safe
/// and the underlying type is `Sync`, it's safe
/// to share garbage collected references between threads.
#[cfg(feature = "sync")]
unsafe impl<'gc, T: GcSafe + Sync> Sync for Gc<'gc, T> {}

#[cfg(feature = "sync")]
type InternalRc<T> = Arc<T>;
#[cfg(not(feature = "sync"))]
type InternalRc<T> = Rc<T>;

/// The roots of a particular collection
struct RootSet {
    /// Pointers to the currently used shadow stacks
    shadow_stacks: Vec<NonNull<ShadowStack>>
}
/// We're careful with the pointers to the shadow stacks
unsafe impl Send for RootSet {}
impl RootSet {
    fn new() -> RootSet {
        RootSet { shadow_stacks: Vec::new() }
    }
    #[inline]
    unsafe fn iter(&self) -> impl Iterator<Item=*mut dyn DynTrace> + '_ {
        self.shadow_stacks.iter()
            .flat_map(|s| (&*s.as_ptr()).0.iter().cloned())
    }
}

pub struct SimpleCollector(InternalRc<RawSimpleCollector>);
impl SimpleCollector {
    pub fn create() -> Self {
        let mut collector = InternalRc::new(RawSimpleCollector {
            pending: PendingCollectionTracker::new(),
            heap: GcHeap::new(INITIAL_COLLECTION_THRESHOLD)
        });
        let collector_ptr = &*collector
            as *const _
            as *mut RawSimpleCollector;
        InternalRc::get_mut(&mut collector).unwrap()
            .heap.allocator.collector_ptr = collector_ptr;
        SimpleCollector(collector)
    }
    /// Create a new context bound to this collector
    ///
    /// Warning: Only one collector should be created per thread.
    /// Doing otherwise can cause deadlocks/panics.
    #[inline]
    pub fn create_context(&self) -> SimpleCollectorContext {
        let shadow_stack = RefCell::new(ShadowStack(
            Vec::with_capacity(4)
        ));
        unsafe { self.0.pending.add_context(); }
        SimpleCollectorContext(InternalRc::new(RawContext {
            shadow_stack, collector: self.0.clone()
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
struct ShadowStack(Vec<*mut dyn DynTrace>);
impl ShadowStack {
    #[inline]
    pub unsafe fn push<'a, T: Trace + 'a>(&mut self, value: &'a mut T) -> *mut dyn DynTrace {
        let short_ptr = value as &mut (dyn DynTrace + 'a)
            as *mut (dyn DynTrace + 'a);
        let long_ptr = std::mem::transmute::<
            *mut (dyn DynTrace + 'a),
            *mut (dyn DynTrace + 'static)
        >(short_ptr);
        self.0.push(long_ptr);
        long_ptr
    }
    #[inline]
    pub fn pop(&mut self) -> Option<*mut dyn DynTrace> {
        self.0.pop()
    }
}


/// The initial memory usage to start a collection
const INITIAL_COLLECTION_THRESHOLD: usize = 2048;

struct GcHeap {
    #[cfg(feature = "sync")]
    threshold: AtomicUsize,
    #[cfg(not(feature = "sync"))]
    threshold: Cell<usize>,
    allocator: SimpleAlloc
}
impl GcHeap {
    #[cfg(feature = "sync")]
    fn new(threshold: usize) -> GcHeap {
        GcHeap {
            threshold: AtomicUsize::new(threshold),
            allocator: SimpleAlloc::new()
        }
    }
    #[cfg(not(feature = "sync"))]
    fn new(threshold: usize) -> GcHeap {
        GcHeap {
            threshold: Cell::new(threshold),
            allocator: SimpleAlloc::new()
        }
    }
    #[inline]
    #[cfg(feature = "sync")]
    fn should_collect(&self) -> bool {
        /*
         * Going with relaxed ordering because it's not essential
         * that we see updates immediately.
         * Eventual consistency should be enough to eventually
         * trigger a collection.
         */
        self.allocator.allocated_size.load(Ordering::Relaxed)
            >= threshold.load(Ordering::Relaxed)
    }
    #[inline]
    #[cfg(not(feature = "sync"))]
    fn should_collect(&self) -> bool {
        self.allocator.allocated_size() >= self.threshold.get()
    }
}

/// A link in the chain of `BigGcObject`s
type BigObjectLinkItem = Option<NonNull<BigGcObject<DynamicObj>>>;
/// A mutable cell for a BigObjectLink
///
/// This is not thread-safe
#[cfg(not(feature = "sync"))]
#[derive(Default)]
struct BigObjectLink(Cell<BigObjectLinkItem>);
#[cfg(not(feature = "sync"))]
impl BigObjectLink {
    #[inline]
    pub fn new(item: BigObjectLinkItem) -> Self {
        BigObjectLink(Cell::new(item))
    }
    #[inline]
    fn item(&self) -> BigObjectLinkItem {
        self.0.get()
    }
    #[inline]
    unsafe fn set_item_forced(&self, val: BigObjectLinkItem) {
        self.0.set(val)
    }
    #[inline]
    fn append_item(&self, big_obj: Box<BigGcObject>) {
        /*
         * This actually isn't a risk since we're not atomic
         * Just putting it here to clarify its relationship
         * to the atomic CAS loop
         */
        debug_assert_eq!(self.0.get(), big_obj.prev.item());
        self.0.set(unsafe {
            Some(NonNull::new_unchecked(Box::into_raw(big_obj)))
        });
    }
}
/// An atomic cell for a BigObjectLink
///
/// This is thread-safe
#[cfg(feature = "sync")]
#[derive(Default)]
struct BigObjectLink(AtomicCell<BigObjectLinkItem>);
#[cfg(feature = "sync")]
impl BigObjectLink {
    #[inline]
    pub fn new(item: BigObjectLinkItem) -> Self {
        BigObjectLink(AtomicCell::new(item))
    }
    #[inline]
    fn item(&self) -> BigObjectLink {
        self.0.load()
    }
    #[inline]
    unsafe fn set_item_forced(&self, val: BigObjectLinkItem) {
        self.0.store(val)
    }
    #[inline]
    fn append_big_object(&self, big_obj: Box<BigGcObject>) {
        // Must use CAS loop in case another thread updates
        let mut expected_prev = big_obj.prev.item();
        let mut updated_item = unsafe {
            NonNull::new_unchecked(Box::into_raw(obj))
        };
        loop {
            match self.0.compare_exchange(expected_prev, updated_item) {
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

/// The single-threaded implementation of an allocator
#[cfg(not(feature = "sync"))]
pub(crate) struct SimpleAlloc {
    // NOTE: We lazy init this...
    collector_ptr: *mut RawSimpleCollector,
    allocated_size: Cell<usize>,
    small_arenas: SmallArenaList,
    big_object_link: BigObjectLink,
    /// Whether the meaning of the mark bit is currently inverted.
    ///
    /// This flips every collection
    mark_inverted: Cell<bool>,
}

#[cfg(not(feature = "sync"))]
impl SimpleAlloc {
    fn new() -> SimpleAlloc {
        SimpleAlloc {
            collector_ptr: std::ptr::null_mut(),
            allocated_size: Cell::new(0),
            small_arenas: SmallArenaList::new(),
            big_object_link: Default::default(),
            mark_inverted: Cell::new(false)
        }
    }
    #[inline]
    fn allocated_size(&self) -> usize {
        self.allocated_size.get()
    }
    #[inline]
    fn add_allocated_size(&self, amount: usize) {
        let old = self.allocated_size.get();
        self.allocated_size.set(old + amount);
    }
    #[inline]
    fn set_allocated_size(&self, amount: usize) {
        self.allocated_size.set(amount)
    }
    #[inline]
    fn mark_inverted(&self) -> bool {
        self.mark_inverted.get()
    }
    #[inline]
    fn set_mark_inverted(&self, b: bool) {
        self.mark_inverted.set(b)
    }
}

/// The thread-safe implementation of an allocator
///
/// Most allocations should avoid locking.
#[cfg(feature = "sync")]
pub(crate) struct SimpleAlloc {
    collector_ptr: *mut RawSimpleCollector,
    small_arenas: SmallArenaList,
    big_object_link: BigObjectLink,
    mark_inverted: AtomicBool
}
#[cfg(feature = "sync")]
impl SimpleAlloc {
    fn new() -> SimpleAlloc {
        SimpleAlloc {
            collector_ptr: std::ptr::null_mut(),
            allocated_size: AtomicUsize::new(0),
            small_arenas: SmallArenaList::new(),
            big_object_link: AtomicCell::new(None),
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
}
#[cfg(feature = "sync")]
unsafe impl Send for SimpleAlloc {}
/// We're careful to be thread safe here
///
/// This isn't auto implemented because of the
/// raw pointer to the collector (we only use it as an id)
#[cfg(feature = "sync")]
unsafe impl Sync for SimpleAlloc {}
/// Code shared between single-threaded and multi-threaded implementations
impl SimpleAlloc {
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
            arena.for_each(|slot| {
                if (*slot).is_free() {
                    /*
                     * Just ignore this. It's already part of our linked list
                     * of allocated objects.
                     */
                } else {
                    match (*slot).header.raw_state().resolve(self.mark_inverted.get()) {
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

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
#[repr(u8)]
enum PendingState {
    /// We aren't collecting anything
    NotCollecting,
    /// We are waiting for a collection
    Waiting,
    /// A collection is in progress
    InProgress
}
/// Keeps track of a pending collection (if any)
#[cfg(not(feature = "sync"))]
struct PendingCollectionTracker {
    state: Cell<PendingState>,
    /// The roots of the
    ///
    /// Once the size of the root set of the pending collection
    /// is equal to the number of `total_contexts`,
    /// collection can safely begin.
    ///
    /// This is None if no collection is pending
    roots: RefCell<Option<RootSet>>,
    /// The number of active contexts
    ///
    /// Leaking a context is "safe" in the sense
    /// it wont cause undefined behavior,
    /// but it will cause deadlock,
    total_contexts: Cell<usize>
}
#[cfg(not(feature = "sync"))]
impl PendingCollectionTracker {
    fn new() -> PendingCollectionTracker {
        PendingCollectionTracker {
            state: Cell::new(PendingState::NotCollecting),
            total_contexts: Cell::new(0),
            roots: RefCell::new(None)
        }
    }
    #[inline]
    pub fn is_waiting(&self) -> bool {
        self.state.get() == PendingState::Waiting
    }
    unsafe fn add_context(&self) {
        let old = self.total_contexts.get();
        let updated = old.checked_add(1).unwrap();
        self.total_contexts.set(updated);
    }
    unsafe fn free_context(&self) {
        let old = self.total_contexts.get();
        assert_ne!(old, 0);
        self.total_contexts.set(old - 1);
    }
    unsafe fn try_begin_collection(&self) {
        match self.state.get() {
            PendingState::InProgress => unreachable!(),
            PendingState::NotCollecting => {
                self.state.set(PendingState::Waiting);
                let mut roots = self.roots.borrow_mut();
                assert!(roots.is_none());
                *roots = Some(RootSet::new());
            },
            PendingState::Waiting => return, // Already waiting
        }
    }
    /// Push a context that's pending collection
    ///
    /// The collection should've already begun
    ///
    /// Undefined behavior if the context roots are invalid in any way.
    unsafe fn push_pending_context(&self, shadow_stack: &ShadowStack) -> PendingStatus {
        let mut roots = self.roots.borrow_mut();
        let known_shadow_stacks: usize = match *roots {
            None => {
                assert_eq!(self.state.get(), PendingState::NotCollecting);
                // We didn't have anything pending
                return PendingStatus::AlreadyFinished
            },
            Some(ref mut roots) => {
                assert_eq!(self.state.get(), PendingState::Waiting);
                roots.shadow_stacks.push(NonNull::from(shadow_stack));
                roots.shadow_stacks.len()
            }
        };
        use std::cmp::Ordering;
        match known_shadow_stacks.cmp(&self.total_contexts.get()) {
            Ordering::Less => {
                // We should already be waiting
                assert_eq!(self.state.get(), PendingState::Waiting);
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
                    self.state.replace(PendingState::InProgress),
                    PendingState::Waiting,
                );
                PendingStatus::ShouldCollect(roots.take().unwrap())
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
    unsafe fn await_collection(&self) {
        unreachable!("Awaiting is meaningless in a single-threaded context")
    }
    unsafe fn finish_collection(&self) {
        assert_eq!(
            self.state.replace(PendingState::NotCollecting),
            PendingState::InProgress
        );
        assert!(self.roots.borrow().is_none());
    }
}
enum PendingStatus {
    AlreadyFinished,
    ShouldCollect(RootSet),
    NeedToWait
}

#[cfg(feature = "sync")]
struct PendingCollectionTracker {
    state: AtomicCell<PendingState>,
    roots: Mutex<Option<RootSet>>,
    wait: Condvar,
    total_contexts: AtomicUsize,
}
#[cfg(feature = "sync")]
impl PendingCollectionTracker {
    fn new() -> PendingCollectionTracker {
        PendingCollectionTracker {
            state: AtomicCell::new(PendingState::NotCollecting),
            roots: Mutex::new(None),
            wait: Condvar::new(),
            total_contexts: AtomicUsize::new(0)
        }
    }
    #[inline]
    pub fn is_waiting(&self) -> bool {
        // NOTE: This implicitly has Acquire ordering
        self.state.get() == PendingState::Waiting
    }
    unsafe fn add_context(&self) {
        let mut old = self.total_contexts.load(Ordering::Acquire);
        loop {
            let updated = old.checked_add(1).unwrap();
            match self.total_contexts.compare_exchange(old, updated, Ordering::AcqRel) {
                Ok(_) => break,
                Err(new_total) => {
                    old = new_total;
                }
            }
        }
    }
    unsafe fn free_context(&self) {
        let mut total = self.total_contexts.load(Ordering::Acquire);
        loop {
            assert_ne!(total, 0);
            match self.total_contexts.compare_exchange(total, total - 1, Ordering::AcqRel) {
                Ok(_) => break,
                Err(new_total) => {
                    total = new_total;
                }
            }
        }
    }
    unsafe fn try_begin_collection(&self) {
        let roots = self.roots.lock();
        match self.state.compare_and_swap(
            PendingState::NotCollecting,
            PendingState::Waiting
        ) {
            PendingState::InProgress => unreachable!(),
            PendingState::NotCollecting => {
                assert!(roots.is_none());
                *roots = Some(RootSet::new());
            },
            PendingState::Waiting => return, // We were already waiting
        }
    }
    unsafe fn push_pending_context(&self, shadow_stack: &ShadowStack) -> PendingStatus {
        // NOTE: See single-threaded impl
        let mut roots = self.roots.lock();
        // Now that we've acquired the lock, these should be consistent
        assert_eq!(self.is_pending(), roots.is_some());
        let known_shadow_stacks: usize = match roots {
            None => {
                // We didn't have anything pending
                return PendingStatus::AlreadyFinished
            },
            Some(ref mut roots) => {
                roots.shadow_stacks.push(NonNull::from(shadow_stack));
                roots.shadow_stacks.len()
            }
        };
        let total_contexts = self.total_contexts
            .load(std::sync::atomic::Ordering::Acquire);
        use std::cmp::Ordering;
        match known_shadow_stacks.cmp(&total_contexts) {
            Ordering::Less => {
                /*
                 * We're still waiting on some other contexts
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
                assert!(self.is_pending.compare_and_set(true, false, Ordering::SeqCst));
                PendingStatus::ShouldCollect(roots.take())
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
    unsafe fn await_collection(&self) {
        loop {
            let mut lock = self.roots.lock();
            // State should always be consistent
            let state = self.state.load();
            match lock {
                Some(_) => {
                    assert_eq!(state, PendingState::Waiting);
                },
                None => {
                    match state {
                        PendingState::Waiting => unreachable!(),
                        PendingState::NotCollecting => {
                            // We're done :)
                            return
                        },
                        PendingState::InProgress => {
                            // Keep waiting
                        }
                    }
                }
            }
            /*
             * Block until collection finishes
             * Parking lot says there shouldn't be any "spurious"
             * (accidental) wakeups. However I guess it's possible
             * we're woken up somehow in the middle of collection.
             * I'm going to loop just in case :)
             */
            self.wait.wait(&mut lock);
        }
    }
    unsafe fn finish_collection(&self) {
        let mut guard = self.roots.lock();
        assert!(guard.is_none());
        assert_eq!(
            self.state.compare_and_set(
                PendingState::InProgress,
                PendingState::NotCollecting
            ),
            PendingState::InProgress
        );
        drop(guard);
        // Notify all blocked threads
        self.wait.notify_all();
    }
}

/// The internal data for a simple collector
struct RawSimpleCollector {
    pending: PendingCollectionTracker,
    heap: GcHeap,
}
impl RawSimpleCollector {
    #[inline]
    fn should_collect(&self) -> bool {
        self.heap.should_collect() || self.pending.is_waiting()
    }
    #[cold]
    #[inline(never)]
    unsafe fn perform_raw_collection(&self, roots: RootSet) {
        let mut task = CollectionTask {
            expected_collector: self as *const Self as *mut Self,
            roots,
            heap: &self.heap,
            grey_stack: if cfg!(feature = "implicit-grey-stack") {
                Vec::new()
            } else {
                Vec::with_capacity(64)
            }
        };
        task.run();
        self.pending.finish_collection();
    }
}
struct CollectionTask<'a> {
    expected_collector: *mut RawSimpleCollector,
    roots: RootSet,
    heap: &'a GcHeap,
    #[cfg_attr(feature = "implicit-grey-stack", allow(dead_code))]
    grey_stack: Vec<*mut GcHeader>
}
impl<'a> CollectionTask<'a> {
    fn run(&mut self) {
        // Mark
        for root in unsafe { self.roots.iter() } {
            let mut visitor = MarkVisitor {
                expected_collector: self.expected_collector,
                grey_stack: &mut self.grey_stack,
                inverted_mark: self.heap.allocator
                    .mark_inverted.get()
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
        self.heap.threshold.set(updated_size + (updated_size / 2));
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
    collector: InternalRc<RawSimpleCollector>,
    shadow_stack: RefCell<ShadowStack>
}
impl RawContext {
    /// Attempt a collection,
    /// potentially blocking on other threads
    ///
    /// Undefined behavior if mutated during collection
    #[cold]
    #[inline(never)]
    unsafe fn maybe_collect(&self) {
        if !self.collector.pending.is_waiting() {
            self.collector.pending.try_begin_collection();
        }
        let shadow_stack = self.shadow_stack.borrow();
        match self.collector.pending.push_pending_context(&*shadow_stack) {
            PendingStatus::ShouldCollect(roots) => {
                self.collector.perform_raw_collection(roots)
            },
            PendingStatus::NeedToWait => {
                self.collector.pending.await_collection();
            }
            PendingStatus::AlreadyFinished => {} // Done :)
        }
    }
}
impl Drop for RawContext {
    fn drop(&mut self) {
        unsafe { self.collector.pending.free_context() }
    }
}
pub struct SimpleCollectorContext(InternalRc<RawContext>);
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
/// A collector context can only be used on a single thread
///
/// This is true even if were operating in thread-safe mode
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
     */
    #[cfg(not(feature = "sync"))]
    raw_state: Cell<RawMarkState>,
    #[cfg(feature = "sync")] // Atomic stores needed for safety
    raw_state: AtomicCell<RawMarkState>,
}
impl GcHeader {
    #[cfg(not(feature = "sync"))]
    #[inline]
    pub fn new(type_info: &'static GcType, raw_state: RawMarkState) -> Self {
        GcHeader { type_info, raw_state: Cell::new(raw_state) }
    }
    #[cfg(feature = "sync")]
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
}
#[cfg(not(feature = "sync"))]
impl GcHeader {
    #[inline]
    fn raw_state(&self) -> RawMarkState {
        self.raw_state.get()
    }
    #[inline]
    fn update_raw_state(&self, raw_state: RawMarkState) {
        self.raw_state.set(raw_state);
    }
}
#[cfg(feature = "sync")]
impl GcHeader {
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
