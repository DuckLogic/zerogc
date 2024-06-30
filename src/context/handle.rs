//! Defines the [`GcSyncHandle`] and [`GcLocalHandle`] types.

use crate::system::CollectorIdSync;
use crate::{CollectorId, NullTrace, Trace};
use core::fmt::{self, Debug};
use core::marker::PhantomData;
use zerogc_derive::unsafe_gc_impl;

/// The underlying implementation of a [`GcHandle`]
///
/// ## Safety
/// These types must be implemented correctly,
/// and preserve values across collections.
pub unsafe trait GcHandleImpl<T: Trace>: NullTrace + 'static {
    /// The id of the collector
    type Id: CollectorId;

    /// Retrieve the underlying object header from this handle.
    ///
    /// ## Safety
    /// The header cannot be used while collections are occurring.
    ///
    /// The underlying object must actually be a regular object (not an array).
    unsafe fn resolve_regular_header(&self) -> <Self::Id as CollectorId>::RegularHeader;

    /// Create a duplicate of this handle
    ///
    /// This is roughly equivalent to [`Clone::clone`] for a [`Arc`](std::sync::Arc).
    fn duplicate(&self) -> Self;
}

macro_rules! common_handle_impl {
    ($target:ident, Id: $bound:ident) => {
        impl<T: Trace, Id: $bound> $target<[T], Id> {

        }
        impl<T: Trace + ?Sized, Id: $bound> Clone for $target<T, Id> {
            #[inline]
            fn clone(&self) -> Self {
                $target::from_impl(self.value.duplicate())
            }
        }
        impl<T: Trace + ?Sized, Id: $bound> Debug for $target {
            fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.debug_struct(stringify!($target))
                    .finish_non_exhaustive()
            }
        }
        unsafe_gc_impl!(
            target => $target<T, ImplId>,
            params => [T: Trace + ?Sized, ImplId: $bound],
            null_trace => always,
            NEEDS_TRACE => false,
            NEEDS_DROP => core::mem::needs_drop::<$target>(),
            collector_id => *,
            trace_template => |self, visitor| { /* nop */ Ok(()) },
        );
    };
}

/// A thread-safe handle to a garbage collected value.
///
/// This is analogous to a [`Arc`](std::sync::Arc)
pub struct GcSyncHandle<T: Trace + ?Sized, Id: CollectorIdSync> {
    value: Id::SyncHandle<T>,
}
common_handle_impl!(GcSyncHandle, Id: CollectorIdSync);
impl<T: Trace + ?Sized, Id: CollectorIdSync> GcSyncHandle<T, Id> {
    #[inline]
    pub(crate) fn from_impl(value: Id::SyncHandle<T>) -> Self {
        GcSyncHandle { value }
    }
}
/// Implements `Sync` as long as `T: Sync`
unsafe impl<T: Sync + Trace + ?Sized, Id: CollectorIdSync> Sync for GcSyncHandle<T, Id> {}
/// Implements `Send` as long as `T: Sync`
///
/// The reason for requiring `T: Sync` is the same as [`Arc`](std::sync::Arc),
/// because these references can be duplicated.
unsafe impl<T: Sync + Trace + ?Sized, Id: CollectorIdSync> Send for GcSyncHandle<T, Id> {}

/// A thread-local handle to a garbage-collected value.
///
/// This is analogous to a [`Rc`](std::rc::Rc), and cannot be sent across threads (is `!Send`).
/// However, it is often faster to create and clone.
pub struct GcLocalHandle<T: ?Sized, Id: CollectorId> {
    value: Id::LocalHandle<T>,
    /// Indicates the type is never `Send` or `Sync`
    marker: PhantomData<*mut T>,
}
common_handle_impl!(GcLocalHandle, Id: CollectorId);
impl<T: ?Sized, Id: CollectorId> GcLocalHandle<T, Id> {
    #[inline]
    pub(crate) fn from_impl(value: Id::SyncHandle<T>) -> Self {
        GcLocalHandle {
            value,
            marker: PhantomData,
        }
    }
}
