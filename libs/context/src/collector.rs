//! The interface to a collector
#![allow(clippy::missing_safety_doc)]

use core::fmt::{self, Debug, Formatter};
use core::hash::{Hash, Hasher};
use core::marker::PhantomData;
use core::ptr::NonNull;

use alloc::sync::Arc;

use slog::{o, Logger};

use zerogc::{Gc, GcArray, GcSafe, GcSimpleAlloc, GcSystem, Trace};

use crate::state::{CollectionManager, RawContext};
use crate::CollectorContext;
use zerogc::vec::raw::GcRawVec;

pub unsafe trait ConstRawCollectorImpl: RawCollectorImpl {
    fn resolve_array_len_const<T>(gc: &GcArray<T, CollectorId<Self>>) -> usize;
}

/// A specific implementation of a collector
pub unsafe trait RawCollectorImpl: 'static + Sized {
    /// A dynamic pointer to a `Trace` root
    ///
    /// The simple collector implements this as
    /// a trait object pointer.
    type DynTracePtr: Copy + Debug + 'static;
    /// The configuration
    type Config: Sized + Default;

    /// A pointer to this collector
    ///
    /// Must be a ZST if the collector is a singleton.
    type Ptr: CollectorPtr<Self>;

    /// The type that manages this collector's state
    type Manager: CollectionManager<Self, Context = Self::RawContext>;

    /// The context
    type RawContext: RawContext<Self>;
    /// The raw representation of a vec
    type RawVec<'gc, T: GcSafe<'gc, CollectorId<Self>>>: GcRawVec<'gc, T, Id = CollectorId<Self>>;

    /// True if this collector is a singleton
    ///
    /// If the collector allows multiple instances,
    /// this *must* be false
    const SINGLETON: bool;

    /// True if this collector is thread-safe.
    const SYNC: bool;

    fn id_for_gc<'a, 'gc, T>(gc: &'a Gc<'gc, T, CollectorId<Self>>) -> &'a CollectorId<Self>
    where
        'gc: 'a,
        T: ?Sized + 'gc;

    // TODO: What if we want to customize 'GcArrayRepr'??

    fn id_for_array<'a, 'gc, T>(
        gc: &'a GcArray<'gc, T, CollectorId<Self>>,
    ) -> &'a CollectorId<Self>
    where
        'gc: 'a;

    fn resolve_array_len<T>(repr: &GcArray<T, CollectorId<Self>>) -> usize;

    /// Convert the specified value into a dyn pointer
    unsafe fn as_dyn_trace_pointer<T: Trace>(t: *mut T) -> Self::DynTracePtr;

    /// Initialize an instance of the collector
    ///
    /// Must panic if the collector is not a singleton
    fn init(config: Self::Config, logger: Logger) -> NonNull<Self>;

    /// The id of this collector
    #[inline]
    fn id(&self) -> CollectorId<Self> {
        CollectorId {
            ptr: unsafe { Self::Ptr::from_raw(self as *const _ as *mut _) },
        }
    }
    unsafe fn gc_write_barrier<'gc, O, V>(
        owner: &Gc<'gc, O, CollectorId<Self>>,
        value: &Gc<'gc, V, CollectorId<Self>>,
        field_offset: usize,
    ) where
        O: GcSafe<'gc, CollectorId<Self>> + ?Sized,
        V: GcSafe<'gc, CollectorId<Self>> + ?Sized;
    /// The logger associated with this collector
    fn logger(&self) -> &Logger;

    fn manager(&self) -> &Self::Manager;

    fn should_collect(&self) -> bool;

    fn allocated_size(&self) -> crate::utils::MemorySize;

    unsafe fn perform_raw_collection(&self, contexts: &[*mut Self::RawContext]);
}

/// A thread safe collector
pub unsafe trait SyncCollector: RawCollectorImpl + Sync {}

/// A collector implemented as a singleton
///
/// This only has one instance
pub unsafe trait SingletonCollector:
    RawCollectorImpl<Ptr = PhantomData<&'static Self>>
{
    /// When the collector is a singleton,
    /// return the global implementation
    fn global_ptr() -> *const Self;

    /// Initialize the global singleton
    ///
    /// Panics if already initialized
    fn init_global(config: Self::Config, logger: Logger);
}

