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
    ptr_metadata, // Needed to abstract over Sized/unsized types
)]
#![allow(
    /*
     * TODO: Should we be relying on vtable address stability?
     * It seems safe as long as we reuse the same pointer....
     */
    clippy::vtable_address_comparisons,
)]
use std::alloc::Layout;
use std::ptr::{NonNull, Pointee};
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

use zerogc::{GcSafe, Trace, GcVisitor, NullTrace};

use zerogc_context::utils::{ThreadId, AtomicCell, MemorySize};

use crate::alloc::{SmallArenaList, small_object_size};

use zerogc_context::collector::{RawSimpleAlloc, RawCollectorImpl};
use zerogc_context::handle::{GcHandleList, RawHandleImpl};
use zerogc_context::{
    CollectionManager as AbstractCollectionManager,
    RawContext as AbstractRawContext
};
use crate::layout::{BigGcObject, SimpleMarkData, SimpleMarkDataSnapshot};
use std::ops::{DerefMut, Deref};
use std::borrow::BorrowMut;
use zerogc::format::{ObjectFormat, GcTypeInfo, OpenAllocObjectFormat, GcLayoutInternals};
use zerogc::format::simple::SimpleObjectFormat;
use std::fmt::{Pointer, Debug};

/// A 'dummy' gc header type used for small allocations, regardless of the actual object format.
///
/// It indicates that we support a word aligned header whose size
/// is at most two words.
#[derive(Default)]
pub(crate) struct DummyGcHeader {
    _first: usize,
    _second: usize
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
mod layout;

#[cfg(feature = "sync")]
type RawContext<C> = zerogc_context::state::sync::RawContext<C>;
#[cfg(feature = "sync")]
type CollectionManager<C> = zerogc_context::state::sync::CollectionManager<C>;
#[cfg(not(feature = "sync"))]
type RawContext<C> = zerogc_context::state::nosync::RawContext<C>;
#[cfg(not(feature = "sync"))]
type CollectionManager<C> = zerogc_context::state::nosync::CollectionManager<C>;


type DefaultFmt = SimpleObjectFormat;
pub type SimpleCollector<Fmt = DefaultFmt> = ::zerogc_context::CollectorRef<RawSimpleCollector<Fmt>>;
pub type SimpleCollectorContext<Fmt = DefaultFmt> = ::zerogc_context::CollectorContext<RawSimpleCollector<Fmt>>;
pub type CollectorId<Fmt = DefaultFmt> = ::zerogc_context::CollectorId<RawSimpleCollector<Fmt>>;
pub type Gc<'gc, T, Fmt = DefaultFmt> = ::zerogc::Gc<'gc, T, CollectorId<Fmt>>;

#[cfg(not(feature = "multiple-collectors"))]
static GLOBAL_COLLECTOR: AtomicPtr<RawSimpleCollector<DefaultFmt>> = AtomicPtr::new(std::ptr::null_mut());

unsafe impl<Fmt: RawObjectFormat> RawSimpleAlloc for RawSimpleCollector<Fmt> {
    #[inline]
    fn alloc<'gc, T>(context: &'gc SimpleCollectorContext<Fmt>, value: T) -> Gc<'gc, T, Fmt> where T: GcSafe + 'gc {
        context.collector().heap.allocator.alloc(value)
    }
}

unsafe impl<Fmt: RawObjectFormat> RawHandleImpl for RawSimpleCollector<Fmt> {
    type TypeInfo = Fmt::TypeInfo;

    #[inline]
    fn type_info_for_val<T: GcSafe>(&self, _val: &T) -> Fmt::TypeInfo {
        self.format.sized_type::<T>()
    }

    #[inline]
    fn handle_list(&self) -> &GcHandleList<Self> {
        &self.handle_list
    }
}

