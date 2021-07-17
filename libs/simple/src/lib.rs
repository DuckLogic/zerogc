//! The simplest implementation of zerogc's garbage collection.
//!
//! Uses mark/sweep collection. This shares shadow stack code with the `zerogc-context`
//! crate, and thus [SimpleCollector] is actually a type alias for that crate.
//!
//! ## Internals
//! The internal layout information is public,
//! and available through the (layout module)[`self::layout`].
//!
//! The garbage collector needs to store some dynamic type information at runtime,
//! to allow dynamic dispatch to trace and drop functions.
//! Each object's [GcHeader] has two fields: a [GcType] and some internal mark data.
//!
//! The mark data is implementation-internal. However, the header as a whole is `repr(C)`
//! and the type information
//!
//! Sometimes, users need to store their own type metadata for other purposes.
//! TODO: Allow users to do this.
#![deny(
    missing_docs, // The 'simple' implementation needs to document its public API
)]
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
    // Used for const layout computation:
    const_raw_ptr_deref,
    const_ptr_offset,
    const_align_of_val,
    // Needed for field_offset!
    const_ptr_offset_from,
    const_maybe_uninit_as_ptr,
    const_refs_to_cell,
)]
#![feature(drain_filter)]
#![allow(
    /*
     * TODO: Should we be relying on vtable address stability?
     * It seems safe as long as we reuse the same pointer....
     */
    clippy::vtable_address_comparisons,
)]
use std::alloc::Layout;
use std::ptr::{NonNull, Pointee, DynMetadata};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, AtomicBool, Ordering};
#[cfg(not(feature = "multiple-collectors"))]
use std::sync::atomic::AtomicPtr;
#[cfg(not(feature = "multiple-collectors"))]
use std::marker::PhantomData;
use std::any::{TypeId};
use std::ops::{Deref, DerefMut};

use slog::{Logger, FnValue, debug};

use zerogc::{GcSafe, Trace, GcVisitor};

use zerogc_context::utils::{ThreadId, MemorySize};

use crate::alloc::{SmallArenaList, SmallArena};
use crate::layout::{StaticGcType, GcType, SimpleVecRepr, DynamicObj, StaticVecType, SimpleMarkData, SimpleMarkDataSnapshot, GcHeader, BigGcObject, HeaderLayout, GcArrayHeader, GcVecHeader, GcTypeLayout};

use zerogc_context::collector::{RawSimpleAlloc, RawCollectorImpl};
use zerogc_context::handle::{GcHandleList, RawHandleImpl};
use zerogc_context::{CollectionManager as AbstractCollectionManager, RawContext as AbstractRawContext, CollectorContext};
use zerogc::vec::{GcRawVec};
use std::cell::Cell;
use std::ffi::c_void;

#[cfg(feature = "small-object-arenas")]
mod alloc;
#[cfg(not(feature = "small-object-arenas"))]
mod alloc {
    use std::alloc::Layout;
    use crate::layout::UnknownHeader;

    pub const fn fits_small_object(_layout: Layout) -> bool {
        false
    }
    pub struct SmallArena;
    impl SmallArena {
        pub(crate) fn add_free(&self, _free: *mut UnknownHeader) {
            unimplemented!()
        }
        pub(crate) fn alloc(&self) -> std::ptr::NonNull<super::GcHeader> {
            unimplemented!()
        }
    }
    pub struct SmallArenaList;
    impl SmallArenaList {
        // Create dummy
        pub fn new() -> Self { SmallArenaList }
        pub fn find(&self, _layout: Layout) -> Option<&SmallArena> { None }
    }
}
pub mod layout;

/// The configuration for a garbage collection
pub struct GcConfig {
    /// Whether to always force a collection at safepoints,
    /// regardless of whether the heuristics say.
    pub always_force_collect: bool,
    /// The initial threshold to trigger garbage collection (in bytes)
    pub initial_threshold: usize,
}
impl Default for GcConfig {
    fn default() -> Self {
        GcConfig {
            always_force_collect: false,
            initial_threshold: 2048,
        }
    }
}

/// The alignment of the singleton empty vector
const EMPTY_VEC_ALIGNMENT: usize = std::mem::align_of::<usize>();