impl<C: RawCollectorImpl> PartialEq for CollectorId<C> {
    #[inline]
    fn eq(&self, other: &Self) -> bool {
        self.ptr == other.ptr
    }
}
impl<C: RawCollectorImpl> Hash for CollectorId<C> {
    #[inline]
    fn hash<H: Hasher>(&self, hasher: &mut H) {
        hasher.write_usize(self.ptr.as_ptr() as usize);
    }
}
impl<C: RawCollectorImpl> Eq for CollectorId<C> {}
impl<C: RawCollectorImpl> Clone for CollectorId<C> {
    #[inline]
    fn clone(&self) -> Self {
        *self
    }
}
impl<C: RawCollectorImpl> Copy for CollectorId<C> {}
impl<C: RawCollectorImpl> Debug for CollectorId<C> {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        let mut debug = f.debug_struct("CollectorId");
        if !C::SINGLETON {
            debug.field("ptr", &format_args!("{:p}", self.ptr.as_ptr()));
        }
        debug.finish()
    }
}

/// An unchecked pointer to a collector
pub unsafe trait CollectorPtr<C: RawCollectorImpl<Ptr = Self>>:
    Copy + Eq + self::sealed::Sealed + 'static
{
    /// A weak reference to the pointer
    type Weak: Clone + 'static;

    unsafe fn from_raw(ptr: *mut C) -> Self;
    unsafe fn clone_owned(&self) -> Self;
    fn as_ptr(&self) -> *mut C;
    unsafe fn drop(self);
    fn upgrade_weak_raw(weak: &Self::Weak) -> Option<Self>;
    #[inline]
    fn upgrade_weak(weak: &Self::Weak) -> Option<CollectorRef<C>> {
        Self::upgrade_weak_raw(weak).map(|ptr| CollectorRef { ptr })
    }
    unsafe fn assume_weak_valid(weak: &Self::Weak) -> Self;
    unsafe fn create_weak(&self) -> Self::Weak;
}
/// This is implemented as a
/// raw pointer via [Arc::into_raw]
unsafe impl<C: RawCollectorImpl<Ptr = Self>> CollectorPtr<C> for NonNull<C> {
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
    fn as_ptr(&self) -> *mut C {
        NonNull::as_ptr(*self)
    }

    #[inline]
    unsafe fn drop(self) {
        drop(Arc::from_raw(self.as_ptr() as *const _))
    }

    #[inline]
    fn upgrade_weak_raw(weak: &Self::Weak) -> Option<Self> {
        weak.upgrade()
            .map(|arc| unsafe { Self::from_raw(Arc::into_raw(arc) as *mut _) })
    }

