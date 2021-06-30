//! An object 'format', controlling the in-memory layout of [Gc] items
//!
//! In practice, most applications (outside of language implementations)
//! should use the ["simple" object format](crate::format::simple::SimpleObjectFormat).
//!
//! Object formats are necessary because a collector must retain (some)
//! runtime type information and also mark bits.
//!
//! Since garbage collectors are (typically) used in Language Virtual Machines,
//! the gc-internal type information can be unified with the language-specific class/type information
//! and other metadata.
//!
//! For example, in the Java Virtual Machine objects typically have the following layout [1]:
//! ```text
//!  0: [mark-word  ]
//!  8: [class-word ]
//! 16: [field 1    ]
//! 24: [field 2    ]
//! 32: [field 3    ]
//! ```
//!
//! In zerogc, you could implement a custom [ObjectLayout] to allow a collector
//! to get type-information from the class word and mark information.
//!
//! Note that in hotspot, the "mark word" is used for other things like locking.
//! That is also possible in zerogc, since `MarkData` is controlled by the client program's
//! object format.
//!
//! [1]: https://rkennke.wordpress.com/2019/05/16/shenandoah-gc-in-jdk-13-part-ii-eliminating-forward-pointer-word/

use crate::{GcSystem, GcVisitor, GcSafe, CollectorId, Gc, NullTrace};
use core::ffi::c_void;
use core::fmt::Debug;

pub mod simple;

/// Runtime information about a type that is garbage collected.
///
/// In some implementations, the type information itself may be garbage collected.
/// It is the client's responsibility to ensure that type information doesn't become
/// invalided between safepoints.
pub unsafe trait GcTypeInfo: GcSafe + Sized + Copy {
    /// A specific visitor that this object is specialized to
    type Visitor: GcVisitor;
    /// The type of [CollectorId]
    type Id: CollectorId;
    /// The type of dynamic, untyped objects
    ///
    /// Should match [ObjectFormat::DynObject]
    type DynObject: Sized;
    /// The fixed size of the type,
    /// or `None` if it is a dynamically sized array.
    fn fixed_size(&self) -> Option<usize>;
    /// The alignment of the type at runtime.
    ///
    /// If this is greater than [GcLayoutInternals::IMPLICIT_ALIGN],
    /// the client code must (implicitly) generate the necessary padding.
    fn align(&self) -> usize;
    /// Determine the size of the specified value.
    ///
    /// If this is an array (whose type is not statically known),
    /// this may require actually dereferencing the pointer.
    ///
    /// This is functionally equivalent to `std::mem::size_of_val`,
    /// but uses this [GcTypeInfo] instead of fat pointers.
    fn determine_size(&self, val: Self::DynObject) -> usize;
    /// Determine the *total* size of the object, including its header (if any)
    fn determine_total_size(&self, val: Self::DynObject) -> usize;
    /// Use the type information to dynamically dispatch to the correct [Trace::trace] function.
    fn trace(&self, item: Self::DynObject, visitor: &mut Self::Visitor) -> Result<(), <Self::Visitor as GcVisitor>::Err>;
    /// Use the type information to dynamically dispatch to the correct [Drop] function
    fn drop(&self, item: Self::DynObject);
    /// Check if this type needs to be traced
    fn needs_trace(&self) -> bool;
    /// Check if this type needs to be dropped
    fn needs_drop(&self) -> bool;
}

/// A trait used to access the internals of a garbage collector's layout code
pub unsafe trait GcLayoutInternals: Sized + 'static {
    /// The visitor used to trace objects.
    type Visitor: GcVisitor<Err=Self::VisitorError>;
    /// The id of this collector
    type Id: CollectorId;
    /// Errors that can occur when visiting objects
    type VisitorError: Sized + Debug;
    /// The specific object format used in this garbage collector
    type Format: ObjectFormat<Self>;
    /// The data used to mark objects.
    ///
    /// In some cases, this is not an exact multiple of a byte.
    /// The number of  `MARK_BITS` gives the exact number.
    type MarkData: Sized + NullTrace + GcSafe + 'static;
    /// The 'implicit' alignment of items allocated in the collector.
    ///
    /// This is the minimum alignment that all objects will conform to,
    /// regardless of where and when they are allocated
    /// or what type they actually conform to at runtime.
    const IMPLICIT_ALIGN: usize;
    /// The number of bits that are actually used to mark objects.
    ///
    /// At most, this is `std::mem::size_of::<Self::MarkData>() * 8`
    const MARK_BITS: usize;
}

