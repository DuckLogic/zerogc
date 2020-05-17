//! A copying garbage collector
// TODO: Use stable rust
#![feature(
    alloc_layout_extra, // Used for GcObject::from_raw
    never_type, // Used for errors (which are currently impossible)
    negative_impls, // impl !Send is much cleaner than PhantomData<Rc<()>>
    exhaustive_patterns, // Allow exhaustive matching against never
    const_if_match, // Const layout logic
    const_alloc_layout, // We want to ensure layout logic can be const-folded
    const_transmute, // We need to transmute function pointers in GcType
    drain_filter, // Used for finalizers
)]
use zerogc::{GarbageCollectionSystem, CollectorId, GcSafe, Trace, GcContext, GcVisitor, GcAllocContext};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::alloc::Layout;
use std::cell::{RefCell, Cell};
use std::ptr::NonNull;
use std::rc::Rc;
use std::os::raw::c_void;
use crate::alloc::{Arena, Chunk};

/// An alias to `zerogc::Gc<T, CopyingCollectorId>` to simplify use
pub type Gc<'gc, T> = zerogc::Gc<'gc, T, CopyingCollectorId>;

/// A garbage collector that internally compacts memory
static NEXT_COLLECTOR_ID: AtomicUsize = AtomicUsize::new(47);

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct CopyingCollectorId(usize);
impl CopyingCollectorId {
    fn acquire() -> CopyingCollectorId {
        loop {
            let prev = NEXT_COLLECTOR_ID.load(Ordering::SeqCst);
            let updated = prev.checked_add(1).expect("Overflow collector ids");
            if NEXT_COLLECTOR_ID.compare_and_swap(prev, updated, Ordering::SeqCst) == prev {
                break CopyingCollectorId(prev)
            }
        }
    }
}
unsafe impl CollectorId for CopyingCollectorId {}
pub struct CopyingCollector(Rc<RawCopyingCollector>);
impl CopyingCollector {
    pub fn create() -> Self {
        let id = CopyingCollectorId::acquire();
        CopyingCollector(Rc::new(RawCopyingCollector {
            id, shadow_stack: RefCell::new(ShadowStack(Vec::new())),
            heap: GcHeap {
                threshold: Cell::new(INITIAL_COLLECTION_THRESHOLD),
                allocator: ArenaAlloc::new(id)
            }
        }))
    }
    /// Make this collector into a context
    #[inline]
    pub fn into_context(self) -> CopyingCollectorContext {
        CopyingCollectorContext {
            collector: self.0
        }
    }
}

unsafe impl GarbageCollectionSystem for CopyingCollector {
    type Id = CopyingCollectorId;

    #[inline]
    fn id(&self) -> Self::Id {
        self.0.id
    }
}

