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
use std::mem::{ManuallyDrop};
use std::marker::PhantomData;
use std::cell::Cell;

use zerogc_context::field_offset;
use zerogc_derive::NullTrace;
use crate::{RawMarkState, CollectorId, DynTrace, MarkVisitor};
use std::ffi::c_void;
use std::alloc::Layout;
use zerogc::{GcSafe, Trace};
use std::mem;


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
pub struct SimpleMarkDataSnapshot {
    pub(crate) state: RawMarkState,
    #[cfg(feature = "multiple-collectors")]
    pub(crate) collector_id_ptr: *mut CollectorId
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
pub(crate) struct DynamicObj;

#[repr(C)]
pub(crate) struct BigGcObject<T = DynamicObj> {
    pub(crate) header: GcHeader,
    /// This is dropped using dynamic type info
    pub(crate) static_value: ManuallyDrop<T>
}
impl<T> BigGcObject<T> {
    #[inline]
    pub(crate) unsafe fn into_dynamic_box(val: Box<Self>) -> Box<BigGcObject<DynamicObj>> {
        std::mem::transmute::<Box<BigGcObject<T>>, Box<BigGcObject<DynamicObj>>>(val)
    }
}
impl<T> Drop for BigGcObject<T> {
    fn drop(&mut self) {
        unsafe {
            let type_info = self.header.type_info;
            if let Some(func) = type_info.drop_func {
                func((&self.header as *const GcHeader as *const u8 as *mut u8)
                    .add(type_info.value_offset)
                    .cast());
            }
        }
    }
}
/// A header for a GC array object
#[repr(C)]
pub struct GcArrayHeader {
    pub(crate) len: usize,
    pub(crate) common_header: GcHeader
}
impl GcArrayHeader {
    #[inline]
    pub(crate) const fn array_value_offset(align: usize) -> usize {
        GcHeader::value_offset_for(align) + field_offset!(GcArrayHeader, common_header)
    }
}

/// A header for a GC object
///
/// This is shared between both small arenas
/// and fallback alloc vis `BigGcObject`
#[repr(C)]
pub struct GcHeader {
    /// The type information
    pub type_info: &'static GcType,
    /// The mark data
    pub mark_data: SimpleMarkData
}
impl GcHeader {
    /// Create a new header
    #[inline]
    pub fn new(type_info: &'static GcType, mark_data: SimpleMarkData) -> Self {
        GcHeader { type_info, mark_data }
    }
    /// A pointer to the header's value
    #[inline]
    pub fn value(&self) -> *mut c_void {
        unsafe {
            (self as *const GcHeader as *mut GcHeader as *mut u8)
                // NOTE: This takes into account the possibility of `BigGcObject`
                .add(self.type_info.value_offset)
                .cast::<c_void>()
        }
    }
    pub(crate) const fn value_offset_for(align: usize) -> usize {
        // NOTE: Header
        let layout = Layout::new::<GcHeader>();
        let regular_padding = layout.padding_needed_for(align);
        let array_padding = Layout::new::<GcArrayHeader>().padding_needed_for(align);
        debug_assert!(regular_padding == array_padding);
        layout.size() + regular_padding
    }
    /// Get the [GcHeader] for the specified value, assuming that its been allocated by this collector.
    ///
    /// ## Safety
    /// Assumes the value was allocated in the simple collector.
    #[inline]
    pub unsafe fn from_value_ptr<T: ?Sized>(ptr: *mut T) -> *mut GcHeader {
        (ptr as *mut u8).sub(GcHeader::value_offset_for(std::mem::align_of_val(&*ptr))).cast()
    }
    #[inline]
    pub(crate) fn raw_mark_state(&self) -> RawMarkState {
        // TODO: Is this safe? Couldn't it be accessed concurrently?
        self.mark_data.load_snapshot().state
    }
    #[inline]
    pub(crate) fn update_raw_mark_state(&self, raw_state: RawMarkState) {
        self.mark_data.update_raw_state(raw_state);
    }
}
/// Layout information on a [GcType]
pub enum GcTypeLayout {
    /// A type with a fixed, statically-known layout
    Fixed(Layout),
    /// An array, whose type can vary at runtime
    Array {
        /// The fixed layout of elements in the array
        ///
        /// The overall alignment of the array is equal to the alignment of each element,
        /// however the size may vary at runtime.
        element_layout: Layout
    }
}

/// A type used by GC
#[repr(C)]
pub struct GcType {
    /// Information on the type's layout
    pub layout: GcTypeLayout,
    /// The offset of the value from the start of the header
    ///
    /// This varies depending on the type's alignment
    pub value_offset: usize,
    /// The function to trace the type, or `None` if it doesn't need to be traced
    pub trace_func: Option<unsafe fn(*mut c_void, &mut MarkVisitor)>,
    /// The function to drop the type, or `None` if it doesn't need to be dropped
    pub drop_func: Option<unsafe fn(*mut c_void)>,
}
impl GcType {
    #[inline]
    unsafe fn determine_size(&self, header: *mut GcHeader) -> usize {
        match self.layout {
            GcTypeLayout::Fixed(layout) => layout.size(),
            GcTypeLayout::Array { element_layout } => {
                let header = (header as *mut u8)
                    .sub(field_offset!(GcArrayHeader, common_header))
                    .cast::<GcArrayHeader>();
                element_layout.repeat((*header).len).unwrap().0
                    .pad_to_align().size()
            }
        }
    }
    #[inline]
    pub(crate) unsafe fn determine_total_size(&self, header: *mut GcHeader) -> usize {
        let mut res = self.value_offset + self.determine_size(header);
        match self.layout {
            GcTypeLayout::Fixed(_) => {}
            GcTypeLayout::Array { .. } => {
                res += field_offset!(GcArrayHeader, common_header);
            }
        }
        res
    }
}

pub(crate) trait StaticGcType {
    const VALUE_OFFSET: usize;
    const STATIC_TYPE: &'static GcType;
}
impl<T: GcSafe> StaticGcType for [T] {
    const VALUE_OFFSET: usize = GcHeader::value_offset_for(std::mem::align_of::<T>());
    const STATIC_TYPE: &'static GcType = &GcType {
        layout: GcTypeLayout::Array { element_layout: Layout::new::<T>() },
        value_offset: Self::VALUE_OFFSET,
        trace_func: if <T as Trace>::NEEDS_TRACE {
            Some({
                unsafe fn visit<T: Trace>(val: *mut c_void, visitor: &mut MarkVisitor) {
                    let len = (*(val as *mut u8)
                        .sub(GcArrayHeader::array_value_offset(std::mem::align_of::<T>()))
                        .cast::<GcArrayHeader>()).len;
                    let slice = std::slice::from_raw_parts_mut(
                        val as *mut T,
                        len
                    );
                    let Ok(()) = <[T] as Trace>::visit(slice, visitor);
                }
                visit::<T> as unsafe fn(*mut c_void, &mut MarkVisitor)
            })
        } else { None },
        drop_func: if <T as GcSafe>::NEEDS_DROP {
            Some({
                unsafe fn drop_gc_slice<T: GcSafe>(val: *mut c_void) {
                    let len = (*(val as *mut u8)
                        .sub(GcArrayHeader::array_value_offset(std::mem::align_of::<T>()))
                        .cast::<GcArrayHeader>()).len;
                    std::ptr::drop_in_place::<[T]>(std::ptr::slice_from_raw_parts_mut(
                        val as *mut T,
                        len
                    ));
                }
                drop_gc_slice::<T> as unsafe fn(*mut c_void)
            })
        } else { None }
    };
}
impl<T: GcSafe> StaticGcType for T {
    const VALUE_OFFSET: usize = GcHeader::value_offset_for(std::mem::align_of::<T>());
    const STATIC_TYPE: &'static GcType = &GcType {
        layout: GcTypeLayout::Fixed(Layout::new::<T>()),
        value_offset: Self::VALUE_OFFSET,
        trace_func: if <T as Trace>::NEEDS_TRACE {
            Some(unsafe { mem::transmute::<_, unsafe fn(*mut c_void, &mut MarkVisitor)>(
                <T as DynTrace>::trace as fn(&mut T, &mut MarkVisitor),
            ) })
        } else { None },
        drop_func: if <T as GcSafe>::NEEDS_DROP {
            unsafe { Some(mem::transmute::<_, unsafe fn(*mut c_void)>(
                std::ptr::drop_in_place::<T> as unsafe fn(*mut T)
            )) }
        } else { None }
    };
}
