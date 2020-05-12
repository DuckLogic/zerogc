// TODO: Use stable rust
#![feature(
    alloc_layout_extra, // Used for GcObject::from_raw
    never_type, // Used for errors (which are currently impossible)
    negative_impls, // impl !Send is much cleaner than PhantomData<Rc<()>>
    exhaustive_patterns, // Allow exhaustive matching against never
    drain_filter, // Used internally
    raw, // Gives the raw definition of trait objects
)]
use zerogc::{GarbageCollectionSystem, CollectorId, GcSafe, Trace, GcContext, GcVisitor, GcAllocContext};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::alloc::Layout;
use std::cell::{RefCell, Cell};
use std::ptr::NonNull;
use std::rc::Rc;

mod raw_arena;

/// An alias to `zerogc::Gc<T, SimpleCollectorId>` to simplify use with zerogc-simple
pub type Gc<'gc, T> = zerogc::Gc<'gc, T, SimpleCollectorId>;
/// A garbage collector that internally compacts memory
pub type CompactingCollector = SimpleCollector<CompactingAlloc>;
/// A context for a [CompactingCollector]
pub type CompactingCollectorContext = SimpleCollectorContext<CompactingAlloc>;
/// A garbage collector useful for debugging
pub type DebugCollector = SimpleCollector<DebugAlloc>;
pub type DebugCollectorContext = SimpleCollectorContext<DebugAlloc>;

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
pub struct SimpleCollector<A: SimpleAlloc = DebugAlloc>(Rc<RawSimpleCollector<A>>);
impl SimpleCollector {
    pub fn create() -> Self {
        Self::create_debug()
    }
    pub fn create_debug() -> Self {
        SimpleCollector::from_alloc(DebugAlloc::new)
    }
}
impl SimpleCollector<CompactingAlloc> {
    pub fn create_compacting() -> Self {
        SimpleCollector::from_alloc(CompactingAlloc::new)
    }
}
impl<A: SimpleAlloc> SimpleCollector<A> {
    fn from_alloc(func: fn(SimpleCollectorId) -> A) -> Self {
        let id = SimpleCollectorId::acquire();
        SimpleCollector(Rc::new(RawSimpleCollector {
            id, shadow_stack: RefCell::new(ShadowStack(Vec::new())),
            heap: GcHeap {
                threshold: Cell::new(INITIAL_COLLECTION_THRESHOLD),
                allocator: func(id)
            }
        }))
    }
    /// Make this collector into a context
    #[inline]
    pub fn into_context(self) -> SimpleCollectorContext<A> {
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

/// Used internally to dynamically dispatch to our visitors
#[doc(hidden)]
pub trait DynTrace {
    fn trace<'a>(&mut self, visitor: &mut MarkVisitor<'a>);
    fn compact(&mut self, visitor: &mut CompactVisitor);
}
impl<T: Trace + ?Sized> DynTrace for T {
    fn trace<'a>(&mut self, visitor: &mut MarkVisitor<'a>) {
        let Ok(()) = self.visit(visitor);
    }
    fn compact(&mut self, visitor: &mut CompactVisitor) {
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

struct GcHeap<A: SimpleAlloc> {
    threshold: Cell<usize>,
    allocator: A
}
impl<A: SimpleAlloc> GcHeap<A> {
    #[inline]
    fn should_collect(&self) -> bool {
        self.allocator.allocated_size() >= self.threshold.get()
    }
}

/// The internal trait for an allocator backend
#[doc(hidden)]
pub unsafe trait SimpleAlloc {
    fn allocated_size(&self) -> usize;
    fn alloc<T: GcSafe>(&self, value: T) -> Gc<'_, T>;
    unsafe fn sweep<'a>(&self, roots: &[*mut (dyn DynTrace + 'a)]);
}
#[doc(hidden)]
pub struct DebugAlloc {
    id: SimpleCollectorId,
    allocated_size: Cell<usize>,
    object_link: Cell<Option<NonNull<GcObject<dyn DynTrace>>>>
}
impl DebugAlloc {
    fn new(id: SimpleCollectorId) -> DebugAlloc {
        DebugAlloc {
            id, allocated_size: Cell::new(0),
            object_link: Cell::new(None)
        }
    }
}
unsafe impl SimpleAlloc for DebugAlloc {
    #[inline]
    fn allocated_size(&self) -> usize {
        self.allocated_size.get()
    }
    fn alloc<T: GcSafe>(&self, value: T) -> Gc<'_, T> {
        let mut object = Box::new(GcObject {
            value, state: MarkState::White, prev: self.object_link.get(),
        });
        let gc = unsafe { Gc::from_raw(
            NonNull::new_unchecked(&mut object.value),
            self.id
        ) };
        {
            let size = object.size();
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
                    expected_size -= obj.size();
                    drop(Box::from_raw(obj));
                },
                MarkState::Grey => panic!("All gray objects should've been processed"),
                MarkState::Black => {
                    // Retain the object
                    actual_size += obj.size();
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

#[doc(hidden)]
pub struct CompactingAlloc {
    id: SimpleCollectorId,
    arena: raw_arena::Arena,
    // NOTE: Doesn't include arena-internal padding
    approx_allocated_bytes: Cell<usize>,
    objects: RefCell<Vec<*mut GcObject<dyn DynTrace>>>
}
impl CompactingAlloc {
    pub fn new(id: SimpleCollectorId) -> Self {
        CompactingAlloc {
            id, arena: raw_arena::Arena::new(),
            approx_allocated_bytes: Cell::new(0),
            objects: RefCell::new(Vec::with_capacity(32))
        }
    }
}
unsafe impl SimpleAlloc for CompactingAlloc {
    #[inline]
    fn allocated_size(&self) -> usize {
        // NOTE: Arena::used_bytes is O(n) the number of chunks
        self.approx_allocated_bytes.get()
    }

    #[inline]
    fn alloc<T: GcSafe>(&self, value: T) -> Gc<'_, T> {
        /*
         * This is much faster than malloc, by a factor of 10x to 100x.
         * This is the compacting collector's main advantage.
         * We can inline since it's just a bounds check and pointer increment
         * in the common case.
         */
        let obj = self.arena.alloc(GcObject {
            value, state: MarkState::White,
            // TODO: Take advantage of linked list.....
            prev: None,
        });
        self.approx_allocated_bytes.set(self.approx_allocated_bytes.get() + obj.size());
        /*
         * TODO: How does this Vec::push impact efficiency?
         * It's certinally much simpler (and safer) than iterating
         * over the arena's raw memory in our reset() method.
         * We could add a linked list internal to GcObject, but that would
         * add memory overhead......
         */
        unsafe {
            self.objects.borrow_mut().push(GcObject::erase_pointer_lifetime(obj));
            Gc::from_raw(NonNull::from(&obj.value), self.id)
        }
    }

    unsafe fn sweep<'a>(&self, roots: &[*mut (dyn DynTrace + 'a)]) {
        // TODO: scopeguard to safely abort-on-panic
        assert!(self.arena.num_chunks() >= 1);
        let used_arena_bytes = self.arena.total_used();
        let mut objects = self.objects.borrow_mut();
        assert!(self.approx_allocated_bytes.get() <= used_arena_bytes);
        /*
         * Reset all chunks. Marking all their memory as 'unused'
         * This will not perform any writes or drops,
         * so its safe to use as temporary scratch.
         */
        {
            let chunks = self.arena.raw_chunks();
            for chunk in &*chunks {
                chunk.reset();
            }
        }
        /*
         * Ensure that the arena's 'current' chunk has enough memory
         * to hold all currently used bytes
         * After compaction, we want all memory confined to a single chunk.
         */
        if used_arena_bytes > self.arena.current_chunk_capacity() {
            self.arena.create_raw_chunk(used_arena_bytes);
        }
        // Clear old chunks
        let expected_num_chunks = self.arena.num_chunks();
        for &ptr in &**objects {
            let obj = &*ptr;
            debug_assert!(std::mem::align_of_val(obj) >= std::mem::align_of::<*mut u8>());
            let relocated_ptr = match obj.state {
                MarkState::White => {
                    /*
                     * This is an object we'll want to clear
                     * In a compacting collector, we can just ignore it :D
                     * However we do want to run the Drop functions/
                     * It implements the unsafe marker GcSafe, so it's destructor promises
                     * not to resurrect objects.
                     */
                    std::ptr::drop_in_place(ptr);
                    std::ptr::null_mut()
                },
                MarkState::Grey => {
                    panic!("All grey objects should've been marked")
                },
                MarkState::Black => {
                    let layout = Layout::for_value(obj);
                    let moved = self.arena.alloc_layout(layout);
                    std::ptr::copy_nonoverlapping(
                        ptr as *const u8,
                        moved,
                        layout.size()
                    );
                    moved
                }
            };
            let old_obj = std::mem::transmute::<
                *mut GcObject<dyn DynTrace>, std::raw::TraitObject
            >(ptr);
            std::ptr::write(old_obj.data as *mut *mut u8, relocated_ptr);
        }
        // Verify that moving memory didn't unexpectedly allocate any more chunks
        assert_eq!(expected_num_chunks, self.arena.num_chunks());
        // Cleanup and relocate our our internal list
        objects.drain_filter(|obj| {
            use std::raw::TraitObject;
            let old_obj = std::mem::transmute::<*mut _, TraitObject>(*obj);
            let relocated_ptr = std::ptr::read(old_obj.data as *mut *mut u8);
            if relocated_ptr.is_null() {
                true // drain
            } else {
                *obj = std::mem::transmute::<_, *mut GcObject<dyn DynTrace>>(TraitObject {
                    data: relocated_ptr as *mut (),
                    vtable: old_obj.vtable
                });
                // Update object to white to prepare for next collection
                (**obj).state = MarkState::White;
                false // do not drain
            }
        }).for_each(std::mem::drop); // ignore everything we drain
        // Actually perform relocations
        {
            let mut visitor = CompactVisitor {
                id: self.id
            };
            for &root in roots {
                (*root).compact(&mut visitor);
            }
        }
        self.arena.free_old_chunks();
        self.approx_allocated_bytes.set(self.arena.total_used());
        // We should've compacted down to one
        assert_eq!(self.arena.num_chunks(), 1);
    }
}

