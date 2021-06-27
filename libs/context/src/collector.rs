//! The interface to a collector

use core::fmt::{self, Debug, Formatter};
use core::ptr::NonNull;
use core::marker::PhantomData;

use alloc::sync::Arc;

use slog::{Logger, o};

use zerogc::{GcSafe, GcSystem, Trace, GcSimpleAlloc, NullTrace, TraceImmutable, GcVisitor, Gc};

use crate::{CollectorContext};
use crate::state::{CollectionManager, RawContext};
use std::mem::ManuallyDrop;

/// A specific implementation of a collector
pub unsafe trait RawCollectorImpl: 'static + Sized {
    /// A dynamic pointer to a GC object
    ///
    /// The simple collector implements this as
    /// a trait object pointer.
    type GcDynPointer: Copy + Debug + 'static;

    /// Whether or not this collector is moving
    ///
    /// See the docs for [::zerogc::CollectorId::MOVING] for more information.
    const MOVING: bool;

    /// A pointer to this collector
    ///
    /// Must be a ZST if the collector is a singleton.
    type Ptr: CollectorPtr<Self>;

    /// The type that manages this collector's state
    type Manager: CollectionManager<Self, Context=Self::RawContext>;

    /// The context
    type RawContext: RawContext<Self>;

    /// True if this collector is a singleton
    ///
    /// If the collector allows multiple instances,
    /// this *must* be false
    const SINGLETON: bool;

    /// True if this collector is thread-safe.
    const SYNC: bool;

    /// Convert the specified value into a dyn pointer
    unsafe fn create_dyn_pointer<T: Trace>(t: *mut T) -> Self::GcDynPointer;

    /// Determine the collector that owns the specified [Gc] pointer.
    fn determine_ptr_from_ref<'gc, T>(gc: Gc<'gc, T, CollectorId<Self>>) -> &'gc Self::Ptr
        where T: GcSafe + ?Sized + 'gc;

    /// Initialize an instance of the collector,
    /// using an [Arc] to manage the underlying memory
    ///
    /// Must panic if the collector is a singleton
    fn init(logger: Logger) -> Arc<Self>;

    /// Create a [CollectorPtr] referring to this pointer
    fn as_ptr(&self) -> Self::Ptr;

    /// The logger associated with this collector
    fn logger(&self) -> &Logger;

    fn manager(&self) -> &Self::Manager;

    fn should_collect(&self) -> bool;

    fn allocated_size(&self) -> crate::utils::MemorySize;

    unsafe fn perform_raw_collection(&self, contexts: &[*mut Self::RawContext]);
}

/// A thread safe collector
pub unsafe trait SyncCollector: RawCollectorImpl + Sync {

}

/// A collector implemented as a singleton
///
/// This only has one instance
pub unsafe trait SingletonCollector: RawCollectorImpl<Ptr=PhantomData<&'static Self>> {
    /// When the collector is a singleton,
    /// return the global implementation
    fn global_ptr() -> &'static Self;

    /// A double indirection to the global pointer
    fn double_global_ptr() -> &'static &'static Self;

    /// Initialize the global singleton
    ///
    /// Panics if already initialized
    fn init_global(logger: Logger) -> &'static Self;
}

/// An unchecked pointer to a collector
/*
 * TODO: Unify with CollectorRef
 * Wait a second...
 * This is used by `CollectorId`, and when its PhantomData
 * that turns the id into a ZST. Shouldn't we preserve that behavior?
 */
pub unsafe trait CollectorPtr<C: RawCollectorImpl<Ptr=Self>>: Copy + Eq
    + self::sealed::SealedPtr + 'static {
    /// A weak reference to the pointer
    type Weak: Clone + 'static;

    unsafe fn from_raw(ptr: *mut C) -> Self;
    unsafe fn clone_owned(&self) -> Self;
    fn double_ptr(&self) -> &'_ GarbageCollector<C>;
    fn as_ptr(&self) -> *mut C;
    unsafe fn drop(self);
    fn upgrade_weak_raw(weak: &Self::Weak) -> Option<Self>;
    #[inline]
    fn upgrade_weak(weak: &Self::Weak) -> Option<C::Ptr> {
        match Self::upgrade_weak_raw(weak) {
            Some(ptr) => Some(ptr),
            None => None
        }
    }
    unsafe fn assume_weak_valid(weak: &Self::Weak) -> Self;
    unsafe fn create_weak(&self) -> Self::Weak;
}
/// This is implemented as a
/// raw pointer via [Arc::into_raw]
unsafe impl<C: RawCollectorImpl<Ptr=Self>> CollectorPtr<C> for NonNull<C> {
    type Weak = alloc::sync::Weak<C>;

