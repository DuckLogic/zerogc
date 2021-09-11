//! Adds a `GcError` type,
//! which implements `std::error::Error + 'static`
//! for garbage collected error types.
//!
//! This allows for easy compatibility with [`anyhow`](https://docs.rs/anyhow/1.0.43/anyhow/)
//! even if you want to use garbage collected data in your errors
//! (which would otherwise require a `'gc` lifetime).
//!
//! The implementation doesn't require any unsafe code.
//! It's simply a thin wrapper around [GcHandle] that uses
//! [GcHandle::use_critical] section to block
//! garbage collection during formatting calls...

use std::fmt::{self, Formatter, Display, Debug};
use std::error::Error as StdError;

use crate::DynTrace;
use crate::prelude::*;

/// A garbage collected [`std::error::Error`] type
///
/// This is automatically implemented for all types that
/// 1. Implement [`std::error::Error`]
/// 2. Implement [GcSafe]
/// 3. Implement [`GcRebrand<'static, Id>`](`crate::GcRebrand`)
/// 4. Have no other lifetimes besides `'gc`
///
/// The fourth point is rather subtle.
/// Another way of saying it is that `T: 'gc` and `T::Branded: 'static`.
pub trait GcErrorType<'gc, Id: CollectorId>: StdError + GcSafe<'gc, Id> + 'gc + GcRebrand<'static, Id>
    + self::sealed::Sealed<Id>
    where <Self as GcRebrand<'static, Id>>::Branded: 'static {}
impl<'gc, Id: CollectorId, T: StdError + 'gc + GcSafe<'gc, Id> + GcRebrand<'static, Id>> GcErrorType<'gc, Id> for T
    where <Self as GcRebrand<'static, Id>>::Branded: 'static {}
impl<'gc, Id: CollectorId, T: StdError + 'gc + GcSafe<'gc, Id> + GcRebrand<'static, Id>> self::sealed::Sealed<Id> for T
    where <Self as GcRebrand<'static, Id>>::Branded: 'static {}


/// A super-trait of [GcErrorType]
/// that is suitable for dynamic dispatch.
///
/// This can only actually implemented
/// if the type is a `GcErrorType` at runtime.
///
/// This is an implementation detail
#[doc(hidden)]
pub trait DynGcErrorType<'gc, Id: CollectorId>: StdError + DynTrace<'gc, Id> + self::sealed::Sealed<Id> {}
impl<'gc, T: GcErrorType<'gc, Id>, Id: CollectorId> DynGcErrorType<'gc, Id> for T
    where <T as GcRebrand<'static, Id>>::Branded: 'static {}


crate::trait_object_trace!(
    impl<'gc, Id> Trace for dyn DynGcErrorType<'gc, Id> { where Id: CollectorId };
    Branded<'new_gc> => (dyn DynGcErrorType<'new_gc, Id> + 'new_gc),
    collector_id => Id,
    gc_lifetime => 'gc
);

/// A wrapper around a dynamically dispatched,
/// [std::error::Error] with garbage collected data.
///
/// The internal `'gc` lifetime has been erased,
/// by wrapping it in a [GcHandle].
/// Because the internal lifetime has been erased,
/// the type can be safely
///
/// This is analogous to [`anyhow::Error`](https://docs.rs/anyhow/1.0.43/anyhow/struct.Error.html)
/// but only for garbage collected .
pub struct GcError<Id: HandleCollectorId> {
    handle: Box<Id::Handle<dyn DynGcErrorType<'static, Id>>>
}
impl<Id: HandleCollectorId> GcError<Id> {
    /// Allocate a new dynamically dispatched [GcError]
    /// by converting from a specified `Gc` object.
    ///
    /// A easier, simpler and recommended alternative
    /// is [GcSimpleAlloc::alloc_error].
    #[cold]
    pub fn from_gc_allocated<'gc, T: GcErrorType<'gc, Id> + 'gc>(gc: Gc<'gc, T, Id>) -> Self
        where <T as GcRebrand<'static, Id>>::Branded: 'static,  {
        let dynamic = gc as Gc<'gc, dyn DynGcErrorType<'gc, Id>, Id>;
        GcError {
            handle: Box::new(Gc::create_handle(&dynamic))
        }
    }
}
impl<Id: HandleCollectorId> Display for GcError<Id> {
    fn fmt(&self, f: &mut Formatter) -> fmt::Result {
        self.handle.use_critical(|err| {
            Display::fmt(&err, f)
        })
    }
}
impl<Id: HandleCollectorId> Debug for GcError<Id> {
    fn fmt(&self, f: &mut Formatter) -> fmt::Result {
        self.handle.use_critical(|err| {
            Debug::fmt(&err, f)
        })
    }
}
impl<Id: HandleCollectorId> StdError for GcError<Id> {
    /*
     * TODO: We can't give 'source' and 'backtrace'
     * because they borrow for `'self`
     * and it is possible a moving garbage
     * collector could relocate
     * internal data
     */
}

mod sealed {
    pub trait Sealed<Id: zerogc::CollectorId> {}
}
