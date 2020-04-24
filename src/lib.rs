//! Zero overhead tracing garbage collection for rust, by abusing the borrow checker.
//!
//!
//! The idea behind this collector that by making all potential for collections explicit,
//! we can take advantage of the borrow checker to statically guarantee everything's valid.
//! We call these potential collections 'safepoints' and the borrow checker can statically prove all uses are valid in between.
//! There is no chance for collection to happen in between these safepoints,
//! and you have unrestricted use of garbage collected pointers until you reach one.
//!
//! ## Major Features
//! 1. Easy to use, since `Gc<T>` is `Copy` and coerces to a reference.
//! 2. Absolutely zero overhead when modifying pointers, since `Gc<T>` is `Copy`.
//! 3. Support for important libraries builtin to the collector
//! 4. Unsafe code has complete freedom to manipulate garbage collected pointers, and it doesn't need to understand the distinction
//! 5. Uses rust's lifetime system to ensure all roots are known at explicit safepoints, without any runtime overhead.
//! 6. Collection can only happen with an explicit `safepoint` call and has no overhead between these calls,
//! 7. Optional graceful handling of allocation failures.
//!
//! ## Usage
//! ````rust
//! let collector = GarbageCollector::default();
//! let retained: Gc<Vec<u32>> = collector.alloc(vec![1, 2, 3]);
//! assert_eq!(retained[0], 1) // Garbage collected references deref directly to slices
//! let other =
//! /*
//!  * Garbage collect `retained`
//!  * This will explicitly trace the retained object `retained`,
//!  * and sweep the rest (`sweeped`) away.
//!  */
//! safepoint!(collector, retained);
//! retained.; // This borrow checker will allow this since `retained` was retained
//! ````

/*
 * Since our compiler plugin's going to eventually require nightly directly,
 * we might as well just require nightly for the collector itself.
 * However, I want this library to use 'mostly' stable features,
 * unless there's good justification to use an unstable feature.
 */
#![feature(
    const_fn, // I refuse to break encapsulation
    optin_builtin_traits, // These are much clearer to use.
    trace_macros // This is a godsend for debugging
)]
extern crate unreachable;
extern crate num_traits;
extern crate core;


use std::mem::{self, ManuallyDrop};
use std::ptr::NonNull;
use std::ops::Deref;
use std::marker::PhantomData;
use std::cell::{RefCell, Cell};
use std::fmt::{self, Display, Formatter};

#[macro_use]
pub mod safepoints;
#[macro_use]
mod manually_traced;
mod alloc;
pub mod cell;


use self::alloc::{GcObject, GcHeader, GcHeap};
pub use self::cell::{GcCell, GcRefCell};

mod utils;


use safepoints::{GcErase, GcUnErase, SafepointBag, SafepointState, SafepointId};
use utils::{AtomicIdCounter, IdCounter, debug_unreachable};
use utils::math::{CheckedMath, OverflowError};

/// Potentially begin a garbage collection,
/// treating the specified variables as the roots of garbage collection
///
/// ## Safety
/// This is completely safe because `possible_collection` is,
/// and it operates in terms of that macro's abstractions.
/// In other words `safepoint!(collector, first, second)` directly expands to
/// ````
/// let (first, second) = possible_collection!(collector, first, second);
/// ````
#[macro_export]
macro_rules! safepoint {
    ($collector:expr, $($var:ident),*) => {
        possible_collection($collector, $($var),*)
    };
}

