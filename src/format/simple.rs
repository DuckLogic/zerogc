//! The 'simple' object format
//!
//! This is the simplest possible implementation that supports
//! all the collector's features.

use core::marker::PhantomData;
use core::alloc::Layout;
use core::ffi::c_void;
use core::cell::Cell;
use core::mem::{self,};

use zerogc_derive::unsafe_gc_impl;

use crate::{GcSafe, Trace, GcVisitor,};
use crate::format::{ObjectFormat, GcTypeLayout, GcArrayHeader as GcArrayHeaderAPI, GcHeader as GcHeaderAPI, GcVecHeader as GcVecHeaderAPI, GcTypeInfo, GcInfo, GcCommonHeader, HeaderLayout as HeaderLayoutAPI, UnknownHeader,};
use crate::vec::repr::GcVecRepr;

/// The simplest implementation of [ObjectFormat]
pub struct SimpleObjectFormat<GC: GcInfo> {
    marker: PhantomData<GC>
}
impl<GC: GcInfo> Default for SimpleObjectFormat<GC> {
    fn default() -> Self {
        SimpleObjectFormat { marker: PhantomData }
    }
}
impl<GC: GcInfo> ObjectFormat for SimpleObjectFormat<GC> {
    type Collector = GC;
    type RawVecRepr = SimpleVecRepr<DynamicObj, GC>;
    type CommonHeader = GcHeader<GC>;
    type TypeInfoRef = &'static GcSimpleType<GC>;

    #[inline]
    unsafe fn resolve_header<T: ?Sized + GcSafe>(ptr: &T) -> &Self::CommonHeader {
        &*GcHeader::LAYOUT.from_value_ptr(ptr as *const T as *mut T)
    }

    type VecHeader = GcVecHeader<GC>;

    #[inline]
    unsafe fn resolve_vec_header<T: GcSafe>(repr: &Self::RawVecRepr) -> &Self::VecHeader {
        &*GcVecHeader::LAYOUT.from_value_ptr(repr.ptr() as *const _ as *const T as *mut T)
    }

    type ArrayHeader = GcArrayHeader<GC>;

    #[inline]
    unsafe fn resolve_array_header<T: GcSafe>(ptr: *mut T) -> *mut Self::ArrayHeader {
        GcArrayHeader::LAYOUT.from_value_ptr(ptr)
    }

    #[inline]
    fn regular_type_info<T: GcSafe>() -> Self::TypeInfoRef {
        <T as StaticGcType<GC>>::STATIC_TYPE
    }

    #[inline]
    fn array_type_info<T: GcSafe>() -> Self::TypeInfoRef {
        <[T] as StaticGcType<GC>>::STATIC_TYPE
    }

    #[inline]
    fn vec_type_info<T: GcSafe>() -> Self::TypeInfoRef {
        <T as StaticVecType<GC>>::STATIC_VEC_TYPE
    }

    #[inline]
    unsafe fn init_vec_header(target: *mut GcVecHeader<GC>, capacity: usize, initial_len: usize) {
        (*target).len.set(initial_len);
        (*target).capacity = capacity;
    }

    #[inline]
    unsafe fn create_common_header(mark_data: GC::MarkData, type_info: Self::TypeInfoRef) -> Self::CommonHeader {
        GcHeader {
            type_info, mark_data
        }
    }

    #[inline]
    unsafe fn init_array_header(target: *mut Self::ArrayHeader, len: usize) {
        (*target).len = len;
    }
}

/// The layout of an object's header
#[derive(Debug)]
pub struct HeaderLayout<H, GC: GcInfo> {
    /// The overall size of the header
    pub header_size: usize,
    /// The offset of the 'common' header,
    /// starting from the start of the real header
    pub common_header_offset: usize,
    marker: PhantomData<*mut H>,
    gc_marker: PhantomData<GC>
}
impl<H, GC: GcInfo> Copy for HeaderLayout<H, GC> {}
impl<H, GC: GcInfo> Clone for HeaderLayout<H, GC> {
    #[inline]
    fn clone(&self) -> Self {
        *self
    }
}

