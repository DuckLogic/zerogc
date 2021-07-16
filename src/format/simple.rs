//! The 'simple' object format
//!
//! This is the simplest possible implementation that supports
//! all the collector's features.

use core::marker::PhantomData;
use core::alloc::Layout;
use core::ffi::c_void;

use crate::{CollectorId, GcSafe, Trace, GcVisitor};
use crate::format::{ObjectFormat, GcTypeLayout, GcArrayHeader as GcArrayHeaderAPI, GcHeader as GcHeaderAPI, GcVecHeader as GcVecHeaderAPI, GcTypeInfo};
use core::mem;
use std::cell::Cell;
use std::mem::MaybeUninit;
use crate::vec::repr::GcVecRepr;

/// The simplest implementation of [ObjectFormat]
pub struct SimpleObjectFormat {
    _priv: ()
}
impl Default for SimpleObjectFormat {
    fn default() -> Self {
        SimpleObjectFormat { _priv: () }
    }
}
impl<Id: CollectorId> ObjectFormat<Id> for SimpleObjectFormat {
    type Header = GcHeader<Id>;
    type TypeInfoRef = &'static GcSimpleType<Id>;
    const REGULAR_HEADER_LAYOUT: Layout = Layout::new::<Self::Header>();
    const VEC_HEADER_LAYOUT: Layout = Layout::new::<Self::VecHeader>();
    const ARRAY_HEADER_LAYOUT: Layout = Layout::new::<Self::ArrayHeader>();

    #[inline]
    unsafe fn resolve_header<T: ?Sized + GcSafe>(ptr: &T) -> &Self::Header {
        &*GcHeader::from_value_ptr(ptr as *const T as *mut T)
    }

    type VecHeader = GcVecHeader<Id>;

    #[inline]
    unsafe fn resolve_vec_header<T: GcSafe>(repr: &Id::RawVecRepr) -> &Self::VecHeader {
        &*GcVecHeader::LAYOUT.from_value_ptr(repr as *const _ as *const T as *mut T)
    }

    type ArrayHeader = GcArrayHeader<Id>;

    #[inline]
    unsafe fn resolve_array_header<T: GcSafe>(ptr: &[T]) -> &Self::ArrayHeader {
        &*GcArrayHeader::LAYOUT.from_value_ptr(ptr.as_ptr() as *mut T)
    }

    #[inline]
    fn regular_type_info<T: GcSafe>() -> Self::TypeInfoRef {
        <T as StaticGcType<Id>>::STATIC_TYPE
    }

    #[inline]
    fn array_type_info<T: GcSafe>() -> Self::TypeInfoRef {
        <[T] as StaticGcType<Id>>::STATIC_TYPE
    }

    #[inline]
    fn vec_type_info<T: GcSafe>() -> Self::TypeInfoRef {
        <T as StaticVecType<Id>>::STATIC_VEC_TYPE
    }

    #[inline]
    unsafe fn init_vec_header(header: *mut Self::VecHeader, mark_data: Id::MarkData, type_info: Self::TypeInfoRef, capacity: usize, initial_len: usize) {
        Self::init_regular_header(&mut (*header).common_header, mark_data, type_info);
        (*header).capacity = capacity;
        (*header).len.set(initial_len);
    }

    #[inline]
    unsafe fn init_regular_header(header: *mut Self::Header, mark_data: Id::MarkData, type_info: Self::TypeInfoRef) {
        (*header).mark_data = mark_data;
        (*header).type_info = type_info;
    }

    #[inline]
    unsafe fn init_array_header(header: *mut Self::ArrayHeader, mark_data: Id::MarkData, type_info: Self::TypeInfoRef, len: usize) {
        Self::init_regular_header(&mut (*header).common_header, mark_data, type_info);
        (*header).len = len;
    }
}

/// Marker type for an unknown header
pub(crate) struct UnknownHeader(());

/// The layout of an object's header
#[derive(Debug)]
pub(crate) struct HeaderLayout<H, Id: CollectorId> {
    /// The overall size of the header
    pub header_size: usize,
    /// The offset of the 'common' header,
    /// starting from the start of the real header
    pub common_header_offset: usize,
    marker: PhantomData<*mut H>,
    id_marker: PhantomData<Id>
}
impl<H, Id: CollectorId> Copy for HeaderLayout<H, Id> {}
impl<H, Id: CollectorId> Clone for HeaderLayout<H, Id> {
    #[inline]
    fn clone(&self) -> Self {
        *self
    }
}