/// The underlying macro behind `safepoint!`,
/// which recreates the values after potentially performing garbage collection on them.
///
/// The specified objects are treated as the roots of the collection,
/// invalidating all other garbage collected pointers by 'mutating' the collector.
///
/// ## Safety
/// This macro expansion operates in terms of completely safe abstractions,
/// and expands to no unsafe code.
/// It is the underlying abstraction behind `safepoint!`,
/// which is recomended for clarity in most circumstances.
/// This is because the borrow checker considers the old objects
/// Once the safepoint is created on the specified collector `$collector`
///
/// However, the downside is the error messages from incorrectly using this will be incredibility cryptic,
/// involving the invocation of hidden methods and hidden traits.
/// Until documentation is written on these indecipherable error messages,
/// you'll pretty much have to guess what is wrong with your code.
/// However, you can rest assured that the abstractions invoked by this macro is completely safe,
/// and all errors involving incorrect usage will be completely caught at compile time.
#[macro_export]
#[doc(hidden)] // This is unstable
macro_rules! possible_collection {
    ($collector:expr, $value:expr) => {
        let collector = $collector;
        let safepoint = collector.initialize_safepoint($value);
        collector.activate_safepoint(safepoint);
        collector.finish_safepoint(safepoint);
    };
}

/// Configures the garbage collector
#[derive(Copy, Clone, PartialEq, Debug)]
pub struct GcConfig {
    /// The initial size of the garbage collector's heap
    pub initial_heap: usize,
    /// The maximum size of the garbage collector's heap or `None` if unlimited.
    ///
    /// Attempting to go over this limit will trigger a panic.
    pub maximum_heap: Option<usize>,
    /// The threshold for collection-related things.
    // TODO: Document and tune this better
    pub collection_threshold: f64,
}

impl GcConfig {
    #[inline]
    pub fn new(initial_heap: usize, maximum_heap: Option<usize>) -> Self {
        GcConfig {
            initial_heap,
            maximum_heap,
            collection_threshold: 0.25,
        }
    }
    fn validate(&self) {
        if let Some(maximum_heap) = self.maximum_heap {
            assert!(self.initial_heap <= maximum_heap);
        }
        assert!(
            self.collection_threshold >= 0.0 && self.collection_threshold <= 1.0,
            "Invalid collection threshold: {}",
            self.collection_threshold
        );
    }
    fn compute_capacity(&self, used_capacity: usize, additional_capacity: usize) -> Result<usize, OverflowError> {
        let needed_capacity = used_capacity.add(additional_capacity)?;
        let prefered_capacity = self.initial_heap.max(
            needed_capacity.round_up_checked(self.initial_heap)?);
        Ok(match self.maximum_heap {
            Some(maximum_heap) => maximum_heap.min(prefered_capacity),
            None => prefered_capacity
        })
    }
}

impl Default for GcConfig {
    #[inline]
    fn default() -> Self {
        GcConfig::new(1024 * 1024, Some(8 * 1024 * 1024))
    }
}

static COLLECTOR_COUNTER: AtomicIdCounter<u16> = AtomicIdCounter::new();

/// The globally unique id of this garbage collector,
/// which is used to brand everything this collector create.
///
/// We count these ids very carefully to ensure there are never any duplicates,
/// and if we run out of ids creating a new collector will panic.
/// This verifies that each object is being used with the proper collector,
/// and not some other completely unexpected garbage collector.
///
/// This prevents garbage collected pointers from one collector being accidentally
/// or intentionally used with an unexpected collector, which could cause undefined behavior.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
#[doc(hidden)]  // This is unstable
pub struct CollectorId(u16);

/// A completely safe, zero-overhead [garbage collector](https://en.wikipedia.org/wiki/Garbage_collection_\(computer_science\))
/// for rust.
///
/// See module documentation for how to use this.
pub struct GarbageCollector {
    id: CollectorId,
    config: GcConfig,
    state: GarbageCollectorState,
    heap: RefCell<GcHeap>,
    /// Counts the number of safepoints, to give them all unique ids
    safepoint_counter: IdCounter<u64>,
    /// The currently active safepoint, which needs to be finished before we can start a new one.
    current_safepoint: Cell<Option<SafepointId>>,
    /// The threshold before we need to consider garbage collection.
    collection_threshold: Cell<usize>,
    marker: PhantomData<*mut ()>,
}

