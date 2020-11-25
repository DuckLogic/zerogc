// TODO: Use stable rust
#![feature(
    alloc_layout_extra, // Used for GcObject::from_raw
    never_type, // Used for errors (which are currently impossible)
    negative_impls, // impl !Send is much cleaner than PhantomData<Rc<()>>
    exhaustive_patterns, // Allow exhaustive matching against never
    const_alloc_layout, // Used for StaticType
    const_fn, // We statically create type info
    const_panic, // Const panic should be stable
    untagged_unions, // Why isn't this stable?
    new_uninit, // Until Rust has const generics, this is how we init arrays..
    min_specialization, // Effectively required by GcRef :(
)]
#![allow(
    /*
     * TODO: Should we be relying on vtable address stability?
     * It seems safe as long as we reuse the same pointer....
     */
    clippy::vtable_address_comparisons,
)]
use zerogc::{GcSystem, GcSafe, Trace, GcVisitor, GcSimpleAlloc, GcRef, GcBrand, GcDirectBarrier, GcCreateHandle};
use std::alloc::Layout;
use std::ptr::NonNull;
use std::os::raw::c_void;
use std::mem::{transmute, ManuallyDrop};
use crate::alloc::{SmallArenaList, small_object_size};
use std::ops::Deref;
use std::hash::{Hash, Hasher};
use std::fmt::{Debug, Formatter};
use std::fmt;
use std::marker::PhantomData;
#[cfg(feature = "multiple-collectors")]
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, AtomicBool, Ordering};
#[cfg(not(feature = "multiple-collectors"))]
use std::sync::atomic::AtomicPtr;
#[cfg(not(feature = "sync"))]
use std::cell::Cell;


use slog::{Logger, FnValue, o, debug};
use crate::context::{RawContext};
use crate::utils::ThreadId;

pub use crate::context::SimpleCollectorContext;
use handles::GcHandleList;

#[cfg(feature = "sync")]
type AtomicCell<T> = ::crossbeam::atomic::AtomicCell<T>;
/// Fallback `AtomicCell` implementation when we actually
/// don't care about thread safety
#[cfg(not(feature = "sync"))]
#[derive(Default)]
struct AtomicCell<T>(Cell<T>);
#[cfg(not(feature = "sync"))]
impl<T: Copy> AtomicCell<T> {
    const fn new(value: T) -> Self {
        AtomicCell(Cell::new(value))
    }
    fn store(&self, value: T) {
        self.0.set(value)
    }
    fn load(&self) -> T {
        self.0.get()
    }
    fn compare_exchange(&self, expected: T, updated: T) -> Result<T, T>
        where T: PartialEq {
        let existing = self.0.get();
        if existing == expected {
            self.0.set(updated);
            Ok(existing)
        } else {
            Err(existing)
        }
    }
}

mod handles;
mod context;
mod utils;
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

pub use handles::GcHandle;

/// Uniquely identifies the collector in case there are
/// multiple collectors.
///
/// If there are multiple collectors `cfg!(feature="multiple-collectors")`,
/// we need to use a pointer to tell them apart.
/// Otherwise, this is a zero-sized structure.
///
/// As long as our memory is valid,
/// it implies this pointer is too..
#[derive(Copy, Clone, Debug, PartialEq)]
#[repr(C)]
struct CollectorId {
    /*
     * TODO: Store a pointer to the Arc to enable clones
     * without having to abuse Arc::from_raw
     */
    #[cfg(feature = "multiple-collectors")]
    ptr: NonNull<RawSimpleCollector>,
}
impl CollectorId {
    #[cfg(feature = "multiple-collectors")]
    pub unsafe fn as_ref(&self) -> &RawSimpleCollector {
        self.ptr.as_ref()
    }
    #[cfg(not(feature = "multiple-collectors"))]
    pub unsafe fn as_ref(&self) -> &RawSimpleCollector {
        &*GLOBAL_COLLECTOR.load(Ordering::Acquire)
    }
    #[cfg(feature = "multiple-collectors")]
    pub unsafe fn weak_ref(&self) -> WeakCollectorRef {
        // TODO: This is a horrible hack
        let arc = Arc::from_raw(
            self.ptr.as_ptr()
        );
        let weak = Arc::downgrade(&arc);
        std::mem::forget(arc);
        WeakCollectorRef { weak }
    }
    #[cfg(not(feature = "multiple-collectors"))]
    pub unsafe fn weak_ref(&self) -> WeakCollectorRef {
        WeakCollectorRef {}
    }
}

