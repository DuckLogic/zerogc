//! The interface to a collector

use std::ptr::NonNull;
use std::sync::Arc;
use std::fmt::{self, Debug, Formatter};

use slog::{Logger, o};

use zerogc::{Gc, GcSafe, GcSystem, Trace, GcSimpleAlloc, GcBrand, GcHandle, GcHandleSystem};

use crate::{SimpleCollectorContext, CollectionManager};

/// A specific implementation of a collector
pub unsafe trait RawCollectorImpl: 'static + Sized {
    /// A dynamic pointer to a GC object
    ///
    /// The simple collector implements this as
    /// a trait object pointer.
    type GcDynPointer: Copy + Debug + 'static;

    /// Convert the specified value into a dyn pointer
    unsafe fn create_dyn_pointer<T: Trace>(t: *mut T) -> Self::GcDynPointer;

    #[cfg(not(feature = "multiple-collectors"))]
    /// When the collector is a singleton,
    /// return the global implementation
    fn global_ptr() -> *const Self;
    #[cfg(not(feature = "multiple-collectors"))]
    fn init_global(logger: Logger);
    #[cfg(feature = "multiple-collectors")]
    fn init(logger: Logger) -> NonNull<Self>;
    /// The id of this collector
    #[inline]
    fn id(&self) -> CollectorId<Self> {
        #[cfg(not(feature = "multiple-collectors"))] {
            CollectorId {}
        }
        #[cfg(feature = "multiple-collectors")] {
            CollectorId { rc: NonNull::from(self) }
        }
    }
    unsafe fn gc_write_barrier<'gc, T, V>(
        owner: &Gc<'gc, T, CollectorId<Self>>,
        value: &Gc<'gc, V, CollectorId<Self>>,
        field_offset: usize
    ) where T: GcSafe + ?Sized + 'gc, V: GcSafe + ?Sized + 'gc;
    /// The logger associated with this collector
    fn logger(&self) -> &Logger;

    fn manager(&self) -> &CollectionManager<Self>;

    fn should_collect(&self) -> bool;

    fn allocated_size(&self) -> crate::utils::MemorySize;

    unsafe fn perform_raw_collection(&self, contexts: &[*mut crate::RawContext<Self>]);
}

impl<C: RawCollectorImpl> PartialEq for CollectorId<C> {
    #[inline]
    fn eq(&self, other: &Self) -> bool {
        #[cfg(feature = "multiple-collectors")] {
            self.rc.as_ptr() == other.rc.as_ptr()
        }
        #[cfg(not(feature = "multiple-collectors"))] {
            true // Singleton
        }
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
        #[cfg(feature = "multiple-collectors")] {
            debug.field("rc", &self.rc);
        }
        debug.finish()
    }
}

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
    #[cfg(feature = "multiple-collectors")]
    /// This is in essence a borrowed reference to
    /// the collector. SimpleCollector is implemented as a
    /// raw pointer via [Arc::into_raw]
    ///
    /// We don't know whether the underlying memory will be valid.
    rc: NonNull<C>,
}
impl<C: RawCollectorImpl> CollectorId<C> {
    #[cfg(feature = "multiple-collectors")]
    #[inline]
    pub unsafe fn from_raw(rc: NonNull<C>) -> CollectorId<C> {
        CollectorId { rc }
    }
    #[cfg(feature = "multiple-collectors")]
    #[inline]
    pub unsafe fn as_ref(&self) -> &C {
        self.rc.as_ref()
    }
    #[cfg(not(feature = "multiple-collectors"))]
    #[inline]
    pub unsafe fn as_ref(&self) -> &C {
        &*GLOBAL_COLLECTOR.load(Ordering::Acquire)
    }
    #[cfg(feature = "multiple-collectors")]
    pub unsafe fn weak_ref(&self) -> WeakCollectorRef<C> {
        let arc = Arc::from_raw(self.rc.as_ptr());
        let weak = Arc::downgrade(&arc);
        std::mem::forget(arc);
        WeakCollectorRef { weak }
    }
    #[cfg(not(feature = "multiple-collectors"))]
    pub unsafe fn weak_ref(&self) -> WeakCollectorRef<C> {
        WeakCollectorRef { }
    }
}
unsafe impl<C: RawCollectorImpl> ::zerogc::CollectorId for CollectorId<C> {
    type System = CollectorRef<C>;

