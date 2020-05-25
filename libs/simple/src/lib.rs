// TODO: Use stable rust
#![feature(
    alloc_layout_extra, // Used for GcObject::from_raw
    never_type, // Used for errors (which are currently impossible)
    negative_impls, // impl !Send is much cleaner than PhantomData<Rc<()>>
    exhaustive_patterns, // Allow exhaustive matching against never
    const_alloc_layout, // Used for StaticType
    const_if_match, // Used for StaticType
    const_transmute, // This can already be acheived with unions...
)]
use zerogc::{GarbageCollectionSystem, CollectorId, GcSafe, Trace, GcContext, GcVisitor, GcAllocContext};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::alloc::Layout;
use std::cell::{RefCell, Cell};
use std::ptr::NonNull;
use std::rc::Rc;
use std::os::raw::c_void;
use std::mem::transmute;
use std::mem;

/// An alias to `zerogc::Gc<T, SimpleCollectorId>` to simplify use with zerogc-simple
pub type Gc<'gc, T> = zerogc::Gc<'gc, T, SimpleCollectorId>;

/// A garbage collector that internally compacts memory
static NEXT_COLLECTOR_ID: AtomicUsize = AtomicUsize::new(0);

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct SimpleCollectorId(usize);
impl SimpleCollectorId {
    fn acquire() -> SimpleCollectorId {
        loop {
            let prev = NEXT_COLLECTOR_ID.load(Ordering::SeqCst);
            let updated = prev.checked_add(1).expect("Overflow collector ids");
            if NEXT_COLLECTOR_ID.compare_and_swap(prev, updated, Ordering::SeqCst) == prev {
                break SimpleCollectorId(prev)
            }
        }
    }
}
unsafe impl CollectorId for SimpleCollectorId {}
pub struct SimpleCollector(Rc<RawSimpleCollector>);
impl SimpleCollector {
    pub fn create() -> Self {
        let id = SimpleCollectorId::acquire();
        SimpleCollector(Rc::new(RawSimpleCollector {
            id, shadow_stack: RefCell::new(ShadowStack(Vec::new())),
            heap: GcHeap {
                threshold: Cell::new(INITIAL_COLLECTION_THRESHOLD),
                allocator: SimpleAlloc::new(id)
            }
        }))
    }
    /// Make this collector into a context
    #[inline]
    pub fn into_context(self) -> SimpleCollectorContext {
        SimpleCollectorContext {
            collector: self.0
        }
    }
}

unsafe impl GarbageCollectionSystem for SimpleCollector {
    type Id = SimpleCollectorId;

    #[inline]
    fn id(&self) -> Self::Id {
        self.0.id
    }
}

trait DynTrace {
    fn trace<'a>(&mut self, visitor: &mut MarkVisitor<'a>);
}
impl<T: Trace + ?Sized> DynTrace for T {
    fn trace<'a>(&mut self, visitor: &mut MarkVisitor<'a>) {
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
    threshold: Cell<usize>,
    allocator: SimpleAlloc
}
impl GcHeap {
    #[inline]
    fn should_collect(&self) -> bool {
        self.allocator.allocated_size() >= self.threshold.get()
    }
}

pub struct SimpleAlloc {
    id: SimpleCollectorId,
    allocated_size: Cell<usize>,
    object_link: Cell<Option<NonNull<GcObject>>>
}
impl SimpleAlloc {
    fn new(id: SimpleCollectorId) -> SimpleAlloc {
        SimpleAlloc {
            id,
            allocated_size: Cell::new(0),
            object_link: Cell::new(None)
        }
    }
    #[inline]
    fn allocated_size(&self) -> usize {
        self.allocated_size.get()
    }
    fn alloc<T: GcSafe>(&self, value: T) -> Gc<'_, T> {
        let mut object = Box::new(GcObject {
            static_value: value, type_info: T::STATIC_TYPE,
            state: MarkState::White, prev: self.object_link.get(),
        });
        let gc = unsafe { Gc::from_raw(
            NonNull::new_unchecked(&mut object.static_value),
            self.id
        ) };
        {
            let size = std::mem::size_of::<GcObject<T>>();
            unsafe { self.object_link.set(Some(NonNull::new_unchecked(
                Box::into_raw(GcObject::erase_box_lifetime(object))
            ))); }
            self.allocated_size.set(self.allocated_size() + size);
        }
        gc
    }
    unsafe fn sweep<'a>(&self, _roots: &[*mut (dyn DynTrace + 'a)]) {
        let mut expected_size = self.allocated_size.get();
        let mut actual_size = 0;
        let mut last_linked = None;
        while let Some(link) = self.object_link.get() {
            let obj = &mut *link.as_ptr();
            self.object_link.set(obj.prev);
            match obj.state {
                MarkState::White => {
                    // Free the object
                    expected_size -= obj.dyn_total_size();
                    drop(Box::from_raw(obj));
                },
                MarkState::Grey => panic!("All gray objects should've been processed"),
                MarkState::Black => {
                    // Retain the object
                    actual_size += obj.dyn_total_size();
                    obj.prev = last_linked;
                    last_linked = Some(NonNull::from(&mut *obj));
                    // Reset state to white
                    obj.state = MarkState::White;
                }
            }
        }
        self.object_link.set(last_linked);
        assert_eq!(expected_size, actual_size);
        self.allocated_size.set(actual_size);
    }
}

