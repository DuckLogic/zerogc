//! An API for [ObjectFormat]
use core::alloc::Layout;
use core::ffi::c_void;
use core::fmt::Debug;

use crate::{GcSafe, GcVisitor, NullTrace};
use crate::vec::repr::GcVecRepr;

pub mod simple;

/// Information on the internals of a GC,
/// which is necessary for object formatting.
///
/// This is used instead of [CollectorId] in order to break circular dependencies.
/// This way,
/// `CollectorId` depends on `ObjectFormat` which depends on `GcInfo`
pub unsafe trait GcInfo: 'static + NullTrace + GcSafe {
    /// The preferred implementation of [GcVisitor],
    /// which implementations of the trace function are specialized to.
    ///
    /// This is parameterized by an [ObjectFormat].
    type PreferredVisitor<Fmt: ObjectFormat<Collector=Self>>: GcVisitor<Err=Self::PreferredVisitorErr>;
    /// The type of errors caused by tracing with a [GcVisitor]
    type PreferredVisitorErr: Debug;
    /// The mark data used by the collector to maintain an object's internal state.
    type MarkData: MarkData;
    /// Whether or not this collector may relocate pointers.
    const IS_MOVING: bool;
}

/// An API for controlling the in-memory layout of objects.
///
/// This allows clients some control over the in-memory layout.
pub trait ObjectFormat: 'static {
    /// Information on the collector that is using this object format.
    type Collector: GcInfo;
    /// The underlying representation of vectors.
    ///
    /// This is also a parameter of [CollectorId],
    /// and needs to be specified regardless of whether or not the
    /// collector implements this object format API.
    type RawVecRepr: GcVecRepr;
    /// The [GcHeader] common to objects.
    ///
    /// For "regular" `Sized` objects, this is all the header information there is.
    type CommonHeader: GcCommonHeader<Collector=Self::Collector, Fmt=Self, CommonHeader=Self::CommonHeader>;
    /// A reference to the type information
    type TypeInfoRef: GcTypeInfo<Self::Collector>;
    /// Resolve the object's header based on its pointer.
    ///
    /// ## Safety
    /// Assumes that the object has been allocated using the correct format.
    unsafe fn resolve_header<T: ?Sized + GcSafe>(ptr: &T) -> &Self::CommonHeader;
    /// The object header for vectors
    type VecHeader: GcVecHeader<Self::Collector, CommonHeader=Self::CommonHeader>;
    /// Resolve the [GcVecHeader] for the specified vector.
    unsafe fn resolve_vec_header<T: GcSafe>(repr: &Self::RawVecRepr) -> &Self::VecHeader;
    /// The object header for arrays
    type ArrayHeader: GcArrayHeader<Self::Collector, CommonHeader=Self::CommonHeader>;
    /// Resolve an array's header, based on a pointer to its value
    ///
    /// ## Safety
    /// The specified pointer is assumed to point to a `GcArray`
    /// corresponding to the current collector (and object format).
    unsafe fn resolve_array_header<T: GcSafe>(ptr: *mut T) -> *mut Self::ArrayHeader;
    /// Get the type information for a regular type, based on its static type info
    fn regular_type_info<T: GcSafe>() -> Self::TypeInfoRef;
    /// Get the type information for an array
    fn array_type_info<T: GcSafe>() -> Self::TypeInfoRef;
    /// Get the type information for a vector
    fn vec_type_info<T: GcSafe>() -> Self::TypeInfoRef;
    /// Initialize the header of a vector
    ///
    /// This doesn't initialize the "common" header.
    /// That is done seperately.
    ///
    /// ## Safety
    /// Must only be used with a vector of the corresponding type.
    ///
    /// The target pointer must be correct.
    unsafe fn init_vec_header(
        ptr: *mut Self::VecHeader,
        capacity: usize,
        initial_len: usize
    );
    /// Create the part of the header which is shared by all objects.
    ///
    /// The common object header is also used by "regular" objects,
    /// that don't need more specialized headers (i.e. not GcVec or GcArray)
    ///
    /// ## Safety
    /// The returned header must only be used with an object
    /// of the corresponding type.
    ///
    /// For example, it would be unsafe to put the header for `ArrayList`
    /// on a `String`.
    unsafe fn create_common_header(
        mark_data: <Self::Collector as GcInfo>::MarkData,
        type_info: Self::TypeInfoRef
    ) -> Self::CommonHeader;
    /// Initialize an array header
    ///
    /// ## Safety
    /// The array header must only be used with an array of the correct type.
    unsafe fn init_array_header(
        ptr: *mut Self::ArrayHeader,
        len: usize
    );
}