impl GarbageCollector {
    pub fn new(config: GcConfig) -> Self {
        config.validate();
        let id = CollectorId(COLLECTOR_COUNTER.try_next().expect("Too many collectors"));
        GarbageCollector {
            config: config.clone(),
            id, state: GarbageCollectorState::Allocating(AllocationState {
                expected_header: GcHeader::new(id, false)
            }),
            heap: RefCell::new(GcHeap::with_capacity(config.initial_heap)),
            safepoint_counter: IdCounter::new(),
            current_safepoint: Cell::new(None),
            collection_threshold: Cell::new(((config.initial_heap as f64) / config.collection_threshold).ceil() as usize),
            marker: PhantomData,
        }
    }
    /// Give the globally unique id of the collector.
    ///
    /// This is guaranteed to be unique for two different collectors in the same process,
    /// but not nautically unique between different processes.
    #[inline]
    pub fn id(&self) -> CollectorId {
        self.id
    }
    /// Allocate the specified object in this garbage collector,
    /// binding it to the lifetime of this collector..
    ///
    /// Collections will never happen until the next safepoint,
    /// which is considered a mutation by the borrow checker and will be statically checked.
    /// Therefore, we can statically guarantee the pointers will survive until the next safepoint.
    /// See `safepoint!` docs on how to properly handle this.
    ///
    /// This gives a immutable reference to the resulting object.
    /// Once allocated, the object can only be correctly modified with a `GcCell`
    #[inline]
    pub fn alloc<T: GarbageCollected>(&self, value: T) -> Gc<T> {
        self.try_alloc(value).unwrap_or_else(|e| e.fail())
    }
    #[inline]
    pub fn try_alloc<T: GarbageCollected>(&self, value: T) -> Result<Gc<T>, GcMemoryError> {
        /*
         * This is safe, because the alternative state is tracing,
         * and it's already considered undefined behavior to allocate while tracing.
         */
        let state = unsafe { self.assume_allocating() };
        let mut heap = self.heap.borrow_mut();
        if heap.can_alloc(GcHeap::size_of::<T>()) {
            self.expand_heap(&mut *heap, GcHeap::size_of::<T>());
        }
        unsafe {
            Ok(Gc::new(heap.try_alloc(state.expected_header, value)?))
        }
    }
    #[cold]
    #[inline(never)]
    fn expand_heap(&self, heap: &mut GcHeap, additional_capacity: usize) {
        assert_eq!(self.current_safepoint.get(), None, "Safepoint in progress!");
        if let Ok(increased_capacity) = self.config.compute_capacity(heap.current_size(), additional_capacity) {
            assert!(increased_capacity >= heap.limit);
            heap.limit = increased_capacity;
            self.collection_threshold.set(((increased_capacity as f64) /
                self.config.collection_threshold).ceil() as usize);
        }
    }
    /// Trace the specified value, preventing it from being destroyed by garbage collection.
    ///
    /// This method always ignores types that don't need to be garbage collected or traced.
    ///
    /// ## Safety
    /// This method is unsafe, since it's completely undefined behavior to call it
    /// unless the collector asks you to trace yourselves
    ///
    /// This method always verifies that it actually owns its input before tracing it,
    /// so it's safe to call it even with garbage collected objects that you don't actually own.
    /// The garbage collector optimistically assumes it owns the data it's tracing,
    /// but we always check to make sure once we reach the pointers.
    /// This is the equivelant of a bounds check for the pointers,
    /// since there could be multiple garbage collectors
    /// and the user accidentally allocated from the wrong one.
    /// At that point we panic and stop collection,
    /// leaving the collector in an unusable (but safe) state.
    /*
     * Forcibly inlining this is fine and should always improve code quality,
     * since we just guard on a constant conditional then delegate to another method.
     * In the worst case scenario, inlining adds extra `if false` or `if true` branches.
     * However, even in debug builds constant branching should be completely removed.
     * Therefore, even at zero optimization this should almost always be a net-win.
     */
    #[inline(always)]
    pub unsafe fn trace<T: ?Sized + GarbageCollected>(&mut self, value: &T) {
        if T::NEEDS_TRACE {
            debug_assert!(self.is_tracing());
            T::raw_trace(value, self)
        }
    }
    /// Initialize a `SafepointBag` with the specified value,
    /// preparing for its activation.
    ///
    /// Panics if a safepoint was already in progress, but hasn't been finished yet.
    #[inline]
    pub fn initialize_safepoint<'unm, T>(&mut self, value: T) -> SafepointBag<'unm, T>
        where T: GarbageCollected + GcErase<'unm> {
        assert_eq!(self.current_safepoint.get(), None, "Safepoint already in progress");
        let id = SafepointId {
            collector: self.id,
            id: self.safepoint_counter.try_next().expect("Too many safepoints"),
        };
        let bag = SafepointBag {
            value: ManuallyDrop::new(value),
            id, state: SafepointState::Initialized,
            marker: PhantomData
        };
        self.current_safepoint.set(Some(bag.id));
        bag
    }
    /// Activate a safepoint on this garbage collector, potentially collecting garbage
    /// invalidating all other garbage collected pointers by 'mutating' the collector.
    ///
    /// This is where we actually perform the collection,
    /// by treating the safepoint as the garbage collector's roots.
    ///
    /// This function is entirely safe, and simply panics on invalid input.
    #[doc(hidden)]
    #[inline]
    pub fn activate_safepoint<'unm, T>(
        &mut self, safepoint: &mut SafepointBag<'unm, T>
    ) where T: GarbageCollected + GcErase<'unm> {
        assert_eq!(self.current_safepoint.get(), Some(safepoint.id), "Unexpected safepoint");
        /*
         * Now that we know this is a valid request and it'd be safe to collect,
         * we'll decide if it'd be worthwhile to perform a collection.
         * In other words, this is just an opprotunity for garbage collection,
         * not an actual demand that we perform it right now.
         */
        if self.heap.borrow().current_size() >= self.collection_threshold.get() {
            let value = safepoint.begin_collection();
            unsafe {
                self.perform_collection(value)
            }
        } else {
            safepoint.ignore_collection()
        }
    }
    /// Finish performing the safepoint on this garbage collector,
    /// transmuting the garbage collected lifetimes of everything in the `SafepointBag`
    /// to the new lifetime of the collector.
    ///
    /// This function is entirely safe, and simply panics on invalid input.
    #[inline]
    pub fn finish_safepoint<'gc, 'unm, T>(&'gc self, safepoint: &mut SafepointBag<'unm, T>) -> T::Corrected
        where 'gc: 'unm, T: GarbageCollected + GcUnErase<'gc, 'unm> {
        assert_eq!(self.current_safepoint.get(), Some(safepoint.id), "Unexpected safepoint");
        let result = safepoint.consume();
        self.current_safepoint.set(None);
        unsafe { result.unerase() }
    }
    /// Update the trace of the specified garbage collected object,
    /// tracing its innards if the object hasn't been traced yet.
    ///
    /// This is the fallback to 'lazily trace' garbage collected pointers,
    /// so if we haven't seen this pointer yet we need to trace the innards,
    /// otherwise we can do the fast-path since we've already traced the pointer.
    #[cold]
    #[inline(never)]
    unsafe fn update_trace<T: GarbageCollected>(&mut self, value: &mut GcObject<T>) {
        /*
         * TODO: Is this panic safe or do we need a `defer!` guard to cleanup?.
         * I don't think we need to unless we decide to be `Send`/`Sync`,
         * since the mutable reference
         */
        let tracing = self.assume_tracing();
        // This assert verifies we're using the
        assert_eq!(value.header, tracing.expected_header);
        value.header = tracing.updated_header;
        // Trace the object's innards
        value.value.raw_trace(self);
    }

    /// Forcibly trigger garbage collection,
    /// if it decides there's work that we need to do in order to clean up.
    ///
    /// This is unsafe since it assumes the safepoint bag is valid.
    #[cold]
    #[inline(never)]
    unsafe fn perform_collection<T: GarbageCollected>(&mut self, value: &mut T) {
        let expected_header = self.assume_allocating().expected_header;
        let updated_header = expected_header.flipped_mark();
        self.state = GarbageCollectorState::Tracing(TracingState {
            expected_header, updated_header
        });
        self.trace(value);
        self.heap.get_mut().sweep(updated_header, expected_header);
    }
    #[inline]
    fn is_tracing(&self) -> bool {
        match self.state {
            GarbageCollectorState::Tracing(_) => true,
            GarbageCollectorState::Allocating(_) => false,
        }
    }
    #[inline]
    unsafe fn assume_tracing(&mut self) -> &mut TracingState {
        match self.state {
            GarbageCollectorState::Tracing(ref mut tracing) => tracing,
            GarbageCollectorState::Allocating(_) => debug_unreachable(),
        }
    }
    #[inline]
    unsafe fn assume_allocating(&self) -> &AllocationState {
        match self.state {
            GarbageCollectorState::Tracing(_) => debug_unreachable(),
            GarbageCollectorState::Allocating(ref allocating) => allocating,
        }
    }
}

