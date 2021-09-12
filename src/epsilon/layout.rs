use std::ffi::c_void;
use std::marker::PhantomData;
use std::ptr::NonNull;
use std::alloc::Layout;
use std::cell::Cell;

use crate::{GcRebrand, GcSafe, GcSimpleAlloc};
use crate::vec::repr::{IGcVec, RawGcVec};

use super::{EpsilonCollectorId, EpsilonContext};

/// The header of an object in the epsilon collector.
///
/// Not all objects need headers.
/// If they are `Copy` and statically sized they can be elided.
/// They are also unnecessary for statically allocated objects.
pub struct EpsilonHeader {
    /// This object's `TypeInfo`, or `None` if it doesn't need any.
    pub type_info: &'static TypeInfo,
    /// The next allocated object, or `None` if this is the final object.
    pub next: Option<NonNull<EpsilonHeader>>
}
/*
 * We are Send + Sync because once we are allocated
 * `next` and `type_info` cannot change
 */
unsafe impl Send for EpsilonHeader {}
unsafe impl Sync for EpsilonHeader {}
impl EpsilonHeader {
    pub const LAYOUT: Layout = Layout::new::<Self>();
    /// Assume the specified object has a header,
    /// and retrieve it if so.
    ///
    /// ## Safety
    /// Undefined behavior if the object doesn't have a header.
    /// Undefined behavior if the object isn't allocated in the epsilon collector.
    #[inline]
    pub unsafe fn assume_header<T: ?Sized>(header: *const T) -> *const EpsilonHeader {
        let (_, offset) = Self::LAYOUT.extend(Layout::for_value(&*header)).unwrap_unchecked();
        (header as *const c_void).sub(offset).cast()
    }
    #[inline]
    #[track_caller]
    pub unsafe fn determine_layout(&self) -> Layout {
        let tp = self.type_info;
        match tp.layout {
            LayoutInfo::Fixed(fixed) => fixed,
            LayoutInfo::Array { element_layout } |
            LayoutInfo::Vec { element_layout } => {
                let array_header = EpsilonArrayHeader::from_common_header(self);
                let len = (*array_header).len;
                element_layout.repeat(len).unwrap_unchecked().0
            }
        }
    }
}
#[repr(C)]
pub struct EpsilonArrayHeader {
    pub len: usize,
    pub common_header: EpsilonHeader,
}
impl EpsilonArrayHeader {
    const COMMON_OFFSET: usize = std::mem::size_of::<Self>() - std::mem::size_of::<EpsilonHeader>();
    #[inline]
    pub unsafe fn from_common_header(header: *const EpsilonHeader) -> *const Self {
        (header as *const c_void).sub(Self::COMMON_OFFSET).cast()
    }
}
#[repr(C)]
pub struct EpsilonVecHeader {
    pub capacity: usize,
    // NOTE: Suffix must be transmutable to `EpsilonArrayHeader`
    pub len: Cell<usize>,
    pub common_header: EpsilonHeader,
}
impl EpsilonVecHeader {
    const COMMON_OFFSET: usize = std::mem::size_of::<Self>() - std::mem::size_of::<EpsilonHeader>();
}
pub enum LayoutInfo {
    Fixed(Layout),
    /// A variable sized array
    Array {
        element_layout: Layout
    },
    /// A variable sized vector
    Vec {
        element_layout: Layout
    }
}
impl LayoutInfo {
    #[inline]
    pub const fn align(&self) -> usize {
        match *self {
            LayoutInfo::Fixed(layout) |
            LayoutInfo::Array { element_layout: layout } |
            LayoutInfo::Vec { element_layout: layout }  => layout.align()
        }
    }
    #[inline]
    pub fn common_header_offset(&self) -> usize {
        match *self {
            LayoutInfo::Fixed(_) => 0,
            LayoutInfo::Array { .. } => EpsilonArrayHeader::COMMON_OFFSET,
            LayoutInfo::Vec { .. } => EpsilonVecHeader::COMMON_OFFSET
        }
    }
}
pub struct TypeInfo {
    /// The function to drop this object, or `None` if the object doesn't need to be dropped
    pub drop_func: Option<unsafe fn(*mut c_void)>,
    pub layout: LayoutInfo
}
impl TypeInfo {
    #[inline]
    pub const fn may_ignore(&self) -> bool {
        // NOTE: We don't care about `size`
        self.drop_func.is_none() &&
            self.layout.align() <= std::mem::align_of::<usize>()
    }
    #[inline]
    pub const fn of<T>() -> &'static TypeInfo {
        <T as StaticTypeInfo>::TYPE_INFO
    }
    #[inline]
    pub const fn of_array<T>() -> &'static TypeInfo {
        <[T] as StaticTypeInfo>::TYPE_INFO
    }
    #[inline]
    pub const fn of_vec<T>() -> &'static TypeInfo {
        // For now, vectors and arrays share type info
        <T as StaticTypeInfo>::VEC_INFO.as_ref().unwrap()
    }
}
trait StaticTypeInfo {
    const TYPE_INFO: &'static TypeInfo;
    const VEC_INFO: &'static Option<TypeInfo>;
}
impl<T> StaticTypeInfo for T {
    const TYPE_INFO: &'static TypeInfo = &TypeInfo {
        drop_func: if std::mem::needs_drop::<T>() {
            Some(unsafe { std::mem::transmute::<unsafe fn(*mut T), unsafe fn(*mut c_void)>(std::ptr::drop_in_place::<T>) })
        } else {
            None
        },
        layout: LayoutInfo::Fixed(Layout::new::<T>()),
    };
    const VEC_INFO: &'static Option<TypeInfo> = &Some(TypeInfo {
        drop_func: if std::mem::needs_drop::<T>() {
            Some(drop_array::<T>)
        } else {
            None
        },
        layout: LayoutInfo::Vec {
            element_layout: Layout::new::<T>()
        }
    });
}
impl<T> StaticTypeInfo for [T] {
    const TYPE_INFO: &'static TypeInfo = &TypeInfo {
        drop_func: if std::mem::needs_drop::<T>() {
            Some(drop_array::<T>)
        } else { None },
        layout: LayoutInfo::Array {
            element_layout: Layout::new::<T>()
        }
    };
    const VEC_INFO: &'static Option<TypeInfo> = &None;
}
/// Drop an array or vector of the specified type
unsafe fn drop_array<T>(ptr: *mut c_void) {
    let header = EpsilonArrayHeader::from_common_header(
        EpsilonHeader::assume_header(ptr as *const _ as *const T)
    );
    let len = (*header).len;
    std::ptr::drop_in_place(std::ptr::slice_from_raw_parts_mut(ptr as *mut T, len));
}


