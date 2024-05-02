use log::debug;
use std::marker::PhantomData;
use std::ops::Deref;
use std::ptr::NonNull;

use crate::context::{GcHeader, GcTypeInfo};
use crate::{Collect, CollectContext, CollectorId, GarbageCollector};

pub struct Gc<'gc, T, Id: CollectorId> {
    ptr: NonNull<T>,
    marker: PhantomData<*const T>,
    collect_marker: PhantomData<&'gc GarbageCollector<Id>>,
}
impl<'gc, T: Collect<Id>, Id: CollectorId> Gc<'gc, T, Id> {
    #[inline]
    pub fn id(&self) -> Id {
        match unsafe { Id::summon_singleton() } {
            None => self.header().id(),
            Some(id) => id,
        }
    }

    #[inline]
    pub(crate) fn header(&self) -> &'_ GcHeader<Id> {
        unsafe {
            &*((self.ptr.as_ptr() as *mut u8).sub(GcHeader::<Id>::FIXED_ALIGNMENT)
                as *mut GcHeader<Id>)
        }
    }

    #[inline]
    pub(crate) fn type_info() -> &'static GcTypeInfo<Id> {
        GcTypeInfo::new::<Self>()
    }

    #[inline(always)]
    pub unsafe fn as_raw_ptr(&self) -> NonNull<T> {
        self.ptr
    }

    #[inline(always)]
    pub unsafe fn from_raw_ptr(ptr: NonNull<T>) -> Self {
        Gc {
            ptr,
            marker: PhantomData,
            collect_marker: PhantomData,
        }
    }
}
unsafe impl<'gc, Id: CollectorId, T: Collect<Id>> Collect<Id> for Gc<'gc, T, Id> {
    type Collected<'newgc> = Gc<'newgc, T::Collected<'newgc>, Id>;
    const NEEDS_COLLECT: bool = true;

    #[inline]
    unsafe fn collect_inplace(target: NonNull<Self>, context: &mut CollectContext<'_, Id>) {
        if matches!(Id::SINGLETON, None) && target.as_ref().id() != context.id() {
            return;
        }
        context.trace_gc_ptr_mut(target)
    }
}
impl<'gc, T, Id: CollectorId> Deref for Gc<'gc, T, Id> {
    type Target = T;

    #[inline(always)]
    fn deref(&self) -> &Self::Target {
        unsafe { self.ptr.as_ref() }
    }
}
impl<'gc, T, Id: CollectorId> Copy for Gc<'gc, T, Id> {}

impl<'gc, T, Id: CollectorId> Clone for Gc<'gc, T, Id> {
    #[inline]
    fn clone(&self) -> Self {
        *self
    }
}