impl<H, Id: CollectorId> HeaderLayout<H, Id> {
    #[inline]
    pub(crate) const fn value_offset_from_common_header(&self, align: usize) -> usize {
        self.value_offset(align) - self.common_header_offset
    }
    #[inline]
    pub(crate) const fn into_unknown(self) -> HeaderLayout<UnknownHeader, Id> {
        HeaderLayout {
            header_size: self.header_size,
            common_header_offset: self.common_header_offset,
            marker: PhantomData,
            id_marker: PhantomData
        }
    }
    /// The alignment of the header
    ///
    /// NOTE: All headers have the same alignment
    pub const ALIGN: usize = std::mem::align_of::<GcHeader<Id>>();
    #[inline]
    pub(crate) const unsafe fn common_header(self, ptr: *mut H) -> *mut GcHeader<Id> {
        (ptr as *mut u8).add(self.common_header_offset).cast()
    }
    #[inline]
    pub(crate) const unsafe fn from_common_header(self, ptr: *mut GcHeader<Id>) -> *mut H {
        (ptr as *mut u8).sub(self.common_header_offset).cast()
    }
    /// Get the header from the specified value pointer
    #[inline]
    pub const unsafe fn from_value_ptr<T: ?Sized>(self, ptr: *mut T) -> *mut H {
        let align = core::mem::align_of_val(&*ptr);
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
/// A header for a GC array object
#[repr(C)]
pub struct GcArrayHeader<Id: CollectorId> {
    pub(crate) len: usize,
    pub(crate) common_header: GcHeader<Id>
}
impl<Id: CollectorId> GcArrayHeader<Id> {
    const LAYOUT: HeaderLayout<Self, Id> = HeaderLayout {
        header_size: core::mem::size_of::<Self>(),
        common_header_offset: unsafe {
            let raw = MaybeUninit::<Self>::uninit();
            core::ptr::addr_of!((*raw.as_ptr()).common_header) as usize
                - raw.as_ptr() as usize
        },
        marker: PhantomData,
        id_marker: PhantomData
    };
}
unsafe impl<Id: CollectorId> GcArrayHeaderAPI for GcArrayHeader<Id> {
    #[inline]
    fn len(&self) -> usize {
        self.len
    }

    #[inline]
    fn element_layout(&self) -> Layout {
        match self.common_header.type_info.layout {
            GcTypeLayout::Array { element_layout } => element_layout,
            _ => {
                #[cfg(debug_assertions)] {
                    unreachable!()
                }
                #[cfg(not(debug_assertions))] {
                    unsafe { core::hint::unreachable_unchecked() }
                }
            }
        }
    }
}
unsafe impl<Id: CollectorId> GcHeaderAPI for GcArrayHeader<Id> {
    type TypeInfoRef = &'static GcSimpleType<Id>;
    type MarkData = Id::MarkData;
    type Id = Id;

    #[inline]
    fn mark_data(&self) -> &'_ Self::MarkData {
        &self.common_header.mark_data
    }

    #[inline]
    fn type_info(&self) -> Self::TypeInfoRef {
        self.common_header.type_info
    }

    #[inline]
    fn value_size(&self) -> usize {
        self.len * self.element_layout().size()
    }

    #[inline]
    fn total_size(&self) -> usize {
        unsafe {
            self.common_header.type_info.determine_total_size(core::ptr::addr_of!(self.common_header) as *mut _)
        }
    }

    unsafe fn value_ptr(&self) -> *mut c_void {
        self.common_header.value()
    }

    unsafe fn dynamic_drop(&self) {
        self.common_header.dynamic_drop()
    }

    unsafe fn dynamic_trace(&self, visitor: &mut <Self::Id as CollectorId>::PreferredVisitor) -> Result<(), <<Self::Id as CollectorId>::PreferredVisitor as GcVisitor>::Err> {
        self.common_header.dynamic_trace(visitor)
    }
}
/// A header for a Gc vector
#[repr(C)]
pub struct GcVecHeader<Id: CollectorId> {
    pub(crate) capacity: usize,
    pub(crate) len: Cell<usize>,
    pub(crate) common_header: GcHeader<Id>
}
impl<Id: CollectorId> GcVecHeader<Id> {
    pub(crate) const LAYOUT: HeaderLayout<Self, Id> = HeaderLayout {
        common_header_offset: unsafe {
            let raw = MaybeUninit::<Self>::uninit();
            core::ptr::addr_of!((*raw.as_ptr()).common_header) as usize
                - raw.as_ptr() as usize
        },
        header_size: core::mem::size_of::<Self>(),
        marker: PhantomData,
        id_marker: PhantomData
    };
}
unsafe impl<Id: CollectorId> GcVecHeaderAPI for GcVecHeader<Id> {
    #[inline]
    fn len(&self) -> usize {
        self.len.get()
    }

