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
use std::cell::Cell;
use std::ptr::NonNull;

use zerogc_derive::{NullTrace, unsafe_gc_impl};

use crate::{RawMarkState, CollectorId, SimpleCollectorContext, RawSimpleCollector, ObjectFormat};
use zerogc::format::{ObjectFormat as ObjectFormatAPI, GcHeader, GcInfo, MarkData, GcCommonHeader, GcTypeInfo, HeaderLayout};

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
#[derive(NullTrace, Clone)]
pub struct SimpleMarkData {
    /// We're assuming that the collector is single threaded (which is true for now)
    data: Cell<usize>,
}

impl SimpleMarkData {
    /// Create mark data from a specified snapshot
    #[inline]
    pub fn from_snapshot<Fmt: ObjectFormat>(snapshot: SimpleMarkDataSnapshot<Fmt>) -> Self {
        SimpleMarkData {
            data: Cell::new(snapshot.packed()),
        }
    }

    #[inline]
    pub(crate) unsafe fn update_raw_state<Fmt: ObjectFormat>(&self, updated: RawMarkState) {
        let mut snapshot = self.load_snapshot::<Fmt>();
        snapshot.state = updated;
        self.data.set(snapshot.packed());
    }
    /// Load a snapshot of the object's current marking state
    ///
    /// ## Safety
    /// Unsafely assumes the format matches.
    #[inline]
    pub unsafe fn load_snapshot<Fmt: ObjectFormat>(&self) -> SimpleMarkDataSnapshot<Fmt> {
        SimpleMarkDataSnapshot::from_packed(self.data.get())
    }
}
unsafe impl MarkData for SimpleMarkData {}

/// A single snapshot of the state of the [SimpleMarkData]
///
/// This is needed because mark data can be updated by several threads,
/// possibly atomically.
pub struct SimpleMarkDataSnapshot<Fmt: ObjectFormat> {
    pub(crate) state: RawMarkState,
    #[cfg(feature = "multiple-collectors")]
    pub(crate) collector_id_ptr: *mut CollectorId<Fmt>
}
impl<Fmt: ObjectFormat> SimpleMarkDataSnapshot<Fmt> {
    pub(crate) fn new(state: RawMarkState, collector_id_ptr: *mut CollectorId<Fmt>) -> Self {
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
            base = self.collector_id_ptr as *const CollectorId<Fmt> as usize;
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
            let collector_id_ptr = id_bytes as *mut CollectorId<Fmt>;
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
pub(crate) struct BigGcObject<Fmt: ObjectFormat> {
    pub(crate) header: NonNull<Fmt::CommonHeader>
}
impl<Fmt: ObjectFormat> BigGcObject<Fmt> {
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
impl<Fmt: ObjectFormat> Drop for BigGcObject<Fmt> {
    fn drop(&mut self) {
        unsafe {
            let type_info = self.header().type_info();
            {
                self.header().dynamic_drop();
            }
            let layout = type_info.determine_total_layout(self.header.as_ref());
            let actual_header = type_info.header_layout()
                .from_common_header(self.header.as_ptr());
            std::alloc::dealloc(actual_header.cast(), layout);
        }
    }
}

/// The implementation of [GcInfo] for the simple collector
#[derive(zerogc_derive::NullTrace)]
pub struct SimpleCollectorInfo {
}
unsafe impl GcInfo for SimpleCollectorInfo {
    type PreferredVisitor<Fmt: ObjectFormat> = crate::MarkVisitor<'static, Fmt>;
    type PreferredVisitorErr = !;
    type MarkData = SimpleMarkData;
    const IS_MOVING: bool = false;
}