    #[inline]
    unsafe fn assume_weak_valid(weak: &Self::Weak) -> Self {
        debug_assert!(weak.upgrade().is_some(), "Dead collector");
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
impl<C: RawCollectorImpl> self::sealed::Sealed for NonNull<C> {}
unsafe impl<C: SingletonCollector<Ptr = Self>> CollectorPtr<C> for PhantomData<&'static C> {
    type Weak = PhantomData<&'static C>;

    #[inline]
    unsafe fn from_raw(ptr: *mut C) -> Self {
        assert!(C::SINGLETON, "Expected a singleton");
        debug_assert_eq!(ptr, C::global_ptr() as *mut _);
        PhantomData
    }

    #[inline]
    unsafe fn clone_owned(&self) -> Self {
        *self
    }

    #[inline]
    fn as_ptr(&self) -> *mut C {
        assert!(C::SINGLETON, "Expected a singleton");
        C::global_ptr() as *mut C
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
impl<C: SingletonCollector> self::sealed::Sealed for PhantomData<&'static C> {}

/// Uniquely identifies the collector in case there are
/// multiple collectors.
///
/// If there are multiple collectors `cfg!(feature="multiple-collectors")`,
/// we need to use a pointer to tell them apart.
/// Otherwise, this is a zero-sized structure.
///
/// As long as our memory is valid,
/// it implies this pointer is too.
#[repr(C)]
pub struct CollectorId<C: RawCollectorImpl> {
    /// This is in essence a borrowed reference to
    /// the collector.
    ///
    /// Depending on whether or not the collector is a singleton,
    ///
    /// We don't know whether the underlying memory will be valid.
    ptr: C::Ptr,
}
impl<C: RawCollectorImpl> CollectorId<C> {
    #[inline]
    pub const unsafe fn from_raw(ptr: C::Ptr) -> CollectorId<C> {
        CollectorId { ptr }
    }
    #[inline]
    pub unsafe fn as_ref(&self) -> &C {
        &*self.ptr.as_ptr()
    }
    #[inline]
    pub unsafe fn weak_ref(&self) -> WeakCollectorRef<C> {
        WeakCollectorRef {
            weak: self.ptr.create_weak(),
        }
    }
}
unsafe impl<C: SyncCollector> Sync for CollectorId<C> {}
unsafe impl<C: SyncCollector> Send for CollectorId<C> {}
unsafe impl<C: RawCollectorImpl> ::zerogc::CollectorId for CollectorId<C> {
    type System = CollectorRef<C>;
    type Context = CollectorContext<C>;
    type RawVec<'gc, T: GcSafe<'gc, Self>> = C::RawVec<'gc, T>;
    // TODO: What if clients want to customize this?
    type ArrayPtr = zerogc::array::repr::ThinArrayPtr<Self>;

    #[inline]
    fn from_gc_ptr<'a, 'gc, T>(gc: &'a Gc<'gc, T, Self>) -> &'a Self
    where
        T: ?Sized,
        'gc: 'a,
    {
        C::id_for_gc(gc)
    }

    #[inline]
    fn resolve_array_id<'a, 'gc, T>(gc: &'a GcArray<'gc, T, Self>) -> &'a Self
    where
        'gc: 'a,
    {
        C::id_for_array(gc)
    }

    #[inline]
    fn resolve_array_len<T>(repr: &GcArray<'_, T, Self>) -> usize {
        C::resolve_array_len(repr)
    }

    #[inline(always)]
    unsafe fn gc_write_barrier<'gc, O, V>(
        owner: &Gc<'gc, O, Self>,
        value: &Gc<'gc, V, Self>,
        field_offset: usize,
    ) where
        O: GcSafe<'gc, Self> + ?Sized,
        V: GcSafe<'gc, Self> + ?Sized,
    {
        C::gc_write_barrier(owner, value, field_offset)
    }

    #[inline]
    unsafe fn assume_valid_system(&self) -> &Self::System {
        // TODO: Make the API nicer? (avoid borrowing and indirection)
        assert_eq!(
            core::mem::size_of::<Self>(),
            core::mem::size_of::<CollectorRef<C>>()
        );
        &*(self as *const CollectorId<C> as *const CollectorRef<C>)
    }
}
zerogc::impl_nulltrace_for_static!(CollectorId<C>, params => [C: RawCollectorImpl]);

pub struct WeakCollectorRef<C: RawCollectorImpl> {
    weak: <C::Ptr as CollectorPtr<C>>::Weak,
}
impl<C: RawCollectorImpl> WeakCollectorRef<C> {
    #[inline]
    pub unsafe fn assume_valid(&self) -> CollectorId<C> {
        CollectorId {
            ptr: C::Ptr::assume_weak_valid(&self.weak),
        }
    }
    pub fn ensure_valid<R>(&self, func: impl FnOnce(CollectorId<C>) -> R) -> R {
        self.try_ensure_valid(|id| match id {
            Some(id) => func(id),
            None => panic!("Dead collector"),
        })
    }
    #[inline]
    pub fn try_ensure_valid<R>(&self, func: impl FnOnce(Option<CollectorId<C>>) -> R) -> R {
        func(C::Ptr::upgrade_weak(&self.weak).map(|r| r.id()))
    }
}

pub unsafe trait RawSimpleAlloc: RawCollectorImpl {
    unsafe fn alloc_uninit<'gc, T: GcSafe<'gc, CollectorId<Self>>>(
        context: &'gc CollectorContext<Self>,
    ) -> *mut T;
    unsafe fn alloc_uninit_slice<'gc, T>(
        context: &'gc CollectorContext<Self>,
        len: usize,
    ) -> *mut T
    where
        T: GcSafe<'gc, CollectorId<Self>>;
    fn alloc_raw_vec_with_capacity<'gc, T>(
        context: &'gc CollectorContext<Self>,
        capacity: usize,
    ) -> Self::RawVec<'gc, T>
    where
        T: GcSafe<'gc, CollectorId<Self>>;
}
unsafe impl<C> GcSimpleAlloc for CollectorContext<C>
where
    C: RawSimpleAlloc,
{
    #[inline]
    unsafe fn alloc_uninit<'gc, T>(&'gc self) -> *mut T
    where
        T: GcSafe<'gc, CollectorId<C>>,
    {
        C::alloc_uninit(self)
    }

    #[inline]
    unsafe fn alloc_uninit_slice<'gc, T>(&'gc self, len: usize) -> *mut T
    where
        T: GcSafe<'gc, CollectorId<C>>,
    {
        C::alloc_uninit_slice(self, len)
    }

    #[inline]
    fn alloc_raw_vec_with_capacity<'gc, T>(&'gc self, capacity: usize) -> C::RawVec<'gc, T>
    where
        T: GcSafe<'gc, CollectorId<C>>,
    {
        C::alloc_raw_vec_with_capacity::<T>(self, capacity)
    }
}