    #[inline(always)]
    unsafe fn gc_write_barrier<'gc, T, V>(
        owner: &Gc<'gc, T, Self>,
        value: &Gc<'gc, V, Self>,
        field_offset: usize
    ) where T: GcSafe + ?Sized + 'gc, V: GcSafe + ?Sized + 'gc {
        C::gc_write_barrier(owner, value, field_offset)
    }

    #[inline]
    unsafe fn assume_valid_system(&self) -> &'_ Self::System {
        // TODO: Make the API nicer? (avoid borrowing and indirection)
        #[cfg(feature = "multiple-collectors")] {
            assert_eq!(
                std::mem::size_of::<Self>(),
                std::mem::size_of::<CollectorRef<C>>()
            );
            &*(self as *const CollectorId<C> as *const CollectorRef<C>)
        }
        #[cfg(not(feature = "multiple-collectors"))] {
            // NOTE: We live forever
            const COLLECTOR: CollectorRef = CollectorRef { };
            &COLLECTOR
        }
    }
}

pub struct WeakCollectorRef<C: RawCollectorImpl> {
    #[cfg(feature = "multiple-collectors")]
    weak: std::sync::Weak<C>,
}
impl<C: RawCollectorImpl> WeakCollectorRef<C> {
    #[cfg(feature = "multiple-collectors")]
    pub unsafe fn assume_valid(&self) -> CollectorId<C> {
        debug_assert!(
            self.weak.upgrade().is_some(),
            "Dead collector"
        );
        CollectorId {
            rc: NonNull::new_unchecked(self.weak.as_ptr() as *mut _)
        }
    }
    #[cfg(not(feature = "multiple-collectors"))]
    pub unsafe fn assume_valid(&self) -> CollectorId<C> {
        CollectorId {}
    }
    pub fn ensure_valid<R>(&self, func: impl FnOnce(CollectorId<C>) -> R) -> R {
        self.try_ensure_valid(|id| match id{
            Some(id) => func(id),
            None => panic!("Dead collector")
        })
    }
    #[cfg(feature = "multiple-collectors")]
    pub fn try_ensure_valid<R>(&self, func: impl FnOnce(Option<CollectorId<C>>) -> R) -> R{
        let rc = self.weak.upgrade();
        func(match rc {
            Some(_) => {
                Some(unsafe { self.assume_valid() })
            },
            None => None
        })
    }
    #[cfg(not(feature = "multiple-collectors"))]
    pub fn try_ensure_valid<R>(&self, func: impl FnOnce(Option<CollectorId<C>>) -> R) -> R {
        // global collector is always valid
        func(Some(CollectorId {}))
    }
}

pub unsafe trait RawSimpleAlloc: RawCollectorImpl {
    fn alloc<'gc, T: GcSafe + 'gc>(context: &'gc SimpleCollectorContext<Self>, value: T) -> Gc<'gc, T, CollectorId<Self>>;
}
unsafe impl<'gc, T, C> GcSimpleAlloc<'gc, T> for SimpleCollectorContext<C>
    where T: GcSafe + 'gc, C: RawSimpleAlloc {
    #[inline]
    fn alloc(&'gc self, value: T) -> Gc<'gc, T, Self::Id> {
        C::alloc(self, value)
    }
}

/// A reference to the collector.
///
/// TODO: Devise better name
#[repr(C)]
pub struct CollectorRef<C: RawCollectorImpl> {
    /// In reality, this is just an [Arc].
    ///
    /// It is implemented as a raw pointer around [Arc::into_raw]
    #[cfg(feature = "multiple-collectors")]
    rc: NonNull<C>
}
/// We actually are thread safe ;)
#[cfg(feature = "sync")]
unsafe impl<C: RawCollectorImpl + Sync> Send for CollectorRef<C> {}
#[cfg(feature = "sync")]
unsafe impl<C: RawCollectorImpl + Sync> Sync for CollectorRef<C> {}

impl<C: RawCollectorImpl> CollectorRef<C> {
    pub fn create() -> Self {
        CollectorRef::with_logger(Logger::root(
            slog::Discard,
            o!()
        ))
    }
    #[cfg(not(feature = "multiple-collectors"))]
    pub fn with_logger(logger: Logger) -> Self {
        unsafe { C::init_global(logger) }
        // NOTE: The raw pointer is implicit (now that we're leaked)
        CollectorRef {}
    }