/// Marker type for a collector's "mark data".
///
/// This should only be implemented by collectors,
/// not by clients.
///
/// Only immutable access to the mark data is given,
/// so collectors will need to use interior mutability to change it.
pub unsafe trait MarkData: Clone + Sized + 'static {
}

/// Dummy impl
unsafe impl MarkData for () {}

/// A header for garbage collected objects
pub unsafe trait GcHeader: Sized {
    /// The type of layout information
    type Layout: HeaderLayout<Self, CommonHeader=Self::CommonHeader>;
    /// The layout of the header
    const LAYOUT: Self::Layout;
    /// Information on the collector
    type Collector: GcInfo;
    /// The format that manages this header
    type Fmt: ObjectFormat<Collector=Self::Collector>;
    /// The header that is common to all objects
    type CommonHeader: GcCommonHeader<Collector=Self::Collector>;
    /// Get a pointer to the header's value
    fn value_ptr(&self) -> *mut c_void;
    /// Access the header common to all objects
    #[inline]
    fn common_header(&self) -> &Self::CommonHeader {
        /*
         * SAFETY: This trait is unsafe, and `CommonHeader` and `LAYOUT` must be implemented correctly
         */
        unsafe { &*Self::LAYOUT.to_common_header(self as *const Self as *mut Self) }
    }
}

/// The header information that is common to all objects
///
/// ## Safety
/// The header must be relied upon to maintain correct type information,
/// so that garbage collectors can trace pointers without knowing their types.
pub unsafe trait GcCommonHeader: GcHeader {
    /// Information on the type
    type TypeInfoRef: GcTypeInfo<Self::Collector, CommonHeader=Self>;
    /// The mark data for this object.
    ///
    /// This may have interior mutability,
    /// and be modified by the GC.
    fn mark_data(&self) -> &'_ <Self::Collector as GcInfo>::MarkData;
    /// Get the object's type information
    fn type_info(&self) -> Self::TypeInfoRef;
    /// Determine the size of the value (excluding header)
    fn value_size(&self) -> usize;
    /// Determine the size of the value (including header)
    fn total_size(&self) -> usize;
    /// Determine the total layout of the value (including header)
    #[inline]
    fn total_layout(&self) -> Layout {
        unsafe {
            Layout::from_size_align_unchecked(
                self.total_size(),
                self.type_info().align()
            )
        }
    }
    /// Dynamically drop the value in the header
    unsafe fn dynamic_drop(&self);
    /// Dynamically trace the value in the header
    unsafe fn dynamic_trace(&self, visitor: &mut <Self::Collector as GcInfo>::PreferredVisitor<Self::Fmt>)
        -> Result<(), <Self::Collector as GcInfo>::PreferredVisitorErr>;
}
/// The header for a vector
pub unsafe trait GcVecHeader<GC: GcInfo>: GcHeader<Collector=GC> {
    /// Get the length of the vector
    fn len(&self) -> usize;
    /// Set the length of the vector
    unsafe fn set_len(&self, new_len: usize);
    /// Get the capacity of the vector.
    ///
    /// This must be fixed at allocation time
    fn capacity(&self) -> usize;
    /// The layout of elements in this vector.
    fn element_layout(&self) -> Layout;
}