    #[inline]
    unsafe fn set_len(&self, new_len: usize) {
        self.len.set(new_len)
    }

    #[inline]
    fn capacity(&self) -> usize {
        self.capacity
    }

    #[inline]
    fn element_layout(&self) -> Layout {
        match self.common_header.type_info.layout {
            GcTypeLayout::Array { element_layout } => element_layout,
            _ => {
                #[cfg(debug_assertions)] {
                    unreachable!()
                }
                #[cfg(not(debug_assertions))] {
                    unsafe { core::hint::unreachable_unchecked() }
                }
            }
        }
    }
}
unsafe impl<Id: CollectorId> GcHeaderAPI for GcVecHeader<Id> {
    type TypeInfoRef = &'static GcSimpleType<Id>;
    type MarkData = Id::MarkData;
    type Id = Id;

    #[inline]
    fn mark_data(&self) -> &'_ Self::MarkData {
        &self.common_header.mark_data
    }

    #[inline]
    fn type_info(&self) -> Self::TypeInfoRef {
        self.common_header.type_info
    }

    #[inline]
    fn value_size(&self) -> usize {
        self.capacity * self.element_layout().size()
    }

    fn total_size(&self) -> usize {
        unsafe {
            self.common_header.type_info.determine_total_size(core::ptr::addr_of!(self.common_header) as *mut _)
        }
    }

    #[inline]
    unsafe fn value_ptr(&self) -> *mut c_void {
        self.common_header.value()
    }

    unsafe fn dynamic_drop(&self) {
        self.common_header.dynamic_drop()
    }

    unsafe fn dynamic_trace(&self, visitor: &mut <Self::Id as CollectorId>::PreferredVisitor) -> Result<(), <<Self::Id as CollectorId>::PreferredVisitor as GcVisitor>::Err> {
        self.common_header.dynamic_trace(visitor)
    }
}