/// The limited subset of type information that is available at compile-time
///
/// For `Sized` types, we know all the type information.
/// However for `dyn` types and slices, we may not.
pub enum StaticTypeInfo<GC: GcLayoutInternals> {
    /// Indicates that the type and size are fixed
    Fixed {
        /// The fixed type
        static_type: &'static <GC::Format as ObjectFormat<GC>>::TypeInfo
    },
    /// Indicates that the type is dynamically dispatched,
    /// and its type (and size) can vary at runtime.
    ///
    /// This is the case for `dyn Trait` pointers.
    Dynamic,
    /// Indicates that the object is an array,
    /// with a fixed element type but unknown size.
    ///
    /// This is also the case for `str`.
    Array {
        /// The static type of the array as a whole
        static_type: &'static <GC::Format as ObjectFormat<GC>>::TypeInfo,
        /// The type of the array's element
        element_type: &'static <GC::Format as ObjectFormat<GC>>::TypeInfo
    }
}

/// An object layout, specialized for a specific garbage collector.
///
/// Some formats (like the "simple" format) can work for any collector.
/// Others may be hard-coded to a fixed implementation or have more specific requirements.
pub unsafe trait ObjectFormat<GC: GcLayoutInternals>: 'static {
    /// A garbage collected object, dynamically dispatched and untyped object.
    type DynObject: Sized + Debug + Copy + 'static;
    /// Runtime type information form
    type TypeInfo: GcTypeInfo<Visitor=GC::Visitor, DynObject=Self::DynObject>;
    /// Create a dynamically typed object from the specified raw pointer.
    unsafe fn untyped_object_from_raw(raw: *mut c_void) -> Self::DynObject;
    /// Cast the specified object into an untyped [Self::DynObject]
    unsafe fn into_untyped_object<'gc, T>(val: Gc<'gc, T, GC::Id>) -> Self::DynObject
        where T: GcSafe + ?Sized + 'gc;
    /// If the 'internal' mark data needs to use atomic loads/stores.
    ///
    /// See [ObjectFormat::INTERNAL_MARK_DATA_MASK] for more details on 'internal' mark data.
    const INTERNAL_ATOMIC_MARK_DATA: bool;
    /// The bit mask of the mark data that is reserved for internal use by the object format.
    ///
    /// For example, this may be needed by locks or `java.lang.System.identityHashCode`
    ///
    /// There can be at most one word of internal mark data (for now).
    /// If this mask is `0`, then there is no internal mark data.
    const INTERNAL_MARK_DATA_MASK: usize;
    /// Access a single word pointing to the mark data of
    /// the specified object (which is assumed to be a [Gc] pointer).
    ///
    /// ## Safety
    /// The internal mark data of the collectors must be respected.
    /// Mark data may contain internal data that is reserved for the object format's use.
    ///
    /// Because of internal mark data,
    /// it is possible that loads/stores need to be atomic,
    /// even if the collector doesn't need to use atomic accesses itself.
    unsafe fn mark_data_ptr(val: Self::DynObject, type_info: Self::TypeInfo) -> *mut GC::MarkData;
    /// Get a mark data pointer
    #[inline]
    unsafe fn mark_data_ptr_for_gc<'gc, T: GcSafe + ?Sized + 'gc>(gc: Gc<'gc, T, GC::Id>) -> *mut GC::MarkData {
        let obj = Self::into_untyped_object(gc);
        Self::mark_data_ptr(obj, Self::determine_type(obj))
    }
    /// Determine the runtime type of the specified value.
    fn determine_type(val: Self::DynObject) -> Self::TypeInfo;
    /// Get the type of a [Sized] object
    fn sized_type<T: GcSafe>(&self) -> Self::TypeInfo;
    /// Get the type of an array
    fn array_type<T: GcSafe>(&self) -> Self::TypeInfo;
}

/// An object format that is 'open' to allocation
/// and has public constructors
///
/// Some object formats are 'closed' and do not have public constructors.
pub trait OpenAllocObjectFormat<GC: GcLayoutInternals>: ObjectFormat<GC> {
    /// The header type to prepend to all 'Sized' allocated objects
    type SizedHeaderType: Sized + NullTrace;
    /// Write a sized object header, given the specified header location and initial marking data
    ///
    /// Returns a pointer to the location of the allocated object.
    ///
    /// Type is automatically inferred.
    unsafe fn write_sized_header<T: GcSafe>(&self, header_location: *mut Self::SizedHeaderType, mark_data: GC::MarkData) -> *mut T;
}
