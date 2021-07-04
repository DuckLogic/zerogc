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
use zerogc::vec::repr::{GcVecRepr, ReallocFailedError};

use zerogc_context::field_offset;
use zerogc_derive::{NullTrace, unsafe_gc_impl};

use crate::{RawMarkState, CollectorId, DynTrace, MarkVisitor};
use crate::alloc::fits_small_object;
use std::ptr::NonNull;

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
pub(crate) struct BigGcObject {
    pub(crate) header: NonNull<GcHeader>
}
impl BigGcObject {
    #[inline]
    pub unsafe fn header(&self) -> &GcHeader {
        self.header.as_ref()
    }
    #[inline]
    pub unsafe fn from_ptr(header: *mut GcHeader) -> BigGcObject {
        debug_assert!(!header.is_null());
        BigGcObject { header: NonNull::new_unchecked(header) }
    }
}
impl Drop for BigGcObject {
    fn drop(&mut self) {
        unsafe {
            let type_info = self.header().type_info;
            if let Some(func) = type_info.drop_func {
                func((self.header.as_ptr() as *const u8 as *mut u8)
                    .add(type_info.value_offset)
                    .cast());
            }
            let layout = type_info.determine_total_layout(self.header.as_ptr());
            std::alloc::dealloc(self.header.as_ptr().cast(), layout);
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
/// A header for a Gc vector
#[repr(C)]
pub struct GcVecHeader {
    pub(crate) capacity: usize,
    pub(crate) len: Cell<usize>,
    pub(crate) common_header: GcHeader
}
impl GcVecHeader {
    #[inline]
    pub(crate) const fn vec_value_offset(align: usize) -> usize {
        GcHeader::value_offset_for(align) + field_offset!(GcVecHeader, common_header)
    }
}

/// The raw representation of a vector in the simple collector
///
/// NOTE: Length and capacity are stored implicitly in the [GcVecHeader]
#[repr(C)]
pub struct SimpleVecRepr<T: GcSafe> {
    marker: PhantomData<T>,
}
impl<T: GcSafe> SimpleVecRepr<T> {
    /// Get the in-memory layout for a [SimpleVecRepr],
    /// including its header
    #[inline]
    pub fn layout(capacity: usize) -> Layout {
        Layout::new::<GcVecHeader>()
            .extend(Layout::array::<T>(capacity).unwrap()).unwrap().0
    }
    #[inline]
    fn header(&self) -> *mut GcVecHeader {
        unsafe {
            (self as *const Self as *mut Self as *mut u8)
                .sub(GcVecHeader::vec_value_offset(std::mem::align_of::<T>()))
                .cast()
        }
    }
}
unsafe impl<T: GcSafe> GcVecRepr for SimpleVecRepr<T> {
    /// We do support reallocation, but only for large sized vectors
    const SUPPORTS_REALLOC: bool = true;

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

    fn realloc_in_place(&self, new_capacity: usize) -> Result<(), ReallocFailedError> {
        if !fits_small_object(Self::layout(new_capacity)) {
            // TODO: Use allocator api for realloc
            todo!("Big object realloc")
        } else {
            Err(ReallocFailedError::SizeUnsupported)
        }
    }

    unsafe fn ptr(&self) -> *const c_void {
        todo!()
    }
}
unsafe_gc_impl!(
    target => SimpleVecRepr<T>,
    params => [T: GcSafe],
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
    /// An array, whose size can vary at runtime
    Array {
        /// The fixed layout of elements in the array
        ///
        /// The overall alignment of the array is equal to the alignment of each element,
        /// however the size may vary at runtime.
        element_layout: Layout
    },
    /// A vector, whose capacity can vary from instance to instance
    Vec {
        /// The fixed layout of elements in the vector.
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
            },
            GcTypeLayout::Vec { element_layout } => {
                let header = (header as *mut u8)
                    .sub(field_offset!(GcVecHeader, common_header))
                    .cast::<GcVecHeader>();
                element_layout.repeat((*header).capacity).unwrap().0
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
            },
            GcTypeLayout::Vec { .. } => {
                res += field_offset!(GcVecHeader, common_header);
            }
        }
        res
    }
    pub(crate) unsafe fn determine_total_layout(&self, header: *mut GcHeader) -> Layout {
        Layout::from_size_align_unchecked(
            self.determine_total_size(header),
            match self.layout {
                GcTypeLayout::Fixed(inner) => inner.align(),
                GcTypeLayout::Array { element_layout } |
                GcTypeLayout::Vec { element_layout } => element_layout.align()
            }
        )
    }
}

pub(crate) trait StaticVecType {
    const STATIC_VEC_TYPE: &'static GcType;
}
impl<T: GcSafe> StaticVecType for T {
    const STATIC_VEC_TYPE: &'static GcType = &GcType {
        layout: GcTypeLayout::Vec {
            element_layout: Layout::new::<T>(),
        },
        // We have same alignment as our members
        value_offset: GcVecHeader::vec_value_offset(std::mem::align_of::<T>()),
        trace_func: if T::NEEDS_TRACE {
            Some({
                unsafe fn visit<T: Trace>(val: *mut c_void, visitor: &mut MarkVisitor) {
                    let len = (*(val as *mut u8)
                        .sub(GcVecHeader::vec_value_offset(std::mem::align_of::<T>()))
                        .cast::<GcVecHeader>()).len.get();
                    let slice = std::slice::from_raw_parts_mut(
                        val as *mut T,
                        len
                    );
                    let Ok(()) = <[T] as Trace>::visit(slice, visitor);
                }
                visit::<T> as unsafe fn(*mut c_void, &mut MarkVisitor)
            })
        } else {
            None
        },
        drop_func: if T::NEEDS_DROP {
            Some({
                unsafe fn drop_gc_vec<T: GcSafe>(val: *mut c_void) {
                    let len = (*(val as *mut u8)
                        .sub(GcVecHeader::vec_value_offset(std::mem::align_of::<T>()))
                        .cast::<GcArrayHeader>()).len;
                    std::ptr::drop_in_place::<[T]>(std::ptr::slice_from_raw_parts_mut(
                        val as *mut T,
                        len
                    ));
                }
                drop_gc_vec::<T> as unsafe fn(*mut c_void)
            })
        } else {
            None
        }
    };
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