/// The internal data for a simple collector
struct RawSimpleCollector<A: SimpleAlloc> {
    id: SimpleCollectorId,
    shadow_stack: RefCell<ShadowStack>,
    heap: GcHeap<A>
}
impl<A: SimpleAlloc> RawSimpleCollector<A> {
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
    Object(*mut GcObject<dyn DynTrace>),
    Other(*mut dyn DynTrace)
}
struct CollectionTask<'a, A: SimpleAlloc> {
    id: SimpleCollectorId,
    roots: Vec<*mut dyn DynTrace>,
    gray_stack: Vec<GreyElement>,
    heap: &'a GcHeap<A>
}
impl<'a, A: SimpleAlloc> CollectionTask<'a, A> {
    fn run(&mut self) {
        self.gray_stack.extend(self.roots.iter()
            .map(|&ptr| GreyElement::Other(ptr)));
        // Mark
        while let Some(target) = self.gray_stack.pop() {
            let target_value = match target {
                GreyElement::Object(obj) => {
                    unsafe {
                        debug_assert_eq!((*obj).state, MarkState::Grey);
                        &mut (*obj).value as &mut dyn DynTrace
                    }
                },
                GreyElement::Other(target) => {
                    unsafe { &mut *target }
                }
            };
            let mut visitor = MarkVisitor {
                id: self.id,
                gray_stack: &mut self.gray_stack,
            };
            (*target_value).trace(&mut visitor);
            match target {
                GreyElement::Object(obj) => {
                    unsafe {
                        // Mark the object black now it's been traced
                        debug_assert_eq!((*obj).state, MarkState::Grey);
                        (*obj).state = MarkState::Black;
                    }
                },
                GreyElement::Other(_) => {},
            }
        }
        // Sweep
        unsafe { self.heap.allocator.sweep(&self.roots) };
        let updated_size = self.heap.allocator.allocated_size();
        // Update the threshold to be 150% of currently used size
        self.heap.threshold.set(updated_size + (updated_size / 2));
    }
}