/// As a precaution, prevent us from being `Send`.
///
/// At this point ,I haven't considered safepoint safety in the case where the collector is used by multiple threads by a lock.
/// With a mutable reference for safepoint the user would never observe that state in single-threaded code.
/// However in multithreaded code, we can't necessarily rely on locks poisoning and threads dying for safety.
/// Alternative lock implementations like `antidote` and `parking_lot` are not guaranteed to poison.
/// Before we do this, we need to seriously consider and discuss the safety of this change.
/// I would need the guarantee that the collector is always in a valid state even after panics.
impl !Send for GarbageCollector {}
/// Both the collector and allocator are not designed to be shared between threads.
///
/// Sharing between threads wouldn't just have the `Send` problems for panic-safety,
/// but allocation would also have trouble since it uses interior-mutability.
impl !Sync for GarbageCollector {}

enum GarbageCollectorState {
    Tracing(TracingState),
    Allocating(AllocationState)
}

struct AllocationState {
    expected_header: GcHeader
}

struct TracingState {
    expected_header: GcHeader,
    updated_header: GcHeader
}

/// An error indicating that attempting to allocate a garbage collected type failed,
/// because there wasn't enough memory available to satisfy the request.
///
/// By default encountering this error in `alloc` will trigger a panic,
/// but it can be explicitly handled with `try_alloc`.
#[derive(Debug, Clone)]
pub struct GcMemoryError {
    requested: usize,
    available: usize,
}
impl GcMemoryError {
    #[inline]
    fn new(requested: usize, available: usize) -> Self {
        assert!(requested < available);
        GcMemoryError { requested, available }
    }
    /// Treat this allocation failure as a fatal error,
    /// and panic with a descriptive error message.
    ///
    /// This should be prefered over `unwrap_err`,
    /// since the error message is somewhat nicer.
    /// However, since this takes ownership of the value,
    /// you can simply call `unwrap_or_else(|e| e.fail())`
    #[cold] #[inline(never)]
    pub fn fail(self) -> ! {
        panic!("{}", self)
    }
    /// The number of bytes of available memory
    #[inline]
    pub fn available(&self) -> usize {
        self.available
    }
    /// The number of bytes of requested memory
    #[inline]
    pub fn requested(&self) -> usize {
        self.requested
    }
}
impl Display for GcMemoryError {
    fn fmt(&self, f: &mut Formatter) -> fmt::Result {
        write!(f, "Out of memory: Unable to allocate {} bytes, since only {} bytes available",
            self.requested, self.available)
    }
}
impl ::std::error::Error for GcMemoryError {
    #[inline]
    fn description(&self) -> &str {
        "Out of memory: GC"
    }
}