struct WeakCollectorRef {
    #[cfg(feature = "multiple-collectors")]
    weak: std::sync::Weak<RawSimpleCollector>,
}
impl WeakCollectorRef {
    #[cfg(feature = "multiple-collectors")]
    pub unsafe fn assume_valid(&self) -> CollectorId {
        debug_assert!(
            self.weak.upgrade().is_some(),
            "Dead collector"
        );
        CollectorId {
            ptr: NonNull::new_unchecked(self.weak.as_ptr() as *mut _)
        }
    }
    #[cfg(not(feature = "multiple-collectors"))]
    pub unsafe fn assume_valid(&self) -> CollectorId {
        CollectorId {}
    }
    pub fn ensure_valid<R>(&self, func: impl FnOnce(CollectorId) -> R) -> R {
        self.try_ensure_valid(|id| match id{
            Some(id) => func(id),
            None => panic!("Dead collector")
        })
    }
    #[cfg(feature = "multiple-collectors")]
    pub fn try_ensure_valid<R>(&self, func: impl FnOnce(Option<CollectorId>) -> R) -> R{
        let rc = self.weak.upgrade();
        func(match rc {
            Some(ref rc) => {
                Some(CollectorId { ptr: NonNull::from(&**rc) })
            },
            None => None
        })
    }
    #[cfg(not(feature = "multiple-collectors"))]
    pub fn try_ensure_valid<R>(&self, func: impl FnOnce(Option<CollectorId>) -> R) -> R {
        // global collector is always valid
        func(Some(CollectorId {}))
    }
}

/// A garbage collected pointer
///
/// See docs for [zerogc::GcRef]
#[repr(C)]
pub struct Gc<'gc, T: GcSafe + 'gc> {
    /// Used to uniquely identify the collector,
    /// to ensure we aren't modifying another collector's pointers
    collector_id: CollectorId,
    // TODO: Field-ordering wise I feel this should come first
    value: NonNull<T>,
    marker: PhantomData<&'gc T>
}
impl<'gc, T: GcSafe + 'gc> Gc<'gc, T> {
    #[inline]
    pub(crate) unsafe fn from_raw(
        id: CollectorId,
        value: NonNull<T>
    ) -> Self {
        Gc { collector_id: id, value, marker: PhantomData }
    }
}
unsafe impl<'gc, T: GcSafe + 'gc> GcRef<'gc, T> for Gc<'gc, T> {
    type System = SimpleCollector;

