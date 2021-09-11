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

use zerogc_context::field_offset;
use zerogc_derive::{NullTrace, unsafe_gc_impl};

use crate::{RawMarkState, CollectorId, DynTrace, MarkVisitor};
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

/// Marker type for an unknown header
pub(crate) struct UnknownHeader(());

/// The layout of an object's header
#[derive(Debug)]
pub struct HeaderLayout<H> {
    /// The overall size of the header
    pub header_size: usize,
    /// The offset of the 'common' header,
    /// starting from the start of the real header
    pub common_header_offset: usize,
    marker: PhantomData<*mut H>
}
impl<H> Copy for HeaderLayout<H> {}
impl<H> Clone for HeaderLayout<H> {
    #[inline]
    fn clone(&self) -> Self {
        *self
    }
}

impl<H> HeaderLayout<H> {
    #[inline]
    pub(crate) const fn value_offset_from_common_header(&self, align: usize) -> usize {
        self.value_offset(align) - self.common_header_offset
    }
    #[inline]
    pub(crate) const fn into_unknown(self) -> HeaderLayout<UnknownHeader> {
        HeaderLayout {
            header_size: self.header_size,
            common_header_offset: self.common_header_offset,
            marker: PhantomData
        }
    }
    /// The alignment of the header
    ///
    /// NOTE: All headers have the same alignment
    pub const ALIGN: usize = std::mem::align_of::<GcHeader>();
    #[inline]
    pub(crate) const unsafe fn common_header(self, ptr: *mut H) -> *mut GcHeader {
        (ptr as *mut u8).add(self.common_header_offset).cast()
    }
    #[inline]
    #[allow(clippy::wrong_self_convention)]
    pub(crate) const unsafe fn from_common_header(self, ptr: *mut GcHeader) -> *mut H {
        (ptr as *mut u8).sub(self.common_header_offset).cast()
    }
    /// Get the header from the specified value pointer
    ///
    /// ## Safety
    /// Undefined behavior if the pointer doesn't point to a an object
    /// allocated in this collector (ie. doesn't have the appropriate header).
    #[inline]
    pub const unsafe fn from_value_ptr<T: ?Sized>(self, ptr: *mut T) -> *mut H {
        let align = std::mem::align_of_val(&*ptr);
        (ptr as *mut u8).sub(self.value_offset(align)).cast()
    }
    /// Get the in-memory layout of the header (doesn't include the value)
    #[inline]
    pub const fn layout(&self) -> Layout {
        unsafe {
            Layout::from_size_align_unchecked(self.header_size, Self::ALIGN)
        }
    }
    /// Get the offset of the value from the start of the header,
    /// given the alignment of its value
    #[inline]
    pub const fn value_offset(&self, align: usize) -> usize {
        let padding = self.layout().padding_needed_for(align);
        self.header_size + padding
    }
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
    trace_template => |self, visitor| {
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
                    .add(type_info.value_offset_from_common_header)
                    .cast());
            }
            let layout = type_info.determine_total_layout(self.header.as_ptr());
            let actual_header = type_info.header_layout()
                .from_common_header(self.header.as_ptr());
            std::alloc::dealloc(actual_header.cast(), layout);
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
    pub(crate) const LAYOUT: HeaderLayout<Self> = HeaderLayout {
        header_size: std::mem::size_of::<Self>(),
        common_header_offset: field_offset!(GcArrayHeader, common_header),
        marker: PhantomData
    };
}
/// A header for a Gc vector
#[repr(C)]
pub struct GcVecHeader {
    pub(crate) capacity: usize,
    pub(crate) len: Cell<usize>,
    pub(crate) common_header: GcHeader
}
impl GcVecHeader {
    pub(crate) const LAYOUT: HeaderLayout<Self> = HeaderLayout {
        common_header_offset: field_offset!(GcVecHeader, common_header),
        header_size: std::mem::size_of::<GcVecHeader>(),
        marker: PhantomData
    };
}

/// The raw representation of a vector in the simple collector
///
/// NOTE: Length and capacity are stored implicitly in the [GcVecHeader]
#[repr(C)]
pub struct SimpleVecRepr<'gc, T: GcSafe<'gc, crate::CollectorId> + Sized> {
    marker: PhantomData<(*const [T], &'gc crate::CollectorId)>,
}
impl<'gc, T: GcSafe<'gc, crate::CollectorId>> SimpleVecRepr<'gc, T> {
    #[inline]
    fn header(&self) -> *mut GcVecHeader {
        unsafe {
            /*
             * TODO: what if we have a non-standard alignment?\
             * Our `T` is erased at runtime....
             * this is a bug in the epsilon collector too
             */
            (self as *const Self as *mut Self as *mut u8)
                .sub(GcVecHeader::LAYOUT.value_offset(std::mem::align_of::<T>()))
                .cast()
        }
    }
}
unsafe impl<'gc, T: GcSafe<'gc, crate::CollectorId>> GcVecRepr<'gc> for SimpleVecRepr<'gc, T> {
    /// Right now, there is no stable API for in-place re-allocation
    const SUPPORTS_REALLOC: bool = false;
    type Id = crate::CollectorId;