/// A garbage collected pointer to a value.
///
/// This should be considered the equivelant of a garbage collected smart-pointer.
/// It's so smart, you can even coerce it to a reference bound to the lifetime of the garbage collector.
/// However, all those references are invalidated by the borrow checker as soon as a safepoint
/// is called, and they can only survive garbage collection if they live in this smart-pointer.
///
/// The smart pointer is simply a guarantees to the garbage collector
/// that this points to a garbage collected object with the correct header,
/// and not some arbitrary bits that you've decided to heap allocate.
#[derive(PartialEq, Eq, PartialOrd, Ord, )]
pub struct Gc<'gc, T: GarbageCollected + ?Sized + 'gc> {
    ptr: &'gc T
}
impl<'gc, T: ?Sized + GarbageCollected + 'gc> Gc<'gc, T> {
    /// Create a new garbage collected pointer to the specified value.
    ///
    /// ## Safety
    /// Not only are you assuming the specified pointer is valid for the `'gc` liftime,
    /// you're also assuming that it points to a garbage collected object.
    ///
    /// Specifically, since you're assuming the value was allocated from part of a valid `GcObject`,
    /// and included .
    /// Dark magic and evil pointer casts are performed to turn the pointer back into a `GcObject`,
    /// so you're assuming that this is valid to perform.
    #[inline]
    unsafe fn new(value: NonNull<T>) -> Gc<'gc, T> {
        Gc { ptr: &*value.as_ptr() }
    }
    #[inline(always)]
    pub fn value(self) -> &'gc T {
        self.ptr
    }
    /// View a pointer to the underlying `GcObject`.
    ///
    /// This is safe to perform, since the wrapper can only be created by a `GarbageCollector`,
    /// which guarantees that this is part of a `GcObject`
    #[inline]
    fn object_ptr(&self) -> NonNull<GcObject<T>> where T: Sized {
        unsafe {
            GcObject::from_value_ptr(NonNull::from(self.ptr))
        }
    }
}
/// In the glorious future where we have a copying collector,
/// we may need to use a `Cell` to allow changing the location of the pointer.
///
/// Therefore, we don't want a sync implementation to bind future versions to thread-safety.
impl<'gc, T: GarbageCollected + 'gc> !Sync for Gc<'gc, T> {}
impl<'gc, T: ?Sized + GarbageCollected + 'gc> Deref for Gc<'gc, T> {
    type Target = &'gc T;

    #[inline(always)]
    fn deref(&self) -> &&'gc T {
        &self.ptr
    }
}

