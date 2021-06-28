use std::mem::ManuallyDrop;
use std::ffi::c_void;
use std::alloc::Layout;
use std::mem;
use std::ptr::{Pointee, DynMetadata, NonNull};

use zerogc::GcSafe;
use zerogc_context::field_offset;
use zerogc_context::utils::AtomicCell;
use crate::{RawMarkState, CollectorId, DynTrace, MarkVisitor};
use std::num::NonZeroUsize;


/// A link in the chain of `BigGcObject`s
type BigObjectLinkItem = Option<NonNull<BigGcObject<DynamicObj>>>;
/// An atomic link in the linked-list of BigObjects
///
/// This is thread-safe
// TODO: Replace with a Vec
#[derive(Default)]
pub(crate) struct BigObjectLink(AtomicCell<BigObjectLinkItem>);
impl BigObjectLink {
    #[inline]
    pub const fn new(item: BigObjectLinkItem) -> Self {
        BigObjectLink(AtomicCell::new(item))
    }
    #[inline]
    pub(crate) fn item(&self) -> BigObjectLinkItem {
        self.0.load()
    }
    #[inline]
    pub(crate) unsafe fn set_item_forced(&self, val: BigObjectLinkItem) {
        self.0.store(val)
    }
    #[inline]
    pub(crate) fn append_item(&self, big_obj: Box<BigGcObject>) {
        // Must use CAS loop in case another thread updates
        let mut expected_prev = big_obj.prev.item();
        let mut updated_item = unsafe {
            NonNull::new_unchecked(Box::into_raw(big_obj))
        };
        loop {
            match self.0.compare_exchange(
                expected_prev, Some(updated_item)
            ) {
                Ok(_) => break,
                Err(actual_prev) => {
                    unsafe {
                        /*
                         * We have exclusive access to `updated_item`
                         * here so we don't need to worry about CAS.
                         * We just need to update its `prev`
                         * link to point to the new value.
                         */
                        updated_item.as_mut().prev.0.store(actual_prev);
                        expected_prev = actual_prev;
                    }
                }
            }
        }
    }
}

pub enum LayoutInfo {
    Fixed(Layout),
    Array {
        element_layout: Layout
    }
}
trait StaticGcType<Metadata> {
    const GC_TYPE_INFO: StaticTypeInfo;
}
/// The limited subset of type information that is available at compile-time
///
/// For `Sized` types, we know all the type information.
/// However for `dyn` types and slices, we may not.
pub enum StaticTypeInfo {
    /// Indicates that the type and size are fixed
    Fixed {
        value_offset: usize,
        static_type: &'static GcType
    },
    /// Indicates that the type is dynamically dispatched,
    /// and its type (and size at runtime) can be determined from a [GcType]
    /// pointer at the specified field offset.
    ///
    /// This is the case for `dyn Trait` pointers.
    TraitObject,
    /// Indicates that the object is an array,
    /// with a fixed element type but unknown size.
    ///
    /// This is also the case for `str`.
    Array {
        value_offset: usize,
        size_offset: usize,
        static_type: &'static GcType,
        element_type: &'static GcType
    }
}

impl StaticTypeInfo {
    #[inline] // NOTE: We expect this to be constant folded
    pub(crate) const fn resolve_type<T: GcSafe + ?Sized>(&self, val: &T) -> &GcType {
        match *self {
            StaticTypeInfo::Fixed { static_type, .. } |
            StaticTypeInfo::Array { static_type, .. } => static_type,
            StaticTypeInfo::TraitObject => {
                unsafe { &(*self.resolve_header(val)).type_info }
            }
        }
    }
    #[inline]
    pub(crate) const fn resolve_header<T: GcSafe + ?Sized>(&self, val: &T) -> &'_ GcHeader {
        match *self {
            StaticTypeInfo::Fixed { value_offset, .. } |
            StaticTypeInfo::Array { value_offset, .. } => {
                unsafe { &*(val as *const T).sub(value_offset) }
            },
            StaticTypeInfo::TraitObject => unsafe {
                unsafe {
                    &*(val as *mut T).sub(GcHeader::value_offset(
                        std::mem::align_of_val(val)
                    )) as *const GcHeader
                }
            },
        }
    }
}