#[derive(Clone)]
struct ShadowStack(Vec<*mut dyn DynVisit>);
impl ShadowStack {
    #[inline]
    pub unsafe fn push<'a, T: Trace + 'a>(&mut self, value: &'a mut T) -> *mut dyn DynVisit {
        let short_ptr = value as &mut (dyn DynVisit + 'a)
            as *mut (dyn DynVisit + 'a);
        let long_ptr = std::mem::transmute::<
            *mut (dyn DynVisit + 'a),
            *mut (dyn DynVisit + 'static)
        >(short_ptr);
        self.0.push(long_ptr);
        long_ptr
    }
    #[inline]
    pub fn pop(&mut self) -> Option<*mut dyn DynVisit> {
        self.0.pop()
    }
}


/// The initial memory usage to start a collection
const INITIAL_COLLECTION_THRESHOLD: usize = 2048;

mod alloc;

struct GcHeap {
    threshold: Cell<usize>,
    allocator: ArenaAlloc
}
impl GcHeap {
    #[inline]
    fn should_collect(&self) -> bool {
        self.allocator.allocated_size() >= self.threshold.get()
    }
}

pub struct ArenaAlloc {
    id: CopyingCollectorId,
    /// A lightweight approximation of the number of objects currently in the arena
    approx_allocated_size: Cell<usize>,
    finalizer_list: RefCell<Vec<*mut GcHeader>>,
    arena: Arena,
}
impl ArenaAlloc {
    fn new(id: CopyingCollectorId) -> ArenaAlloc {
        ArenaAlloc {
            id,
            approx_allocated_size: Cell::new(0),
            finalizer_list: RefCell::new(Vec::new()),
            arena: Arena::new()
        }
    }
    #[inline]
    fn allocated_size(&self) -> usize {
        self.approx_allocated_size.get()
    }
    #[inline]
    fn alloc<T: GcSafe>(&self, value: T) -> Gc<'_, T> {
        let gc_type: &GcType = <T as StaticType>::GC_TYPE;
        // NOTE: We don't want to involve align_of::<T>
        // to make it possible to iterate over GcHeaders
        let raw = self.arena.alloc_bytes(
            gc_type.complete_layout().size());
        let header = raw as *mut GcHeader;
        let value_ptr: *mut T;
        unsafe {
            value_ptr = raw.add(gc_type.value_offset()).cast();
            header.write(GcHeader {
                state: ObjState::Active,
                info: HeaderInfo {
                    type_info: gc_type
                }
            });
            value_ptr.write(value);
        }
        if std::mem::needs_drop::<T>() {
            assert!(gc_type.drop.is_some());
            self.finalizer_list.borrow_mut().push(header);
        }
        self.approx_allocated_size.set(
            self.approx_allocated_size.get() + gc_type.complete_layout().size()
        );
        unsafe { Gc::new(self.id, NonNull::new_unchecked(value_ptr)) }
    }
}

/// The internal data for a simple collector
struct RawCopyingCollector {
    id: CopyingCollectorId,
    shadow_stack: RefCell<ShadowStack>,
    heap: GcHeap
}
impl RawCopyingCollector {
    #[inline]
    unsafe fn maybe_collect(&self) {
        if self.heap.should_collect() {
            self.perform_collection()
        }
    }
    #[cold]
    #[inline(never)]
    unsafe fn perform_collection(&self) {
        let total_used = self.heap.allocator.arena.total_used();
        let last_chunk_capacity = self.heap.allocator.arena.current_chunk_capacity();
        let new_size = if total_used < last_chunk_capacity {
            last_chunk_capacity // Last chunk was enough
        } else {
            // Double capacity to ensure amortized growth
            std::cmp::max(total_used, last_chunk_capacity * 2)
        };
        self.debug_visit();
        let chunk = Chunk::alloc(new_size);
        let task = CollectionTask {
            id: self.id,
            roots: self.shadow_stack.borrow().0.clone(),
            heap: &self.heap,
            updated_chunk: chunk
        };
        task.run();
        self.debug_visit();
    }
    #[cfg(not(debug_assertions))]
    fn debug_visit(&self) {}
    #[cfg(debug_assertions)]
    fn debug_visit(&self) {
        let stack = self.shadow_stack.borrow();
        let mut visitor = DebugVisitor {
            id: self.id,
            arena: &self.heap.allocator.arena
        };
        for &root in &stack.0 {
            unsafe { (*root).visit_debug(&mut visitor) };
        }
    }
}
#[cfg(debug_assertions)]
pub struct DebugVisitor<'a> {
    id: CopyingCollectorId,
    arena: &'a Arena,
}
#[cfg(debug_assertions)]
unsafe impl GcVisitor for DebugVisitor<'_> {
    type Err = !;

    fn visit_gc<T, Id>(&mut self, gc: &mut zerogc::Gc<T, Id>) -> Result<(), !>
        where T: GcSafe, Id: CollectorId {
        assert_eq!(gc.id().try_cast::<CopyingCollectorId>(), Some(&self.id));
        assert_ne!(
            self.arena.check_contains_ptr(unsafe { gc.as_raw_ptr() } as *mut u8),
            None
        );
        unsafe { (&mut *gc.as_raw_ptr()).visit(self) }
    }
}
struct CollectionTask<'a> {
    id: CopyingCollectorId,
    roots: Vec<*mut dyn DynVisit>,
    heap: &'a GcHeap,
    updated_chunk: Chunk
}
impl<'a> CollectionTask<'a> {
    fn run(self) {
        // Scan initial roots
        for &root in &self.roots {
            let mut visitor = CopyVisitor {
                updated_chunk: &self.updated_chunk,
                id: self.id,
            };
            unsafe { (*root).visit_copying(&mut visitor) };
        }
        let mut scan_ptr = self.updated_chunk.start();
        while self.updated_chunk.is_used(scan_ptr) {
            debug_assert_eq!(scan_ptr as usize & (std::mem::align_of::<GcHeader>() - 1), 0);
            let header = scan_ptr as *mut GcHeader;
            unsafe {
                debug_assert_ne!((*header).state, ObjState::Relocated);
                if let Some(trace) = (*header).info.type_info.trace {
                    let mut visitor = CopyVisitor {
                        updated_chunk: &self.updated_chunk,
                        id: self.id,
                    };
                    trace((*header).value() as *mut c_void, &mut visitor);
                }
                scan_ptr = (*header).wrapping_next_obj() as *mut u8;
            }
        }
        // Run finalizers
        {
            let mut allocators = self.heap.allocator.finalizer_list.borrow_mut();
            allocators.drain_filter(|header| unsafe {
                debug_assert!(!self.updated_chunk.contains(*header as *mut u8));
                match (**header).state {
                    ObjState::Active => {
                        /*
                         * The fact that the object hasn't been
                         * relocated means that its implicitly dead.
                         * Drain it from the list, eventually running its finalizer
                         */
                        true // drain
                    },
                    ObjState::Relocated => {
                        /*
                         * The object has been relocated, meaning its still live
                         * Update the header to point ot the relocated location
                         */
                        let relocated_header = (**header).info.relocated;
                        debug_assert!(self.updated_chunk.is_used(relocated_header as *mut u8));
                        (*header) = relocated_header;
                        false // retain (with newly updated location)
                    },
                }
            }).for_each(|header| unsafe {
                debug_assert_eq!((*header).state, ObjState::Active);
                /*
                 * Run destructor
                 * Because the type implements the unsafe trait `GcSafe`,
                 * they are guarenteeing they wont resurrect objects
                 */
                let info = (*header).info.type_info;
                if let Some(drop) = info.drop {
                    drop(header.cast::<u8>()
                        .add(info.value_offset()).cast());
                }
            });
        }
        self.heap.allocator.approx_allocated_size.set(self.updated_chunk.used_bytes());
        // Reset arena to use only the updated chunkk
        unsafe {
            self.heap.allocator.arena.reset_single_chunk(self.updated_chunk);
        }
        let updated_size = self.heap.allocator.allocated_size();
        // Update the threshold to be 150% of currently used size
        self.heap.threshold.set(updated_size + (updated_size / 2));
    }
}