    #[inline]
    fn element_layout(&self) -> Layout {
        unsafe {
            match (*self.header()).common_header.type_info.layout {
                GcTypeLayout::Vec {
                    element_layout, ..
                } => element_layout,
                _ => std::hint::unreachable_unchecked()
            }
        }
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
}
unsafe_gc_impl!(
    target => SimpleVecRepr<'gc, T>,
    params => ['gc, T: GcSafe<'gc, crate::CollectorId>],
    bounds => {
        GcRebrand => { where T: zerogc::GcRebrand<'new_gc, crate::CollectorId>,
            T::Branded: Sized + zerogc::GcSafe<'new_gc, crate::CollectorId> },
    },
    NEEDS_TRACE => T::NEEDS_TRACE,
    NEEDS_DROP => T::NEEDS_DROP,
    null_trace => { where T: ::zerogc::NullTrace },
    trace_mut => |self, visitor| {
        // Trace our innards
        unsafe {
            let start: *mut T = self.ptr() as *const T as *mut T;
            for i in 0..self.len() {
                visitor.trace(&mut *start.add(i))?;
            }
        }
        Ok(())
    },
    trace_immutable => |self, visitor| {
        // Trace our innards
        unsafe {
            let start: *mut T = self.ptr() as *const T as *mut T;
            for i in 0..self.len() {
                visitor.trace_immutable(&*start.add(i))?;
            }
        }
        Ok(())
    },
    branded_type => SimpleVecRepr<'new_gc, T::Branded>,
    collector_id => CollectorId
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
    /// The layout of the header
    pub const LAYOUT: HeaderLayout<Self> = HeaderLayout {
        header_size: std::mem::size_of::<Self>(),
        common_header_offset: 0,
        marker: PhantomData
    };
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
                .add(self.type_info.value_offset_from_common_header)
                .cast::<c_void>()
        }
    }
    /// Get the [GcHeader] for the specified value, assuming that its been allocated by this collector.
    ///
    /// ## Safety
    /// Assumes the value was allocated in the simple collector.
    #[inline]
    pub unsafe fn from_value_ptr<T: ?Sized>(ptr: *mut T) -> *mut GcHeader {
        GcHeader::LAYOUT.from_value_ptr(ptr)
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
    pub value_offset_from_common_header: usize,
    /// The function to trace the type, or `None` if it doesn't need to be traced
    pub trace_func: Option<unsafe fn(*mut c_void, &mut MarkVisitor)>,
    /// The function to drop the type, or `None` if it doesn't need to be dropped
    pub drop_func: Option<unsafe fn(*mut c_void)>,
}
impl GcType {
    #[inline]
    fn align(&self) -> usize {
        match self.layout {
            GcTypeLayout::Fixed(fixed) => fixed.align(),
            GcTypeLayout::Array { element_layout } |
            GcTypeLayout::Vec { element_layout } => element_layout.align()
        }
    }
    pub(crate) fn header_layout(&self) -> HeaderLayout<UnknownHeader> {
        match self.layout {
            GcTypeLayout::Fixed(_) => GcHeader::LAYOUT.into_unknown(),
            GcTypeLayout::Array { .. } => GcArrayHeader::LAYOUT.into_unknown(),
            GcTypeLayout::Vec { .. } => GcVecHeader::LAYOUT.into_unknown()
        }
    }
    #[inline]
    unsafe fn determine_size(&self, header: *mut GcHeader) -> usize {
        match self.layout {
            GcTypeLayout::Fixed(layout) => layout.size(),
            GcTypeLayout::Array { element_layout } => {
                let header = GcArrayHeader::LAYOUT.from_common_header(header);
                element_layout.repeat((*header).len).unwrap().0.size()
            },
            GcTypeLayout::Vec { element_layout } => {
                let header = GcVecHeader::LAYOUT.from_common_header(header);
                element_layout.repeat((*header).capacity).unwrap().0.size()
            }
        }
    }
    #[inline]
    pub(crate) unsafe fn determine_total_size(&self, header: *mut GcHeader) -> usize {
        self.determine_total_layout(header).size()
    }
    #[inline]
    pub(crate) unsafe fn determine_total_layout(&self, header: *mut GcHeader) -> Layout {
        self.header_layout().layout()
            .extend(Layout::from_size_align_unchecked(
                self.determine_size(header),
                self.align()
            )).unwrap().0.pad_to_align()
    }
    /// Get the [GcType] for the specified `Sized` type
    #[inline]
    pub const fn for_regular<'gc, T: GcSafe<'gc, crate::CollectorId>>() -> &'static Self {
        <T as StaticGcType>::STATIC_TYPE
    }
}

