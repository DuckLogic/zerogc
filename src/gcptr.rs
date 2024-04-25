use std::alloc::Layout;
use std::any::Any;
use std::marker::PhantomData;
use std::mem::ManuallyDrop;
use std::ops::Deref;
use std::ptr::NonNull;

use crate::context::{GcHeader, GcTypeInfo};
use crate::{Collect, CollectContext, CollectorId};

pub struct Gc<'gc, T, Id: CollectorId> {
    ptr: NonNull<T>,
    marker: PhantomData<*const T>,
    collect_marker: PhantomData<&'gc Id::Collector>,
}
impl<'gc, T, Id: CollectorId> Gc<'gc, T, Id> {
    #[inline]
    pub fn id(&self) -> Id {
        match unsafe { Id::summon_singleton() } {
            None => self.header().id(),
            Some(id) => id,
        }
    }

    pub(crate) fn header(&self) -> &'_ GcHeader<Id> {
        unsafe {
            &*((self.ptr.as_ptr() as *mut u8)
                .sub(const { GcHeader::determine_layout(Layout::new()).value_offset })
                as *mut GcHeader<Id>)
        }
    }

    pub(crate) fn type_info() -> &'static GcTypeInfo<Id> {
        GcTypeInfo::of::<Self>()
    }

    #[inline(always)]
    pub unsafe fn from_raw_ptr(ptr: NonNull<T>, id: Id) -> Self {
        Gc {
            ptr,
            id,
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
        let resolved = target.as_ref();
        if matches!(Id::SINGLETON, None) && resolved.id() != context.id() {
            return;
        }
        context.trace_gcptr(target)
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
