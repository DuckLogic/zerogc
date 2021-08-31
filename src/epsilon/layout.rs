use std::ffi::c_void;
use std::ptr::NonNull;
use std::alloc::Layout;
use std::cell::Cell;

use crate::vec::repr::GcVecRepr;

/// The header of an object in the epsilon collector.
///
/// Not all objects need headers.
/// If they are `Copy` and statically sized they can be elided.
/// They are also unnecessary for statically allocated objects.
pub struct EpsilonHeader {
    /// This object's `TypeInfo`, or `None` if it doesn't need any.
    pub type_info: Option<&'static TypeInfo>,
    /// The next allocated object, or `None` if this is the final object.
    pub next: Option<NonNull<EpsilonHeader>>
}
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
pub struct TypeInfo {
    /// The function to drop this object, or `None` if the object doesn't need to be dropped
    pub drop_func: Option<unsafe fn(*mut c_void)>,
    /// The size of the object, or `None` if this is an array or vector
    pub size: Option<usize>
}
impl TypeInfo {
    #[inline]
    pub const fn of<T>() -> Option<&'static TypeInfo> {
        if std::mem::needs_drop::<T>() {
            Some(<T as StaticTypeInfo>::TYPE_INFO)
        } else {
            None
        }
    }
    #[inline]
    pub const fn of_array<T>() -> Option<&'static TypeInfo> {
        if std::mem::needs_drop::<T>() {
            Some(<[T] as StaticTypeInfo>::TYPE_INFO)
        } else {
            None
        }
    }
    #[inline]
    pub const fn of_vec<T>() -> Option<&'static TypeInfo> {
        // For now, vectors and arrays share type info
        Self::of_array::<T>()
    }
}
trait StaticTypeInfo {
    const TYPE_INFO: &'static TypeInfo;
}
impl<T> StaticTypeInfo for T {
    const TYPE_INFO: &'static TypeInfo = &TypeInfo {
        drop_func: if std::mem::needs_drop::<T>() {
            Some(unsafe { std::mem::transmute::<unsafe fn(*mut T), unsafe fn(*mut c_void)>(std::ptr::drop_in_place::<T>) })
        } else {
            None
        },
        size: Some(std::mem::size_of::<T>())
    };
}
impl<T> StaticTypeInfo for [T] {
    const TYPE_INFO: &'static TypeInfo = &TypeInfo {
        drop_func: if std::mem::needs_drop::<T>() {
            Some(drop_array::<T>)
        } else { None },
        size: None
    };
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
pub struct EpsilonVecRepr {
    _priv: ()
}
impl EpsilonVecRepr {
    #[inline]
    fn header(&self) -> *const EpsilonVecHeader {
        /*
         * todo: what if we have a non-standard alignment?
         * this is a bug in the simple collector too
         */
        unsafe {
            (self as *const Self as *mut Self as *mut u8)
                .sub(std::mem::size_of::<EpsilonVecHeader>())
                .cast()
        }
    }
}
zerogc_derive::unsafe_gc_impl!(
    target => EpsilonVecRepr,
    params => [],
    bounds => {
        TraceImmutable => never
    },
    NEEDS_TRACE => true, // meh
    NEEDS_DROP => true, // unable to know at compile time (so be conservative)
    null_trace => never,
    trace_mut => |self, visitor| {
        // TODO: What if someone wants to trace our innards using a different collector?
        todo!("tracing EpsilonVecRepr")
    },
);
unsafe impl<'gc> GcVecRepr<'gc> for EpsilonVecRepr {
    // It is meaningless to reallocate with bump-pointer allocation
    const SUPPORTS_REALLOC: bool = false;
    type Id = super::EpsilonCollectorId;

    fn element_layout(&self) -> Layout {
        todo!()
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
