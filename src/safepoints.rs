//! The dark magic that allows garbage collected pointers to survive a safepoint,
//! and temporarily bypass the borrow checker.
//!
//! The code is separated into a seperate module, since it's so disgusting and unsafe.
//! It is recommended that you don't use these types and methods manually,
//! and instead use the `safepoint!` and `safepoint_vars!` macros instead.
//! These macros eliminate much of the boilerplate,
//! and help you avoid using the hacky (but safe interface).
//!
//! Remember, the primary goal of this safepoint interface is to be completely safe,
//! and ensure that the garbage collector considers all other garbage collected pointers invalid
//! that haven't been explicitly carried across the safepoint.
use super::{GarbageCollected};

/// Unsafely assume that the generic implementation of `GcErase` is valid,
/// if and only if it's valid for the generic lifetime and type parameters.
///
/// Always _prefer automatically derived implementations where possible_,
/// since they can never cause undefined behavior.
/// This macro is only nessicarry if you have raw pointers internally,
/// which can't have automatically derived safe implementations.
/// This is basically an _unsafe automatically derived_ implementation,
/// to be used only when a safe automatically derived implementation isn't possible (like with `Vec`).
///
/// This macro takes a varying number of parameters referring to the type's generic parameters,
/// which are all properly bounded and required to implement `GcErase` correctly.
///
/// This macro can only cause undefined behavior if there are garbage collected pointers
/// that aren't included in the type parameter.
/// For example including `Gc<u32>` would be completely undefined behavior,
/// since we'd blindly erase its lifetime.
///
/// However, almost 99% of the time this macro provides the correct implementation,
/// so you almost always be using it if your type can be garbage collected.
/// Currently the only exception is when you have garbage collected lifetimes like `Gc`.
#[macro_export]
macro_rules! unsafe_erase {
    ($target:tt) => {
        unsafe impl $crate::safepoints::GcErase for $target {
            type Erased = $target;

            #[inline]
            unsafe fn erase(self) -> Self::Erased {
                ::std::mem::transmute(self)
            }
        }
        unsafe impl<'gc> $crate::safepoints::GcUnErase<'gc> for $target {
            type Corrected = $target;

            #[inline]
            unsafe fn unerase(self) -> Self::Corrected {
                ::std::mem::transmute(self)
            }
        }
    };
    ($target:ident, $($param:ident),+) => {
        unsafe impl<$($param),*> $crate::safepoints::GcErase for $target<$($param),*>
            where $($param: $crate::safepoints::GcErase),* {
            type Erased = $target<$($param::Erased),*>;

            #[inline]
            unsafe fn erase(self) -> Self::Erased {
                let erased = ::std::mem::transmute_copy(&self);
                std::mem::forget(self);
                erased
            }
        }
        unsafe impl<'gc, $($param),*> $crate::safepoints::GcUnErase<'gc> for $target<$($param),*>
            where $($param: $crate::GarbageCollected + $crate::safepoints::GcUnErase<'gc>),* {
            type Corrected = $target<$($param::Corrected),*>;

            #[inline]
            unsafe fn unerase(self) -> Self::Corrected {
                let unerased = ::std::mem::transmute_copy(&self);
                std::mem::forget(self);
                unerased
            }
        }
    };
}

/// Indicates that the lifetime of all garbage collected pointers in a type can be erased,
/// so they can be put in a `SafepointBag`.
///
/// Even after unsafely bypassing the lifetime guarantees of garbage collected pointers,
/// the lifetime of unmanaged references must remain unchanged after being erased.
///
/// For example `(&'a String, Gc<'g, String>)` can implement `GcErase<'a>` since `&'a String: 'unm`,
/// allowing the compiler to verify that `Self: 'unm`.
/// We can't just erase the `&'a String` lifetime and bring it back (like we do with the `Gc<'g, String>`),
/// since it's not managed memory and we know nothing about it.
pub unsafe trait GcErase: GarbageCollected {
    type Erased: GarbageCollected;
    /// Erase all garbage collected lifetimes in the specified object,
    /// extending them to `'static`, while keeping the managed lifetimes untouched.
    ///
    /// The implementation of this method
    /// is _required_ to be equivelant to `mem::transmute`
    /// or undefined behavior will result.
    /// This is so containers simply transmute things and cast pointers,
    /// no questions asked.
    /// In other words, if `erase` was allowed to have behavior beyond `mem::transmute`,
    /// then transmuting a `Vec<Crazy>` to `Vec<CrazyErased>`
    /// wouldn't be correct since there might be arbitrary code to execute for each element.
    ///
    /// The reason we can't just transmute the types in the first place,
    /// is because of the stupid lint that tells me we that `mem::transmute::<Self, Self::Erased>`
    /// might be invalid because the types aren't nessciarrly the same size.
    unsafe fn erase(self) -> Self::Erased;
}

/// Indicates that a type's lifetime can be recreated from its erased form.
///
/// This trait is the opposite of `GcErase`
/// and corrects all garbage collected lifetimes from `'static` back to `'gc`.
pub unsafe trait GcUnErase<'gc>: 'gc {
    type Corrected: GarbageCollected + 'gc;
    /// Correct the lifetimes of all garbage collected pointers,
    /// transmuting their lifetimes from '`static` back to '`gc`,
    /// while keeping unmanaged lifetimes untouched.
    ///
    /// The implementation of this method
    /// is _required_ to be equivelant to `mem::transmute`
    /// or undefined behavior will result.
    /// This is so containers simply transmute things and cast pointers,
    /// no questions asked.
    /// In other words, if `erase` was allowed to have behavior beyond `mem::transmute`,
    /// then transmuting a `Vec<Crazy>` to `Vec<CrazyErased>`
    /// wouldn't be correct since there might be arbitrary code to execute for each element.
    unsafe fn unerase(self) -> Self::Corrected;
}