/// The internal data for a simple collector
struct RawSimpleCollector {
    id: SimpleCollectorId,
    shadow_stack: RefCell<ShadowStack>,
    heap: GcHeap
}
impl RawSimpleCollector {
    #[inline]
    unsafe fn maybe_collect(&self) {
        if self.heap.should_collect() {
            self.perform_collection()
        }
    }
    #[cold]
    #[inline(never)]
    unsafe fn perform_collection(&self) {
        let mut task = CollectionTask {
            id: self.id,
            roots: self.shadow_stack.borrow().0.clone(),
            gray_stack: Vec::with_capacity(64),
            heap: &self.heap
        };
        task.run();
    }
}
enum GreyElement {
    Object(*mut GcObject),
    Other(*mut dyn DynTrace)
}
struct CollectionTask<'a> {
    id: SimpleCollectorId,
    roots: Vec<*mut dyn DynTrace>,
    gray_stack: Vec<GreyElement>,
    heap: &'a GcHeap
}
impl<'a> CollectionTask<'a> {
    fn run(&mut self) {
        self.gray_stack.extend(self.roots.iter()
            .map(|&ptr| GreyElement::Other(ptr)));
        // Mark
        while let Some(target) = self.gray_stack.pop() {
            let mut visitor = MarkVisitor {
                id: self.id,
                gray_stack: &mut self.gray_stack,
            };
            match target {
                GreyElement::Object(obj) => {
                    unsafe {
                        debug_assert_eq!((*obj).state, MarkState::Grey);
                        ((*obj).type_info.trace_func)(
                            &mut *(*obj).dyn_value(),
                            &mut visitor
                        );
                        // Mark the object black now it's innards have been traced
                        (*obj).state = MarkState::Black;
                    }
                },
                GreyElement::Other(target) => {
                    // Dynamically dispatched
                    unsafe { (*target).trace(&mut visitor); }
                }
            };
        }
        // Sweep
        unsafe { self.heap.allocator.sweep(&self.roots) };
        let updated_size = self.heap.allocator.allocated_size();
        // Update the threshold to be 150% of currently used size
        self.heap.threshold.set(updated_size + (updated_size / 2));
    }
}

struct MarkVisitor<'a> {
    id: SimpleCollectorId,
    gray_stack: &'a mut Vec<GreyElement>,
}
unsafe impl<'a> GcVisitor for MarkVisitor<'a> {
    type Err = !;