/// Indicates that a type can be garbage collected,
/// and properly traced by the `GarbageCollector`.
///
/// ## Safety
/// See the documentation of the `trace`
/// In order to make garbage collected types always safe to deallocate,
/// you can't have custom destructors that reference garbage collected pointers.
/// The garbage collector assumes that dropping objects never does this,
/// giving the implementation much more freedom than with java finalizers.
///
///
/// However, attempting to allocate or mutate garbage collected objects are always considered undefined behavior.
/// Additionally drop functions are forbidden
pub unsafe trait GarbageCollected {
    /// Whether this type needs to be traced by the garbage collector.
    ///
    /// Some primitive types don't need to be traced at all,
    /// and can be simply ignored by the garbage collector.
    ///
    /// Collections should usually delegate this decision to their element type,
    /// claiming the need for tracing only if their elements do.
    /// For example, to decide `Vec<u32>::NEEDS_TRACE` you'd check whether `u32::NEEDS_TRACE` (false),
    /// and so then `Vec<u32>` doesn't need to be traced.
    /// By the same logic, `Vec<Gc<u32>>` does need to be traced,
    /// since it contains a garbage collected pointer.
    ///
    /// If there are multiple types involved, you should check if any of them need tracing.
    /// One perfect example of this is structure/tuple types which check
    /// `field1::NEEDS_TRACE || field2::NEEDS_TRACE || field3::needs_trace`.
    /// The fields which don't need tracing will always ignored by `GarbageCollector::trace`,
    /// while the fields that do will be properly traced.
    ///
    /// False negatives will always result in completely undefined behavior,
    /// but false positives could possibly result in some unnecessary tracing.
    /// Therefore, when in doubt you always assume this is true.
    const NEEDS_TRACE: bool;
    /// Trace this object, directly delegating to `GarbageCollector::trace`
    ///
    /// This method is simply a utility required to delegate to `GarbgeCollector::trace`,
    /// and overriding it is undefined behavior
    #[inline(always)] // This always delegates, so inlining has zero cost
    unsafe fn trace(&self, collector: &mut GarbageCollector) {
        collector.trace::<Self>(self)
    }
    /// Used by the garbage collector to perform a raw trace on this type,
    /// usually just tracing each of their fields.
    ///
    /// Users should never invoke this method, and always use `GarbageCollector::trace` instead.
    /// Only the collector itself is premited to call this method,
    /// and **it is undefined behavior for the user to invoke this**.
    ///
    /// Structures should trace each of their fields,
    /// and collections should trace each of their elements.
    ///
    ///
    /// ### Safety
    /// Some types (like `Gc`) need special actions taken when they're traced,
    /// but those are somewhat rare and are usually already provided by the garbage collector.
    ///
    /// Unless I explicitly document actions as legal I may decide to change i.
    /// I am only bound by the constraints of [semantic versioning](http://semver.org/) in the trace function
    /// if I explicitly document it as safe behavior in this method's documentation.
    /// If you try something that isn't explicitly documented here as permitted behavior,
    /// the collector may choose to override your memory with `0xDEADBEEF`.
    /// ## Always Permitted
    /// - Reading your own memory (includes iteration)
    ///   - Interior mutation is undefined behavior, even if you use `GcCell`
    /// - Calling `GarbageCollector::trace` with the specified collector
    ///   - `GarbageCollector::trace` already verifies that it owns the data, so you don't need to do that
    /// - Panicking
    ///   - This should be reserved for cases where you are seriously screwed up,
    ///       and can't fulfill your contract to trace your interior properly.
    ///     - One example is `Gc<T>` which panics if the garbage collectors are mismatched
    ///   - This rule may change in future versions, depending on how we deal with multi-threading.
    /// ## Never Permitted Behavior
    /// - Forgetting a element of a collection, or field of a structure
    ///   - If you forget an element undefined behavior will result and I will make you `0xDEADBEEF`
    ///   - This is honestly quite serious, since we have absolutely no way of knowing
    ///     it's used and we may decide to destroy it
    ///   - This is why we always prefer automatically derived implementations where possible.
    ///     - You will never trigger undefined behavior with an automatic implementation,
    ///       and it'll always be completely sufficient for safe code (aside from destructors).
    ///     - With an automatically derived implementation you will never miss a field,
    ///       and a bug is will always be my fault not yours.
    /// - Invoking this function directly, without using the wrapper `GarbageCollector::trace` function
    unsafe fn raw_trace(&self, collector: &mut GarbageCollector);
}