/// The header for an array
pub unsafe trait GcArrayHeader<GC: GcInfo>: GcHeader<Collector=GC> {
    /// The length of the array.
    ///
    /// This must be immutable.
    fn len(&self) -> usize;
    /// The layout of elements in this array.
    fn element_layout(&self) -> Layout;
}

/// Runtime type information for the garbage collector
///
/// TODO: Scrap this in favor of [GcHeader]??
pub unsafe trait GcTypeInfo<GC: GcInfo>: Clone {
    /// The kind of header associated with this value.
    type CommonHeader: GcCommonHeader<Collector=GC>;
    /// Determine the alignment of the type.
    ///
    /// This must be statically known,
    /// even if the type is dynamically sized.
    fn align(&self) -> usize;
    /// The header's layout type
    type HeaderLayout: HeaderLayout<UnknownHeader, CommonHeader=Self::CommonHeader>;
    /// The layout of this object's header
    fn header_layout(&self) -> Self::HeaderLayout;
    /// Determine if this type actually needs to be traced
    fn needs_trace(&self) -> bool;
    /// Determine if this type actually needs to be dropped
    fn needs_drop(&self) -> bool;
    /// Determine the total size of this type,
    /// including the header.
    fn determine_total_size(&self, header: &Self::CommonHeader) -> usize;
    /// Determine the total layout of this type,
    /// including the header
    #[inline]
    fn determine_total_layout(&self, header: &Self::CommonHeader) -> Layout {
        unsafe {
            Layout::from_size_align_unchecked(
                self.determine_total_size(header),
                self.align()
            )
        }
    }
    /// Resolve the object's common header given a pointer to its value.
    unsafe fn resolve_common_header<T: GcSafe + ?Sized>(&self, value_ptr: *mut T) -> *mut Self::CommonHeader;
}

/// The in-memory layout of a [GcHeader]
pub trait HeaderLayout<H>: Copy {
    /// The common part of the header,
    /// shared between all objects
    type CommonHeader: GcCommonHeader;
    /// A marker type for statically-unknown headers (see 'into_unknown_header')
    type UnknownHeader: HeaderLayout<UnknownHeader, CommonHeader=Self::CommonHeader>;

    /// Erase the static type parameter of this layout,
    /// switching it to an 'UnknownHeader'
    ///
    /// ## Safety
    /// The resulting header's type parameter must not be confused
    /// with an actual header. This method is only available
    /// to ease static type restrictions.
    ///
    /// The `UnknownHeader` is an uninhabited enum,
    /// which must not ever have any values at runtime.
    fn into_unknown_header(self) -> Self::UnknownHeader;

    /// Cast a pointer from this specific header,
    /// to a pointer to the common header.
    ///
    /// ## Safety
    /// Assumes the pointer to header is valid,
    /// so the pointer arithmetic will work
    unsafe fn to_common_header(&self, ptr: *mut H) -> *mut Self::CommonHeader;

    /// Cast a pointer from the "common" object header,
    /// to a pointer to this specific header
    ///
    /// ## Safety
    /// Assumes the header is actually of this type,
    /// so that the pointer arithmetic will be valid
    unsafe fn from_common_header(&self, ptr: *mut Self::CommonHeader) -> *mut H;

    /// Get a pointer to the header, based on the specified pointer to the value.
    ///
    /// ## Safety
    /// Assumes the specified pointer points to a garbage collected
    /// object with this header.
    unsafe fn from_value_ptr<T: ?Sized>(&self, ptr: *mut T) -> *mut H;

    /// Get the in-memory layout of the header (doesn't include the value)
    fn layout(&self) -> Layout;

    /// Get the offset of the value from the start of the header,
    /// given the alignment of its value
    fn value_offset(&self, align: usize) -> usize;
}

/// Statically known layout information for [GcTypeInfo]
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

/// A marker value, standing in for a [GcHeader] of unknown type
///
/// ## Safety
/// This is not actually a meaningful value. Do not use it as such.
///
/// In fact, it is an uninhabited enum. Having any instance
/// of this type at runtime is immediate undefined behavior.
pub enum UnknownHeader {
}