/// A wrapper for [GcHandleList] that implements [DynTrace]
#[repr(transparent)]
struct GcHandleListWrapper<Fmt: RawObjectFormat>(GcHandleList<RawSimpleCollector<Fmt>>);
unsafe impl<Fmt: RawObjectFormat> Trace for GcHandleListWrapper<Fmt> {
    const NEEDS_TRACE: bool = true;

    fn visit<V: GcVisitor>(&mut self, visitor: &mut V) -> Result<(), V::Err> {
        unsafe {
            assert_eq!(
                TypeId::of::<V>(),
                TypeId::of::<MarkVisitor<'static, Fmt>>(),
            );
            let visitor = &mut *(visitor as *mut V as *mut MarkVisitor<'static, Fmt>);
            self.0.trace(|raw_ptr, type_info| {
                let type_info = Fmt::determine_type(raw_ptr);
                let mark_data = Fmt::mark_data_ptr(raw_ptr, type_info);
                let mark_data = &*(mark_data as *const SimpleMarkData<Fmt>);
                // Mark grey
                mark_data.update_raw_state(MarkState::Grey.
                    to_raw(visitor.inverted_mark));
                // Visit innards
                type_info.trace(raw_ptr, visitor);
                // Mark black
                mark_data.update_raw_state(MarkState::Black.
                    to_raw(visitor.inverted_mark));
                Ok(())
            })
        }
    }
}


/// The initial memory usage to start a collection
const INITIAL_COLLECTION_THRESHOLD: usize = 2048;

struct GcHeap<Fmt: RawObjectFormat> {
    threshold: AtomicUsize,
    allocator: SimpleAlloc<Fmt>
}
impl<Fmt: RawObjectFormat> GcHeap<Fmt> {
    fn new(format: &'static Fmt, threshold: usize) -> Self {
        GcHeap {
            threshold: AtomicUsize::new(threshold),
            allocator: SimpleAlloc::new(format)
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

/// The abstract specification of a [Lock],
/// shared between thread-safe and thread-unsafe code
trait ILock<'a, T> {
    type Guard: Sized + Deref<Target=T> + DerefMut<Target=T> + 'a;
    fn lock(&'a self) -> Self::Guard;
    fn get_mut(&'a mut self) -> &'a mut T;
}

#[cfg(feature = "sync")]
struct Lock<T>(::parking_lot::Mutex<T>);
#[cfg(feature = "sync")]
impl<T> From<T> for Lock<T> {
    fn from(val: T) -> Self {
        Lock(::parking_lot::Mutex::new(val))
    }
}
#[cfg(feature = "sync")]
impl<'a, T: 'a> ILock<'a, T> for Lock<T> {
    type Guard = ::parking_lot::MutexGuard<'a, T>;

    #[inline]
    fn lock(&'a self) -> Self::Guard {
        self.0.lock()
    }

    #[inline]
    fn get_mut(&'a mut self) -> &'a mut T {
        self.0.get_mut()
    }
}

#[cfg(not(feature = "sync"))]
struct Lock<T>(::std::cell::RefCell<T>);
#[cfg(not(feature = "sync"))]
impl<T> From<T> for Lock<T> {
    fn from(val: T) -> Self {
        Lock(::std::cell::RefCell::new(val))
    }
}
#[cfg(not(feature = "sync"))]
impl<'a, T> ILock<'a, T> for Lock<T> {
    type Guard = ::std::cell::RefMut<'a, T>;

    #[inline]
    fn lock(&self) -> Self::Guard {
        self.0.borrow_mut()
    }

    #[inline]
    fn get_mut(&'a mut self) -> &'a mut T {
        self.0.get_mut()
    }
}

/// The thread-safe implementation of an allocator
///
/// Most allocations should avoid locking.
pub(crate) struct SimpleAlloc<Fmt: RawObjectFormat> {
    collector_id: Option<CollectorId<Fmt>>,
    small_arenas: SmallArenaList<Fmt>,
    big_objects: Lock<Vec<Box<BigGcObject<Fmt>>>>,
    /// Whether the meaning of the mark bit is currently inverted.
    ///
    /// This flips every collection
    mark_inverted: AtomicBool,
    allocated_size: AtomicUsize,
    format: &'static Fmt
}
impl<Fmt: RawObjectFormat> SimpleAlloc<Fmt> {
    fn new(format: &'static Fmt) -> Self {
        SimpleAlloc {
            collector_id: None,
            allocated_size: AtomicUsize::new(0),
            small_arenas: SmallArenaList::new(),
            big_objects: Lock::from(Vec::new()),
            mark_inverted: AtomicBool::new(false),
            format
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
    fn cast_header(dummy: *mut DummyGcHeader) -> *mut Fmt::SizedHeaderType {
        assert!(std::mem::size_of::<DummyGcHeader>() >= std::mem::size_of::<Fmt::SizedHeaderType>());
        assert!(std::mem::align_of::<DummyGcHeader>() >= std::mem::align_of::<Fmt::SizedHeaderType>());
        dummy.cast()
    }

    #[inline]
    fn alloc<T: GcSafe>(&self, value: T) -> Gc<'_, T, Fmt> {
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
                let value_ptr = self.format.write_sized_header::<T>(
                    Self::cast_header(header.as_ptr()),
                    SimpleMarkData::from_snapshot(SimpleMarkDataSnapshot::new(
                        MarkState::White.to_raw(self.mark_inverted()),
                        collector_id,
                    ))
                );
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
    fn alloc_big<T: GcSafe>(&self, value: T) -> Gc<'_, T, Fmt> {
        let collector_id = self.collector_id.unwrap();
        let mut object = Box::new(BigGcObject {
            header: DummyGcHeader::default(),
            static_value: ManuallyDrop::new(value),
        });
        unsafe {
            self.format.write_sized_header(
                Self::cast_header(&mut object.header),
                SimpleMarkData::from_snapshot(SimpleMarkDataSnapshot::new(
                    MarkState::White.to_raw(self.mark_inverted()),
                    collector_id
                ))
            );
        }
        let gc = unsafe { Gc::from_raw(
            self.collector_id.unwrap(),
            NonNull::new_unchecked(&mut *object.static_value),
        ) };
        {
            let size = std::mem::size_of::<BigGcObject<T, Fmt>>();
            {
                let mut objs = self.big_objects.lock();
                objs.push(unsafe { BigGcObject::into_dynamic_box(object) });
            }
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
                            if let Some(drop) = Fmt::determine_type((*slot).header).drop_func {
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
                            actual_size += self.format.determine_type((*slot)).total_size();
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
unsafe impl<Fmt: RawObjectFormat> Send for SimpleAlloc<Fmt> {}
/// We're careful to be thread safe here
///
/// This isn't auto implemented because of the
/// raw pointer to the collector (we only use it as an id)
unsafe impl<Fmt: RawObjectFormat> Sync for SimpleAlloc<Fmt> {}

/// We're careful - I swear :D
unsafe impl<Fmt: RawObjectFormat> Send for RawSimpleCollector<Fmt> {}
unsafe impl<Fmt: RawObjectFormat> Sync for RawSimpleCollector<Fmt> {}

/// Trait hack to avoid overflow with [ObjectFormat]
#[doc(hidden)]
pub trait RawObjectFormat: Sized + OpenAllocObjectFormat<RawSimpleCollector<Self>> {
}
impl RawObjectFormat for SimpleObjectFormat {

}

/// The internal data for a simple collector
#[doc(hidden)]
pub struct RawSimpleCollector<Fmt: RawObjectFormat> {
    logger: Logger,
    heap: GcHeap<Fmt>,
    manager: CollectionManager<Self>,
    /// Tracks object handles
    handle_list: GcHandleList<Self>,
    format: &'static Fmt
}
unsafe impl<Fmt: RawObjectFormat> GcLayoutInternals for RawSimpleCollector<Fmt> {
    type Visitor = MarkVisitor<'static, Fmt>;
    type Id = CollectorId;
    type VisitorError = !;
    type Format = Fmt;
    type MarkData = SimpleMarkData<Fmt>;
    const IMPLICIT_ALIGN: usize = 0; // TODO
    const MARK_BITS: usize = usize::MAX; // We use all the bits
}
unsafe impl<Fmt: RawObjectFormat> ::zerogc_context::collector::RawCollectorImpl for RawSimpleCollector<Fmt> {
    type GcDynPointer = Fmt::DynObject;

    unsafe fn dyn_ptr_from_raw(raw: *mut c_void) -> Self::GcDynPointer {

    }

    #[cfg(feature = "multiple-collectors")]
    type Ptr = NonNull<Self>;

    #[cfg(not(feature = "multiple-collectors"))]
    type Ptr = PhantomData<&'static Self>;

    type Manager = CollectionManager<Self>;

    type RawContext = RawContext<Self>;

    const SINGLETON: bool = cfg!(not(feature = "multiple-collectors"));

    const SYNC: bool = cfg!(feature = "sync");

    #[inline]
    fn id_for_gc<'a, 'gc, T>(gc: &'a Gc<'gc, T, Fmt>) -> &'a CollectorId<Fmt> where 'gc: 'a, T: GcSafe + ?Sized + 'gc {
        #[cfg(feature = "multiple-collectors")] {
            unsafe {
                let mark_data = Fmt::mark_data_ptr(Fmt::into_untyped_object(gc));
                mark_data.collector_id
            }
        }
        #[cfg(not(feature = "multiple-collectors"))] {
            const ID: CollectorId = unsafe { CollectorId::from_raw(PhantomData) };
            &ID
        }
    }


    #[inline]
    unsafe fn create_dyn_pointer<T: GcSafe>(&self, value: *mut T) -> Self::GcDynPointer {
        debug_assert!(!value.is_null());
        self.format.as_untyped_object(Gc::from_raw(self.id(), NonNull::new_unchecked(value)))
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
        _owner: &Gc<'gc, T, Fmt>,
        _value: &Gc<'gc, V, Fmt>,
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
unsafe impl<Fmt: RawObjectFormat> ::zerogc_context::collector::SyncCollector for RawSimpleCollector<Fmt> {}
#[cfg(not(feature = "multiple-collectors"))]
unsafe impl<Fmt: RawObjectFormat> zerogc_context::collector::SingletonCollector for RawSimpleCollector<Fmt> {
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
impl<Fmt: RawObjectFormat> RawSimpleCollector<Fmt> {
    unsafe fn with_logger(format: &'static Fmt, logger: Logger) -> Self {
        RawSimpleCollector {
            logger,
            manager: CollectionManager::new(),
            heap: GcHeap::new(INITIAL_COLLECTION_THRESHOLD),
            handle_list: GcHandleList::new(format),
            format
        }
    }
    #[cold]
    #[inline(never)]
    unsafe fn perform_raw_collection(
        &self, contexts: &[*mut RawContext<Self>]
    ) {
        debug_assert!(self.manager.is_collecting());
        let roots: Vec<Self::GcDynPointer> = contexts.iter()
            .flat_map(|ctx| {
                (**ctx).assume_valid_shadow_stack()
                    .reverse_iter().map(NonNull::as_ptr)
            })
            .chain(std::iter::once(Root::HandleList(&self.handle_list
                // Cast to wrapper type
                as *const GcHandleList<Self> as *const GcHandleListWrapper<Fmt>
            )))
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
enum Root<Fmt: RawObjectFormat> {
    Object(Fmt::DynObject),
    HandleList(*const GcHandleListWrapper<Fmt>)
}
struct CollectionTask<'a, Fmt: RawObjectFormat> {
    expected_collector: CollectorId,
    roots: Vec<Root<Fmt>>,
    heap: &'a GcHeap<Fmt>,
    #[cfg_attr(feature = "implicit-grey-stack", allow(dead_code))]
    grey_stack: Vec<Fmt::DynObject>
}
impl<'a, Fmt: RawObjectFormat> CollectionTask<'a, Fmt> {
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
pub struct MarkVisitor<'a, Fmt: RawObjectFormat> {
    expected_collector: CollectorId<Fmt>,
    #[cfg_attr(feature = "implicit-grey-stack", allow(dead_code))]
    grey_stack: &'a mut Vec<Fmt::DynObject>,
    /// If this meaning of the mark bit is currently inverted
    ///
    /// This flips every collection
    inverted_mark: bool
}
impl<'a, Fmt: RawObjectFormat> MarkVisitor<'a, Fmt> {
    fn visit_raw_gc(
        &mut self, obj: Fmt::DynObject,
        trace_func: impl FnOnce(&mut Fmt::DynObject, &mut MarkVisitor<'a, Fmt>)
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
unsafe impl<Fmt: RawObjectFormat> GcVisitor for MarkVisitor<'_, Fmt> {
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
            assert_eq!(*gc.collector_id(), self.expected_collector);
            self._visit_own_gc(gc);
            Ok(())
        } else {
            // Just ignore
            Ok(())
        }
    }
}
impl<Fmt: RawObjectFormat> MarkVisitor<'_, Fmt> {
    /// Visit a GC type whose [::zerogc::CollectorId] matches our own
    ///
    /// The caller should only use `GcVisitor::visit_gc()`
    unsafe fn _visit_own_gc<'gc, T: GcSafe + 'gc>(&mut self, gc: &mut Gc<'gc, T>) {
        // Verify this again (should be checked by caller)
        debug_assert_eq!(gc.collector_id(), self.expected_collector);
        let untyped = Fmt::into_untyped_object(gc);
        let mark_data = Fmt::mark_data_ptr(untyped, Fmt::determine_type(untyped));
        self.visit_raw_gc(untyped, |obj, visitor| {
            let inverted_mark = visitor.inverted_mark;
            if !T::NEEDS_TRACE {
                /*
                 * We don't need to mark this grey
                 * It has no internals that need to be traced.
                 * We can directly move it directly to the black set
                 */
                (*mark_data).update_raw_state(MarkState::Black.to_raw(inverted_mark));
            } else {
                /*
                 * We need to mark this object grey and push it onto the grey stack.
                 * It will be processed later
                 */
                (*mark_data).update_raw_state(MarkState::Grey.to_raw(inverted_mark));
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
                    <T as Trace>::visit(
                        &mut *(gc.value() as *const T as *mut T),
                        visitor
                    );
                    /*
                     * Mark the object black now it's innards have been traced
                     * NOTE: We do **not** do this with an implicit stack.
                     */
                    (*mark_data).update_raw_state(MarkState::Black.to_raw(
                        inverted_mark
                    ));
                }
            }
        });
    }
}

/// The raw mark state of an object
///
/// Every cycle the meaning of the white/black states
/// flips. This allows us to implicitly mark objects
/// without actually touching their bits :)
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
#[repr(u8)]
enum RawMarkState {
    /// Normally this marks the white state
    ///
    /// If we're inverted, this marks black
    Red = 0,
    /// This always marks the grey state
    ///
    /// Inverting the mark bit doesn't affect the
    /// grey state
    Grey = 1,
    /// Normally this marks the blue state
    ///
    /// If we're inverted, this marks white
    Blue = 2
}
impl RawMarkState {
    #[inline]
    fn from_byte(b: u8) -> Self {
        assert!(b < 3);
        unsafe { std::mem::transmute::<u8, RawMarkState>(b) }
    }
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
    White = 0,
    /// The object is in the gray set and needs to be traversed to look for reachable memory
    ///
    /// After being scanned this object will end up in the black set.
    Grey = 1,
    /// The object is in the black set and is reachable from the roots.
    ///
    /// This object cannot be freed.
    Black = 2
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
