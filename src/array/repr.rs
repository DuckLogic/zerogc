//! Defines the underlying reprsention of a [GcArray] pointer.
//!
//!
//! Two default implementations are also included:
//! 1. FatArrayRepr - Represents arrays as a fat pointer
//! 2. ThinArrayRepr - Represents arrays as a thin pointer,
//!    with the length stored indirectly in the object header.
use core::marker::PhantomData;
use core::ptr::NonNull;


use crate::{CollectorId, Gc};


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
pub unsafe trait GcArrayRepr<'gc, T>: Copy {
    /// The repr's collector
    type Id: CollectorId;
    /// Construct an array representation from a combination
    /// of a pointer and length.
    ///
    /// This is the garbage collected equivalent of [std::slice::from_raw_parts]
    ///
    /// ## Safety
    /// The combination of pointer + length must be valid. 
    unsafe fn from_raw_parts(ptr: NonNull<T>, len: usize) -> Self;
    /// Get the length of the array
    fn len(&self) -> usize;
    /// Check if the array is empty
    #[inline]
    fn is_empty(&self) -> bool {
        self.len() == 0
    }
    /// Load a raw pointer to the array's value
    fn as_raw_ptr(&self) -> *mut T;
    /// Get the value of the array as a slice
    #[inline]
    fn as_slice(&self) -> &'gc [T] {
        // SAFETY: Guaranteed by the unsafety of the trait
        unsafe {
            std::slice::from_raw_parts(
                self.as_raw_ptr() as *const T,
                self.len()
            )
        }
    }
}

/// Represents an array as a fat pointer.
///
/// ## Safety
/// This type is guarenteed to be compatible with `&[T]`.
/// Transmuting back and forth is safe.
#[repr(transparent)]
pub struct FatArrayRepr<'gc, T: 'gc, Id: CollectorId> {
    slice: NonNull<[T]>,
    marker: PhantomData<Gc<'gc, [T], Id>>
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
    fn len(&self) -> usize {
        unsafe { self.slice.as_ref().len() }
    }

    #[inline]
    fn as_raw_ptr(&self) -> *mut T {
        self.slice.as_ptr() as *mut T
    }

    #[inline]
    fn as_slice(&self) -> &'gc [T] {
        unsafe { &*self.slice.as_ptr() }
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
pub struct ThinArrayRepr<'gc, T: 'gc, Id: ThinArrayGcAccess> {
    elements: NonNull<T>,
    marker: PhantomData<Gc<'gc, [T], Id>>    
}
impl<'gc, T, Id: ThinArrayGcAccess> Copy for ThinArrayRepr<'gc, T, Id> {}
impl<'gc, T, Id: ThinArrayGcAccess> Clone for ThinArrayRepr<'gc, T, Id> {
    #[inline]
    fn clone(&self) -> Self {
        *self
    }
}
unsafe impl<'gc, T, Id: ThinArrayGcAccess> GcArrayRepr<'gc, T> for ThinArrayRepr<'gc, T, Id> {
    type Id = Id;
    #[inline]
    fn len(&self) -> usize {
        Id::resolve_array_len(*self)
    }

    #[inline]
    unsafe fn from_raw_parts(ptr: NonNull<T>, len: usize) -> Self {
        let res = ThinArrayRepr { elements: ptr, marker: PhantomData };
        debug_assert_eq!(res.len(), len);
        res
    }

    #[inline]
    fn as_raw_ptr(&self) -> *mut T {
        self.elements.as_ptr()
    }
}

/// The raw array access used to implement []
///
/// This should be considered an implementation detail.
///
/// Usage of this type is incredibly niche unless you
/// plan on implementing your own collector.
///
/// ## Safety
/// The returned length must be correct.
#[doc(hidden)]
pub unsafe trait ThinArrayGcAccess: CollectorId {
    /// Resolve the length of the specified [GcArray]
    fn resolve_array_len<'gc, T>(repr: ThinArrayRepr<'gc, T, Self>) -> usize
        where T: 'gc;
}