//
// Fundamental implementations
//

unsafe impl<'gc, T: GarbageCollected + 'gc> GarbageCollected for Gc<'gc, T> {
    /// Garbage collected pointers always need to be unconditionally traced.
    ///
    /// They are the whole reason we trace in the first place,
    /// since we need to mark them as used to prevent them from being freed.
    const NEEDS_TRACE: bool = true;

    #[inline]
    unsafe fn raw_trace(&self, collector: &mut GarbageCollector) {
        let mut object = self.object_ptr();
        let object = object.as_mut();
        let tracing = collector.assume_tracing();
        if object.header != tracing.updated_header {
            // Only update this pointer if we haven't done it already
            collector.update_trace(object)
        }
    }

}

unsafe impl<'gc, 'unm: 'gc, T: GarbageCollected> GcErase<'unm> for Gc<'gc, T>
    where T: 'gc + GcErase<'unm> {
    type Erased = Gc<'unm, T::Erased>;

    #[inline]
    unsafe fn erase(self) -> Self::Erased {
        mem::transmute(self)
    }
}

unsafe impl<'unm, 'gc, T> GcUnErase<'unm, 'gc> for Gc<'unm, T>
    where 'unm: 'gc, T: GarbageCollected + GcUnErase<'unm, 'gc> {
    type Corrected = Gc<'gc, T::Corrected>;

    #[inline]
    unsafe fn unerase(self) -> Self::Corrected {
        mem::transmute(self)
    }
}