#[doc(hidden)] // Used by DynTrace
pub struct CompactVisitor {
    id: SimpleCollectorId
}
unsafe impl GcVisitor for CompactVisitor {
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
        debug_assert!(std::mem::align_of::<GcObject<T>>() >= std::mem::align_of::<*mut GcObject<T>>());
        unsafe {
            let old_ptr = GcObject::ptr_from_raw(gc.as_raw_ptr());
            let updated_location = (old_ptr as *mut *mut GcObject<T>).read();
            // Check it's state has been updated to white :)
            debug_assert_eq!((*updated_location).state, MarkState::White);
            // Actually perform the relocation
            let updated_gc = Gc::new(self.id, NonNull::new_unchecked(&mut (*updated_location).value));
            // We know `Id == SimpleCollectorId`
            *gc = std::mem::transmute_copy::<Gc<T>, zerogc::Gc<T, Id>>(&updated_gc);
        }
        Ok(())
    }
}
#[doc(hidden)] // Used by DynTrace
pub struct MarkVisitor<'a> {
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
                    self.gray_stack.push(GreyElement::Object(
                        unsafe {
                            std::mem::transmute::<_, *mut GcObject<(dyn DynTrace + 'static)>>(
                                &mut *obj as *mut _ as *mut GcObject<dyn DynTrace>
                            )
                        }
                    ));
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
impl<A: SimpleAlloc> !Send for RawSimpleCollector<A> {}

