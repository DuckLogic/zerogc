//! Defines the underlying reprsention of a [GcArray] pointer.
//!
//!
//! Two possible implementations are also available:
//! 1. FatArrayRepr - Represents arrays as a fat pointer
//! 2. ThinArrayRepr - Represents arrays as a thin pointer,
//!    with the length stored indirectly in the object header.
#![allow(
    clippy::len_without_is_empty, // This is really an internal interface...
)]
use core::marker::PhantomData;
use core::ptr::NonNull;


use crate::{CollectorId, Gc};

/// The type of [GcArrayRepr] impl
#[derive(Copy, Clone, Debug)]
pub enum ArrayReprKind {
    /// A `FatArrayRepr`, which can be transmuted <-> to `&[T]`
    Fat,
    /// A `ThinArrayRepr`, which can be transmuted <-> to `NonNull<T>`
    Thin
}

/// The raw representation of a GcArray pointer.
///
///
/// NOTE: This is only for customizing the *pointer*
/// representation. The in-memory layout of the array and its
/// header can be controlled separately from the pointer
/// (ie. feel free to use a builtin impl even if you have a custom header).
///
/// ## Safety
/// The length and never change (and be valid for
/// the corresponding allocation).
///
/// The underlying 'repr' is responsible
/// for dropping memory as appropriate.
pub unsafe trait GcArrayRepr<'gc, T>: Copy + sealed::Sealed {
    /// The repr's collector
    type Id: CollectorId;
    /// The "kind" of the array (whether fat or thin)
    ///
    /// This is necessary to correctly
    /// transmute in a const-fn context
    const UNCHECKED_KIND: ArrayReprKind;
    /// Construct an array representation from a combination
    /// of a pointer and length.
    ///
    /// This is the garbage collected equivalent of [std::slice::from_raw_parts]
    ///
    /// ## Safety
    /// The combination of pointer + length must be valid. 
    unsafe fn from_raw_parts(ptr: NonNull<T>, len: usize) -> Self;
    /// Convert the value to a slice
    fn as_slice(&self) -> &'gc [T];
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
pub struct FatArrayRepr<'gc, T: 'gc, Id: CollectorId> {
    slice: NonNull<[T]>,
    marker: PhantomData<Gc<'gc, [T], Id>>
}
impl<'gc, T, Id: CollectorId> self::sealed::Sealed for FatArrayRepr<'gc, T, Id> {}
impl<'gc, T, Id: CollectorId> FatArrayRepr<'gc, T, Id> {
    /// Get the length of this fat array (stored inline)
    #[inline]
    pub const fn len(&self) -> usize {
        unsafe { (&*self.slice.as_ptr()).len() }
    }
}
impl<'gc, T, Id: CollectorId> Copy for FatArrayRepr<'gc, T, Id> {}
impl<'gc, T, Id: CollectorId> Clone for FatArrayRepr<'gc, T, Id> {
    #[inline]
    fn clone(&self) -> Self {
        *self
    }
}

unsafe impl<'gc, T, Id: CollectorId> GcArrayRepr<'gc, T> for FatArrayRepr<'gc, T, Id> {
    type Id = Id;
    const UNCHECKED_KIND: ArrayReprKind = ArrayReprKind::Fat;

    #[inline]
    unsafe fn from_raw_parts(ptr: NonNull<T>, len: usize) -> Self {
        FatArrayRepr {
            slice: NonNull::new_unchecked(
                core::ptr::slice_from_raw_parts(
                    ptr.as_ptr() as *const T, len
                ) as *mut [T]
            ),
            marker: PhantomData
        }
    }

    #[inline]
    fn as_slice(&self) -> &'gc [T] {
        unsafe { &*self.slice.as_ptr() }
    }

    #[inline]
    fn as_raw_ptr(&self) -> *mut T {
        self.slice.as_ptr() as *mut T
    }

    #[inline]
    fn len(&self) -> usize {
        self.len()
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
pub struct ThinArrayRepr<'gc, T: 'gc, Id: CollectorId> {
    elements: NonNull<T>,
    marker: PhantomData<Gc<'gc, [T], Id>>    
}
impl<'gc, T, Id: CollectorId> Copy for ThinArrayRepr<'gc, T, Id> {}
impl<'gc, T, Id: CollectorId> Clone for ThinArrayRepr<'gc, T, Id> {
    #[inline]
    fn clone(&self) -> Self {
        *self
    }
}
impl<'gc, T, Id: CollectorId> self::sealed::Sealed for ThinArrayRepr<'gc, T, Id> {}
unsafe impl<'gc, T, Id: CollectorId> GcArrayRepr<'gc, T> for ThinArrayRepr<'gc, T, Id> {
    type Id = Id;
    const UNCHECKED_KIND: ArrayReprKind = ArrayReprKind::Thin;
    #[inline]
    unsafe fn from_raw_parts(ptr: NonNull<T>, len: usize) -> Self {
        let res = ThinArrayRepr { elements: ptr, marker: PhantomData };
        debug_assert_eq!(
            res.len(),
            len
        );
        res
    }
    #[inline]
    fn as_slice(&self) -> &'gc [T] {
        unsafe { core::slice::from_raw_parts(
            self.elements.as_ptr(),
            self.len()
        ) }
    }
    #[inline]
    fn as_raw_ptr(&self) -> *mut T {
        self.elements.as_ptr()
    }
    #[inline]
    fn len(&self) -> usize {
        unsafe { Id::resolve_array_len(
            &*(self as *const Self
                as *const Id::ArrayRepr<'gc, T>)
        ) }
    }
}

mod sealed {
    pub trait Sealed {}
}