pub(crate) trait StaticVecType {
    const STATIC_VEC_TYPE: &'static GcType;
}
impl<'gc, T: GcSafe<'gc, crate::CollectorId>> StaticVecType for T {
    const STATIC_VEC_TYPE: &'static GcType = &GcType {
        layout: GcTypeLayout::Vec {
            element_layout: Layout::new::<T>(),
        },
        value_offset_from_common_header: {
            // We have same alignment as our members
            let align = std::mem::align_of::<T>();
            GcArrayHeader::LAYOUT.value_offset_from_common_header(align)
        },
        trace_func: if <T as Trace>::NEEDS_TRACE {
            Some({
                unsafe fn visit<T: Trace>(val: *mut c_void, visitor: &mut MarkVisitor) {
                    let len = (*GcVecHeader::LAYOUT.from_value_ptr(val as *mut T)).len.get();
                    let slice = std::slice::from_raw_parts_mut(
                        val as *mut T,
                        len
                    );
                    let Ok(()) = <[T] as Trace>::trace(slice, visitor);
                }
                visit::<T> as unsafe fn(*mut c_void, &mut MarkVisitor)
            })
        } else {
            None
        },
        drop_func: if T::NEEDS_DROP {
            Some({
                unsafe fn drop_gc_vec<'gc, T: GcSafe<'gc, crate::CollectorId>>(val: *mut c_void) {
                    let len = (*GcVecHeader::LAYOUT.from_value_ptr(val as *mut T)).len.get();
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
    const STATIC_TYPE: &'static GcType;
}
impl<'gc, T: GcSafe<'gc, crate::CollectorId>> StaticGcType for [T] {
    const STATIC_TYPE: &'static GcType = &GcType {
        layout: GcTypeLayout::Array { element_layout: Layout::new::<T>() },
        value_offset_from_common_header: {
            GcArrayHeader::LAYOUT.value_offset_from_common_header(std::mem::align_of::<T>())
        },
        trace_func: if <T as Trace>::NEEDS_TRACE {
            Some({
                unsafe fn visit<T: Trace>(val: *mut c_void, visitor: &mut MarkVisitor) {
                    let header = GcArrayHeader::LAYOUT.from_value_ptr(val as *mut T);
                    let len = (*header).len;
                    let slice = std::slice::from_raw_parts_mut(
                        val as *mut T,
                        len
                    );
                    let Ok(()) = <[T] as Trace>::trace(slice, visitor);
                }
                visit::<T> as unsafe fn(*mut c_void, &mut MarkVisitor)
            })
        } else { None },
        drop_func: if <T as Trace>::NEEDS_DROP {
            Some({
                unsafe fn drop_gc_slice<'gc, T: GcSafe<'gc, crate::CollectorId>>(val: *mut c_void) {
                    let len = (*GcArrayHeader::LAYOUT.from_value_ptr(val as *mut T)).len;
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
impl<'gc, T: GcSafe<'gc, crate::CollectorId>> StaticGcType for T {
    const STATIC_TYPE: &'static GcType = &GcType {
        layout: GcTypeLayout::Fixed(Layout::new::<T>()),
        value_offset_from_common_header: {
            GcHeader::LAYOUT.value_offset_from_common_header(std::mem::align_of::<T>())
        },
        trace_func: if <T as Trace>::NEEDS_TRACE {
            Some(unsafe { mem::transmute::<_, unsafe fn(*mut c_void, &mut MarkVisitor)>(
                <T as DynTrace>::trace as fn(&mut T, &mut MarkVisitor),
            ) })
        } else { None },
        drop_func: if <T as Trace>::NEEDS_DROP {
            unsafe { Some(mem::transmute::<_, unsafe fn(*mut c_void)>(
                std::ptr::drop_in_place::<T> as unsafe fn(*mut T)
            )) }
        } else { None }
    };
}
