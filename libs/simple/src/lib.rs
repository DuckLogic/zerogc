// TODO: Use stable rust
#![feature(
    alloc_layout_extra, // Used for GcObject::from_raw
    never_type, // Used for errors (which are currently impossible)
    negative_impls, // impl !Send is much cleaner than PhantomData<Rc<()>>
    exhaustive_patterns, // Allow exhaustive matching against never
    const_alloc_layout, // Used for StaticType
    const_panic, // Const panic should be stable
    untagged_unions, // Why isn't this stable?
    new_uninit, // Until Rust has const generics, this is how we init arrays..
)]
#![allow(
    /*
     * TODO: Should we be relying on vtable address stability?
     * It seems safe as long as we reuse the same pointer....
     */
    clippy::vtable_address_comparisons,
)]
use std::alloc::Layout;
use std::ptr::NonNull;
use std::os::raw::c_void;
use std::mem::{transmute, ManuallyDrop};
#[cfg(feature = "multiple-collectors")]
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, AtomicBool, Ordering};
#[cfg(not(feature = "multiple-collectors"))]
use std::sync::atomic::AtomicPtr;
#[cfg(not(feature = "multiple-collectors"))]
use std::marker::PhantomData;
use std::any::TypeId;

use slog::{Logger, FnValue, debug};

use zerogc::{GcSafe, Trace, GcVisitor};

use zerogc_context::utils::{ThreadId, AtomicCell, MemorySize};

use crate::alloc::{SmallArenaList, small_object_size};

use zerogc_context::collector::{RawSimpleAlloc};
use zerogc_context::handle::{GcHandleList, RawHandleImpl};
use zerogc_context::{
    CollectionManager as AbstractCollectionManager,
    RawContext as AbstractRawContext
};

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

#[cfg(feature = "sync")]
type RawContext<C> = zerogc_context::state::sync::RawContext<C>;
#[cfg(feature = "sync")]
type CollectionManager<C> = zerogc_context::state::sync::CollectionManager<C>;
#[cfg(not(feature = "sync"))]
type RawContext<C> = zerogc_context::state::nosync::RawContext<C>;
#[cfg(not(feature = "sync"))]
type CollectionManager<C> = zerogc_context::state::nosync::CollectionManager<C>;


pub type SimpleCollector = ::zerogc_context::CollectorRef<RawSimpleCollector>;
pub type SimpleCollectorContext = ::zerogc_context::CollectorContext<RawSimpleCollector>;
pub type CollectorId = ::zerogc_context::CollectorId<RawSimpleCollector>;
pub type Gc<'gc, T> = ::zerogc::Gc<'gc, T, CollectorId>;

#[cfg(not(feature = "multiple-collectors"))]
static GLOBAL_COLLECTOR: AtomicPtr<RawSimpleCollector> = AtomicPtr::new(std::ptr::null_mut());

unsafe impl RawSimpleAlloc for RawSimpleCollector {
    #[inline]
    fn alloc<'gc, T>(context: &'gc SimpleCollectorContext, value: T) -> Gc<'gc, T> where T: GcSafe + 'gc {
        context.collector().heap.allocator.alloc(value)
    }
}

#[doc(hidden)] // NOTE: Needs be public for RawCollectorImpl
pub unsafe trait DynTrace {
    fn trace(&mut self, visitor: &mut MarkVisitor);
}
unsafe impl<T: Trace + ?Sized> DynTrace for T {
    fn trace(&mut self, visitor: &mut MarkVisitor) {
        let Ok(()) = self.visit(visitor);
    }
}

unsafe impl RawHandleImpl for RawSimpleCollector {
    type TypeInfo = GcType;

    #[inline]
    fn type_info_of<T: GcSafe>() -> &'static Self::TypeInfo {
        &<T as StaticGcType>::STATIC_TYPE
    }

    #[inline]
    fn handle_list(&self) -> &GcHandleList<Self> {
        &self.handle_list
    }
}