impl<H, GC: GcInfo> HeaderLayout<H, GC> {
    #[inline]
    const fn value_offset_from_common_header(&self, align: usize) -> usize {
        self.value_offset(align) - self.common_header_offset
    }
}
impl<H, GC: GcInfo> HeaderLayout<H, GC> {
    #[inline]
    const fn layout(&self) -> Layout {
        unsafe {
            Layout::from_size_align_unchecked(self.header_size, core::mem::align_of::<GcHeader<GC>>())
        }
    }
    #[inline]
    const fn value_offset(&self, align: usize) -> usize {
        let padding = self.layout().padding_needed_for(align);
        self.layout().size() + padding
    }
}
impl<H, GC: GcInfo> HeaderLayoutAPI<H> for HeaderLayout<H, GC> {
    type CommonHeader = GcHeader<GC>;
    type UnknownHeader = HeaderLayout<UnknownHeader, GC>;

    #[inline]
    fn into_unknown_header(self) -> HeaderLayout<UnknownHeader, GC> {
        HeaderLayout {
            header_size: self.header_size,
            common_header_offset: self.common_header_offset,
            marker: PhantomData,
            gc_marker: PhantomData
        }
    }

    #[inline]
    unsafe fn to_common_header(&self, ptr: *mut H) -> *mut GcHeader<GC> {
        (ptr as *mut u8).add(self.common_header_offset).cast()
    }
    #[inline]
    unsafe fn from_common_header(&self, ptr: *mut GcHeader<GC>) -> *mut H {
        (ptr as *mut u8).sub(self.common_header_offset).cast()
    }

    /// Get the header from the specified value pointer
    #[inline]
    unsafe fn from_value_ptr<T: ?Sized>(&self, ptr: *mut T) -> *mut H {
        let align = core::mem::align_of_val(&*ptr);
        (ptr as *mut u8).sub(self.value_offset(align)).cast()
    }
    /// Get the in-memory layout of the header (doesn't include the value)
    #[inline]
    fn layout(&self) -> Layout {
        (*self).layout() // delegates to inherent impl
    }

    #[inline]
    fn value_offset(&self, align: usize) -> usize {
        (*self).value_offset(align) // delegates to inherent impl
    }
}
/// A header for a GC array object
#[repr(C)]
pub struct GcArrayHeader<GC: GcInfo> {
    len: usize,
    common_header: GcHeader<GC>
}
unsafe impl<GC: GcInfo> GcArrayHeaderAPI<GC> for GcArrayHeader<GC> {
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
unsafe impl<GC: GcInfo> GcHeaderAPI for GcArrayHeader<GC> {
    #[inline]
    fn value_ptr(&self) -> *mut c_void {
         self.common_header.value_ptr()
    }
    type Layout = HeaderLayout<Self, GC>;
    const LAYOUT: Self::Layout = HeaderLayout {
        header_size: core::mem::size_of::<Self>(),
        common_header_offset: field_offset!(GcArrayHeader<GC>, common_header),
        marker: PhantomData,
        gc_marker: PhantomData
    };
    type Collector = GC;
    type CommonHeader = GcHeader<GC>;
    type Fmt = SimpleObjectFormat<GC>;
}
/// A header for a Gc vector
#[repr(C)]
pub struct GcVecHeader<GC: GcInfo> {
    pub(crate) capacity: usize,
    pub(crate) len: Cell<usize>,
    pub(crate) common_header: GcHeader<GC>
}
unsafe impl<GC: GcInfo> GcVecHeaderAPI<GC> for GcVecHeader<GC> {
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
            GcTypeLayout::Vec { element_layout } => element_layout,
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
unsafe impl<GC: GcInfo> GcHeaderAPI for GcVecHeader<GC> {
    type Layout = HeaderLayout<Self, GC>;
    const LAYOUT: Self::Layout = HeaderLayout {
        common_header_offset: field_offset!(Self, common_header),
        header_size: core::mem::size_of::<Self>(),
        marker: PhantomData,
        gc_marker: PhantomData
    };
    type Collector = GC;
    type Fmt = SimpleObjectFormat<GC>;
    type CommonHeader = GcHeader<GC>;

