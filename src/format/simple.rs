//! A "simple" implementation of the [ObjectFormat] interface.
//!
//! This requires the "alloc" feature
#![allow(missing_docs)]

use core::marker::PhantomData;
use core::{mem, ptr};

use core::alloc::Layout;

use zerogc_derive::NullTrace;

use crate::format::{GcLayoutInternals, ObjectFormat, GcTypeInfo, StaticTypeInfo, OpenAllocObjectFormat};
use crate::{GcSafe, GcSystem, Gc, CollectorId, NullTrace};
use std::ffi::c_void;

/// Information on a layout
pub enum LayoutInfo {
    Fixed(Layout),
    Array {
        element_layout: Layout
    }
}

/// Marker for a garbage collected object of unknown type
pub struct DynObject(());

/// A pointer to a garbage collected object with an unknown type
#[derive(Copy, Clone, Debug)]
pub struct DynObjectPtr(*mut DynObject);

#[derive(NullTrace)]
#[zerogc(ignore_params(GC))]
pub struct GcHeader<GC: GcLayoutInternals> {
    gc_type: &'static SimpleGcType<GC>,
    mark_data: GC::MarkData,
}
impl<GC: GcLayoutInternals> GcHeader<GC> {
    #[inline]
    pub unsafe fn header_for<T: ?Sized>(ptr: *mut T) -> *mut Self {
        (ptr as *mut u8)
            .sub(Self::regular_value_offset(std::mem::align_of_val(&*ptr)))
            .cast()
    }
    #[inline]
    pub const fn regular_value_offset(align: usize) -> usize {
        let layout = Layout::new::<Self>();
        let regular_padding = layout.padding_needed_for(align);
        let array_padding = Layout::new::<Self>().padding_needed_for(align);
        debug_assert!(array_padding == regular_padding);
        layout.size() + regular_padding
    }
}
struct GcArrayHeader<GC: GcLayoutInternals> {
    len: usize,
    #[allow(dead_code)] // Not actually dead. It's read by pointer offset
    header: GcHeader<GC>,
}
impl<GC: GcLayoutInternals> GcArrayHeader<GC> {
    unsafe fn dyn_drop_func<T>(ptr: *mut DynObject) {
        let size = (*Self::array_header_for(ptr)).len;
        std::ptr::drop_in_place::<[T]>(std::ptr::slice_from_raw_parts_mut(
            ptr as *mut T,
            size
        ))

    }
    #[inline]
    pub unsafe fn array_header_for<T: ?Sized>(ptr: *mut T) -> *mut Self {
        (ptr as *mut u8).sub(Self::array_value_offset(std::mem::align_of_val(&*ptr))).cast()
    }
    #[inline]
    pub const fn array_value_offset(align: usize) -> usize {
        let layout = Layout::new::<Self>();
        layout.size() + layout.padding_needed_for(align)
    }
}