    #[cfg(feature = "multiple-collectors")]
    pub fn with_logger(logger: Logger) -> Self {
        let raw_ptr = C::init(logger);
        CollectorRef { rc: raw_ptr }
    }
    #[cfg(feature = "multiple-collectors")]
    pub(crate) fn clone_internal(&self) -> CollectorRef<C> {
        let original = unsafe { Arc::from_raw(self.rc.as_ptr()) };
        let cloned = Arc::clone(&original);
        std::mem::forget(original);
        CollectorRef { rc: unsafe { NonNull::new_unchecked(
            Arc::into_raw(cloned) as *mut _
        ) } }
    }
    #[cfg(not(feature = "multiple-collectors"))]
    pub(crate) fn clone_internal(&self) -> CollectorRef<C> {
        CollectorRef {}
    }
    #[cfg(feature = "multiple-collectors")]
    #[inline]
    pub fn as_raw(&self) -> &C {
        unsafe { self.rc.as_ref() }
    }
    #[cfg(not(feature = "multiple-collectors"))]
    #[inline]
    pub fn as_raw(&self) -> &C {
        let ptr = GLOBAL_COLLECTOR.load(Ordering::Acquire);
        assert!(!ptr.is_null());
        unsafe { &*ptr }
    }

    /// Create a new context bound to this collector
    ///
    /// Warning: Only one collector should be created per thread.
    /// Doing otherwise can cause deadlocks/panics.
    #[cfg(feature = "sync")]
    pub fn create_context(&self) -> SimpleCollectorContext<C> {
        unsafe { SimpleCollectorContext::register_root(&self) }
    }
    /// Convert this collector into a unique context
    ///
    /// The single-threaded implementation only allows a single context,
    /// so this method is nessicarry to support it.
    pub fn into_context(self) -> SimpleCollectorContext<C> {
        #[cfg(feature = "sync")]
            { self.create_context() }
        #[cfg(not(feature = "sync"))]
            unsafe { SimpleCollectorContext::from_collector(&self) }
    }
}
impl<C: RawCollectorImpl> Drop for CollectorRef<C> {
    fn drop(&mut self) {
        #[cfg(feature = "multiple-collectors")] {
            drop(unsafe { Arc::from_raw(self.rc.as_ptr() as *const _) })
        }
    }
}

unsafe impl<C: RawCollectorImpl> GcSystem for CollectorRef<C> {
    type Id = CollectorId<C>;
    type Context = SimpleCollectorContext<C>;
}

unsafe impl<'gc, T, C> GcHandleSystem<'gc, T> for CollectorRef<C>
    where C: RawHandleImpl<'gc, T>,
          T: GcSafe + 'gc,
          T: GcBrand<'static, CollectorId<C>>,
          T::Branded: GcSafe {
    type Handle = C::Handle;

    #[inline]
    fn create_handle(gc: Gc<'gc, T, Self::Id>) -> Self::Handle {
        C::create_handle(gc)
    }
}

pub unsafe trait RawHandleImpl<'gc, T>: RawCollectorImpl
    where T: GcSafe + 'gc,
          T: GcBrand<'static, CollectorId<Self>>,
          <T as GcBrand<'static, CollectorId<Self>>>::Branded: GcSafe {
    /// The type of handles to the object `T`.
    type Handle: GcHandle<<T as GcBrand<'static, CollectorId<Self>>>::Branded, System=CollectorRef<Self>>;

    /// Create a handle to the specified GC pointer,
    /// which can be used without a context
    ///
    /// The system is implicit in the [Gc]
    fn create_handle(gc: Gc<'gc, T, CollectorId<Self>>) -> Self::Handle;
}
