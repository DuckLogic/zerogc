//! Our specific implementation of [::zerogc::GcRef]

use std::marker::PhantomData;
use std::ptr::NonNull;
use std::hash::{Hash, Hasher};

use zerogc::prelude::*;
use zerogc_derive::unsafe_gc_impl;

use super::CollectorId as SimpleCollectorId;

/// A garbage collected smart pointer
///
/// This is safely copyable. It can be freely
/// dereferenced and is guarenteed to live
/// until the next [safepoint](::zerogc::safepoint!)
#[repr(C)]
pub struct Gc<'gc, T: GcSafe + 'gc> {
    ptr: NonNull<T>,
    marker: PhantomData<&'gc T>
}
impl<'gc, T: GcSafe + 'gc> Gc<'gc, T> {
    /// The value of the pointer
    ///
    /// This is the same as [GcRef::value](::zerogc::GcRef::value)
    /// but is available as an inherent method for convenience.
    #[inline(always)]
    pub fn value(&self) -> &'gc T {
       unsafe { &*(&self.ptr as *const NonNull<T> as *const &T) }
    }
    #[inline]
    unsafe fn header(&self) -> *mut super::GcHeader {
        super::GcHeader::from_value_ptr(
            self.value() as *const T as *mut T,
            <T as super::StaticGcType>::STATIC_TYPE
        )
    }
}
unsafe impl<'gc, T: GcSafe + 'gc> GcRef<'gc, T> for Gc<'gc, T> {
    type Id = SimpleCollectorId;
    #[inline]
    fn collector_id(&self) -> Self::Id {
        unsafe {
            (*self.header()).collector_id
        }
    }
    #[inline]
    unsafe fn from_raw(id: SimpleCollectorId, ptr: NonNull<T>) -> Self {
        let res = Gc { ptr, marker: PhantomData };
        debug_assert_eq!(res.collector_id(), id);
        res
    }
    #[inline(always)]
    fn value(&self) -> &'gc T {
       unsafe { &*(&self.ptr as *const NonNull<T> as *const &T) }
    }
    #[inline]
    unsafe fn as_raw_ptr(&self) -> *mut T {
        self.ptr.as_ptr()
    }
    #[inline(always)]
    fn system(&self) -> &'_ <Self::Id as CollectorId>::System {
        // This assumption is safe - see the docs
        unsafe { (*self.header()).collector_id.assume_valid_system() }
    }
}
unsafe impl<'gc, O, V> ::zerogc::GcDirectBarrier<'gc, Gc<'gc, O>> for Gc<'gc, V>
    where O: GcSafe + 'gc, V: GcSafe + 'gc {
    #[inline(always)]
    unsafe fn write_barrier(&self, _owner: &Gc<'gc, O>, _field_offset: usize) {
        // This is a NOP 
    }
}
impl<'gc, T: GcSafe + 'gc> Copy for Gc<'gc, T> {}
impl<'gc, T: GcSafe + 'gc> Clone for Gc<'gc, T> {
    #[inline(always)]
    fn clone(&self) -> Self {
        *self
    }
}
unsafe_gc_impl!(
    target => Gc<'gc, T>,
    params => ['gc, T: GcSafe + 'gc],
    null_trace => never,
    bounds => {
        TraceImmutable => never,
        GcRebrand => { where T: GcRebrand<'new_gc, Id>, T::Branded: GcSafe },
        GcErase => { where T: GcErase<'min, Id>, T::Erased: GcSafe }
    },
    branded_type => Gc<'new_gc, T::Branded>,
    erased_type => Gc<'min, T::Erased>,
    NEEDS_TRACE => true, // We're the whole point
    NEEDS_DROP => false, // Copy
    trace_mut => |self, visitor| {
        unsafe { visitor.visit_gc(self) }
    }
);
impl<'gc, T: GcSafe +'gc> std::ops::Deref for Gc<'gc, T> {
    type Target = &'gc T;
    #[inline(always)]
    fn deref(&self) -> &&'gc T {
        unsafe { &*(&self.ptr as *const NonNull<T> as *const &T) }
    }
}

impl<'gc, T> PartialEq for Gc<'gc, T> 
    where T: GcSafe + PartialEq + 'gc {
    #[inline]
    fn eq(&self, other: &Self) -> bool {
        *self.value() == *other.value()
    }
}
impl<'gc, T> PartialEq<T> for Gc<'gc, T> 
    where T: GcSafe + PartialEq<T> + 'gc {
    #[inline]
    fn eq(&self, other: &T) -> bool {
        *self.value() == *other
    }
}
impl<'gc, T: GcSafe + Eq + 'gc> Eq for Gc<'gc, T> {}

impl<'gc, T> Hash for Gc<'gc, T> where T: GcSafe + Hash + 'gc {
    #[inline]
    fn hash<H: Hasher>(&self, hasher: &mut H) {
        T::hash(self.value(), hasher)
    }
}


/// In order to send *references* between threads,
/// the underlying type must be sync.
///
/// This is the same reason that `Arc<T>: Send` requires `T: Sync`
#[cfg(feature = "sync")]
unsafe impl<'gc, T> Send for Gc<'gc, T>
    where T: GcSafe + Sync {}

/// If the underlying type is `Sync`, it's safe
/// to share garbage collected references between threads.
///
/// The safety of the collector itself depends on the flag `cfg!(feature = "sync")`
/// If it is, the whole garbage collection implenentation should be as well.
#[cfg(feature = "sync")]
unsafe impl<'gc, T> Sync for Gc<'gc, T>
    where T: GcSafe + Sync {}
