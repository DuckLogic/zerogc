use std::mem::ManuallyDrop;
use std::ffi::c_void;
use std::alloc::Layout;
use std::mem;
use std::ptr::{Pointee, DynMetadata, NonNull};

use zerogc::GcSafe;
use zerogc_context::field_offset;
use zerogc_context::utils::AtomicCell;
use zerogc_derive::NullTrace;
use crate::{RawMarkState, CollectorId, MarkVisitor, MarkState, RawSimpleCollector, RawObjectFormat};
use std::num::NonZeroUsize;
use std::sync::atomic::{Ordering, AtomicUsize};
use zerogc::format::{ObjectFormat, OpenAllocObjectFormat, GcTypeInfo};
use std::marker::PhantomData;
use std::cell::Cell;

/// Everything but the lower 2 bits of mark data are unused
/// for CollectorId.
///
/// This is because we assume `align(usize) >= 4`
const STATE_MASK: usize = 0b11;
/// The 'mark data' used for the simple collector
///
/// TODO: Do we need to use atomics?
/// I don't think so, since we're a simple collector
#[repr(transparent)]
#[derive(NullTrace)]
#[zerogc(ignore_params(Fmt))]
pub struct SimpleMarkData<Fmt: RawObjectFormat> {
    /// We're assuming that the collector is single threaded (which is true for now)
    data: Cell<usize>,
    #[zerogc(unsafe_skip_trace)]
    fmt: PhantomData<CollectorId<Fmt>>
}

impl<Fmt: RawObjectFormat> SimpleMarkData<Fmt> {

}

impl<Fmt: RawObjectFormat> SimpleMarkData<Fmt> {
    #[inline]
    pub fn from_snapshot(snapshot: SimpleMarkDataSnapshot<Fmt>) -> Self {
        SimpleMarkData {
            data: Cell::new(snapshot.packed()),
            fmt: PhantomData
        }
    }
    #[inline]
    pub(crate) fn update_raw_state(&self, updated: RawMarkState) {
        self.data.set(updated.packed());
    }
    #[inline]
    pub fn load_snapshot(&self) -> SimpleMarkDataSnapshot<Fmt> {
        unsafe { SimpleMarkDataSnapshot::from_packed(self.data.get()) }
    }
    #[inline]
    fn compare_exchange_snapshot(&self, old: SimpleMarkDataSnapshot<Fmt>, new: SimpleMarkDataSnapshot<Fmt>) -> Result<SimpleMarkDataSnapshot<Fmt>, SimpleMarkDataSnapshot<Fmt>> {
        let actual = self.data.get();
        if actual == old.packed() {
            self.data.set(new.packed());
            Ok(old)
        } else {
            Err(unsafe { SimpleMarkDataSnapshot::from_packed(actual) })
        }
    }
}

pub struct SimpleMarkDataSnapshot<Fmt: RawObjectFormat> {
    pub state: RawMarkState,
    #[cfg(feature = "multiple-collectors")]
    pub collector_id: CollectorId<Fmt>
}
impl<Fmt: RawObjectFormat> SimpleMarkDataSnapshot<Fmt> {
    pub fn new(state: RawMarkState, collector_id: CollectorId<Fmt>) -> Self {
        #[cfg(feature = "multiple-collectors")] {
            SimpleMarkDataSnapshot { state, collector_id }
        }
        #[cfg(not(feature = "multiple-collectors"))] {
            drop(collector_id); // avoid warnings
            SimpleMarkDataSnapshot { state }
        }
    }
    #[inline]
    fn packed(&self) -> usize {
        let base: usize;
        #[cfg(feature = "multiple-collectors")] {
            base = self.collector_id.as_ref() as *const RawSimpleCollector<Fmt> as usize;
        }
        #[cfg(not(feature = "multiple-collectors"))] {
            base = 0;
        }
        debug_assert_eq!(base & STATE_MASK, 0);;
        (base & !STATE_MASK) | (self.state as u8 as usize & STATE_MASK)
    }
    #[inline]
    unsafe fn from_packed(packed: usize) -> Self {
        let state = MarkState::from_byte((packed & STATE_MASK) as u8);
        let id_bytes: usize = packed & !STATE_MASK;
        #[cfg(feature="multiple-collectors")] {
            let collector_id = CollectorId::from_raw(NonNull::new_unchecked(id_bytes));
            SimpleMarkDataSnapshot { state, collector_id }
        }
        #[cfg(not(feature = "multiple-collectors"))] {
            SimpleMarkDataSnapshot { state }
        }
    }
}

/// Marker for an unknown GC object
struct DynamicObj;

#[repr(C)]
pub(crate) struct BigGcObject<Fmt: RawObjectFormat, T = DynamicObj> {
    pub(crate) header: Fmt::SizedHeaderType,
    /// This is dropped using dynamic type info
    pub(crate) static_value: ManuallyDrop<T>
}
impl<Fmt: RawObjectFormat, T> BigGcObject<Fmt, T> {
    #[inline]
    pub(crate) unsafe fn into_dynamic_box(val: Box<Self>) -> Box<BigGcObject<Fmt, DynamicObj>> {
        std::mem::transmute::<Box<BigGcObject<Fmt, T>>, Box<BigGcObject<Fmt, DynamicObj>>>(val)
    }
    #[inline]
    fn as_dynamic(&self) -> Fmt::DynObject {
        unsafe { Fmt::untyped_object_from_raw(&*self.static_value as *const T as *mut c_void) }
    }
}
impl<Fmt: RawObjectFormat, T> Drop for BigGcObject<Fmt, T> {
    fn drop(&mut self) {
        unsafe {
            let type_info = Fmt::determine_type(self.as_dynamic());
            type_info.drop(self.as_dynamic());
        }
    }
}
