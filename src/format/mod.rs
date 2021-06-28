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

use crate::{GcSystem, GcVisitor, GcSafe, CollectorId, Gc};
use std::ffi::c_void;

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
    fn determine_size<T: GcSafe + ?Sized>(&self, val: Gc<'gc, T, Self::Id>) -> usize;
    /// Use the type information to dynamically dispatch to the correct [Trace::trace] function.
    fn trace(&mut self, item: *mut c_void, visitor: &mut Self::Visitor) -> Result<(), <Self::Visitor as GcVisitor>::Err>;
}

/// A trait used to access the internals of a garbage collector's layout code
///
/// This extends a [GcSystem], because any collector with an exposed layout system
/// naturally "is a" garbage collector in the general sense too.
pub unsafe trait GcLayoutInternals: GcSystem {
    /// The visitor used to trace objects.
    type Visitor: GcVisitor;
    /// The specific object format used in this garbage collector
    type Format: ObjectFormat<Self>;
    /// The data used to mark objects.
    ///
    /// In some cases, this is not an exact multiple of a byte.
    /// The number of  `MARK_BITS` gives the exact number.
    type MarkData: Sized + Copy + 'static;
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

/// An object layout, specialized for a specific garbage collector.
///
/// Some formats (like the "simple" format) can work for any collector.
/// Others may be hard-coded to a fixed implementation or have more specific requirements.
pub unsafe trait ObjectFormat<GC: GcLayoutInternals> {
    /// A garbage collected object, dynamically dispatched and untyped object.
    type DynObject: Sized + Copy;
    /// Runtime type information form
    type TypeInfo: GcTypeInfo<Visitor=GC::Visitor>;
    /// Cast the specified object into an untyped [Self::DynObject]
    unsafe fn as_untyped_object<'gc, T>(&self, val: Gc<'gc, T, GC::Id>)
        where T: GcSafe + ?Sized + 'gc;
    /// Extract mark data from the specified value.
    fn extract_mark_data(&self, val: Self::DynObject) -> GC::MarkData;
    /// Determine the runtime type of the specified value.
    fn determine_type(&self, val: Self::DynObject) -> Self::TypeInfo;
}