#[cfg(feature = "sync")]
type RawContext<C> = zerogc_context::state::sync::RawContext<C>;
#[cfg(feature = "sync")]
type CollectionManager<C> = zerogc_context::state::sync::CollectionManager<C>;
#[cfg(not(feature = "sync"))]
type RawContext<C> = zerogc_context::state::nosync::RawContext<C>;
#[cfg(not(feature = "sync"))]
type CollectionManager<C> = zerogc_context::state::nosync::CollectionManager<C>;


/// A "simple" garbage collector
pub type SimpleCollector = ::zerogc_context::CollectorRef<RawSimpleCollector>;
/// The context of a simple collector
pub type SimpleCollectorContext = ::zerogc_context::CollectorContext<RawSimpleCollector>;
/// The id for a simple collector
pub type CollectorId = ::zerogc_context::CollectorId<RawSimpleCollector>;
/// A garbage collected pointer, allocated in the "simple" collector
pub type Gc<'gc, T> = ::zerogc::Gc<'gc, T, CollectorId>;
/// A garbage collected array, allocated in the "simple" collector
pub type GcArray<'gc, T> = ::zerogc::vec::GcArray<'gc, T, CollectorId>;
/// A garbage colelcted vector, allocated in the "simple" collector
pub type GcVec<'gc, T> = ::zerogc::vec::GcVec<'gc, T, SimpleCollectorContext>;

#[cfg(not(feature = "multiple-collectors"))]
static GLOBAL_COLLECTOR: AtomicPtr<RawSimpleCollector> = AtomicPtr::new(std::ptr::null_mut());

unsafe impl RawSimpleAlloc for RawSimpleCollector {
    #[inline]
    unsafe fn alloc_uninit<'gc, T>(context: &'gc SimpleCollectorContext) -> (CollectorId, *mut T) where T: GcSafe + 'gc {
        let (_header, ptr) = context.collector().heap.allocator.alloc_layout(
            GcHeader::LAYOUT,
            Layout::new::<T>(),
            T::STATIC_TYPE
        );
        (context.collector().id(), ptr as *mut T)
    }

    unsafe fn alloc_uninit_slice<'gc, T>(context: &'gc CollectorContext<Self>, len: usize) -> (CollectorId, *mut T) where T: GcSafe + 'gc {
        let (header, ptr) = context.collector().heap.allocator.alloc_layout(
            GcArrayHeader::LAYOUT,
            Layout::array::<T>(len).unwrap(),
            <[T] as StaticGcType>::STATIC_TYPE
        );
        (*header).len = len;
        (context.collector().id(), ptr.cast())
    }

    #[inline]
    fn alloc_vec<'gc, T>(context: &'gc CollectorContext<Self>) -> GcVec<'gc, T> where T: GcSafe + 'gc {
        if std::mem::align_of::<T>() > EMPTY_VEC_ALIGNMENT {
            // We have to do an actual allocation because we want higher alignment :(
            return Self::alloc_vec_with_capacity(context, 0)
        }
        let header = context.collector().heap.empty_vec();
        // NOTE: Assuming header is already initialized
        unsafe {
            debug_assert_eq!((*header).len.get(), 0);
            debug_assert_eq!((*header).capacity, 0);
        }
        let id = context.collector().id();
        let ptr = unsafe { NonNull::new_unchecked((header as *mut u8)
            .add(GcVecHeader::LAYOUT.value_offset(EMPTY_VEC_ALIGNMENT))) };
        GcVec {
            raw: unsafe { GcRawVec::from_repr(Gc::from_raw(id, ptr.cast())) },
            context
        }
    }

    fn alloc_vec_with_capacity<'gc, T>(context: &'gc CollectorContext<Self>, capacity: usize) -> GcVec<'gc, T> where T: GcSafe + 'gc {
        if capacity == 0 && std::mem::align_of::<T>() <= EMPTY_VEC_ALIGNMENT {
            return Self::alloc_vec(context)
        }
        let (header, value_ptr) = context.collector().heap.allocator.alloc_layout(
            GcVecHeader::LAYOUT,
            Layout::array::<T>(capacity).unwrap(),
            <T as StaticVecType>::STATIC_VEC_TYPE
        );
        let ptr = unsafe {
            (*header).capacity = capacity;
            (*header).len.set(0);
            NonNull::new_unchecked(value_ptr as *mut SimpleVecRepr<T>)
        };
        let id = context.collector().id();
        GcVec {
            raw: unsafe { GcRawVec::from_repr(Gc::from_raw(id, ptr.cast())) },
            context
        }
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
                let header = &mut *GcHeader::from_value_ptr(raw_ptr);
                // Mark grey
                header.update_raw_mark_state(MarkState::Grey.
                    to_raw(visitor.inverted_mark));
                // Visit innards
                if let Some(func) = type_info.trace_func {
                    func(header.value(), visitor);
                }
                // Mark black
                header.update_raw_mark_state(MarkState::Black.
                    to_raw(visitor.inverted_mark));
                Ok(())
            });
        }
    }
}