    #[inline]
    fn value(&self) -> &'gc T {
        unsafe { &mut *self.value.as_ptr() }
    }
}
/// We support handles
unsafe impl<'gc, T> GcCreateHandle<'gc, T> for Gc<'gc, T>
    where T: GcSafe, T: GcBrand<'static, SimpleCollector>,
        T::Branded: GcSafe {
    type Handle = handles::GcHandle<T::Branded>;
    #[inline]
    fn create_handle(&self) -> Self::Handle {
        unsafe {
            let collector = self.collector_id;
            let value = self.value.as_ptr();
            let raw = collector.as_ref().handle_list
                .alloc_raw_handle(value as *mut ());
            /*
             * WARN: Undefined Behavior
             * if we don't finish initializing
             * the handle!!!
             *
             * TODO: Encapsulate
             */
            raw.type_info.store(
                <T as StaticGcType>::STATIC_TYPE
                    as *const GcType
                    as *mut GcType,
                Ordering::Release
            );
            raw.refcnt.store(1, Ordering::Release);
            let weak_collector = self.collector_id.weak_ref();
            GcHandle::new(NonNull::from(raw), weak_collector)
        }
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
                .field("collector_id", &self.collector_id)
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

#[cfg(not(feature = "multiple-collectors"))]
static GLOBAL_COLLECTOR: AtomicPtr<RawSimpleCollector> = AtomicPtr::new(std::ptr::null_mut());

pub struct SimpleCollector {
    #[cfg(feature = "multiple-collectors")]
    rc: Arc<RawSimpleCollector>
}

impl SimpleCollector {
    pub fn create() -> Self {
        SimpleCollector::with_logger(Logger::root(
            slog::Discard,
            o!()
        ))
    }
    #[cfg(not(feature = "multiple-collectors"))]
    pub fn with_logger(logger: Logger) -> Self {
        let marker_ptr = NonNull::dangling().as_ptr();
        /*
         * There can only be one collector (due to configuration).
         *
         * Exchange with marker pointer while we're initializing.
         */
        assert!(GLOBAL_COLLECTOR.compare_and_swap(
            std::ptr::null_mut(), marker_ptr,
            Ordering::SeqCst
        ).is_null(), "Collector already exists");
        let raw = Box::new(
            unsafe { RawSimpleCollector::with_logger(logger) }
        );
        // It shall reign forever!
        let raw = Box::leak(raw);
        assert_eq!(
            GLOBAL_COLLECTOR.compare_and_swap(
                marker_ptr, raw as *mut RawSimpleCollector,
                Ordering::SeqCst
            ),
            marker_ptr, "Unexpected modification"
        );
        // NOTE: The raw pointer is implicit (now that we're leaked)
        SimpleCollector {}
    }

    #[cfg(feature = "multiple-collectors")]
    pub fn with_logger(logger: Logger) -> Self {
        // We're assuming its safe to create multiple (as given by config)
        let raw = unsafe { RawSimpleCollector::with_logger(logger) };
        let mut collector = Arc::new(raw);
        let collector_ptr = &*collector
            as *const _
            as *mut RawSimpleCollector;
        Arc::get_mut(&mut collector).unwrap()
            .heap.allocator.collector_ptr = collector_ptr;
        SimpleCollector { rc: collector }
    }
    #[cfg(feature = "multiple-collectors")]
    fn clone_internal(&self) -> SimpleCollector {
        SimpleCollector { rc: self.rc.clone() }
    }
    #[cfg(not(feature = "multiple-collectors"))]
    fn clone_internal(&self) -> SimpleCollector {
        SimpleCollector {}
    }
    #[cfg(feature = "multiple-collectors")]
    fn as_raw(&self) -> &RawSimpleCollector {
        &*self.rc
    }
    #[cfg(not(feature = "multiple-collectors"))]
    fn as_raw(&self) -> &RawSimpleCollector {
        let ptr = GLOBAL_COLLECTOR.load(Ordering::Acquire);
        assert!(!ptr.is_null());
        unsafe { &*ptr }
    }

    /// Create a new context bound to this collector
    ///
    /// Warning: Only one collector should be created per thread.
    /// Doing otherwise can cause deadlocks/panics.
    #[cfg(feature = "sync")]
    pub fn create_context(&self) -> SimpleCollectorContext {
        unsafe { SimpleCollectorContext::register_root(&self) }
    }
    /// Convert this collector into a unique context
    ///
    /// The single-threaded implementation only allows a single context,
    /// so this method is nessicarry to support it.
    pub fn into_context(self) -> SimpleCollectorContext {
        #[cfg(feature = "sync")]
            { self.create_context() }
        #[cfg(not(feature = "sync"))]
        unsafe { SimpleCollectorContext::from_collector(&self) }
    }
}

unsafe impl GcSystem for SimpleCollector {}

unsafe impl<'gc, T: GcSafe + 'gc> GcSimpleAlloc<'gc, T> for SimpleCollectorContext {
    type Ref = Gc<'gc, T>;

    #[inline]
    fn alloc(&'gc self, value: T) -> Gc<'gc, T> {
        self.collector().heap.allocator.alloc(value)
    }
}

unsafe trait DynTrace {
    fn trace(&mut self, visitor: &mut MarkVisitor);
}
unsafe impl<T: Trace + ?Sized> DynTrace for T {
    fn trace(&mut self, visitor: &mut MarkVisitor) {
        let Ok(()) = self.visit(visitor);
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
    fn should_collect_relaxed(&self) -> bool {
        /*
         * Going with relaxed ordering because it's not essential
         * that we see updates immediately.
         * Eventual consistency should be enough to eventually
         * trigger a collection.
         *
         * This is much cheaper on ARM (since it avoids a fence)
         * and is much easier to use with a JIT.
         * A JIT will insert these very often, so it's important to make
         * them fast!
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
    pub const fn new(item: BigObjectLinkItem) -> Self {
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
                    (*self.collector_ptr).unchecked_id(),
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
            (*self.collector_ptr).unchecked_id(),
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
    unsafe fn sweep(&self) {
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

/// We're careful - I swear :D
unsafe impl Send for RawSimpleCollector {}
unsafe impl Sync for RawSimpleCollector {}

/// The internal data for a simple collector
struct RawSimpleCollector {
    logger: Logger,
    heap: GcHeap,
    manager: self::context::CollectionManager,
    /// Tracks object handles
    handle_list: GcHandleList
}
impl RawSimpleCollector {
    unsafe fn with_logger(logger: Logger) -> Self {
        RawSimpleCollector {
            logger,
            manager: context::CollectionManager::new(),
            heap: GcHeap::new(INITIAL_COLLECTION_THRESHOLD),
            handle_list: GcHandleList::new(),
        }
    }
    #[inline]
    unsafe fn unchecked_id(&self) -> CollectorId {
        #[cfg(feature = "multiple-collectors")] {
            CollectorId {
                ptr: NonNull::from(self)
            }
        }
        #[cfg(not(feature = "multiple-collectors"))] {
            CollectorId {}
        }
    }
    #[inline]
    fn should_collect(&self) -> bool {
        /*
         * Use relaxed ordering, just like `should_collect`
         *
         * This is faster on ARM since it avoids a memory fence.
         * More importantly this is easier for a JIT to implement inline!!!
         * As of this writing cranelift doesn't even seem to support fences :o
         */
        self.heap.should_collect_relaxed() ||
            self.manager.should_trigger_collection()
    }
    #[cold]
    #[inline(never)]
    unsafe fn perform_raw_collection(
        &self, contexts: &[*mut RawContext]
    ) {
        debug_assert!(self.manager.is_collecting());
        let roots: Vec<*mut dyn DynTrace> = contexts.iter()
            .flat_map(|ctx| {
                (**ctx).assume_valid_shadow_stack()
                    .reverse_iter()
            })
            .chain(std::iter::once(&self.handle_list
                as *const GcHandleList as *const dyn DynTrace
                as *mut dyn DynTrace
            ))
            .collect();
        let num_roots = roots.len();
        let mut task = CollectionTask {
            expected_collector: self.unchecked_id(),
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
impl Deref for RawSimpleCollector {
    type Target = context::CollectionManager;

    #[inline]
    fn deref(&self) -> &Self::Target {
        &self.manager // TODO: Do this explicitly
    }
}
struct CollectionTask<'a> {
    expected_collector: CollectorId,
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
        #[cfg(not(feature = "implicit-grey-stack"))] unsafe {
            let was_inverted_mark = self.heap.allocator.mark_inverted();
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
    expected_collector: CollectorId,
    #[cfg_attr(feature = "implicit-grey-stack", allow(dead_code))]
    grey_stack: &'a mut Vec<*mut GcHeader>,
    /// If this meaning of the mark bit is currently inverted
    ///
    /// This flips every collection
    inverted_mark: bool
}
impl<'a> MarkVisitor<'a> {
    fn visit_raw_gc(
        &mut self, obj: &mut GcHeader,
        trace_func: impl FnOnce(&mut GcHeader, &mut MarkVisitor<'a>)
    ) {
        match obj.raw_state().resolve(self.inverted_mark) {
            MarkState::White => trace_func(obj, self),
            MarkState::Grey => {
                /*
                 * We've already pushed this object onto the gray stack
                 * It will be traversed eventually, so we don't need to do anything.
                 */
            },
            MarkState::Black => {
                /*
                 * We've already traversed this object.
                 * It's already known to be reachable
                 */
            },
        }
    }
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
        assert_eq!(gc.collector_id, self.expected_collector);
        unsafe {
            let header = GcHeader::from_value_ptr(
                gc.as_raw_ptr(),
                T::STATIC_TYPE
            );
            self.visit_raw_gc(&mut *header, |obj, visitor| {
                let inverted_mark = visitor.inverted_mark;
                if !T::NEEDS_TRACE {
                    /*
                     * We don't need to mark this grey
                     * It has no internals that need to be traced.
                     * We can directly move it directly to the black set
                     */
                    obj.update_raw_state(MarkState::Black.to_raw(inverted_mark));
                } else {
                    /*
                     * We need to mark this object grey and push it onto the grey stack.
                     * It will be processed later
                     */
                    (*obj).update_raw_state(MarkState::Grey.to_raw(inverted_mark));
                    #[cfg(not(feature = "implicit-grey-stack"))] {
                        visitor.grey_stack.push(obj as *mut GcHeader);
                    }
                    #[cfg(feature = "implicit-grey-stack")] {
                        /*
                         * The user wants an implicit grey stack using
                         * recursion. This risks stack overflow but can
                         * boost performance (See 9a9634d68a4933d).
                         * On some workloads this is fine.
                         */
                        T::trace(
                            &mut *((*obj).value() as *mut T),
                            visitor
                        );
                        /*
                         * Mark the object black now it's innards have been traced
                         * NOTE: We do **not** do this with an implicit stack.
                         */
                        (*obj).update_raw_state(MarkState::Black.to_raw(
                            inverted_mark
                        ));
                    }
                }
            });
        }
    }
}

struct GcType {
    value_size: usize,
    value_offset: usize,
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
    pub unsafe fn from_value_ptr<T>(ptr: *mut T, static_type: &GcType) -> *mut GcHeader {
        (ptr as *mut u8).sub(static_type.value_offset).cast()
    }
    #[inline]
    fn raw_state(&self) -> RawMarkState {
        // TODO: Is this safe? Couldn't it be accessed concurrently?
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
