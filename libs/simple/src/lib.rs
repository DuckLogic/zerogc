// TODO: Use stable rust
#![feature(
    alloc_layout_extra, // Used for GcObject::from_raw
    never_type, // Used for errors (which are currently impossible)
    negative_impls, // impl !Send is much cleaner than PhantomData<Rc<()>>
    exhaustive_patterns, // Allow exhaustive matching against never
)]
use zerogc::{GarbageCollectionSystem, CollectorId, GcSafe, Trace, GcContext, GcVisitor, TraceImmutable, GcAllocContext};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::alloc::Layout;
use std::cell::RefCell;
use std::ptr::NonNull;
use std::rc::Rc;

/// An alias to `zerogc::Gc<T, SimpleCollectorId>` to simplify use with zerogc-simple
pub type Gc<'gc, T> = zerogc::Gc<'gc, T, SimpleCollectorId>;

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
    pub fn create() -> SimpleCollector {
        let id = SimpleCollectorId::acquire();
        SimpleCollector(Rc::new(RawSimpleCollector {
            id, shadow_stack: RefCell::new(ShadowStack(Vec::new())),
            heap: RefCell::new(GcHeap {
                allocated_size: 0,
                last_retained_size: None,
                objects: Vec::with_capacity(32)
            })
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
    fn trace<'a>(&mut self, visitor: &mut CollectionTask<'a>);
}
impl<T: Trace + ?Sized> DynTrace for T {
    fn trace<'a>(&mut self, visitor: &mut CollectionTask<'a>) {
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

struct GcHeap {
    allocated_size: usize,
    last_retained_size: Option<usize>,
    objects: Vec<Box<GcObject<dyn DynTrace>>>,
}
impl GcHeap {
    fn should_collect(&self) -> bool {
        /// The minimum memory usage to start a collection
        const MIN_COLLECTION_THRESHOLD: usize = 8192;
        match self.last_retained_size {
            None => self.allocated_size >= MIN_COLLECTION_THRESHOLD,
            Some(last_size) => {
                if self.allocated_size < MIN_COLLECTION_THRESHOLD {
                    return false
                }
                let extra_usage = self.allocated_size - last_size;
                // Trigger if we are 50% larger than the last retained size
                extra_usage >= last_size / 2
            }
        }
    }
}

/// The internal data for a simple collector
struct RawSimpleCollector {
    id: SimpleCollectorId,
    shadow_stack: RefCell<ShadowStack>,
    heap: RefCell<GcHeap>
}
impl RawSimpleCollector {
    #[inline]
    unsafe fn maybe_collect(&self) {
        if self.heap.borrow().should_collect() {
            self.perform_collection()
        }
    }
    #[cold]
    #[inline(never)]
    unsafe fn perform_collection(&self) {
        let mut heap = self.heap.borrow_mut();
        let mut task = CollectionTask {
            id: self.id,
            gray_stack: Vec::with_capacity(64),
            heap: &mut *heap
        };
        let shadow_stack = self.shadow_stack.borrow();
        for &root in &shadow_stack.0 {
            task.gray_stack.push(GreyElement::Other(root));
        }
        task.run();
    }
}
enum GreyElement {
    Object(*mut GcObject<dyn DynTrace>),
    Other(*mut dyn DynTrace)
}
struct CollectionTask<'a> {
    id: SimpleCollectorId,
    gray_stack: Vec<GreyElement>,
    heap: &'a mut GcHeap
}
impl<'a> CollectionTask<'a> {
    fn run(&mut self) {
        if cfg!(debug_assertions) {
            // Verify everything is white
            for obj in &self.heap.objects {
                assert_eq!(obj.state, MarkState::White);
            }
        }
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
            (*target_value).trace(self);
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
        let mut expected_size = self.heap.allocated_size;
        let mut actual_size = 0;
        self.heap.objects.retain(|obj| {
            match obj.state {
                MarkState::White => {
                    // Free the object
                    expected_size -= obj.size();
                    false
                },
                MarkState::Grey => panic!("All grey objects should've been processed"),
                MarkState::Black => {
                    // Retain the object
                    actual_size += obj.size();
                    true
                }
            }
        });
        // Reset all remaining objects to white
        for obj in &mut self.heap.objects {
            obj.state = MarkState::White;
        }
        assert_eq!(expected_size, actual_size);
        self.heap.allocated_size = actual_size;
        self.heap.last_retained_size = Some(actual_size);
    }
}
unsafe impl<'a> GcVisitor for CollectionTask<'a> {
    type Err = !;

    #[inline(always)]
    fn visit<T: Trace + ?Sized>(&mut self, value: &mut T) -> Result<(), Self::Err> {
        value.visit(self)
    }

    #[inline(always)]
    fn visit_immutable<T: TraceImmutable + ?Sized>(&mut self, value: &T) -> Result<(), Self::Err> {
        value.visit_immutable(self)
    }

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
impl !Send for RawSimpleCollector {}

pub struct SimpleCollectorContext {
    collector: Rc<RawSimpleCollector>
}
unsafe impl GcContext for SimpleCollectorContext {
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
unsafe impl GcAllocContext for SimpleCollectorContext {
    // Right now Box doesn't implement failable alloc
    type MemoryErr = !;

    fn alloc<T: GcSafe>(&self, value: T) -> Gc<'_, T> {
        let mut object = Box::new(GcObject {
            value, state: MarkState::White
        });
        let gc = unsafe { Gc::from_raw(
            NonNull::new_unchecked(&mut object.value),
            self.collector.id
        ) };
        {
            let mut heap = self.collector.heap.borrow_mut();
            let size = object.size();
            heap.objects.push(unsafe { object.into_erased_static() });
            heap.allocated_size += size;
        }
        gc
    }

    #[inline]
    fn try_alloc<T: GcSafe>(&self, value: T) -> Result<Gc<'_, T>, Self::MemoryErr> {
        Ok(self.alloc(value))
    }
}

/// A heap-allocated GC object
struct GcObject<T: ?Sized + DynTrace> {
    state: MarkState,
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
    pub unsafe fn into_erased_static(self: Box<Self>) -> Box<GcObject<(dyn DynTrace + 'static)>> {
        std::mem::transmute::<_, Box<GcObject<(dyn DynTrace + 'static)>>>(
            self as Box<GcObject<(dyn DynTrace + '_)>>
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