    #[inline]
    unsafe fn from_raw(ptr: *mut C) -> Self {
        assert!(!C::SINGLETON, "Collector is a singleton!");
        debug_assert!(!ptr.is_null());
        NonNull::new_unchecked(ptr)
    }

    #[inline]
    unsafe fn clone_owned(&self) -> Self {
        let original = Arc::from_raw(self.as_ptr());
        let cloned = Arc::clone(&original);
        core::mem::forget(original);
        NonNull::new_unchecked(Arc::into_raw(cloned) as *mut _)
    }

    #[inline]
    fn double_ptr(&self) -> &GarbageCollector<C> {
        unsafe {
            &*(self as *const Self as *mut *mut C as *mut GarbageCollector<C>)
        }
    }

    #[inline]
    fn as_ptr(&self) -> *mut C {
        NonNull::as_ptr(*self)
    }

    #[inline]
    unsafe fn drop(self) {
        drop(Arc::from_raw(self.as_ptr() as *const _))
    }

    #[inline]
    fn upgrade_weak_raw(weak: &Self::Weak) -> Option<Self> {
        match weak.upgrade() {
            Some(arc) => {
                Some(unsafe {
                    Self::from_raw(Arc::into_raw(arc) as *mut _)
                })
            },
            None => None
        }
    }

    #[inline]
    unsafe fn assume_weak_valid(weak: &Self::Weak) -> Self {
        debug_assert!(
            weak.upgrade().is_some(),
            "Dead collector"
        );
        NonNull::new_unchecked(weak.as_ptr() as *mut _)
    }

    #[inline]
    unsafe fn create_weak(&self) -> Self::Weak {
        let arc = Arc::from_raw(self.as_ptr());
        let weak = Arc::downgrade(&arc);
        core::mem::forget(arc);
        weak
    }
}
/// Dummy implementation
impl<C: RawCollectorImpl<Ptr=Self>> self::sealed::SealedPtr for NonNull<C> {}
unsafe impl<C: SingletonCollector<Ptr=Self>> CollectorPtr<C> for PhantomData<&'static C> {
    type Weak = PhantomData<&'static C>;

    #[inline]
    unsafe fn from_raw(ptr: *mut C) -> Self {
        assert!(C::SINGLETON, "Expected a singleton");
        debug_assert_eq!(ptr, C::global_ptr() as *const C as *mut C);
        PhantomData
    }

    #[inline]
    unsafe fn clone_owned(&self) -> Self {
        *self
    }

    #[inline]
    fn as_ptr(&self) -> *mut C {
        assert!(C::SINGLETON, "Expected a singleton");
        C::global_ptr() as *const C as *mut C
    }
    #[inline]
    fn double_ptr(&self) -> &'_ GarbageCollector<C> {
        assert!(C::SINGLETON, "Expected a singleton");
        unsafe {
            std::mem::transmute::<
                &'static &'static C,
                &'static GarbageCollector<C>
            >(C::double_global_ptr())
        }
    }

    #[inline]
    unsafe fn drop(self) {}

    #[inline]
    fn upgrade_weak_raw(weak: &Self::Weak) -> Option<Self> {
        assert!(C::SINGLETON);
        Some(*weak) // gloal is always valid
    }

    #[inline]
    unsafe fn assume_weak_valid(weak: &Self::Weak) -> Self {
        assert!(C::SINGLETON); // global is always valid
        *weak
    }

    #[inline]
    unsafe fn create_weak(&self) -> Self::Weak {
        *self
    }
}
/// Dummy implementation
impl<C: SingletonCollector<Ptr=Self>> self::sealed::SealedPtr for PhantomData<&'static C> {}

