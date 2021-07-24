//! An API for [ObjectFormat]
use core::alloc::Layout;
use core::ffi::c_void;
use core::fmt::Debug;

use crate::{CollectorId, GcSafe, GcVisitor, GcArray};
use crate::vec::repr::GcVecRepr;

pub mod simple;

/// Information on the internals of a GC,
/// which is necessary for object formatting.
///
/// This is used instead of [CollectorId] in order to break circular dependencies.
/// This way,
/// `CollectorId` depends on `ObjectFormat` which depends on `GcInfo`
pub unsafe trait GcInfo {
    /// The preferred implementation of [GcVisitor],
    /// which implementations of the trace function are specialized to.
    type PreferredVisitor: GcVisitor<Err=Self::PreferredVisitorErr>;
    /// The type of errors caused by tracing with a [GcVisitor]
    type PreferredVisitorErr: Debug;
    /// The mark data used by the collector to maintain an object's internal state.
    type MarkData: MarkData;
}

/// An API for controlling the in-memory layout of objects.
///
/// This allows clients some control over the in-memory layout.
pub trait ObjectFormat<GC: GcInfo> {
    /// The underlying representation of vectors.
    ///
    /// This is also a parameter of [CollectorId],
    /// and needs to be specified regardless of whether or not the
    /// collector implements this object format API.
    type RawVecRepr: GcVecRepr;
    /// The [GcHeader] common to objects.
    ///
    /// For "regular" `Sized` objects, this is all the header information there is.
    type CommonHeader: GcHeader<GC, TypeInfoRef=Self::TypeInfoRef>;
    /// A reference to the type information
    type TypeInfoRef: GcTypeInfo<GC>;
    /// Resolve the object's header based on its pointer.
    ///
    /// ## Safety
    /// Assumes that the object has been allocated using the correct format.
    unsafe fn resolve_header<T: ?Sized + GcSafe>(ptr: &T) -> &Self::CommonHeader;
    /// The object header for vectors
    type VecHeader: GcVecHeader<GC>;
    /// Resolve the [GcVecHeader] for the specified vector.
    unsafe fn resolve_vec_header<T: GcSafe>(repr: &GC::RawVecRepr) -> &Self::VecHeader;
    /// The object header for arrays
    type ArrayHeader: GcArrayHeader<GC>;
    /// Resolve an array's header, based on a pointer to its value
    unsafe fn resolve_array_header<T: GcSafe>(ptr: &GcArray<T>) -> &Self::ArrayHeader;
    /// Get the type information for a regular type, based on its static type info
    fn regular_type_info<T: GcSafe>() -> Self::TypeInfoRef;
    /// Get the type information for an array
    fn array_type_info<T: GcSafe>() -> Self::TypeInfoRef;
    /// Get the type information for a vector
    fn vec_type_info<T: GcSafe>() -> Self::TypeInfoRef;
    /// Initialize the header of a vector
    ///
    /// ## Safety
    /// Must only be used with a vector of the corresponding type.
    unsafe fn init_vec_header(
        common_header: Self::CommonHeader,
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
        mark_data: GC::MarkData,
        type_info: Self::TypeInfoRef
    ) -> Self::CommonHeader;
    /// Initialize an array header
    ///
    /// ## Safety
    /// The array header must only be used with an array of the correct type.
    unsafe fn init_array_header(
        common_header: Self::CommonHeader,
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

/// The header information that is common to all objects
///
/// ## Safety
/// The header must be relied upon to maintain correct type information,
/// so that garbage collectors can trace pointers without knowing their types.
pub unsafe trait GcHeader<GC: GcInfo> {
    /// The in-memory layout of the type.
    const LAYOUT: Layout = Layout::new::<Self>();
    /// Information on the type
    type TypeInfoRef: GcTypeInfo<GC>;
    /// The mark data for this object.
    ///
    /// This may have interior mutability,
    /// and be modified by the GC.
    fn mark_data(&self) -> &'_ GC::MarkData;
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
    /// An unchecked pointer to the value
    unsafe fn value_ptr(&self) -> *mut c_void;
    /// Dynamically drop the value in the header
    unsafe fn dynamic_drop(&self);
    /// Dynamically trace the value in the header
    unsafe fn dynamic_trace(&self, visitor: &mut GC::PreferredVisitor) -> Result<(), GC::PreferredVisitorErr>;
}
/// The header for a vector
pub unsafe trait GcVecHeader<GC: GcInfo> {
    const LAYOUT: Layout = Layout::new::<Self>();
    /// The common header, that is shared between all types
    type CommonHeader: GcHeader<GC>;
    /// Get a reference to the 'common' header,
    /// that is shared with all other objects.
    ///
    /// This contains mark data and type information.
    fn common_header(&self) -> &Self::CommonHeader;
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
pub unsafe trait GcArrayHeader<GC: GcInfo> {
    const LAYOUT: Layout = Layout::new::<Self>();
    /// The common header, shared between all objects
    type CommonHeader: GcHeader<GC>;
    /// Access the common header,
    /// that is shared with all other types of objects.
    fn common_header(&self) -> &Self::CommonHeader;
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
    type Header: GcHeader<GC>;
    /// Determine the alignment of the type.
    ///
    /// This must be statically known,
    /// even if the type is dynamically sized.
    fn align(&self) -> usize;
    /// Determine if this type actually needs to be traced
    fn needs_trace(&self) -> bool;
    /// Determine if this type actually needs to be dropped
    fn needs_drop(&self) -> bool;
    /// Resolve the object's header given a pointer to its value.
    unsafe fn resolve_header<T: GcSafe + ?Sized>(&self, value_ptr: *mut T) -> *mut Self::Header;
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