struct GcHeap {
    config: Arc<GcConfig>,
    threshold: AtomicUsize,
    allocator: SimpleAlloc,
    // NOTE: This is only public so it can be traced
    cached_empty_vec: Cell<Option<*mut GcVecHeader>>
}
impl GcHeap {
    fn new(config: Arc<GcConfig>) -> GcHeap {
        GcHeap {
            threshold: AtomicUsize::new(if config.always_force_collect {
                0
            } else {
                config.initial_threshold
            }),
            allocator: SimpleAlloc::new(),
            config, cached_empty_vec: Cell::new(None)
        }
    }
    #[inline]
    pub fn empty_vec(&self) -> *mut GcVecHeader {
        match self.cached_empty_vec.get() {
            Some(cached) => cached,
            None => {
                let res = self.create_empty_vec();
                self.cached_empty_vec.set(Some(self.create_empty_vec()));
                res
            }
        }
    }
    #[cold]
    fn create_empty_vec<'gc>(&self) -> *mut GcVecHeader {
        const DUMMY_LAYOUT: Layout = unsafe { Layout::from_size_align_unchecked(
            0, EMPTY_VEC_ALIGNMENT
        ) };
        const DUMMY_TYPE: GcType = GcType {
            layout: GcTypeLayout::Vec {
                element_layout: DUMMY_LAYOUT,
            },
            value_offset_from_common_header: GcVecHeader::LAYOUT
                .value_offset_from_common_header(EMPTY_VEC_ALIGNMENT),
            drop_func: None,
            trace_func: None
        };
        let (header, _) = self.allocator.alloc_layout(
            GcVecHeader::LAYOUT,
            DUMMY_LAYOUT,
            &DUMMY_TYPE
        );
        unsafe {
            (*header).capacity = 0;
            (*header).len.set(0);
            header
        }
    }
    #[inline]
    fn should_collect_relaxed(&self) -> bool {
        /*
         * Double check that 'config.always_force_collect' implies
         * a threshold of zero.
         *
         * NOTE: This is not part of the ABI, it's only for internal debugging
         */
        if cfg!(debug_assertions) && self.config.always_force_collect {
            debug_assert_eq!(
                self.threshold.load(Ordering::Relaxed),
                0
            );
        }
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
impl<'a, T: 'a> ILock<'a, T> for Lock<T> {
    type Guard = ::std::cell::RefMut<'a, T>;

    #[inline]
    fn lock(&'a self) -> Self::Guard {
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
pub(crate) struct SimpleAlloc {
    collector_id: Option<CollectorId>,
    small_arenas: SmallArenaList,
    big_objects: Lock<Vec<BigGcObject>>,
    small_objects: Lock<Vec<*mut GcHeader>>,
    /// Whether the meaning of the mark bit is currently inverted.
    ///
    /// This flips every collection
    mark_inverted: AtomicBool,
    allocated_size: AtomicUsize
}
#[derive(Debug)]
struct TargetLayout<H> {
    value_layout: Layout,
    header_layout: HeaderLayout<H>,
    value_offset: usize,
    overall_layout: Layout
}
impl SimpleAlloc {
    fn new() -> SimpleAlloc {
        SimpleAlloc {
            collector_id: None,
            allocated_size: AtomicUsize::new(0),
            small_arenas: SmallArenaList::new(),
            big_objects: Lock::from(Vec::new()),
            small_objects: Lock::from(Vec::new()),
            mark_inverted: AtomicBool::new(false),
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
    fn alloc_layout<H>(&self, header_layout: HeaderLayout<H>, value_layout: Layout, static_type: &'static GcType) -> (*mut H, *mut u8) {
        let collector_id_ptr = match self.collector_id {
            Some(ref collector) => collector,
            None => {
                #[cfg(debug_assertions)] {
                    unreachable!("Invalid collector id")
                }
                #[cfg(not(debug_assertions))] {
                    unsafe { std::hint::unreachable_unchecked() }
                }
            }
        };
        let (mut overall_layout, value_offset) = header_layout.layout()
            .extend(value_layout).unwrap();
        debug_assert_eq!(header_layout.value_offset(value_layout.align()), value_offset);
        overall_layout  = overall_layout.pad_to_align();
        debug_assert_eq!(
            value_offset,
            header_layout.value_offset(value_layout.align())
        );
        let target_layout = TargetLayout {
            header_layout, value_offset, value_layout, overall_layout
        };
        let (header, value_ptr) = if let Some(arena) = self.small_arenas.find(target_layout.overall_layout) {
            unsafe { self.alloc_layout_small(arena, target_layout) }
        } else {
            self.alloc_layout_big(target_layout)
        };
        unsafe {
            header_layout.common_header(header).write(GcHeader::new(
                static_type,
                SimpleMarkData::from_snapshot(SimpleMarkDataSnapshot::new(
                    MarkState::White.to_raw(self.mark_inverted()),
                    collector_id_ptr as *const _ as *mut _,
                ))
            ));
        }
        (header, value_ptr)
    }
    #[inline]
    unsafe fn alloc_layout_small<H>(&self, arena: &SmallArena, target_layout: TargetLayout<H>) -> (*mut H, *mut u8) {
        let ptr = arena.alloc();
        debug_assert_eq!(
            ptr.as_ptr() as usize % target_layout.header_layout.layout().align(),
            0
        );
        self.add_allocated_size(target_layout.overall_layout.size());
        {
            let mut lock = self.small_objects.lock();
            lock.push((ptr.as_ptr() as *mut u8).add(target_layout.header_layout.common_header_offset).cast());
        }
        (ptr.as_ptr().cast(), (ptr.as_ptr() as *mut u8).add(target_layout.value_offset))
    }
    fn alloc_layout_big<H>(&self, target_layout: TargetLayout<H>) -> (*mut H, *mut u8) {
        let header: *mut H;
        let value_ptr = unsafe {
            header = std::alloc::alloc(target_layout.overall_layout).cast();
            (header as *mut u8).add(target_layout.value_offset)
        };
        {
            unsafe {
                let mut objs = self.big_objects.lock();
                let common_header = (header as *mut u8)
                    .add(target_layout.header_layout.common_header_offset)
                    .cast();
                objs.push(BigGcObject::from_ptr(common_header));
            }
            self.add_allocated_size(target_layout.overall_layout.size());
        }
        (header, value_ptr)
    }
    unsafe fn sweep(&self) {
        let mut expected_size = self.allocated_size();
        let mut actual_size = 0;
        // Clear small arenas
        let was_mark_inverted = self.mark_inverted.load(Ordering::SeqCst);
        self.small_objects.lock().drain_filter(|&mut common_header| {
            let total_size = (*common_header).type_info.determine_total_size(common_header);
            match (*common_header).raw_mark_state().resolve(was_mark_inverted) {
                MarkState::White => {
                    // Free the object, dropping if necessary
                    expected_size -= total_size;
                    true // Drain, implicitly adding to free list
                },
                MarkState::Grey => panic!("All grey objects should've been processed"),
                MarkState::Black => {
                    /*
                     * Retain the object
                     * State will be implicitly set to white
                     * by inverting mark the meaning of the mark bits.
                     */
                    actual_size += total_size;
                    false // keep
                }
            }
        }).for_each(|freed_common_header| {
            let type_info = (*freed_common_header).type_info;
            if let Some(func) = type_info.drop_func {
                func((*freed_common_header).value());
            }
            let overall_layout = type_info.determine_total_layout(freed_common_header);
            let actual_start = type_info.header_layout()
                .from_common_header(freed_common_header);
            self.small_arenas.find(overall_layout).unwrap().add_free(actual_start)
        });
        // Clear large objects
        debug_assert_eq!(was_mark_inverted, self.mark_inverted());
        self.big_objects.lock().drain_filter(|big_item| {
            let total_size = big_item.header().type_info.determine_total_size(big_item.header.as_ptr());
            match big_item.header().mark_data.load_snapshot().state.resolve(was_mark_inverted) {
                MarkState::White => {
                    // Free the object
                    expected_size -= total_size;
                    true // remove from list
                },
                MarkState::Grey => panic!("All gray objects should've been processed"),
                MarkState::Black => {
                    /*
                     * Retain the object
                     * State will be implicitly set to white
                     * by inverting mark the meaning of the mark bits.
                     */
                    actual_size += total_size;
                    false // Keep
                }
            }
        }).for_each(drop);
        /*
         * Flip the meaning of the mark bit,
         * implicitly resetting all Black (reachable) objects
         * to White.
         */
        self.set_mark_inverted(!was_mark_inverted);
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
    config: Arc<GcConfig>
}

unsafe impl ::zerogc_context::collector::RawCollectorImpl for RawSimpleCollector {
    type DynTracePtr = NonNull<dyn DynTrace>;
    type Config = GcConfig;

    #[cfg(feature = "multiple-collectors")]
    type Ptr = NonNull<Self>;

    #[cfg(not(feature = "multiple-collectors"))]
    type Ptr = PhantomData<&'static Self>;

    type Manager = CollectionManager<Self>;

    type RawContext = RawContext<Self>;

    type RawVecRepr = SimpleVecRepr<DynamicObj>;

    const SINGLETON: bool = cfg!(not(feature = "multiple-collectors"));

    const SYNC: bool = cfg!(feature = "sync");

    #[inline]
    fn id_for_gc<'a, 'gc, T>(gc: &'a Gc<'gc, T>) -> &'a CollectorId where 'gc: 'a, T: GcSafe + ?Sized + 'gc {
        #[cfg(feature = "multiple-collectors")] {
            unsafe {
                let header = GcHeader::from_value_ptr(gc.as_raw_ptr());
                &*(&*header).mark_data.load_snapshot().collector_id_ptr
            }
        }
        #[cfg(not(feature = "multiple-collectors"))] {
            const ID: CollectorId = unsafe { CollectorId::from_raw(PhantomData) };
            &ID
        }
    }

    #[inline]
    fn id_for_array<'a, 'gc, T>(gc: &'a GcArray<'gc, T>) -> &'a CollectorId where 'gc: 'a, T: GcSafe + 'gc {
        #[cfg(feature = "multiple-collectors")] {
            unsafe {
                let header = GcArrayHeader::LAYOUT.from_value_ptr(gc.as_raw_ptr());
                &*(&*header).common_header.mark_data.load_snapshot().collector_id_ptr
            }
        }
        #[cfg(not(feature = "multiple-collectors"))] {
            const ID: CollectorId = unsafe { CollectorId::from_raw(PhantomData) };
            &ID
        }
    }

    #[inline]
    fn resolve_array_len<'gc, T>(gc: zerogc::GcArray<'gc, T, zerogc_context::CollectorId<Self>>) -> usize where T: GcSafe + 'gc {
        unsafe {
            let header = GcArrayHeader::LAYOUT.from_value_ptr(gc.as_raw_ptr());
            (*header).len
        }
    }


    #[inline]
    unsafe fn as_dyn_trace_pointer<T: Trace>(value: *mut T) -> Self::DynTracePtr {
        debug_assert!(!value.is_null());
        NonNull::new_unchecked(
            std::mem::transmute::<
                *mut dyn DynTrace,
                *mut (dyn DynTrace + 'static)
            >(value as *mut dyn DynTrace)
        )
    }

    #[cfg(not(feature = "multiple-collectors"))]
    fn init(config: GcConfig, _logger: Logger) -> NonNull<Self> {
        panic!("Not a singleton")
    }

    #[cfg(feature = "multiple-collectors")]
    fn init(config: GcConfig, logger: Logger) -> NonNull<Self> {
        // We're assuming its safe to create multiple (as given by config)
        let raw = unsafe { RawSimpleCollector::with_logger(config, logger) };
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

    fn init_global(config: GcConfig, logger: Logger) {
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
            unsafe { RawSimpleCollector::with_logger(config, logger) }
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
    unsafe fn with_logger(config: GcConfig, logger: Logger) -> Self {
        let config = Arc::new(config);
        RawSimpleCollector {
            logger,
            manager: CollectionManager::new(),
            heap: GcHeap::new(Arc::clone(&config)),
            handle_list: GcHandleList::new(),
            config
        }
    }
    #[cold]
    #[inline(never)]
    unsafe fn perform_raw_collection(
        &self, contexts: &[*mut RawContext<Self>]
    ) {
        debug_assert!(self.manager.is_collecting());
        let roots = {
            let mut roots: Vec<*mut dyn DynTrace> = Vec::new();
            for ctx in contexts.iter() {
                roots.extend((**ctx).assume_valid_shadow_stack()
                    .reverse_iter().map(NonNull::as_ptr));
            }
            roots.push(&self.handle_list
                // Cast to wrapper type
                as *const GcHandleList<Self> as *const GcHandleListWrapper
                // Make into virtual pointer
                as *const dyn DynTrace
                as *mut dyn DynTrace
            );
            #[repr(transparent)]
            struct CachedEmptyVec(GcVecHeader);
            unsafe impl DynTrace for CachedEmptyVec {
                fn trace(&mut self, visitor: &mut MarkVisitor) {
                    let cached_vec_header = self as *mut Self as *mut GcVecHeader;
                    unsafe {
                        let target_repr = (cached_vec_header as *mut u8)
                            .add(GcVecHeader::LAYOUT.value_offset(EMPTY_VEC_ALIGNMENT))
                            .cast::<SimpleVecRepr<()>>();
                        let mut target_gc = *(&target_repr
                            as *const *mut SimpleVecRepr<()>
                            as *const Gc<SimpleVecRepr<DynamicObj>>);
                        // TODO: Assert not moving?
                        let Ok(()) = visitor.visit_vec::<(), _>(&mut target_gc);
                    }
                }
            }
            if let Some(cached_vec_header) = self.heap.cached_empty_vec.get() {
                roots.push(cached_vec_header as *mut CachedEmptyVec as *mut dyn DynTrace)
            }
            roots
        };
        let num_roots = roots.len();
        let mut task = CollectionTask {
            config: &*self.config,
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
    config: &'a GcConfig,
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
                    (*obj).raw_mark_state().resolve(was_inverted_mark),
                    MarkState::Grey
                );
                let mut visitor = MarkVisitor {
                    expected_collector: self.expected_collector,
                    grey_stack: &mut self.grey_stack,
                    inverted_mark: was_inverted_mark
                };
                if let Some(trace) = (*obj).type_info.trace_func {
                    (trace)(
                        (*obj).value(),
                        &mut visitor
                    );
                }
                // Mark the object black now it's innards have been traced
                (*obj).update_raw_mark_state(MarkState::Black.to_raw(was_inverted_mark));
            }
        }
        // Sweep
        unsafe { self.heap.allocator.sweep() };
        let updated_size = self.heap.allocator.allocated_size();
        if self.config.always_force_collect {
            assert_eq!(
                self.heap.threshold.load(Ordering::SeqCst),
                0
            );
        } else {
            // Update the threshold to be 150% of currently used size
            self.heap.threshold.store(
                updated_size + (updated_size / 2),
                Ordering::SeqCst
            );
        }
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
        match obj.raw_mark_state().resolve(self.inverted_mark) {
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
            assert_eq!(*gc.collector_id(), self.expected_collector);
            self._visit_own_gc(gc);
            Ok(())
        } else {
            // Just ignore
            Ok(())
        }
    }

    unsafe fn visit_trait_object<'gc, T, Id>(&mut self, gc: &mut zerogc::Gc<'gc, T, Id>) -> Result<(), Self::Err>
        where T: ?Sized + GcSafe + 'gc + Pointee<Metadata=DynMetadata<T>> + zerogc::DynTrace, Id: zerogc::CollectorId {
        if TypeId::of::<Id>() == TypeId::of::<crate::CollectorId>() {
            /*
             * The TypeIds match, so this cast is safe. See `visit_gc` for details
             */
            let gc = std::mem::transmute::<
                &mut ::zerogc::Gc<'gc, T, Id>,
                &mut ::zerogc::Gc<'gc, T, crate::CollectorId>
            >(gc);
            let header = GcHeader::from_value_ptr(gc.as_raw_ptr());
            self._visit_own_gc_with(gc, |ptr, visitor| {
                if let Some(func) = (*header).type_info.trace_func {
                    func(ptr as *mut T as *mut c_void, visitor)
                }
                Ok(())
            });
            Ok(())
        } else {
            Ok(())
        }
    }

    #[inline]
    unsafe fn visit_vec<'gc, T, Id>(&mut self, raw: &mut ::zerogc::Gc<'gc, Id::RawVecRepr, Id>) -> Result<(), Self::Err>
        where T: GcSafe + 'gc, Id: ::zerogc::CollectorId {
        // Just visit our innards as if they were a regular `Gc` reference
        self.visit_gc(raw)
    }

    #[inline]
    unsafe fn visit_array<'gc, T, Id>(&mut self, array: &mut ::zerogc::vec::GcArray<'gc, T, Id>) -> Result<(), Self::Err>
        where T: GcSafe + 'gc, Id: ::zerogc::CollectorId {
        if TypeId::of::<Id>() == TypeId::of::<crate::CollectorId>() {
            /*
             * See comment in 'visit_gc'.
             * Essentially this is a checked cast
             */
            let array = std::mem::transmute::<
                &mut ::zerogc::vec::GcArray<'gc, T, Id>,
                &mut ::zerogc::vec::GcArray<'gc, T, crate::CollectorId>
            >(array);
            /*
             * Check the collectors match. Otherwise we're mutating
             * other people's data.
             */
            assert_eq!(*array.collector_id(), self.expected_collector);
            let len = array.len();
            let mut gc = Gc::from_raw(
                *array.collector_id(),
                NonNull::from(array.as_slice())
            );
            self._visit_own_gc(&mut gc);
            *array = GcArray::from_raw_ptr(
                NonNull::new_unchecked(gc.value().as_ptr() as *mut T),
                len
            );
            Ok(())
        } else {
            Ok(())
        }
    }
}
impl MarkVisitor<'_> {

    /// Visit a GC type whose [::zerogc::CollectorId] matches our own,
    /// tracing it with the specified closure
    ///
    /// The caller should only use `GcVisitor::visit_gc()`
    unsafe fn _visit_own_gc<'gc, T: GcSafe + Trace + ?Sized>(
        &mut self, gc: &mut Gc<'gc, T>
    ) {
        self._visit_own_gc_with(gc, |val, visitor| <T as Trace>::visit(val, visitor))
    }
    /// Visit a GC type whose [::zerogc::CollectorId] matches our own,
    /// tracing it with the specified closure
    ///
    /// The caller should only use `GcVisitor::visit_gc()`
    unsafe fn _visit_own_gc_with<'gc, T: GcSafe + ?Sized + 'gc>(
        &mut self, gc: &mut Gc<'gc, T>,
        trace_func: impl FnOnce(&mut T, &mut MarkVisitor) -> Result<(), !>
    ) {
        // Verify this again (should be checked by caller)
        debug_assert_eq!(*gc.collector_id(), self.expected_collector);
        let header = GcHeader::from_value_ptr(gc.as_raw_ptr());
        self.visit_raw_gc(&mut *header, |obj, visitor| {
            let inverted_mark = visitor.inverted_mark;
            if !T::NEEDS_TRACE {
                /*
                 * We don't need to mark this grey
                 * It has no internals that need to be traced.
                 * We can directly move it directly to the black set
                 */
                obj.update_raw_mark_state(MarkState::Black.to_raw(inverted_mark));
            } else {
                /*
                 * We need to mark this object grey and push it onto the grey stack.
                 * It will be processed later
                 */
                (*obj).update_raw_mark_state(MarkState::Grey.to_raw(inverted_mark));
                #[cfg(not(feature = "implicit-grey-stack"))] {
                    visitor.grey_stack.push(obj as *mut GcHeader);
                    drop(trace_func);
                }
                #[cfg(feature = "implicit-grey-stack")] {
                    /*
                     * The user wants an implicit grey stack using
                     * recursion. This risks stack overflow but can
                     * boost performance (See 9a9634d68a4933d).
                     * On some workloads this is fine.
                     */
                    let Ok(()) = trace_func(
                        &mut *(gc.value() as *const T as *mut T),
                        visitor
                    );
                    /*
                     * Mark the object black now it's innards have been traced
                     * NOTE: We do **not** do this with an implicit stack.
                     */
                    (*obj).update_raw_mark_state(MarkState::Black.to_raw(
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
