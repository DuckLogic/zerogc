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

use crate::{CollectorId};

/// The type of [GcArrayPtr] impl
#[derive(Copy, Clone, Debug)]
pub enum ArrayPtrKind {
    /// A `FatArrayRepr`, which can be transmuted <-> to `&[T]`
    Fat,
    /// A `ThinArrayRepr`, which can be transmuted <-> to `NonNull<T>`
    Thin
}

/// The raw representation of a GcArray pointer.
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
/// ## Safety
/// The length and never change (and be valid for
/// the corresponding allocation).
///
/// The underlying 'repr' is responsible
/// for dropping memory as appropriate.
pub unsafe trait GcArrayPtr<T>: Copy + sealed::Sealed {
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
    unsafe fn from_raw_parts(ptr: NonNull<T>, len: usize) -> Self;
    /// Convert this pointer into a slice.
    ///
    /// ## Safety
    /// Doesn't check the resulting lifetime is valid.
    unsafe fn as_slice_unchecked<'a>(&self) -> &'a [T]
        where T: 'a;
    /// Get a raw pointer to this array's elements.
    fn as_raw_ptr(&self) -> *mut T;
    /// Get the length of this value
    fn len(&self) -> usize;
}

/// Represents an array as a fat pointer.
///
/// ## Safety
/// This type is guaranteed to be compatible with `&[T]`.
/// Transmuting back and forth is safe.
#[repr(transparent)]
pub struct FatArrayPtr<T, Id: CollectorId> {
    slice: NonNull<[T]>,
    marker: PhantomData<Id>
}
impl<T, Id: CollectorId> self::sealed::Sealed for FatArrayPtr<T, Id> {}
impl<T, Id: CollectorId> FatArrayPtr<T, Id> {
    /// Get the length of this fat array (stored inline)
    #[inline]
    pub const fn len(&self) -> usize {
        unsafe { (&*self.slice.as_ptr()).len() }
    }
}
impl<T, Id: CollectorId> Copy for FatArrayPtr<T, Id> {}
impl<T, Id: CollectorId> Clone for FatArrayPtr<T, Id> {
    #[inline]
    fn clone(&self) -> Self {
        *self
    }
}

unsafe impl<T, Id: CollectorId> GcArrayPtr<T> for FatArrayPtr<T, Id> {
    type Id = Id;
    const UNCHECKED_KIND: ArrayPtrKind = ArrayPtrKind::Fat;

    #[inline]
    unsafe fn from_raw_parts(ptr: NonNull<T>, len: usize) -> Self {
        FatArrayPtr {
            slice: NonNull::new_unchecked(
                core::ptr::slice_from_raw_parts(
                    ptr.as_ptr() as *const T, len
                ) as *mut [T]
            ),
            marker: PhantomData
        }
    }

    #[inline]
    unsafe fn as_slice_unchecked<'a>(&self) -> &'a [T] where T: 'a {
        &*self.slice.as_ptr()
    }

    #[inline]
    fn as_raw_ptr(&self) -> *mut T {
        self.slice.as_ptr() as *mut T
    }

    #[inline]
    fn len(&self) -> usize {
        self.len() // delegates to inherent impl
    }
}

/// Represents an array as a thin pointer,
/// storing the length indirectly in the object's header.
///
/// ## Safety
/// This type has the same layout as `&T`,
/// and can be transmuted back and forth
/// (assuming the appropriate invariants are met).
#[repr(transparent)]
pub struct ThinArrayPtr<T, Id: CollectorId> {
    elements: NonNull<T>,
    marker: PhantomData<Id>
}
impl<T, Id: CollectorId> Copy for ThinArrayPtr<T, Id> {}
impl<T, Id: CollectorId> Clone for ThinArrayPtr<T, Id> {
    #[inline]
    fn clone(&self) -> Self {
        *self
    }
}
impl<T, Id: CollectorId> self::sealed::Sealed for ThinArrayPtr<T, Id> {}
unsafe impl<T, Id: CollectorId> GcArrayPtr<T> for ThinArrayPtr<T, Id> {
    type Id = Id;
    const UNCHECKED_KIND: ArrayPtrKind = ArrayPtrKind::Thin;
    #[inline]
    unsafe fn from_raw_parts(ptr: NonNull<T>, len: usize) -> Self {
        let res = ThinArrayPtr { elements: ptr, marker: PhantomData };
        debug_assert_eq!(
            res.len(),
            len
        );
        res
    }
    #[inline]
    unsafe fn as_slice_unchecked<'a>(&self) -> &'a [T] where T: 'a {
        core::slice::from_raw_parts(
            self.elements.as_ptr(),
            self.len()
        )
    }
    #[inline]
    fn as_raw_ptr(&self) -> *mut T {
        self.elements.as_ptr()
    }
    #[inline]
    fn len(&self) -> usize {
        unsafe { Id::resolve_array_len(
            &*(self as *const Self
                as *const super::GcArray<'static, T, Id>)
        ) }
    }
}


mod sealed {
    pub trait Sealed {}
}
