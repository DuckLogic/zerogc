//! A "simple" implementation of the [ObjectFormat] interface.

use std::marker::PhantomData;
use crate::format::GcLayoutInternals;


/// The specific type, which is known to
pub struct SimpleGcType<GC: GcLayoutInternals> {
    size: LayoutInfo,
    value_offset: usize,
    trace_func: Option<unsafe fn(*mut c_void, &mut MarkVisitor)>,
    pub drop_func: Option<unsafe fn(*mut c_void)>,
}
impl SimpleGcType {
    /// Create a 'simple' gc type for the specified value,
    /// whose size is fixed and statically known
    pub const fn type_for_sized<T: GcSafe + Sized>() -> SimpleGcType {
        SimpleGcType {
            size: LayoutInfo::Fixed(Layout::new::<T>())
            value_offset: Self::value_offset_for_sized::<T>(),
            trace_func: if T::NEEDS_TRACE {
                unsafe { Some(mem::transmute::<_, unsafe fn(*mut c_void, &mut MarkVisitor)>(
                    <T as DynTrace>::trace as fn(&mut T, &mut MarkVisitor),
                )) }
            } else {
                None
            },
            drop_func: if <T as GcSafe>::NEEDS_DROP {
                unsafe { Some(mem::transmute::<_, unsafe fn(*mut c_void)>(
                    std::ptr::drop_in_place::<T> as unsafe fn(*mut T)
                )) }
            } else { None }
        }
    }
    #[inline]
    const fn value_offset_for_align(align: usize) -> usize {
        // Small object
        let layout = Layout::new::<GcHeader>();
        layout.size() + layout.padding_needed_for(align)
    }
    #[inline]
    pub const fn static_info<T: GcSafe + ?Sized>() -> &'static StaticTypeInfo {
        &<T as StaticGcType<T::Metadata>>::GC_TYPE_INFO
    }
    #[inline]
    pub const fn type_for_val<'a, T: GcSafe + ?Sized>(val: &'a T) -> &'a SimpleGcType {
        Self::static_info::<T>().resolve_type(val)
    }
}

pub struct SimpleObjectFormat<GC: GcLayoutInternals> {
    marker: PhantomData<GC>,
}
impl<GC: GcLayoutInternals> SimpleObjectFormat<GC> {

}