/// A reference to the collector.
///
/// TODO: Devise better name
#[repr(C)]
pub struct CollectorRef<C: RawCollectorImpl> {
    /// When using singleton collectors, this is a ZST.
    ///
    /// When using multiple collectors, this is just an [Arc].
    ///
    /// It is implemented as a raw pointer around [Arc::into_raw]
    ptr: C::Ptr,
}
/// We actually are thread safe ;)
unsafe impl<C: SyncCollector> Send for CollectorRef<C> {}
#[cfg(feature = "sync")]
unsafe impl<C: SyncCollector> Sync for CollectorRef<C> {}

/// Internal trait for initializing a collector
#[doc(hidden)]
pub trait CollectorInit<C: RawCollectorImpl<Ptr = Self>>: CollectorPtr<C> {
    fn create() -> CollectorRef<C> {
        Self::with_logger(C::Config::default(), Logger::root(slog::Discard, o!()))
    }
    fn with_logger(config: C::Config, logger: Logger) -> CollectorRef<C>;
}

impl<C: RawCollectorImpl<Ptr = NonNull<C>>> CollectorInit<C> for NonNull<C> {
    fn with_logger(config: C::Config, logger: Logger) -> CollectorRef<C> {
        assert!(!C::SINGLETON);
        let raw_ptr = C::init(config, logger);
        CollectorRef { ptr: raw_ptr }
    }
}
impl<C> CollectorInit<C> for PhantomData<&'static C>
where
    C: SingletonCollector,
{
    fn with_logger(config: C::Config, logger: Logger) -> CollectorRef<C> {
        assert!(C::SINGLETON);
        C::init_global(config, logger); // TODO: Is this safe?
                                        // NOTE: The raw pointer is implicit (now that we're leaked)
        CollectorRef { ptr: PhantomData }
    }
}

impl<C: RawCollectorImpl> CollectorRef<C> {
    #[inline]
    pub fn create() -> Self
    where
        C::Ptr: CollectorInit<C>,
    {
        <C::Ptr as CollectorInit<C>>::create()
    }

    #[inline]
    pub fn with_logger(logger: Logger) -> Self
    where
        C::Ptr: CollectorInit<C>,
    {
        Self::with_config(C::Config::default(), logger)
    }

    pub fn with_config(config: C::Config, logger: Logger) -> Self
    where
        C::Ptr: CollectorInit<C>,
    {
        <C::Ptr as CollectorInit<C>>::with_logger(config, logger)
    }

    #[inline]
    pub(crate) fn clone_internal(&self) -> CollectorRef<C> {
        CollectorRef {
            ptr: unsafe { self.ptr.clone_owned() },
        }
    }

    #[inline]
    pub fn as_raw(&self) -> &C {
        unsafe { &*self.ptr.as_ptr() }
    }

    /// The id of this collector
    #[inline]
    pub fn id(&self) -> CollectorId<C> {
        CollectorId { ptr: self.ptr }
    }

    /// Convert this collector into a unique context
    ///
    /// The single-threaded implementation only allows a single context,
    /// so this method is nessicary to support it.
    pub fn into_context(self) -> CollectorContext<C> {
        unsafe { CollectorContext::register_root(&self) }
    }
}
impl<C: SyncCollector> CollectorRef<C> {
    /// Create a new context bound to this collector
    ///
    /// Warning: Only one collector should be created per thread.
    /// Doing otherwise can cause deadlocks/panics.
    pub fn create_context(&self) -> CollectorContext<C> {
        unsafe { CollectorContext::register_root(self) }
    }
}
impl<C: RawCollectorImpl> Drop for CollectorRef<C> {
    #[inline]
    fn drop(&mut self) {
        unsafe {
            self.ptr.drop();
        }
    }
}

unsafe impl<C: RawCollectorImpl> GcSystem for CollectorRef<C> {
    type Id = CollectorId<C>;
    type Context = CollectorContext<C>;
}

mod sealed {
    pub trait Sealed {}
}