    #[inline]
    fn value_ptr(&self) -> *mut c_void {
        self.common_header.value_ptr()
    }
}

/// A header for a GC object
///
/// This is shared between both small arenas
/// and fallback alloc vis `BigGcObject`
#[repr(C)]
pub struct GcHeader<GC: GcInfo> {
    /// The object's type information
    type_info: &'static GcSimpleType<GC>,
    /// The mark data
    mark_data: GC::MarkData
}
unsafe impl<GC: GcInfo> GcHeaderAPI for GcHeader<GC> {
    const LAYOUT: HeaderLayout<Self, GC> = HeaderLayout {
        header_size: core::mem::size_of::<Self>(),
        common_header_offset: 0,
        marker: PhantomData,
        gc_marker: PhantomData
    };
    type Layout = HeaderLayout<Self, GC>;
    type Collector = GC;
    type CommonHeader = Self;
    #[inline]
    fn value_ptr(&self) -> *mut c_void {
        unsafe {
            (self as *const Self as *mut Self as *mut u8)
                // NOTE: This takes into account the possibility of `BigGcObject`
                .add(self.type_info.value_offset_from_common_header)
                .cast::<c_void>()
        }
    }
    type Fmt = SimpleObjectFormat<GC>;
}
unsafe impl<GC: GcInfo> GcCommonHeader for GcHeader<GC> {
    type TypeInfoRef = &'static GcSimpleType<GC>;

    #[inline]
    fn mark_data(&self) -> &'_ GC::MarkData {
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

    unsafe fn dynamic_drop(&self) {
        if let Some(func) = self.type_info.drop_func {
            func(self.value_ptr())
        }
    }

    #[inline]
    unsafe fn dynamic_trace(&self, visitor: &mut GC::PreferredVisitor<SimpleObjectFormat<GC>>) -> Result<(), <GC as GcInfo>::PreferredVisitorErr> {
        if let Some(func) = self.type_info.trace_func {
            func(self.value_ptr(), visitor)
        } else {
            Ok(())
        }
    }
}

/// A type used by GC
#[repr(C)]
pub struct GcSimpleType<GC: GcInfo> {
    /// Information on the type's layout
    pub layout: GcTypeLayout,
    /// The offset of the value from the start of the header
    ///
    /// This varies depending on the type's alignment
    pub value_offset_from_common_header: usize,
    /// The function to trace the type, or `None` if it doesn't need to be traced
    pub trace_func: Option<unsafe fn(*mut c_void, &mut GC::PreferredVisitor<SimpleObjectFormat<GC>>) -> Result<(), GC::PreferredVisitorErr>>,
    /// The function to drop the type, or `None` if it doesn't need to be dropped
    pub drop_func: Option<unsafe fn(*mut c_void)>,
}
impl<GC: GcInfo> GcSimpleType<GC> {
    #[inline]
    unsafe fn determine_size(&self, header: *mut GcHeader<GC>) -> usize {
        match self.layout {
            GcTypeLayout::Fixed(layout) => layout.size(),
            GcTypeLayout::Array { element_layout } => {
                let header = GcArrayHeader::<GC>::LAYOUT.from_common_header(header);
                element_layout.repeat((*header).len).unwrap().0.size()
            },
            GcTypeLayout::Vec { element_layout } => {
                let header = GcVecHeader::<GC>::LAYOUT.from_common_header(header);
                element_layout.repeat((*header).capacity).unwrap().0.size()
            }
        }
    }
    #[inline]
    pub(crate) unsafe fn determine_total_size(&self, header: *mut GcHeader<GC>) -> usize {
        self.determine_total_layout(header).size()
    }
    #[inline]
    pub(crate) unsafe fn determine_total_layout(&self, header: *mut GcHeader<GC>) -> Layout {
        self.header_layout().layout()
            .extend(Layout::from_size_align_unchecked(
                self.determine_size(header),
                self.align()
            )).unwrap().0.pad_to_align()
    }
}
impl<GC: GcInfo> GcSimpleType<GC> {
    #[inline]
    fn align(&self) -> usize {
        match self.layout {
            GcTypeLayout::Fixed(fixed) => fixed.align(),
            GcTypeLayout::Array { element_layout } |
            GcTypeLayout::Vec { element_layout } => element_layout.align()
        }
    }
    #[inline]
    fn header_layout(&self) -> HeaderLayout<UnknownHeader, GC> {
        match self.layout {
            GcTypeLayout::Fixed(_) => GcHeader::LAYOUT.into_unknown_header(),
            GcTypeLayout::Array { .. } => GcArrayHeader::LAYOUT.into_unknown_header(),
            GcTypeLayout::Vec { .. } => GcVecHeader::LAYOUT.into_unknown_header()
        }
    }
}
unsafe impl<GC: GcInfo> GcTypeInfo<GC> for &'static GcSimpleType<GC> {
    type CommonHeader = GcHeader<GC>;

