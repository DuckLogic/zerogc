//! The in memory layout of objects using the simple collector.
//!
//! All objects are allocated with a [GcHeader],
//! that contains type information along with some internal mark bits.
//!
//! ## Safety
//! Relying on this internal layout is incredibly unsafe.
//!
//! However, `zerogc-simple` is committed to a stable ABI, so this should (hopefully)
//! be relatively well documented.
use std::mem::{self, };
use std::marker::PhantomData;
use std::cell::Cell;
use std::ffi::c_void;
use std::alloc::Layout;

use zerogc::{GcSafe, Trace};
use zerogc::vec::repr::{GcVecRepr,};

use zerogc::field_offset;
use zerogc_derive::{NullTrace, unsafe_gc_impl};

use crate::{RawMarkState, CollectorId, DynTrace, MarkVisitor};
use std::ptr::NonNull;
use zerogc::format::{ObjectFormat, GcHeader};

/// Everything but the lower 2 bits of mark data are unused
/// for CollectorId.
///
/// This is because we assume `align(usize) >= 4`
const STATE_MASK: usize = 0b11;
/// The internal 'marking data' used for the simple collector.
///
/// The internals of this structure are private.
/// As of right now, the simple collector has no extra room
/// for any user-defined metadata in this type.
#[repr(transparent)]
#[derive(NullTrace)]
pub struct SimpleMarkData {
    /// We're assuming that the collector is single threaded (which is true for now)
    data: Cell<usize>,
    #[zerogc(unsafe_skip_trace)]
    fmt: PhantomData<CollectorId>
}

impl SimpleMarkData {
    /// Create mark data from a specified snapshot
    #[inline]
    pub fn from_snapshot(snapshot: SimpleMarkDataSnapshot) -> Self {
        SimpleMarkData {
            data: Cell::new(snapshot.packed()),
            fmt: PhantomData
        }
    }
    #[inline]
    pub(crate) fn update_raw_state(&self, updated: RawMarkState) {
        let mut snapshot = self.load_snapshot();
        snapshot.state = updated;
        self.data.set(snapshot.packed());
    }
    /// Load a snapshot of the object's current marking state
    #[inline]
    pub fn load_snapshot(&self) -> SimpleMarkDataSnapshot {
        unsafe { SimpleMarkDataSnapshot::from_packed(self.data.get()) }
    }
}

/// A single snapshot of the state of the [SimpleMarkData]
///
/// This is needed because mark data can be updated by several threads,
/// possibly atomically.
pub struct SimpleMarkDataSnapshot<Fmt: ObjectFormat> {
    pub(crate) state: RawMarkState,
    #[cfg(feature = "multiple-collectors")]
    pub(crate) collector_id_ptr: *mut CollectorId<Fmt>
}
impl SimpleMarkDataSnapshot {
    pub(crate) fn new(state: RawMarkState, collector_id_ptr: *mut CollectorId) -> Self {
        #[cfg(feature = "multiple-collectors")] {
            SimpleMarkDataSnapshot { state, collector_id_ptr }
        }
        #[cfg(not(feature = "multiple-collectors"))] {
            drop(collector_id_ptr); // avoid warnings
            SimpleMarkDataSnapshot { state }
        }
    }
    #[inline]
    fn packed(&self) -> usize {
        let base: usize;
        #[cfg(feature = "multiple-collectors")] {
            base = self.collector_id_ptr as *const CollectorId as usize;
        }
        #[cfg(not(feature = "multiple-collectors"))] {
            base = 0;
        }
        debug_assert_eq!(base & STATE_MASK, 0);
        (base & !STATE_MASK) | (self.state as u8 as usize & STATE_MASK)
    }
    #[inline]
    unsafe fn from_packed(packed: usize) -> Self {
        let state = RawMarkState::from_byte((packed & STATE_MASK) as u8);
        let id_bytes: usize = packed & !STATE_MASK;
        #[cfg(feature="multiple-collectors")] {
            let collector_id_ptr = id_bytes as *mut CollectorId;
            SimpleMarkDataSnapshot { state, collector_id_ptr }
        }
        #[cfg(not(feature = "multiple-collectors"))] {
            SimpleMarkDataSnapshot { state }
        }
    }
}