impl StaticTypeInfo {
    pub const fn for_type<T: GcSafe + ?Sized>() -> &'static StaticTypeInfo {
        &<T as StaticGcType<<T as Pointee>::Metadata>>::GC_TYPE_INFO
    }
    #[inline]
    pub const fn resolve_total_size<T: GcSafe + ?Sized>(&self, val: &T) -> usize {
        Self::for_type::<T>()
    }
}
impl<T: GcSafe + Sized + Pointee<Metadata=()>> StaticGcType<()> for T {
    const GC_TYPE_INFO: StaticTypeInfo = StaticTypeInfo::Fixed {
        value_offset: GcType::value_offset_for_sized::<T>(),
        static_type: &GcType::type_for_sized::<T>()
    };
}
impl<T: GcSafe + Sized> StaticGcType<usize> for [T] {
    const GC_TYPE_INFO: StaticTypeInfo = StaticTypeInfo::Array {
        element_type: &GcType::type_for_sized::<T>(),
        size_offset: field_offset!(GcHeader, static_type)
    };
}
impl StaticGcType<usize> for str {
    /// A `str` has exactly the same runtime layout as `[u8]`
    const GC_TYPE_INFO: StaticTypeInfo = <[u8] as StaticGcType<usize>>::GC_TYPE_INFO;
}
impl<Dyn: GcSafe + Pointee<Metadata=DynMetadata<Dyn>> + ?Sized> StaticGcType<DynMetadata<Dyn>> for Dyn {
    const GC_TYPE_INFO: StaticTypeInfo = StaticTypeInfo::TraitObject {
        runtime_type_offset: field_offset!(GcHeader, static_type)
    };
}


/// A header for a GC object
///
/// This is uniform for all objects
#[repr(C)]
pub(crate) struct GcHeader {
    /// The type of this object, or `None` if it is an array
    pub(crate) type_info: &'static GcType,
    /*
     * NOTE: State byte should come last
     * If the value is small `(u32)`, we could reduce
     * the padding to a 3 bytes and fit everything in a word.
     *
     * Do we really need to use atomic stores?
     */
    pub(crate) raw_state: AtomicCell<RawMarkState>,
    pub(crate) collector_id: CollectorId,
}
impl GcHeader {
    #[inline]
    pub fn new(type_info: &'static GcType, raw_state: RawMarkState, collector_id: CollectorId) -> Self {
        GcHeader { type_info, raw_state: AtomicCell::new(raw_state), collector_id, prev: BigObjectLink::new() }
    }
    #[inline]
    pub fn value(&self) -> *mut c_void {
        unsafe {
            (self as *const GcHeader as *mut GcHeader as *mut u8)
                // NOTE: This takes into account the alignment and possible padding
                .add(self.type_info.value_offset)
                .cast::<c_void>()
        }
    }
    #[inline]
    pub unsafe fn from_value_ptr<T>(ptr: *mut T, static_type_info: &StaticTypeInfo) -> *mut GcHeader {
        (ptr as *mut u8).sub(static_type.value_offset).cast()
    }
    #[inline]
    pub(crate) fn raw_state(&self) -> RawMarkState {
        // TODO: Is this safe? Couldn't it be accessed concurrently?
        self.raw_state.load()
    }
    #[inline]
    pub(crate) fn update_raw_state(&self, raw_state: RawMarkState) {
        self.raw_state.store(raw_state);
    }
    #[inline]
    pub const fn value_offset(align: usize) -> usize {
        // Big object
        let layout = Layout::new::<BigGcObject<()>>();
        layout.size() + layout.padding_needed_for(align)
    }
}

/// Marker for an unknown GC object
struct DynamicObj;


#[repr(C)]
struct ArrayGcObject<T = DynamicObj> {
    header: GcHeader,
    /// This is dropped using dynamic type info
    static_value: ManuallyDrop<[T]>
}
impl ArrayGcObject {
    #[inline]
    fn size(&self) -> usize {
        self.static_value.as_ref().len()
    }
}
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
            if let Some(drop) = self.header.type_info.drop_func {
                drop(&mut *self.static_value as *mut T as *mut c_void);
            }
        }
    }
}