/// The raw representation of a vector in the "epsilon" collector
///
/// NOTE: Length and capacity are stored implicitly in the [GcVecHeader]
pub struct EpsilonRawVec<'gc, T> {
    header: NonNull<EpsilonVecHeader>,
    context: &'gc EpsilonContext,
    marker: PhantomData<crate::Gc<'gc, [T], EpsilonCollectorId>>
}
impl<'gc, T> Copy for EpsilonRawVec<'gc, T> {}
impl<'gc, T> Clone for EpsilonRawVec<'gc, T> {
    #[inline]
    fn clone(&self) -> Self {
        *self
    }
}
impl<'gc, T> EpsilonRawVec<'gc, T> {
    #[inline]
    pub(in super) unsafe fn from_raw_parts(
        header: NonNull<EpsilonVecHeader>,
        context: &'gc EpsilonContext
    ) -> Self {
        EpsilonRawVec {
            header, context, marker: PhantomData
        }
    }
    #[inline]
    fn header(&self) -> *const EpsilonVecHeader {
        self.header.as_ptr() as *const EpsilonVecHeader
    }
}
zerogc_derive::unsafe_gc_impl!(
    target => EpsilonRawVec<'gc, T>,
    params => ['gc, T: GcSafe<'gc, EpsilonCollectorId>],
    bounds => {
        TraceImmutable => never,
        GcRebrand => { where T: GcRebrand<'new_gc, EpsilonCollectorId>, T::Branded: Sized }
    },
    branded_type => EpsilonRawVec<'new_gc, T::Branded>,
    collector_id => EpsilonCollectorId,
    NEEDS_TRACE => true, // meh
    NEEDS_DROP => T::NEEDS_DROP,
    null_trace => never,
    trace_mut => |self, visitor| {
        unsafe { visitor.trace_vec(self) }
    },
);
#[inherent::inherent]
unsafe impl<'gc, T: GcSafe<'gc, EpsilonCollectorId>> RawGcVec<'gc, T> for EpsilonRawVec<'gc, T> {
    pub fn iter(&self) -> zerogc::vec::repr::RawVecIter<'gc, T, Self>
        where T: Copy;
}
#[inherent::inherent]
unsafe impl<'gc, T: GcSafe<'gc, EpsilonCollectorId>> IGcVec<'gc, T> for EpsilonRawVec<'gc, T> {
    type Id = EpsilonCollectorId;

    #[inline]
    pub fn with_capacity_in(capacity: usize, ctx: &'gc EpsilonContext) -> Self {
        ctx.alloc_raw_vec_with_capacity(capacity)
    }

    #[inline]
    pub fn len(&self) -> usize {
        unsafe {
            (*self.header()).len.get()
        }
    }

    #[inline]
    pub unsafe fn set_len(&mut self, len: usize) {
        (*self.header()).len.set(len)
    }

    #[inline]
    pub fn capacity(&self) -> usize {
        unsafe { (*self.header()).capacity }
    }

    #[inline]
    pub fn reserve_in_place(&mut self, _additional: usize) -> Result<(), crate::vec::repr::ReallocFailedError> {
        Err(crate::vec::repr::ReallocFailedError::Unsupported)
    }

    #[inline]
    pub unsafe fn as_ptr(&self) -> *const T {
        const LAYOUT: Layout = Layout::new::<EpsilonVecHeader>();
        let offset = LAYOUT.size() + 
            LAYOUT.padding_needed_for(core::mem::align_of::<T>());
        (self.header() as *const u8).add(offset) as *const T
    }

    #[inline]
    pub fn context(&self) -> &'gc EpsilonContext {
        self.context
    }

    // Default methods:
    pub unsafe fn as_mut_ptr(&mut self) -> *mut T;
    pub fn replace(&mut self, index: usize, val: T) -> T;
    pub fn set(&mut self, index: usize, val: T);
    pub fn extend_from_slice(&mut self, src: &[T])
        where T: Copy;
    pub fn push(&mut self, val: T);
    pub fn reserve(&mut self, additional: usize);
    pub fn is_empty(&self) -> bool;
    pub fn new_in(ctx: &'gc EpsilonContext) -> Self;
    pub fn copy_from_slice(src: &[T], ctx: &'gc EpsilonContext) -> Self
        where T: Copy;
    pub fn from_vec(src: Vec<T>, ctx: &'gc EpsilonContext) -> Self;
    pub fn get(&mut self, index: usize) -> Option<T>
        where T: Copy;
    pub unsafe fn as_slice_unchecked(&self) -> &[T];
}
impl<'gc, T: GcSafe<'gc, EpsilonCollectorId>> Extend<T> for EpsilonRawVec<'gc, T> {
    #[inline]
    fn extend<E: IntoIterator<Item=T>>(&mut self, iter: E) {
        let iter = iter.into_iter();
        self.reserve(iter.size_hint().1.unwrap_or(0));
        for val in iter {
            self.push(val);
        }
    }
}