/// Uniquely identifies the collector in case there are
/// multiple collectors.
///
/// If there are multiple collectors `cfg!(feature="multiple-collectors")`,
/// we need to use a pointer to tell them apart.
/// Otherwise, this is a zero-sized structure.
///
/// This type is a marker type. It can never be created.
pub struct CollectorId<C: RawCollectorImpl> {
    pub(crate) collector: C::Ptr
}
impl<C: RawCollectorImpl> Eq for CollectorId<C> {}
impl<C: RawCollectorImpl> PartialEq for CollectorId<C> {
    #[inline]
    fn eq(&self, other: &Self) -> bool {
        if C::SINGLETON {
            true
        } else {
            std::ptr::eq(self.collector.as_ptr(), other.collector.as_ptr())
        }
    }
}
impl<C: RawCollectorImpl> Debug for CollectorId<C> {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.debug_struct("CollectorId")
            .field("type", &std::any::type_name::<C>())
            .field("ptr", &format_args!("{:p}", self.collector.as_ptr()))
            .finish()
    }
}
unsafe impl<C: RawCollectorImpl> ::zerogc::CollectorId for CollectorId<C> {
    type System = GarbageCollector<C>;

    #[inline]
    fn system(&self) -> &Self::System {
        unsafe {
            &*self.collector.double_ptr()
        }
    }

    #[inline(always)] // NOP
    fn write_barrier<'gc, V, O>(&self, _gc: Gc<'gc, V, Self>, _owner: Gc<'gc, O, Self>, _field_offset: usize)
        where O: GcSafe + ?Sized + 'gc, V: GcSafe + ?Sized + 'gc {
        // TODO: what if the collector doesn't have a NOP implementation?
    }