unsafe trait DynObjectTrace<GC: GcLayoutInternals> {
    unsafe fn dyn_trace(ptr: *mut DynObject, visitor: &mut GC::Visitor) -> Result<(), GC::VisitorError>;
}
unsafe impl<GC: GcLayoutInternals, T: GcSafe> DynObjectTrace<GC> for T {
    unsafe fn dyn_trace(ptr: *mut DynObject, visitor: &mut GC::Visitor) -> Result<(), GC::VisitorError> {
        <T as crate::Trace>::visit(&mut *(ptr as *mut T), visitor)
    }
}
unsafe impl<GC: GcLayoutInternals, T: GcSafe> DynObjectTrace<GC> for [T] {
    unsafe fn dyn_trace(ptr: *mut DynObject, visitor: &mut GC::Visitor) -> Result<(), <GC as GcLayoutInternals>::VisitorError> {
        let header = GcArrayHeader::<GC>::array_header_for(ptr);
        let slice = std::slice::from_raw_parts_mut(ptr as *mut T, (*header).len);
        <[T] as crate::Trace>::visit(slice, visitor)
    }
}
/// Type information
#[derive(NullTrace)]
#[zerogc(ignore_params(GC))]
pub struct SimpleGcType<GC: GcLayoutInternals> {
    #[zerogc(unsafe_skip_trace)]
    layout: LayoutInfo,
    #[zerogc(unsafe_skip_trace)]
    trace_func: Option<unsafe fn(*mut DynObject, &mut GC::Visitor) -> Result<(), GC::VisitorError>>,
    #[zerogc(unsafe_skip_trace)]
    drop_func: Option<unsafe fn(*mut DynObject)>,
}
impl<GC: GcLayoutInternals> SimpleGcType<GC> {
    #[inline]
    fn header_size(&self) -> usize {
        match self.layout {
            LayoutInfo::Fixed(layout) => GcHeader::<GC>::regular_value_offset(layout.align()),
            LayoutInfo::Array { element_layout } => GcArrayHeader::<GC>::array_value_offset(element_layout.align()),
        }
    }
    #[inline]
    pub const fn array_type<T: GcSafe>() -> SimpleGcType<GC> {
        SimpleGcType {
            layout: LayoutInfo::Array {
                element_layout: Layout::new::<T>()
            },
            trace_func: if T::NEEDS_TRACE {
                Some(<[T] as DynObjectTrace<GC>>::dyn_trace as unsafe fn(*mut DynObject, &mut GC::Visitor) -> _)
            } else { None },
            drop_func: if T::NEEDS_DROP {
                Some(GcArrayHeader::<GC>::dyn_drop_func::<T> as unsafe fn(*mut DynObject))
            } else { None },
        }
    }
    /// Create a 'simple' gc type for the specified value,
    /// whose size is fixed and statically known
    #[inline]
    pub const fn type_for_sized<T: GcSafe + Sized>() -> SimpleGcType<GC> {
        SimpleGcType {
            layout: LayoutInfo::Fixed(Layout::new::<T>()),
            trace_func: if T::NEEDS_TRACE {
                unsafe { Some(
                    <T as DynObjectTrace<GC>>::dyn_trace as unsafe fn(_, &mut GC::Visitor) -> _,
                ) }
            } else {
                None
            },
            drop_func: if <T as GcSafe>::NEEDS_DROP {
                unsafe { Some(mem::transmute::<_, unsafe fn(*mut DynObject)>(
                    ptr::drop_in_place::<T> as unsafe fn(*mut T)
                )) }
            } else { None }
        }
    }
    #[inline]
    const fn value_offset_for_align(align: usize) -> usize {
        let layout = Layout::new::<Self>();
        layout.size() + layout.padding_needed_for(align)
    }
}

unsafe impl<GC: GcLayoutInternals> GcTypeInfo for &'static SimpleGcType<GC> {
    type Visitor = GC::Visitor;
    type Id = GC::Id;
    type DynObject = DynObjectPtr;

    #[inline]
    fn fixed_size(&self) -> Option<usize> {
        match self.layout {
            LayoutInfo::Fixed(layout) => Some(layout.size()),
            LayoutInfo::Array { .. } => None
        }
    }

    #[inline]
    fn align(&self) -> usize {
        match self.layout {
            LayoutInfo::Fixed(layout) => layout.align(),
            LayoutInfo::Array { element_layout } => {
                // NOTE: The alignment of arrays is the same as its member
                element_layout.align()
            }
        }
    }

    #[inline]
    fn determine_size(&self, val: Self::DynObject) -> usize {
        match self.layout {
            LayoutInfo::Fixed(layout) => layout.size(),
            LayoutInfo::Array { element_layout } => {
                let len = unsafe { (*GcArrayHeader::<GC>::array_header_for(val.0)).len };
                len * element_layout.size()
            }
        }
    }

    #[inline]
    fn determine_total_size(&self, val: Self::DynObject) -> usize {
        self.determine_size(val) + self.header_size()
    }


    #[inline]
    fn trace(&self, item: DynObjectPtr, visitor: &mut Self::Visitor) -> Result<(), GC::VisitorError> {
        if let Some(func) = self.trace_func {
            unsafe { func(item.0, visitor) }
        } else {
            Ok(())
        }
    }

    #[inline]
    fn drop(&self, item: Self::DynObject) {
        if let Some(drop) = self.drop_func {
            unsafe { drop(item.0) }
        }
    }

    #[inline]
    fn needs_trace(&self) -> bool {
        self.trace_func.is_some()
    }

    #[inline]
    fn needs_drop(&self) -> bool {
        self.drop_func.is_some()
    }
}