/// A wrapper for [GcHandleList] that implements [DynTrace]
#[repr(transparent)]
struct GcHandleListWrapper(GcHandleList<RawSimpleCollector>);
unsafe impl DynTrace for GcHandleListWrapper {
    fn trace(&mut self, visitor: &mut MarkVisitor) {
        unsafe {
            let Ok(()) = self.0.trace::<_, !>(|raw_ptr, type_info| {
                let header = &mut *GcHeader::from_value_ptr(raw_ptr, type_info);
                // Mark grey
                header.update_raw_state(MarkState::Grey.
                    to_raw(visitor.inverted_mark));
                // Visit innards
                (type_info.trace_func)(header.value(), visitor);
                // Mark black
                header.update_raw_state(MarkState::Black.
                    to_raw(visitor.inverted_mark));
                Ok(())
            });
        }
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
    collector_id: Option<CollectorId>,
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
            collector_id: None,
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
                let collector_id = match self.collector_id {
                    Some(collector) => collector,
                    None => {
                        #[cfg(debug_assertions)] {
                            unreachable!("Invalid collector id")
                        }
                        #[cfg(not(debug_assertions))] {
                            std::hint::unreachable_unchecked()
                        }
                    }
                };
                header.as_ptr().write(GcHeader::new(
                    T::STATIC_TYPE,
                    MarkState::White.to_raw(self.mark_inverted()),
                    collector_id
                ));
                let value_ptr = header.as_ref().value().cast::<T>();
                value_ptr.write(value);
                self.add_allocated_size(small_object_size::<T>());
                Gc::from_raw(
                    collector_id,
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
                MarkState::White.to_raw(self.mark_inverted()),
                self.collector_id.unwrap()
            ),
            static_value: ManuallyDrop::new(value),
            prev: BigObjectLink::new(self.big_object_link.item()),
        });
        let gc = unsafe { Gc::from_raw(
            self.collector_id.unwrap(),
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
#[doc(hidden)]
pub struct RawSimpleCollector {
    logger: Logger,
    heap: GcHeap,
    manager: CollectionManager<Self>,
    /// Tracks object handles
    handle_list: GcHandleList<Self>,
}

unsafe impl ::zerogc_context::collector::RawCollectorImpl for RawSimpleCollector {
    type GcDynPointer = NonNull<dyn DynTrace>;

    #[cfg(feature = "multiple-collectors")]
    type Ptr = NonNull<Self>;

    #[cfg(not(feature = "multiple-collectors"))]
    type Ptr = PhantomData<&'static Self>;

    type Manager = CollectionManager<Self>;

    type RawContext = RawContext<Self>;

    const SINGLETON: bool = cfg!(not(feature = "multiple-collectors"));

    const SYNC: bool = cfg!(feature = "sync");

    #[inline]
    unsafe fn create_dyn_pointer<T: Trace>(value: *mut T) -> Self::GcDynPointer {
        debug_assert!(!value.is_null());
        NonNull::new_unchecked(
            std::mem::transmute::<
                *mut dyn DynTrace,
                *mut (dyn DynTrace + 'static)
            >(value as *mut dyn DynTrace)
        )
    }

    #[cfg(not(feature = "multiple-collectors"))]
    fn init(_logger: Logger) -> NonNull<Self> {
        panic!("Not a singleton")
    }

    #[cfg(feature = "multiple-collectors")]
    fn init(logger: Logger) -> NonNull<Self> {
        // We're assuming its safe to create multiple (as given by config)
        let raw = unsafe { RawSimpleCollector::with_logger(logger) };
        let mut collector = Arc::new(raw);
        let raw_ptr = unsafe { NonNull::new_unchecked(
            Arc::as_ptr(&collector) as *mut RawSimpleCollector
        ) };
        Arc::get_mut(&mut collector).unwrap()
            .heap.allocator.collector_id = Some(unsafe { CollectorId::from_raw(raw_ptr) });
        std::mem::forget(collector); // We own it as a raw pointer...
        raw_ptr
    }

    #[inline(always)]
    unsafe fn gc_write_barrier<'gc, T, V>(
        _owner: &Gc<'gc, T>,
        _value: &Gc<'gc, V>,
        _field_offset: usize
    ) where T: GcSafe + ?Sized + 'gc, V: GcSafe + ?Sized + 'gc {
        // Simple GC doesn't need write barriers
    }
    #[inline]
    fn logger(&self) -> &Logger {
        &self.logger
    }
    #[inline]
    fn manager(&self) -> &CollectionManager<Self> {
        &self.manager
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
    #[inline]
    fn allocated_size(&self) -> MemorySize {
        MemorySize { bytes: self.heap.allocator.allocated_size() }
    }
    #[inline]
    unsafe fn perform_raw_collection(&self, contexts: &[*mut RawContext<Self>]) {
        self.perform_raw_collection(contexts)
    }
}
#[cfg(feature = "sync")]
unsafe impl ::zerogc_context::collector::SyncCollector for RawSimpleCollector {}
#[cfg(not(feature = "multiple-collectors"))]
unsafe impl zerogc_context::collector::SingletonCollector for RawSimpleCollector {
    #[inline]
    fn global_ptr() -> *const Self {
        GLOBAL_COLLECTOR.load(Ordering::Acquire)
    }

    fn init_global(logger: Logger) {
        let marker_ptr = NonNull::dangling().as_ptr();
        /*
         * There can only be one collector (due to configuration).
         *
         * Exchange with marker pointer while we're initializing.
         */
        assert_eq!(GLOBAL_COLLECTOR.compare_exchange(
            std::ptr::null_mut(), marker_ptr,
            Ordering::SeqCst, Ordering::SeqCst
        ), Ok(std::ptr::null_mut()), "Collector already exists");
        let mut raw = Box::new(
            unsafe { RawSimpleCollector::with_logger(logger) }
        );
        const GLOBAL_ID: CollectorId = unsafe { CollectorId::from_raw(PhantomData) };
        raw.heap.allocator.collector_id = Some(GLOBAL_ID);
        // It shall reign forever!
        let raw = Box::leak(raw);
        assert_eq!(
            GLOBAL_COLLECTOR.compare_exchange(
                marker_ptr, raw as *mut RawSimpleCollector,
                Ordering::SeqCst, Ordering::SeqCst
            ),
            Ok(marker_ptr), "Unexpected modification"
        );
    }
}
impl RawSimpleCollector {
    unsafe fn with_logger(logger: Logger) -> Self {
        RawSimpleCollector {
            logger,
            manager: CollectionManager::new(),
            heap: GcHeap::new(INITIAL_COLLECTION_THRESHOLD),
            handle_list: GcHandleList::new(),
        }
    }
    #[cold]
    #[inline(never)]
    unsafe fn perform_raw_collection(
        &self, contexts: &[*mut RawContext<RawSimpleCollector>]
    ) {
        debug_assert!(self.manager.is_collecting());
        let roots: Vec<*mut dyn DynTrace> = contexts.iter()
            .flat_map(|ctx| {
                (**ctx).assume_valid_shadow_stack()
                    .reverse_iter().map(NonNull::as_ptr)
            })
            .chain(std::iter::once(&self.handle_list
                // Cast to wrapper type
                as *const GcHandleList<Self> as *const GcHandleListWrapper
                // Make into virtual pointer
                as *const dyn DynTrace
                as *mut dyn DynTrace
            ))
            .collect();
        let num_roots = roots.len();
        let mut task = CollectionTask {
            expected_collector: self.heap.allocator.collector_id.unwrap(),
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
            "original_size" => %MemorySize { bytes: original_size },
            "memory_freed" => %MemorySize { bytes: original_size - updated_size },
        );
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

#[doc(hidden)] // NOTE: Needs be public for RawCollectorImpl
pub struct MarkVisitor<'a> {
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

    #[inline]
    unsafe fn visit_gc<'gc, T, Id>(
        &mut self, gc: &mut ::zerogc::Gc<'gc, T, Id>
    ) -> Result<(), Self::Err>
        where T: GcSafe + 'gc, Id: ::zerogc::CollectorId {
        if TypeId::of::<Id>() == TypeId::of::<crate::CollectorId>() {
            /*
             * Since the `TypeId`s match, we know the generic `Id`
             * matches our own `crate::CollectorId`.
             * Therefore its safe to specific the generic `Id` into the
             * `Gc` into its more specific type.
             */
            let gc = std::mem::transmute::<
                &mut ::zerogc::Gc<'gc, T, Id>,
                &mut ::zerogc::Gc<'gc, T, crate::CollectorId>
            >(gc);
            /*
             * Check the collectors match. Otherwise we're mutating
             * other people's data.
             */
            assert_eq!(gc.collector_id(), self.expected_collector);
            self._visit_own_gc(gc);
            Ok(())
        } else {
            // Just ignore
            Ok(())
        }
    }
}
impl MarkVisitor<'_> {
    /// Visit a GC type whose [::zerogc::CollectorId] matches our own
    ///
    /// The caller should only use `GcVisitor::visit_gc()`
    unsafe fn _visit_own_gc<'gc, T: GcSafe + 'gc>(&mut self, gc: &mut Gc<'gc, T>) {
        // Verify this again (should be checked by caller)
        debug_assert_eq!(gc.collector_id(), self.expected_collector);
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

#[doc(hidden)] // NOTE: Needed for GcHandleImpl
pub struct GcType {
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
    collector_id: CollectorId
}
impl GcHeader {
    #[inline]
    pub fn new(type_info: &'static GcType, raw_state: RawMarkState, collector_id: CollectorId) -> Self {
        GcHeader { type_info, raw_state: AtomicCell::new(raw_state), collector_id }
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