    #[inline]
    fn align(&self) -> usize {
        (**self).align() // delegates to inherent impl
    }

    type HeaderLayout = HeaderLayout<UnknownHeader, GC>;
    #[inline]
    fn header_layout(&self) -> HeaderLayout<UnknownHeader, GC> {
        (**self).header_layout() // delegates to inherent impl
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
    fn determine_total_size(&self, header: &Self::CommonHeader) -> usize {
        unsafe { (**self).determine_total_size(header as *const _ as *mut _) }
    }


    #[inline]
    unsafe fn resolve_common_header<T: GcSafe + ?Sized>(&self, value_ptr: *mut T) -> *mut Self::CommonHeader {
        GcHeader::<GC>::LAYOUT.from_value_ptr(value_ptr)
    }
}

pub(crate) trait StaticVecType<GC: GcInfo> {
    const STATIC_VEC_TYPE: &'static GcSimpleType<GC>;
}
impl<T: GcSafe, GC: GcInfo> StaticVecType<GC> for T {
    const STATIC_VEC_TYPE: &'static GcSimpleType<GC> = &GcSimpleType {
        layout: GcTypeLayout::Vec {
            element_layout: Layout::new::<T>(),
        },
        value_offset_from_common_header: {
            // We have same alignment as our members
            let align = core::mem::align_of::<T>();
            GcArrayHeader::<GC>::LAYOUT.value_offset_from_common_header(align)
        },
        trace_func: if <T as Trace>::NEEDS_TRACE {
            Some({
                unsafe fn visit<T: Trace, V: GcVisitor, GC: GcInfo>(val: *mut c_void, visitor: &mut V) -> Result<(), V::Err> {
                    let len = (*GcVecHeader::<GC>::LAYOUT.from_value_ptr(val as *mut T)).len.get();
                    let slice = core::slice::from_raw_parts_mut(
                        val as *mut T,
                        len
                    );
                    <[T] as Trace>::visit(slice, visitor)
                }
                visit::<T, GC::PreferredVisitor<SimpleObjectFormat<GC>>, GC>
                    as unsafe fn(*mut c_void, &mut GC::PreferredVisitor<_>)
                    -> Result<(), GC::PreferredVisitorErr>
            })
        } else {
            None
        },
        drop_func: if T::NEEDS_DROP {
            Some({
                unsafe fn drop_gc_vec<T: GcSafe, GC: GcInfo>(val: *mut c_void) {
                    (&mut *((*GcVecHeader::<GC>::LAYOUT.from_value_ptr(val as *mut T))
                            .common_header.value_ptr() as *mut SimpleVecRepr<T, GC>)).unchecked_drop::<T>()
                }
                drop_gc_vec::<T, GC> as unsafe fn(*mut c_void)
            })
        } else {
            None
        }
    };
}
trait StaticGcType<GC: GcInfo> {
    const STATIC_TYPE: &'static GcSimpleType<GC>;
}
impl<T: GcSafe, GC: GcInfo> StaticGcType<GC> for [T] {
    const STATIC_TYPE: &'static GcSimpleType<GC> = &GcSimpleType {
        layout: GcTypeLayout::Array { element_layout: Layout::new::<T>() },
        value_offset_from_common_header: {
            GcArrayHeader::<GC>::LAYOUT.value_offset_from_common_header(core::mem::align_of::<T>())
        },
        trace_func: if <T as Trace>::NEEDS_TRACE {
            Some({
                unsafe fn visit<T: Trace, V: GcVisitor, GC: GcInfo>(val: *mut c_void, visitor: &mut V) -> Result<(), V::Err> {
                    let header = GcArrayHeader::<GC>::LAYOUT.from_value_ptr(val as *mut T);
                    let len = (*header).len;
                    let slice = core::slice::from_raw_parts_mut(
                        val as *mut T,
                        len
                    );
                    <[T] as Trace>::visit(slice, visitor)
                }
                visit::<T, GC::PreferredVisitor<SimpleObjectFormat<GC>>, GC>
                    as unsafe fn(*mut c_void, &mut GC::PreferredVisitor<_>) -> Result<(), _>
            })
        } else { None },
        drop_func: if <T as Trace>::NEEDS_DROP {
            Some({
                unsafe fn drop_gc_slice<T: GcSafe, GC: GcInfo>(val: *mut c_void) {
                    let len = (*GcArrayHeader::<GC>::LAYOUT.from_value_ptr(val as *mut T)).len;
                    core::ptr::drop_in_place::<[T]>(core::ptr::slice_from_raw_parts_mut(
                        val as *mut T,
                        len
                    ));
                }
                drop_gc_slice::<T, GC> as unsafe fn(*mut c_void)
            })
        } else { None }
    };
}
impl<T: GcSafe, GC: GcInfo> StaticGcType<GC> for T {
    const STATIC_TYPE: &'static GcSimpleType<GC> = &GcSimpleType {
        layout: GcTypeLayout::Fixed(Layout::new::<T>()),
        value_offset_from_common_header: {
            GcHeader::<GC>::LAYOUT.value_offset_from_common_header(core::mem::align_of::<T>())
        },
        trace_func: if <T as Trace>::NEEDS_TRACE {
            Some(unsafe { mem::transmute::<_, unsafe fn(*mut c_void, &mut GC::PreferredVisitor<SimpleObjectFormat<GC>>)
                -> Result<(), GC::PreferredVisitorErr>>(
                <T as Trace>::visit::<GC::PreferredVisitor<SimpleObjectFormat<GC>>> as
                    fn(&mut T, &mut GC::PreferredVisitor<SimpleObjectFormat<GC>>) -> Result<(), GC::PreferredVisitorErr>,
            ) })
        } else { None },
        drop_func: if <T as Trace>::NEEDS_DROP {
            unsafe { Some(mem::transmute::<_, unsafe fn(*mut c_void)>(
                core::ptr::drop_in_place::<T> as unsafe fn(*mut T)
            )) }
        } else { None }
    };
}