    fn visit_gc<T, Id>(&mut self, gc: &mut zerogc::Gc<'_, T, Id>) -> Result<(), Self::Err>
        where T: GcSafe, Id: CollectorId {
        match gc.id().try_cast::<SimpleCollectorId>() {
            Some(&other_id) => {
                // If other instanceof SimpleCollectorId, assert runtime match
                assert_eq!(other_id, self.id);
            }
            None => {
                return Ok(());  // Belongs to another collector
            }
        }
        let obj = unsafe { &mut *GcObject::ptr_from_raw(gc.as_raw_ptr()) };
        match obj.state {
            MarkState::White => {
                if !T::NEEDS_TRACE {
                    /*
                     * We don't need to mark this grey
                     * It has no internals that need to be traced.
                     * We can directly move it directly to the black set
                     */
                    obj.state = MarkState::Black;
                } else {
                    /*
                     * We need to mark this object grey and push it onto the grey stack.
                     * It will be processed later
                     */
                    obj.state = MarkState::Grey;
                    self.gray_stack.push(GreyElement::Object(obj.as_dynamic()));
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
        Ok(())
    }
}

/// A simple collector can only be used from a single thread
impl !Send for RawSimpleCollector {}

pub struct SimpleCollectorContext {
    collector: Rc<RawSimpleCollector>
}
unsafe impl GcContext for SimpleCollectorContext {
    type Id = SimpleCollectorId;

    unsafe fn basic_safepoint<T: Trace>(&mut self, value: &mut &mut T) {
        let dyn_ptr = self.collector.shadow_stack.borrow_mut().push(value);
        self.collector.maybe_collect();
        assert_eq!(
            self.collector.shadow_stack.borrow_mut().pop(),
            Some(dyn_ptr)
        );
    }

    unsafe fn recurse_context<T, F, R>(&self, value: &mut &mut T, func: F) -> R
        where T: Trace, F: for<'gc> FnOnce(&'gc mut Self, &'gc mut T) -> R {
        let dyn_ptr = self.collector.shadow_stack.borrow_mut().push(value);
        let mut sub_context = SimpleCollectorContext {
            collector: self.collector.clone()
        };
        let result = func(&mut sub_context, value);
        drop(sub_context);
        assert_eq!(
            self.collector.shadow_stack.borrow_mut().pop(),
            Some(dyn_ptr)
        );
        result
    }
}
unsafe impl GcAllocContext for SimpleCollectorContext {
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
    value_size: usize,
    value_offset: usize,
    trace_func: unsafe fn(*mut c_void, &mut MarkVisitor),
    drop_func: Option<unsafe fn(*mut c_void)>,
}
trait StaticGcType {
    const VALUE_OFFSET: usize;
    const STATIC_TYPE: &'static GcType;
}
impl<T: GcSafe> StaticGcType for T {
    const VALUE_OFFSET: usize = {
        let layout = Layout::new::<GcObject<()>>();
        layout.size() + layout.padding_needed_for(std::mem::align_of::<T>())
    };
    const STATIC_TYPE: &'static GcType = &GcType {
        value_size: std::mem::size_of::<T>(),
        value_offset: Self::VALUE_OFFSET,
        trace_func: unsafe { transmute::<_, unsafe fn(*mut c_void, &mut MarkVisitor)>(
            <T as DynTrace>::trace as fn(&mut T, &mut MarkVisitor),
        ) },
        drop_func: if std::mem::needs_drop::<T>() {
            unsafe { Some(transmute::<_, unsafe fn(*mut c_void)>(
                std::ptr::drop_in_place::<T> as unsafe fn(*mut T)
            )) }
        } else { None }
    };
}

/// Marker for an unknown GC object
struct DynamicObj;

/// A heap-allocated GC object
#[repr(C)]
struct GcObject<T = DynamicObj> {
    state: MarkState,
    type_info: &'static GcType,
    /// The previous object in the linked list of allocated objects,
    /// or null if its the end
    ///
    /// If this object has been relocated,
    /// then it points to the updated location
    prev: Option<NonNull<GcObject>>,
    static_value: T
}
impl GcObject<DynamicObj> {
    #[inline]
    pub const fn dyn_total_size(&self) -> usize {
        mem::size_of::<GcObject<()>>() + self.type_info.value_size
    }
}
impl<T> GcObject<T> {
    // NOTE: Drop impl needs this generic for all T
    #[inline]
    pub fn dyn_value(&self) -> *mut c_void {
        unsafe {
            (self as *const GcObject<T> as *mut GcObject<T> as *mut u8)
                .add(self.type_info.value_offset)
                .cast::<c_void>()
        }
    }
}
impl<T: DynTrace> GcObject<T> {
    #[inline]
    fn as_dynamic(&self) -> *mut GcObject<DynamicObj> {
        // Just cast....
        self as *const Self as *mut GcObject<T> as *mut GcObject<DynamicObj>
    }
    #[inline]
    unsafe fn erase_box_lifetime(val: Box<Self>) -> Box<GcObject<DynamicObj>> {
        std::mem::transmute::<Box<GcObject<T>>, Box<GcObject<DynamicObj>>>(val)
    }
    #[inline]
    pub unsafe fn ptr_from_raw(raw: *mut T) -> *mut GcObject<T> {
        let result = raw.cast::<u8>()
            .sub(object_value_offset(&*raw))
            .cast::<GcObject<T>>();
        // Debug verify object_value_offset is working
        debug_assert_eq!(&mut (*result).static_value as *mut T, raw);
        result
    }
}
impl<T> Drop for GcObject<T> {
    fn drop(&mut self) {
        // NOTE: We have to dynamic drop in every case....
        unsafe {
            if let Some(drop_func) = self.type_info.drop_func {
                drop_func(self.dyn_value())
            }
        }
    }
}
#[inline]
fn object_value_offset<T: ?Sized>(val: &T) -> usize {
    // Based on Arc::from_raw
    let type_alignment = std::mem::align_of_val(val);
    let layout = Layout::new::<GcObject<()>>();
    layout.size() + layout.padding_needed_for(type_alignment)
}

/// The current mark state of the object
///
/// See [Tri Color Marking](https://en.wikipedia.org/wiki/Tracing_garbage_collection#Tri-color_marking)
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
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