/// A header for a GC object
///
/// This is shared between both small arenas
/// and fallback alloc vis `BigGcObject`
#[repr(C)]
pub struct GcHeader<Id: CollectorId> {
    /// The object's type information
    pub type_info: &'static GcSimpleType<Id>,
    /// The mark data
    pub mark_data: Id::MarkData
}
impl<Id: CollectorId> GcHeader<Id> {
    /// The layout of the header
    const LAYOUT: HeaderLayout<Self, Id> = HeaderLayout {
        header_size: core::mem::size_of::<Self>(),
        common_header_offset: 0,
        marker: PhantomData,
        id_marker: PhantomData
    };
    /// Create a new header
    #[inline]
    pub fn new(type_info: &'static GcSimpleType<Id>, mark_data: Id::MarkData) -> Self {
        GcHeader { type_info, mark_data }
    }
    /// A pointer to the header's value
    #[inline]
    pub fn value(&self) -> *mut c_void {
        unsafe {
            (self as *const Self as *mut Self as *mut u8)
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
    pub unsafe fn from_value_ptr<T: ?Sized>(ptr: *mut T) -> *mut GcHeader<Id> {
        GcHeader::LAYOUT.from_value_ptr(ptr)
    }
}
unsafe impl<Id: CollectorId> GcHeaderAPI for GcHeader<Id> {
    type TypeInfoRef = &'static GcSimpleType<Id>;
    type MarkData = Id::MarkData;
    type Id = Id;

    #[inline]
    fn mark_data(&self) -> &'_ Self::MarkData {
        &self.mark_data
    }

    #[inline]
    fn type_info(&self) -> Self::TypeInfoRef {
        self.type_info
    }

    #[inline]
    fn value_size(&self) -> usize {
        unsafe { self.type_info.determine_size(core::ptr::addr_of!(*self) as *mut _) }
    }

    #[inline]
    fn total_size(&self) -> usize {
        unsafe { self.type_info.determine_total_size(core::ptr::addr_of!(*self) as *mut _) }
    }

    #[inline]
    unsafe fn value_ptr(&self) -> *mut c_void {
        (*self).value()
    }

    unsafe fn dynamic_drop(&self) {
        if let Some(func) = self.type_info.drop_func {
            func(self.value())
        }
    }

    #[inline]
    unsafe fn dynamic_trace(&self, visitor: &mut <Self::Id as CollectorId>::PreferredVisitor) -> Result<(), <<Self::Id as CollectorId>::PreferredVisitor as GcVisitor>::Err> {
        if let Some(func) = self.type_info.trace_func {
            func(self.value(), visitor)
        } else {
            Ok(())
        }
    }
}

/// A type used by GC
#[repr(C)]
pub struct GcSimpleType<Id: CollectorId> {
    /// Information on the type's layout
    pub layout: GcTypeLayout,
    /// The offset of the value from the start of the header
    ///
    /// This varies depending on the type's alignment
    pub value_offset_from_common_header: usize,
    /// The function to trace the type, or `None` if it doesn't need to be traced
    pub trace_func: Option<unsafe fn(*mut c_void, &mut Id::PreferredVisitor)
        -> Result<(), <Id::PreferredVisitor as GcVisitor>::Err>>,
    /// The function to drop the type, or `None` if it doesn't need to be dropped
    pub drop_func: Option<unsafe fn(*mut c_void)>,
}
impl<Id: CollectorId> GcSimpleType<Id> {
    #[inline]
    fn align(&self) -> usize {
        match self.layout {
            GcTypeLayout::Fixed(fixed) => fixed.align(),
            GcTypeLayout::Array { element_layout } |
            GcTypeLayout::Vec { element_layout } => element_layout.align()
        }
    }
    pub(crate) fn header_layout(&self) -> HeaderLayout<UnknownHeader, Id> {
        match self.layout {
            GcTypeLayout::Fixed(_) => GcHeader::LAYOUT.into_unknown(),
            GcTypeLayout::Array { .. } => GcArrayHeader::LAYOUT.into_unknown(),
            GcTypeLayout::Vec { .. } => GcVecHeader::LAYOUT.into_unknown()
        }
    }
    #[inline]
    unsafe fn determine_size(&self, header: *mut GcHeader<Id>) -> usize {
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
    pub(crate) unsafe fn determine_total_size(&self, header: *mut GcHeader<Id>) -> usize {
        self.determine_total_layout(header).size()
    }
    #[inline]
    pub(crate) unsafe fn determine_total_layout(&self, header: *mut GcHeader<Id>) -> Layout {
        self.header_layout().layout()
            .extend(Layout::from_size_align_unchecked(
                self.determine_size(header),
                self.align()
            )).unwrap().0.pad_to_align()
    }
}
unsafe impl<Id: CollectorId> GcTypeInfo for &'static GcSimpleType<Id> {
    type Header = GcHeader<Id>;

    #[inline]
    fn align(&self) -> usize {
        (*self).align()
    }

    #[inline]
    fn needs_trace(&self) -> bool {
        self.trace_func.is_some()
    }

    #[inline]
    fn needs_drop(&self) -> bool {
        self.drop_func.is_some()
    }

