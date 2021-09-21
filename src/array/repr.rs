//! Defines the underlying representation of a [GcArray](`crate::array::GcArray`) pointer.
//!
//!
//! Two possible implementations are also available:
//! 1. FatArrayPtr - Represents arrays as a fat pointer
//! 2. ThinArraPtr - Represents arrays as a thin pointer,
//!    with the length stored indirectly in the object header.
#![allow(
    clippy::len_without_is_empty, // This is really an internal interface...
)]
use core::marker::PhantomData;
use core::ptr::NonNull;
use core::ffi::c_void;

use crate::{CollectorId};

/// The type of [GcArrayPtr] impl
#[derive(Copy, Clone, Debug)]
pub enum ArrayPtrKind {
    /// A `FatArrayRepr`, which can be transmuted <-> to `&[T]`
    Fat,
    /// A `ThinArrayRepr`, which can be transmuted <-> to `NonNull<T>`
    Thin
}

/// The raw, untyped representation of a GcArray pointer.
///
/// NOTE: This is only for customizing the *pointer*
/// representation. The in-memory layout of the array and its
/// header can be controlled separately from the pointer.
///
/// This trait is sealed, and there are only two possible
/// implementations:
/// 1. fat pointers
/// 2. thin pointers
///
/// This needs to be untyped,
/// because we expect the type to be [covariant](https://doc.rust-lang.org/nomicon/subtyping.html#variance).
/// If we were to use `Id::ArrayPtr<T>` the borrow checker would infer the type
/// `T` to be invariant. Instead, we just treat it as a `NonNull<u8>`,
/// and add an extra `PhantomData`. Variance problem solved :p
///
/// ## Safety
/// If the length is stored inline in the array (like a fat pointer),
/// then the length and never change.
///
/// If the length is *not* stored inline, then it must
/// be retrieved from the corresponding [CollectorId].
///
/// The underlying 'repr' is responsible
/// for dropping memory as appropriate.
pub unsafe trait GcArrayPtr: Copy + sealed::Sealed {
    /// The repr's collector
    type Id: CollectorId;
    /// The "kind" of the array pointer (whether fat or thin)
    ///
    /// This is necessary to correctly
    /// transmute in a const-fn context
    const UNCHECKED_KIND: ArrayPtrKind;
    /// Construct an array representation from a combination
    /// of a pointer and length.
    ///
    /// This is the garbage collected equivalent of [std::slice::from_raw_parts]
    ///
    /// ## Safety
    /// The combination of pointer + length must be valid. 
    ///
    /// The pointer must be the correct type (the details are erased at runtime).
    unsafe fn from_raw_parts<T>(ptr: NonNull<T>, len: usize) -> Self;
    /// Get a raw pointer to this array's elements.
    ///
    /// The pointer is untyped.
    fn as_raw_ptr(&self) -> *mut c_void;
    /// Get the length of this array,
    /// or `None` if it's not stored in the pointer (it's a thin pointer).
    ///
    /// If this type is a fat pointer it will return
    /// `Some`. 
    /// If this is a thin pointer, then it must return `None`.
    ///
    /// NOTE: Despite the fact that `as_raw_ptr` returns `c_void`,
    /// the length is in terms of the (erased) runtime type `T`,
    /// not in terms of bytes.
    fn len(&self) -> Option<usize>;
}

/// Represents an array as a fat pointer.
///
/// ## Safety
/// This pointer is stored as a `NonNull<[c_void]>`
///
/// Transmuting back and forth is safe if and only if
/// it is cast to a `T` first.
#[repr(transparent)]
pub struct FatArrayPtr<Id: CollectorId> {
    /// NOTE: The length of this slice is in terms of `T`,
    /// not in terms of bytes.
    ///
    /// It is (probably) an under-estimation
    slice: NonNull<[c_void]>,
    marker: PhantomData<Id>
}
impl<Id: CollectorId> self::sealed::Sealed for FatArrayPtr<Id> {}
impl<Id: CollectorId> FatArrayPtr<Id> {
    /// Get the length of this fat array (stored inline)
    #[inline]
    pub const fn len(&self) -> usize {
        unsafe { (&*self.slice.as_ptr()).len() }
    }
}
impl<Id: CollectorId> Copy for FatArrayPtr<Id> {}
impl<Id: CollectorId> Clone for FatArrayPtr<Id> {
    #[inline]
    fn clone(&self) -> Self {
        *self
    }
}

unsafe impl<Id: CollectorId> GcArrayPtr for FatArrayPtr<Id> {
    type Id = Id;
    const UNCHECKED_KIND: ArrayPtrKind = ArrayPtrKind::Fat;

    #[inline]
    unsafe fn from_raw_parts<T>(ptr: NonNull<T>, len: usize) -> Self {
        FatArrayPtr {
            slice: NonNull::new_unchecked(
                core::ptr::slice_from_raw_parts(
                    ptr.as_ptr() as *const T, len
                ) as *mut [T] as *mut [c_void]
            ),
            marker: PhantomData
        }
    }

    #[inline]
    fn as_raw_ptr(&self) -> *mut c_void {
        self.slice.as_ptr() as *mut c_void
    }

    #[inline]
    fn len(&self) -> Option<usize> {
        Some(self.len()) // delegates to inherent impl
    }
}

/// Represents an array as a thin pointer,
/// storing the length indirectly in the object's header.
///
/// ## Safety
/// This type has the same layout as `NonNull<c_void>`.
/// This representation can be relied upon if and only
/// if is cast to `NonNull<T>` first.
#[repr(transparent)]
pub struct ThinArrayPtr<Id: CollectorId> {
    elements: NonNull<c_void>,
    marker: PhantomData<Id>
}
impl<Id: CollectorId> Copy for ThinArrayPtr<Id> {}
impl<Id: CollectorId> Clone for ThinArrayPtr<Id> {
    #[inline]
    fn clone(&self) -> Self {
        *self
    }
}
impl<Id: CollectorId> self::sealed::Sealed for ThinArrayPtr<Id> {}
unsafe impl<Id: CollectorId> GcArrayPtr for ThinArrayPtr<Id> {
    type Id = Id;
    const UNCHECKED_KIND: ArrayPtrKind = ArrayPtrKind::Thin;
    #[inline]
    unsafe fn from_raw_parts<T>(ptr: NonNull<T>, _len: usize) -> Self {
        ThinArrayPtr { elements: ptr.cast(), marker: PhantomData }
    }
    #[inline]
    fn as_raw_ptr(&self) -> *mut c_void {
        self.elements.as_ptr()
    }
    #[inline]
    fn len(&self) -> Option<usize> {
        None
    }
}


mod sealed {
    pub trait Sealed {}
}