/// Marker for an unknown GC object
#[repr(C)]
pub struct DynamicObj;
unsafe_gc_impl!(
    target => DynamicObj,
    params => [],
    NEEDS_TRACE => true,
    NEEDS_DROP => true,
    null_trace => never,
    visit => |self, visitor| {
        unreachable!()
    }
);

#[repr(C)]
pub(crate) struct BigGcObject<Fmt: ObjectFormat<CollectorId<Fmt>>> {
    pub(crate) header: NonNull<Fmt::VecHeader>
}
impl<Fmt: ObjectFormat<CollectorId<Fmt>>> BigGcObject<Fmt> {
    #[inline]
    pub unsafe fn header(&self) -> &Fmt::CommonHeader {
        self.header.as_ref()
    }
    #[inline]
    pub unsafe fn from_ptr(header: *mut Fmt::CommonHeader) -> BigGcObject<Fmt> {
        debug_assert!(!header.is_null());
        BigGcObject { header: NonNull::new_unchecked(header) }
    }
}
impl<Fmt: ObjectFormat<CollectorId<Fmt>>> Drop for BigGcObject<Fmt> {
    fn drop(&mut self) {
        unsafe {
            let type_info = self.header().type_info();
            {
                self.header().dynamic_drop();
            }
            let layout = type_info.determine_total_layout(self.header.as_ptr());
            let actual_header = type_info.header_layout()
                .from_common_header(self.header.as_ptr());
            std::alloc::dealloc(actual_header.cast(), layout);
        }
    }
}

/// The raw representation of a vector in the simple collector
///
/// NOTE: Length and capacity are stored implicitly in the [GcVecHeader]
#[repr(C)]
pub struct SimpleVecRepr<T: GcSafe, Fmt: ObjectFormat<CollectorId<Fmt>>> {
    marker: PhantomData<T>,
}
impl<T: GcSafe, Fmt: ObjectFormat<CollectorId<Fmt>>> SimpleVecRepr<T, Fmt> {
    #[inline]
    fn header(&self) -> *mut Fmt::VecHeader {
        unsafe {
            Fmt::resolve_vec_header(self) as *const Fmt::VecHeader as *mut Fmt::VecHeader
        }
    }
}
unsafe impl<T: GcSafe, Fmt: ObjectFormat<CollectorId<Fmt>>> GcVecRepr for SimpleVecRepr<T, Fmt> {
    /// Right now, there is no stable API for in-place re-allocation
    const SUPPORTS_REALLOC: bool = false;

    #[inline]
    fn element_layout(&self) -> Layout {
        Layout::new::<T>()
    }

    #[inline]
    fn len(&self) -> usize {
        unsafe { (*self.header()).len.get() }
    }

    #[inline]
    unsafe fn set_len(&self, len: usize) {
        debug_assert!(len <= self.capacity());
        (*self.header()).len.set(len);
    }

    #[inline]
    fn capacity(&self) -> usize {
        unsafe { (*self.header()).capacity }
    }

    #[inline]
    unsafe fn ptr(&self) -> *const c_void {
        self as *const Self as *const c_void // We are actually just a GC pointer to the value ptr
    }

    unsafe fn unchecked_drop<ActualT: GcSafe>(&mut self) {
        std::ptr::drop_in_place::<[ActualT]>(core::ptr::slice_from_raw_parts_mut(
            self.ptr() as *mut ActualT,
            self.len()
        ))
    }
}
unsafe_gc_impl!(
    target => SimpleVecRepr<T, Fmt>,
    params => [T: GcSafe, Fmt: ObjectFormat<CollectorId<Fmt>>],
    NEEDS_TRACE => T::NEEDS_TRACE,
    NEEDS_DROP => T::NEEDS_DROP,
    null_trace => { where T: ::zerogc::NullTrace },
    trace_mut => |self, visitor| {
        // Trace our innards
        unsafe {
            let start: *mut T = self.ptr() as *const T as *mut T;
            for i in 0..self.len() {
                visitor.visit(&mut *start.add(i))?;
            }
        }
        Ok(())
    },
    trace_immutable => |self, visitor| {
        // Trace our innards
        unsafe {
            let start: *mut T = self.ptr() as *const T as *mut T;
            for i in 0..self.len() {
                visitor.visit_immutable(&*start.add(i))?;
            }
        }
        Ok(())
    }
);


/// Marker type for an unknown header
pub(crate) struct UnknownHeader(());