/// The raw representation of a vector using the "simple" object format
///
/// NOTE: Length and capacity are stored implicitly in the [GcVecHeader]
#[repr(C)]
pub struct SimpleVecRepr<T: GcSafe, GC: GcInfo> {
    marker: PhantomData<T>,
    gc_marker: PhantomData<&'static GC>
}
impl<T: GcSafe, GC: GcInfo> SimpleVecRepr<T, GC> {
    #[inline]
    fn header(&self) -> *mut GcVecHeader<GC> {
        unsafe {
            GcVecHeader::LAYOUT.from_value_ptr(self as *const Self as *const T as *mut T)
        }
    }
}
unsafe impl<T: GcSafe, GC: GcInfo> GcVecRepr for SimpleVecRepr<T, GC> {
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
        core::ptr::drop_in_place::<[ActualT]>(core::ptr::slice_from_raw_parts_mut(
            self.ptr() as *mut ActualT,
            self.len()
        ))
    }
}
unsafe_gc_impl!(
    target => SimpleVecRepr<T, GC>,
    params => [T: GcSafe, GC: GcInfo],
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

/// Marker type for a dynamic object
pub struct DynamicObj {
    _priv: ()
}
unsafe_gc_impl!(
    target => DynamicObj,
    params => [],
    bounds => {
        TraceImmutable => never,
    },
    null_trace => never,
    NEEDS_TRACE => true,
    NEEDS_DROP => true,
    trace_mut => |self, visitor| {
        unimplemented!()
    }
);