pub struct SimpleCollectorContext<A: SimpleAlloc = DebugAlloc> {
    collector: Rc<RawSimpleCollector<A>>
}
unsafe impl<A: SimpleAlloc> GcContext for SimpleCollectorContext<A> {
    type Id = SimpleCollectorId;

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
        let mut sub_context = SimpleCollectorContext {
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
unsafe impl<A: SimpleAlloc> GcAllocContext for SimpleCollectorContext<A> {
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

/// A heap-allocated GC object
struct GcObject<T: ?Sized + DynTrace> {
    state: MarkState,
    /// The previous object in the linked list of allocated objects,
    /// or null if its the end
    ///
    /// If this object has been relocated,
    /// then it points to the updated location
    prev: Option<NonNull<GcObject<dyn DynTrace>>>,
    value: T
}
impl<T: ?Sized + DynTrace> GcObject<T> {
    #[inline]
    pub fn size(&self) -> usize {
        std::mem::size_of_val(self)
    }
}
impl<T: DynTrace> GcObject<T> {
    #[inline]
    unsafe fn erase_box_lifetime(val: Box<Self>) -> Box<GcObject<(dyn DynTrace + 'static)>> {
        std::mem::transmute::<_, Box<GcObject<(dyn DynTrace + 'static)>>>(
            val as Box<GcObject<(dyn DynTrace + '_)>>
        )
    }
    #[inline]
    unsafe fn erase_pointer_lifetime(ptr: *mut Self) -> *mut GcObject<(dyn DynTrace + 'static)> {
        std::mem::transmute::<_, *mut GcObject<(dyn DynTrace + 'static)>>(
            ptr as *mut GcObject<(dyn DynTrace + '_)>
        )
    }
    #[inline]
    pub unsafe fn ptr_from_raw(raw: *mut T) -> *mut GcObject<T> {
        let result = raw.cast::<u8>()
            .sub(object_value_offset(&*raw))
            .cast::<GcObject<T>>();
        // Debug verify object_value_offset is working
        debug_assert_eq!(&mut (*result).value as *mut T, raw);
        result
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
