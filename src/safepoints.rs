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
use std::{ptr};
use std::mem::ManuallyDrop;
use super::{CollectorId, GarbageCollected};
use std::fmt::{self, Debug, Formatter};
use std::marker::PhantomData;

/// The universally unique identifier of a `SafepointBag`.
///
/// This is made unique against not just this collector,
/// but all collectors by combining this with a `CollectorId`.
/// We count the ids carefully and verify there's never overflows or duplicates,
/// Therefore unsafe collector code can and does rely on this being unique for each process.
///
/// This allows us to guard against people using safepoint bag when they're not supposed to,
/// as we'll verify that the id is correct and it has the correct state.
/// This also allows us to verify that a safepoint has been completely finished before we allow a new one,
/// since we only want to allow one safepoint (and one set of roots) to be considered active at a time.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct SafepointId {
    pub(crate) collector: CollectorId,
    pub(crate) id: u64,
}

/// Carries a set of garbage collected value `T` across a safepoint,
/// by treating it as the roots of potential garbage collection.
///
/// All objects that need to survive the safepoint and are put in a `SafepointBag`,
/// and will be traced the garbage collector if a collection occurs at the safepoint.
/// The borrow checker will consider all other garbage collected pointers after
/// the safepoint invalid (since it's a mutation),
/// and only the objects explicitly sent across the safepoint will remain.
///
/// This is intentional, as the only values that can survive a safepoint
/// are the ones explicitly passed to it.
/// In order for the objects safely pass through the safepoint,
/// the lifetime of the garbage collected pointers is changed to `'static`,
/// tricking the borrow checker into thinking the garbage collected pointers are always valid.
/// After the safepoint is finished, we restore them to the previous value.
/// All of this should happen with no overhead, since they're just `mem::transmute`s.
///
/// The debug implementation of this type is safe,
/// and ensures we never attempt to debug types that are potentially invalid .
///
/// ## Safety
/// All unchecked assumptions are encapsulated and hidden from the user as implementation details,
/// and it's impossible to break them in safe code due to either runtime checks,
/// rust ownership rules, or rust encapsulation and unsafe.
///
/// The collector has maximum freedom to put the value in a temporarily invalid sate,
/// as long as it's corrected after the collection is finished.
/// This is because we `ManuallyDrop` the underlying value,
/// and it can only be recovered after potential collections have been safely finished.
/// Attempting to recover the value before collection is finished is undefined behavior,
/// and the collector can optimize based on this assumption.
///
/// States are only permitted to advance directly from one to the next,
/// or undefined behavior will occur.
/// This is dynamically checked by ,
/// so incorrectly using the safepoint behavior on.
#[must_use]
#[doc(hidden)]
pub struct SafepointBag<'unm, T: GarbageCollected + 'unm> {
    /// The unique id of the safepoint,
    /// which is used to ensure that this safepoint bag is current.
    pub(crate) id: SafepointId,
    /// The raw memory of the bag's underlying value.
    ///
    /// This is manually dropped, since we may panic when collection is in progress.
    /// We want to give the collector maximum freedom to modify the state of the values,
    /// so we simply never run the destructor until the bag and safepoint are finished.
    /// However, instead of giving the collector complete ownership of the value,
    /// we simply provide it a `&mut T` reference to avoid excessive moving of values.
    /// Collection is considered the cold path and should never be inlined,
    /// so it's currently impossible for LLVM to optimize away the moves because of the ABI.
    ///
    /// Once the collection and safepoint are over it's safe to continue,
    /// the user can explicitly recover the value with `finish_safepoint` at the proper time.
    /// This is the only way the value can be safely recovered by the user,
    /// and any other way will leak the value without dropping it.
    pub(crate) value: ManuallyDrop<T>,
    /// The dynamically tracked state of this bag,
    /// in order to ensure collections only happen once per safepoint.
    pub(crate) state: SafepointState,
    /// This magical marker makes our lifetime invariant.
    ///
    /// This whole bleeping thing is so confusing I just wish we could explicitly express our intent.
    pub(crate) marker: PhantomData<*mut &'unm ()>,
}
impl<'unm, T: GarbageCollected + 'unm> SafepointBag<'unm, T> {
    /// Give the globally unique id of this safepoint.
    #[inline]
    pub fn id(&self) -> SafepointId {
        self.id
    }
    #[inline]
    pub fn begin_collection(&mut self) -> &mut T {
        self.state.update_state(SafepointState::Initialized, SafepointState::CollectionInProgress);
        &mut *self.value
    }
    #[inline]
    pub fn finish_collection(&mut self) {
        self.state.update_state(SafepointState::CollectionInProgress, SafepointState::Completed);
    }
    #[inline]
    pub fn ignore_collection(&mut self) {
        self.state.update_state(SafepointState::Initialized, SafepointState::Completed);
    }
    /// Consumes ownership of the value,
    /// if and only if the safepoint's already been completed.
    ///
    /// This is the only way to regain ownership of the value without leaking.
    ///
    /// ## Safety
    /// It is safe to give the user a copy of the value's ownership with a mutable reference,
    /// since the state ensures this will only happen once and never occur again.
    ///
    /// Additionally is the value is always forgotten by a `ManuallyDrop`,
    /// so a panic could never cause us to drop the value.
    /// Therefore, the value can safely have two physical owners,
    /// since the one in the safepoint is never used again.
    #[inline]
    pub fn consume(&mut self) -> T {
        self.state.update_state(SafepointState::Completed, SafepointState::Consumed);
        unsafe {
            ptr::read(&*self.value)
        }
    }
}
impl<'a, T: GarbageCollected> Debug for SafepointBag<'a, T> {
    fn fmt(&self, f: &mut Formatter) -> fmt::Result {
        f.debug_struct("SafepointBag")
            .field("id", &self.id)
            .field("value", &format_args!("ManuallyDrop"))
            .field("state", &self.state)
            .finish()
    }
}
/// The state of a `SafepointBag` that is currently in progress,
///
/// These have varying states of ownership and control over the `SafepointBag`s roots,
/// with the ultimate intent of returning control over the garbage collected roots back to the user.
/// If everything goes fine, the user will successfully `finish_safepoint` and we're home free.
/// However, we need to panic if the safepoint is dropped during collection,
/// since it'd be undefined behavior to use .
///
/// We need to be robust in the face of the user to calling any combination of the safepoint methods,
/// with the safepoint in any possible state.
/// Only one of these states is correct at any given time,
/// and the user can only use the ever `activate`.
/// All other combinations must either panic or poison the collector,
/// since we've marked every method in the API as safe and I take that very seriously.
#[derive(Copy, Clone, PartialEq, PartialOrd, Ord, Eq, Debug)]
pub(crate) enum SafepointState {
    /// The `SafepointBag` has been initialized to the roots of garbage collection,
    /// and is ready for activation and being sent across the safepoint.
    ///
    /// The only valid operation for this safepoint is activation with `activate_safepoint`.
    Initialized,
    /// Garbage collection is currently in progress for this safepoint,
    /// and the bag's data is being used as the roots.
    ///
    /// They could be compacting it or tracing it or god only knows,
    /// so we just simply decide 'the collectors in charge'.
    /// We can only transition out of this state after successful garbage collection,
    /// and once the collector hands us control back over the data.
    CollectionInProgress,
    /// The `SafepointBag` has already survived the safepoint,
    /// and any garbage collection that occurred has already finished.
    ///
    /// At this point we're almost finished and are the 'logical' owner of the data once again,
    /// even though the underlying memory hasn't been freed.
    /// We're effectively waiting for the user to 'consume' the safepoint
    /// and become the actual owner once again.
    Completed,
    /// The safepoint has been finished and consumed,
    /// and all of its data has been successfully recovered.
    ///
    /// At this point the user has consumed the data,
    /// and although it's no longer logically owned,
    /// we still physically own the value.
    /// This means there are two seperate physical copies of the data,
    /// highlighting the importance of being careful when tracking logical ownership.
    ///
    /// Only once we reach this state is it actually valid to consider this safepoint over,
    /// and only then can we begin to start working on another one.
    Consumed
}
impl SafepointState {
    #[inline]
    pub(crate) fn check_state(self, expected: SafepointState) {
        assert_eq!(self, expected, "Unexpected safepoint state");
    }
    #[inline]
    pub(crate) fn update_state(&mut self, expected: SafepointState, updated: SafepointState) {
        self.check_state(expected);
        *self = updated;
    }
}

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
        unsafe impl<'unm> $crate::safepoints::GcErase<'unm> for $target {
            type Erased = $target;

            #[inline]
            unsafe fn erase(self) -> Self::Erased {
                ::std::mem::transmute(self)
            }
        }
        unsafe impl<'unm: 'gc, 'gc> $crate::safepoints::GcUnErase<'unm, 'gc> for $target {
            type Corrected = $target;

            #[inline]
            unsafe fn unerase(self) -> Self::Corrected {
                ::std::mem::transmute(self)
            }
        }
    };
    ($target:ident, $($param:ident),+) => {
        unsafe impl<'unm, $($param),*> $crate::safepoints::GcErase<'unm> for $target<$($param),*>
            where $($param: $crate::safepoints::GcErase<'unm>),* {
            type Erased = $target<$($param::Erased),*>;

            #[inline]
            unsafe fn erase(self) -> Self::Erased {
                let erased = ::std::mem::transmute_copy(&self);
                std::mem::forget(self);
                erased
            }
        }
        unsafe impl<'unm: 'gc, 'gc, $($param),*> $crate::safepoints::GcUnErase<'unm, 'gc> for $target<$($param),*>
            where $($param: $crate::GarbageCollected + $crate::safepoints::GcUnErase<'unm, 'gc>),* {
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
pub unsafe trait GcErase<'unm>: GarbageCollected {
    type Erased: GarbageCollected + 'unm;
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
pub unsafe trait GcUnErase<'unm, 'gc>: 'unm where 'unm: 'gc {
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