#[derive(Default)]
pub struct SimpleObjectFormat;
unsafe impl<GC: GcLayoutInternals> ObjectFormat<GC> for SimpleObjectFormat {
    type DynObject = DynObjectPtr;
    type TypeInfo = &'static SimpleGcType<GC>;

    #[inline]
    unsafe fn untyped_object_from_raw(raw: *mut c_void) -> Self::DynObject {
        DynObjectPtr(raw as *mut DynObject)
    }

    #[inline]
    unsafe fn into_untyped_object<'gc, T>(val: Gc<'gc, T, GC::Id>) -> DynObjectPtr
        where T: GcSafe + ?Sized + 'gc {
        DynObjectPtr(val.as_raw_ptr() as *mut DynObject)
    }

    /// We have no internal mark data, so there is no need to keep it atomic
    const INTERNAL_ATOMIC_MARK_DATA: bool = false;
    /// We have no internal mark data, so there's no mask
    const INTERNAL_MARK_DATA_MASK: usize = 0;

    #[inline]
    unsafe fn mark_data_ptr(val: Self::DynObject, type_info: Self::TypeInfo) -> *mut GC::MarkData {
        &mut (*GcHeader::<GC>::header_for(val.0)).mark_data
    }

    #[inline]
    fn determine_type(val: Self::DynObject) -> Self::TypeInfo {
        unsafe { (*GcHeader::<GC>::header_for(val.0)).gc_type }
    }

    #[inline]
    fn sized_type<T: GcSafe>(&self) -> Self::TypeInfo {
        &<T as StaticGcType<GC>>::STATIC_TYPE
    }

    #[inline]
    fn array_type<T: GcSafe>(&self) -> Self::TypeInfo {
        &<[T] as StaticGcType<GC>>::STATIC_TYPE
    }
}
impl<GC: GcLayoutInternals> OpenAllocObjectFormat<GC> for SimpleObjectFormat {
    type SizedHeaderType = GcHeader<GC>;

    #[inline]
    unsafe fn untyped_object_from_header(header: *mut Self::SizedHeaderType) -> Self::DynObject {
        DynObjectPtr((header as *mut u8).add(GcHeader::<GC>::regular_value_offset((*header).gc_type.align())).cast())
    }

    #[inline]
    unsafe fn write_sized_header<T: GcSafe>(&self, header_location: *mut Self::SizedHeaderType, mark_data: GC::MarkData) -> *mut T {
        *header_location = GcHeader::<GC> {
            gc_type: self.sized_type::<T>(),
            mark_data
        };
        (header_location as *mut u8).add(GcHeader::<GC>::regular_value_offset(std::mem::align_of::<T>())).cast()
    }
}
trait StaticGcType<GC: GcLayoutInternals> {
    const STATIC_TYPE: SimpleGcType<GC>;
}
impl<GC: GcLayoutInternals, T: Sized + GcSafe> StaticGcType<GC> for T {
    const STATIC_TYPE: SimpleGcType<GC> = SimpleGcType::type_for_sized::<T>();
}
impl<GC: GcLayoutInternals, T: Sized + GcSafe> StaticGcType<GC> for [T] {
    const STATIC_TYPE: SimpleGcType<GC> = SimpleGcType::array_type::<T>();
}