struct CopyVisitor<'a> {
    id: CopyingCollectorId,
    updated_chunk: &'a Chunk
}
unsafe impl<'a> GcVisitor for CopyVisitor<'a> {
    type Err = !;

    fn visit_gc<T, Id>(&mut self, gc: &mut zerogc::Gc<'_, T, Id>) -> Result<(), Self::Err>
        where T: GcSafe, Id: CollectorId {
        match gc.id().try_cast::<CopyingCollectorId>() {
            Some(&other_id) => {
                // If other instanceof SimpleCollectorId, assert runtime match
                assert_eq!(other_id, self.id);
            }
            None => {
                return Ok(());  // Belongs to another collector
            }
        }
        let gc_type: &GcType = <T as StaticType>::GC_TYPE;
        let header = unsafe { &mut *GcHeader::from_value_ptr(gc.as_raw_ptr()) };
        let relocated_ptr = match header.state {
            ObjState::Active => {
                // The object has not been relocated!
                debug_assert!(!self.updated_chunk.contains(header as *mut _ as *mut u8));
                let raw_relocated = self.updated_chunk
                    .try_alloc(gc_type.complete_layout().size())
                    .unwrap().as_ptr();
                let relocated_header = raw_relocated as *mut GcHeader;
                unsafe {
                    relocated_header.write(GcHeader {
                        state: ObjState::Active,
                        info: HeaderInfo {
                            type_info: gc_type
                        }
                    });
                    raw_relocated.add(gc_type.value_offset())
                        .cast::<T>()
                        .copy_from_nonoverlapping(
                            header.value().cast::<T>(),
                            1
                        );
                }
                header.state = ObjState::Relocated;
                header.info.relocated = relocated_header;
                relocated_header
            },
            ObjState::Relocated => unsafe { header.info.relocated } // Already copied
        };
        debug_assert!(self.updated_chunk.is_used(relocated_ptr as *mut u8));
        Ok(())
    }
}

/// A simple collector can only be used from a single thread
impl !Send for RawCopyingCollector {}

pub struct CopyingCollectorContext {
    collector: Rc<RawCopyingCollector>
}
unsafe impl GcContext for CopyingCollectorContext {
    type Id = CopyingCollectorId;

    unsafe fn basic_safepoint<T: Trace>(&mut self, value: &mut T) {
        let dyn_ptr = self.collector.shadow_stack.borrow_mut().push(value);
        self.collector.maybe_collect();
        assert_eq!(
            self.collector.shadow_stack.borrow_mut().pop(),
            Some(dyn_ptr)
        );
    }

    unsafe fn recurse_context<T, F, R>(&self, value: &mut T, func: F) -> R
        where T: Trace, F: FnOnce(&mut Self, &Self, &mut T) -> R {
        let dyn_ptr = self.collector.shadow_stack.borrow_mut().push(value);
        let mut sub_context = CopyingCollectorContext {
            collector: self.collector.clone()
        };
        let result = func(&mut sub_context, &*self, value);
        drop(sub_context);
        assert_eq!(
            self.collector.shadow_stack.borrow_mut().pop(),
            Some(dyn_ptr)
        );
        result
    }
}
unsafe impl GcAllocContext for CopyingCollectorContext {
    // Right now we don't support failable alloc
    type MemoryErr = !;