    #[inline]
    unsafe fn resolve_header<T: GcSafe + ?Sized>(&self, value_ptr: *mut T) -> *mut Self::Header {
        GcHeader::from_value_ptr(value_ptr)
    }
}

pub(crate) trait StaticVecType<Id: CollectorId> {
    const STATIC_VEC_TYPE: &'static GcSimpleType<Id>;
}
impl<T: GcSafe, Id: CollectorId> StaticVecType<Id> for T {
    const STATIC_VEC_TYPE: &'static GcSimpleType<Id> = &GcSimpleType {
        layout: GcTypeLayout::Vec {
            element_layout: Layout::new::<T>(),
        },
        value_offset_from_common_header: {
            // We have same alignment as our members
            let align = std::mem::align_of::<T>();
            GcArrayHeader::<Id>::LAYOUT.value_offset_from_common_header(align)
        },
        trace_func: if <T as Trace>::NEEDS_TRACE {
            Some({
                unsafe fn visit<T: Trace, V: GcVisitor, Id: CollectorId>(val: *mut c_void, visitor: &mut V) -> Result<(), V::Err> {
                    let len = (*GcVecHeader::<Id>::LAYOUT.from_value_ptr(val as *mut T)).len.get();
                    let slice = std::slice::from_raw_parts_mut(
                        val as *mut T,
                        len
                    );
                    <[T] as Trace>::visit(slice, visitor)
                }
                visit::<T, Id::PreferredVisitor, Id> as unsafe fn(*mut c_void, &mut Id::PreferredVisitor)
                    -> Result<(), <Id::PreferredVisitor as GcVisitor>::Err>
            })
        } else {
            None
        },
        drop_func: if T::NEEDS_DROP {
            Some({
                unsafe fn drop_gc_vec<T: GcSafe, Id: CollectorId>(val: *mut c_void) {
                    (&mut *((*GcVecHeader::<Id>::LAYOUT.from_value_ptr(val as *mut T))
                            .common_header.value() as *mut Id::RawVecRepr)).unchecked_drop::<T>()
                }
                drop_gc_vec::<T, Id> as unsafe fn(*mut c_void)
            })
        } else {
            None
        }
    };
}
trait StaticGcType<Id: CollectorId> {
    const STATIC_TYPE: &'static GcSimpleType<Id>;
}
impl<T: GcSafe, Id: CollectorId> StaticGcType<Id> for [T] {
    const STATIC_TYPE: &'static GcSimpleType<Id> = &GcSimpleType {
        layout: GcTypeLayout::Array { element_layout: Layout::new::<T>() },
        value_offset_from_common_header: {
            GcArrayHeader::<Id>::LAYOUT.value_offset_from_common_header(std::mem::align_of::<T>())
        },
        trace_func: if <T as Trace>::NEEDS_TRACE {
            Some({
                unsafe fn visit<T: Trace, V: GcVisitor, Id: CollectorId>(val: *mut c_void, visitor: &mut V) -> Result<(), V::Err> {
                    let header = GcArrayHeader::<Id>::LAYOUT.from_value_ptr(val as *mut T);
                    let len = (*header).len;
                    let slice = std::slice::from_raw_parts_mut(
                        val as *mut T,
                        len
                    );
                    <[T] as Trace>::visit(slice, visitor)
                }
                visit::<T, Id::PreferredVisitor, Id> as unsafe fn(*mut c_void, &mut Id::PreferredVisitor) -> Result<(), _>
            })
        } else { None },
        drop_func: if <T as Trace>::NEEDS_DROP {
            Some({
                unsafe fn drop_gc_slice<T: GcSafe, Id: CollectorId>(val: *mut c_void) {
                    let len = (*GcArrayHeader::<Id>::LAYOUT.from_value_ptr(val as *mut T)).len;
                    std::ptr::drop_in_place::<[T]>(std::ptr::slice_from_raw_parts_mut(
                        val as *mut T,
                        len
                    ));
                }
                drop_gc_slice::<T, Id> as unsafe fn(*mut c_void)
            })
        } else { None }
    };
}
impl<T: GcSafe, Id: CollectorId> StaticGcType<Id> for T {
    const STATIC_TYPE: &'static GcSimpleType<Id> = &GcSimpleType {
        layout: GcTypeLayout::Fixed(Layout::new::<T>()),
        value_offset_from_common_header: {
            GcHeader::<Id>::LAYOUT.value_offset_from_common_header(std::mem::align_of::<T>())
        },
        trace_func: if <T as Trace>::NEEDS_TRACE {
            Some(unsafe { mem::transmute::<_, unsafe fn(*mut c_void, &mut Id::PreferredVisitor)
                -> Result<(), <Id::PreferredVisitor as GcVisitor>::Err>>(
                <T as Trace>::visit::<Id::PreferredVisitor> as fn(&mut T, &mut Id::PreferredVisitor)
                    -> Result<(), <Id::PreferredVisitor as GcVisitor>::Err>,
            ) })
        } else { None },
        drop_func: if <T as Trace>::NEEDS_DROP {
            unsafe { Some(mem::transmute::<_, unsafe fn(*mut c_void)>(
                std::ptr::drop_in_place::<T> as unsafe fn(*mut T)
            )) }
        } else { None }
    };
}
