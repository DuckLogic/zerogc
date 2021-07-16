//! An API for [ObjectFormat]
use core::alloc::Layout;

use crate::{CollectorId, GcSafe, GcVisitor};
use std::ffi::c_void;

pub mod simple;

/// An API for controlling the in-memory layout of objects.
///
/// This allows clients some control over the in-memory layout.
pub trait ObjectFormat<Id: CollectorId> {
    /// The [GcHeader] common to all regular objects
    type Header: GcHeader<TypeInfoRef=Self::TypeInfoRef, Id=Id>;
    /// The type information
    type TypeInfoRef: GcTypeInfo;
    /// The in-memory layout of regular object's header.
    const REGULAR_HEADER_LAYOUT: Layout;
    /// The in-memory layout of a garbage collected vector.
    const VEC_HEADER_LAYOUT: Layout;
    /// The in-memory layout of an array
    const ARRAY_HEADER_LAYOUT: Layout;
    /// Resolve the object's header based on its pointer.
    ///
    /// ## Safety
    /// Assumes that the object has been allocated using the correct format.
    unsafe fn resolve_header<T: ?Sized + GcSafe>(ptr: &T) -> &Self::Header;
    /// The object header for vectors
    type VecHeader: GcVecHeader;
    /// Resolve the [GcVecHeader] for the specified vector.
    unsafe fn resolve_vec_header<T: GcSafe>(repr: &<Id as CollectorId>::RawVecRepr) -> &Self::VecHeader;
    /// The object header for arrays
    type ArrayHeader: GcArrayHeader;
    /// Resolve an array's header, based on a pointer to its value
    unsafe fn resolve_array_header<T: GcSafe>(ptr: &[T]) -> &Self::ArrayHeader;
    /// Get the type information for a regular type, based on its static type info
    fn regular_type_info<T: GcSafe>() -> Self::TypeInfoRef;
    /// Get the type information for an array
    fn array_type_info<T: GcSafe>() -> Self::TypeInfoRef;
    /// Get the type information for a vector
    fn vec_type_info<T: GcSafe>() -> Self::TypeInfoRef;
    /// Initialize the header of a vec
    unsafe fn init_vec_header(
        header: *mut Self::VecHeader,
        mark_data: Id::MarkData,
        type_info: Self::TypeInfoRef,
        capacity: usize,
        initial_len: usize
    );
    /// Initialize an object header
    unsafe fn init_regular_header(
        header: *mut Self::Header,
        mark_data: Id::MarkData,
        type_info: Self::TypeInfoRef
    );
    /// Initialize an array header
    unsafe fn init_array_header(
        header: *mut Self::ArrayHeader,
        mark_data: Id::MarkData,
        type_info: Self::TypeInfoRef,
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
pub unsafe trait GcHeader {
    /// Information on the type
    type TypeInfoRef: GcTypeInfo;
    /// The internal mark data (used by the GC)
    type MarkData: MarkData;
    /// The id of the garbage collector that owns this format
    type Id: CollectorId;
    /// The mark data for this object.
    ///
    /// This may have interior mutability,
    /// and be modified by the GC.
    fn mark_data(&self) -> &'_ Self::MarkData;
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
    unsafe fn dynamic_trace(&self, visitor: &mut <Self::Id as CollectorId>::PreferredVisitor)
        -> Result<(), <<Self::Id as CollectorId>::PreferredVisitor as GcVisitor>::Err>;
}
/// The header for a vector
pub unsafe trait GcVecHeader: GcHeader {
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
pub unsafe trait GcArrayHeader: GcHeader {
    /// The length of the array.
    ///
    /// This must be immutable.
    fn len(&self) -> usize;
    /// The layout of elements in this array.
    fn element_layout(&self) -> Layout;
}

/// Runtime type information for the garbage collector
pub unsafe trait GcTypeInfo: Clone {
    /// The kind of header associated with this value.
    type Header: GcHeader;
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