    #[inline]
    fn alloc<T: GcSafe>(&self, value: T) -> Gc<'_, T> {
        self.collector.heap.allocator.alloc(value)
    }

    #[inline]
    fn try_alloc<T: GcSafe>(&self, value: T) -> Result<Gc<'_, T>, Self::MemoryErr> {
        Ok(self.alloc(value))
    }
}

struct GcType {
    value_layout: Layout,
    /// The trace function, or `None` if this type doesn't need to be traced
    trace: Option<unsafe fn(*mut c_void, &mut CopyVisitor)>,
    /// The drop function, or `None` if this type doesn't need to be dropped
    drop: Option<unsafe fn(*mut c_void)>
}
impl GcType {
    #[inline]
    const fn value_offset(&self) -> usize {
        let header_layout = Layout::new::<GcHeader>();
        header_layout.size() + header_layout
            .padding_needed_for(self.value_layout.align())
    }
    /// The complete layout of this object.
    ///
    /// Does not take into account the value's alignment
    #[inline]
    const fn complete_layout(&self) -> Layout {
        let header_layout = Layout::new::<GcHeader>();
        let offset = self.value_offset();
        unsafe {
            let trailing_padding = Layout::from_size_align_unchecked(
                offset, header_layout.align()
            ).padding_needed_for(std::mem::align_of::<GcHeader>());
            Layout::from_size_align_unchecked(
                offset + self.value_layout.size() + trailing_padding,
                header_layout.align()
            )
        }
    }
}
trait StaticType {
    const GC_TYPE: &'static GcType;
}
impl<T: GcSafe> StaticType for T {
    const GC_TYPE: &'static GcType = &GcType {
        value_layout: Layout::new::<T>(),
        trace: if T::NEEDS_TRACE {
            unsafe {
                Some(std::mem::transmute(
                    T::visit::<CopyVisitor> as fn(_, _) -> _
                ))
            }
        } else { None },
        drop: if std::mem::needs_drop::<T>() {
            // Erase from *mut T -> *mut c_void
            Some(unsafe { std::mem::transmute(std::ptr::drop_in_place::<T> as unsafe fn(_)) })
        } else {
            None
        }
    };
}
trait DynVisit {
    fn visit_copying(&mut self, visitor: &mut CopyVisitor);
    #[cfg(debug_assertions)]
    fn visit_debug(&mut self, visitor: &mut DebugVisitor);
}
impl<T: Trace> DynVisit for T {
    #[inline]
    fn visit_copying(&mut self, visitor: &mut CopyVisitor) {
        let Ok(()) = self.visit(visitor);
    }

    #[cfg(debug_assertions)]
    fn visit_debug(&mut self, visitor: &mut DebugVisitor) {
        let Ok(()) = self.visit(visitor);
    }
}

#[repr(C)]
union HeaderInfo {
    type_info: &'static GcType,
    relocated: *mut GcHeader
}

/// The header for a heap-allocated GC object
#[repr(C)]
struct GcHeader {
    info: HeaderInfo,
    // NOTE: This should come after to improve layout
    state: ObjState,
}
impl GcHeader {
    /// A pointer to the value, assuming the object hasn't been relocated
    #[inline]
    pub unsafe fn value(&self) -> *mut c_void {
        debug_assert_ne!(self.state, ObjState::Relocated);
        (self as *const Self as *mut Self as *mut u8)
            .add(self.info.type_info.value_offset())
            .cast()
    }
    #[inline]
    pub unsafe fn from_value_ptr<T: GcSafe>(value: *mut T) -> *mut GcHeader {
        let gc_type: &GcType = <T as StaticType>::GC_TYPE;
        (value as *mut u8).sub(gc_type.value_offset()) as *mut GcHeader
    }
    /// A pointer to the next value,
    /// assuming the object hasn't been relocated
    #[inline]
    pub unsafe fn wrapping_next_obj(&self) -> *mut GcHeader {
        debug_assert_ne!(self.state, ObjState::Relocated);
        let ptr = self as *const Self as *mut Self as *mut u8;
        let layout = self.info.type_info.complete_layout();
        ptr.add(layout.size())
            .add(layout.padding_needed_for(std::mem::align_of::<GcHeader>()))
            .cast()
    }
}

/// The state of the object
///
/// Either the object is relocated, or it is 'active'
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
#[repr(u8)]
enum ObjState {
    /// The object is 'active' and has not been relocated
    Active,
    /// The object has been relocated.
    ///
    /// Access its new location from `HeaderInfo::relocated`
    Relocated,
}
