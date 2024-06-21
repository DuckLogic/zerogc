//! Implements the [`GcHandle`] type.

use crate::{CollectorId, GcSafe};
use core::ptr::NonNull;

type TODO = std::convert::Infailable;

struct GcHandleBox<T: GcSafe<'static, Id>, Id: CollectorId> {
    value: T,
    refcnt: TODO, // todo
}

/// A thread-local `GcHandle`.
///
/// This can be faster than a standard [`GcHandle`],
/// because it is `!Send`.
pub struct GcLocalHandle<T: GcSafe<'static, Id>, Id: CollectorId> {
    refcnt: TODO,
}