    #[inline]
    fn determine_from_ref<'gc, T: GcSafe + ?Sized + 'gc>(gc: Gc<'gc, T, Self>) -> &'gc Self {
        unsafe {
            std::mem::transmute::<
                &'gc C::Ptr,
                &'gc CollectorId<C>
            >(C::determine_ptr_from_ref(gc))
        }
    }
}
unsafe impl<C: RawCollectorImpl> Trace for CollectorId<C> {
    const NEEDS_TRACE: bool = false;
    #[inline(always)]
    fn visit<V: GcVisitor>(&mut self, _visitor: &mut V) -> Result<(), V::Err> {
        Ok(())
    }
}
unsafe impl<C: RawCollectorImpl> TraceImmutable for CollectorId<C> {
    #[inline(always)]
    fn visit_immutable<V: GcVisitor>(&self, _visitor: &mut V) -> Result<(), <V as GcVisitor>::Err> {
        Ok(())
    }
}
unsafe impl<C: RawCollectorImpl> NullTrace for CollectorId<C> {}
unsafe impl<C: RawCollectorImpl> GcSafe for CollectorId<C> {
    const NEEDS_DROP: bool = false;
}

pub(crate) struct WeakCollectorRef<C: RawCollectorImpl> {
    pub(crate) weak: <C::Ptr as CollectorPtr<C>>::Weak,
}
impl<C: RawCollectorImpl> WeakCollectorRef<C> {
    #[inline]
    pub unsafe fn assume_valid(&self) -> C::Ptr {
         C::Ptr::assume_weak_valid(&self.weak)
    }
    pub fn ensure_valid<R>(&self, func: impl FnOnce(C::Ptr) -> R) -> R {
        self.try_ensure_valid(|id| match id{
            Some(id) => func(id),
            None => panic!("Dead collector")
        })
    }
    #[inline]
    pub fn try_ensure_valid<R>(&self, func: impl FnOnce(Option<C::Ptr>) -> R) -> R{
        func(C::Ptr::upgrade_weak(&self.weak))
    }
}

pub unsafe trait RawSimpleAlloc<'gc, T>: RawCollectorImpl 
    where T: GcSafe + 'gc {
    /// Allocate a new garbage collected reference
    fn alloc(
        context: &'gc CollectorContext<Self>,
        value: T
    ) -> Gc<'gc, T, CollectorId<Self>>;
}
unsafe impl<'gc, T, C> GcSimpleAlloc<'gc, T> for CollectorContext<C>
    where T: GcSafe + 'gc, C: RawSimpleAlloc<'gc, T> {
    #[inline]
    fn alloc(&'gc self, value: T) -> Gc<'gc, T, CollectorId<C>> {
        C::alloc(self, value)
    }
}


/// An internal reference to a garbage collector.\
///
/// When an implementation is a singleton,
/// this is a `&'static C'.
///
/// When using multiple collectors, this is an [Arc].
/// Reference counting is needed to ensure the collector
/// lives longer than its longest [GcContext].
/*
 * TODO: Consider panicking if a GcContext outlives its owning GarbageCollector.
 * It seems unintuitive that a GcContext can implicitly extend the lifetime
 * of its logically owning GarbageCollector
 */
#[doc(hidden)]
pub struct CollectorRef<C: RawCollectorImpl> {
    ptr: NonNull<C>,
    marker: PhantomData<Arc<C>>
}
impl<C: RawCollectorImpl> CollectorRef<C> {
    #[inline]
    unsafe fn dangling() -> Self {
        CollectorRef {
            ptr: NonNull::dangling(),
            marker: PhantomData
        }
    }
    #[inline]
    fn from_arc(arc: Arc<C>) -> Self {
        assert!(!C::SINGLETON);
        CollectorRef {
            ptr: unsafe { NonNull::new_unchecked(Arc::into_raw(arc) as *mut C) },
            marker: PhantomData
        }
    }
    #[inline]
    fn from_static(s: &'static C) -> Self {
        assert!(C::SINGLETON);
        CollectorRef { ptr: NonNull::from(s), marker: PhantomData }
    }
    #[inline]
    pub(crate) fn create_weak(&self) -> WeakCollectorRef<C> {
        WeakCollectorRef {
            weak: unsafe { self.as_raw().as_ptr().create_weak() }
        }
    }
    #[inline]
    pub(crate) fn as_raw(&self) -> &C {
        unsafe { self.ptr.as_ref() }
    }
    /// Steal an [Arc] referring to the underlying collector.
    ///
    /// ## Safety
    /// Undefined behavior if the [Arc] is dropped while this reference
    /// is still in use.
    ///
    /// Must never be used with singleton collectors.
    #[inline]
    unsafe fn steal_arc(&self) -> ManuallyDrop<Arc<C>> {
        assert!(!C::SINGLETON);
        ManuallyDrop::new(Arc::from_raw(self.ptr.as_ptr() as *const C))
    }
    #[inline]
    fn into_arc(self) -> Arc<C> {
        unsafe { ManuallyDrop::into_inner(self.steal_arc()) }
    }
}
impl<C: RawCollectorImpl> Clone for CollectorRef<C> {
    fn clone(&self) -> Self {
        if !C::SINGLETON {
            unsafe {
                Arc::increment_strong_count(self.ptr.as_ptr() as *const C);
            }
        }
        CollectorRef { ptr: self.ptr, marker: PhantomData }
    }
}
impl<C: RawCollectorImpl> Drop for CollectorRef<C> {
    fn drop(&mut self) {
        if !C::SINGLETON {
            unsafe {
                drop(ManuallyDrop::into_inner(self.steal_arc()));
            }
        }
    }
}

/// A garbage collector
///
/// This is a wrapper around a [RawCollectorImpl],
/// that manages the shared "context" code relating to [GcContext]
/// and the implementation of shadow stacks.
#[repr(transparent)]
pub struct GarbageCollector<C: RawCollectorImpl> {
    raw_ref: ManuallyDrop<CollectorRef<C>>,
    marker: PhantomData<C>
}
/// We actually are thread safe ;)
unsafe impl<C: SyncCollector> Send for GarbageCollector<C> {}
#[cfg(feature = "sync")]
unsafe impl<C: SyncCollector> Sync for GarbageCollector<C> {}

/// Internal trait for initializing a collector
#[doc(hidden)]
pub trait CollectorInit<C: RawCollectorImpl> {
    fn create() -> CollectorRef<C> {
        Self::with_logger(Logger::root(
            slog::Discard,
            o!()
        ))
    }
    fn with_logger(logger: Logger) -> CollectorRef<C>;
}

impl<C: RawCollectorImpl<Ptr=NonNull<C>>> CollectorInit<C> for NonNull<C> {
    fn with_logger(logger: Logger) -> CollectorRef<C> {
        assert!(!C::SINGLETON);
        let raw = C::init(logger);
        CollectorRef::from_arc(raw)
    }
}
impl<C> CollectorInit<C> for PhantomData<&'static C>
    where C: SingletonCollector {
    fn with_logger(logger: Logger) -> CollectorRef<C> {
        assert!(C::SINGLETON);
        let raw = C::init_global(logger);
        CollectorRef::from_static(raw)
   }
}


impl<C: RawCollectorImpl> GarbageCollector<C> {
    #[inline]
    pub fn create() -> GarbageCollector<C> where C::Ptr: CollectorInit<C> {
        GarbageCollector::from_raw(<C::Ptr as CollectorInit<C>>::create())
    }

    #[inline]
    pub fn with_logger(logger: Logger) -> Self where C::Ptr: CollectorInit<C> {
        GarbageCollector::from_raw(<C::Ptr as CollectorInit<C>>::with_logger(logger))
    }

    #[inline]
    pub(crate) fn from_raw(raw: CollectorRef<C>) -> Self {
        GarbageCollector { raw_ref: ManuallyDrop::new(raw), marker: PhantomData }
    }

    /// The underlying collector
    ///
    /// ## Safety
    /// This is marked unsafe as a more of a lint.
    ///
    /// It is possible the internal implementation of the collector may
    /// have semi-private state that users shouldn't access.
    ///
    /// Regardless, users shouldn't be accessing this in the vast majority of cases.
    #[inline]
    pub unsafe fn as_raw(&self) -> &C {
        self.raw_ref.as_raw()
    }

    /// An internal reference to the collector
    #[inline]
    pub(crate) fn as_ref(&self) -> &CollectorRef<C> {
        &self.raw_ref
    }

    /// Convert this collector into a unique context
    ///
    /// The single-threaded implementation only allows a single context,
    /// so this method is necessary to support it.
    pub fn into_context(self) -> CollectorContext<C> {
        unsafe { CollectorContext::register_root(&self) }
    }
}
impl<C: SyncCollector> GarbageCollector<C> {

    /// Create a new context bound to this collector
    ///
    /// Warning: Only one collector should be created per thread.
    /// Doing otherwise can cause deadlocks/panics.
    pub fn create_context(&self) -> CollectorContext<C> {
        unsafe { CollectorContext::register_root(&self) }
    }
}
impl<C: RawCollectorImpl> Drop for GarbageCollector<C> {
    fn drop(&mut self) {
        if C::SINGLETON {
            // Singletons live forever
        } else {
            let raw_ref = std::mem::replace(
                &mut self.raw_ref,
                ManuallyDrop::new(unsafe { CollectorRef::dangling() })
            );
            let arc = ManuallyDrop::into_inner(raw_ref).into_arc();
            if !std::thread::panicking() {
                /*
                 * Check that there are no outstanding references to the collector.
                 * This may happen if a GcContext outlives its parent GarbageCollector.
                 * A GarbageCollector is the logical owner of all its child contexts,
                 * and its unintuitive if the child context outlives the parent collector.
                 * However, if we are already panicking, we ignore this check.
                 * A double-panic triggers an abort, which would be unhelpful.
                 */
                match Arc::try_unwrap(arc) {
                    Ok(inner) => drop::<C>(inner),
                    Err(arc) => {
                        panic!("Multiple outstanding references to collector: {}", Arc::strong_count(&arc) - 1)
                    }
                }
            } else {
                drop(arc);
            }
        }
    }
}

unsafe impl<C: RawCollectorImpl> GcSystem for GarbageCollector<C> {
    type Id = CollectorId<C>;
    const MOVING: bool = C::MOVING;
    type Context = CollectorContext<C>;
}

mod sealed {
    use crate::collector::{CollectorInit, RawCollectorImpl};

    // TODO: This is stupid
    pub trait SealedPtr {}
}