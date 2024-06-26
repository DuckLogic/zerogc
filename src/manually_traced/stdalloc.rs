//! Implementations for types in the standard `alloc` crate
//!
//! These can be used in `#![no_std]` crates without requiring
//! the entire standard library.
#[cfg(not(feature = "std"))]
use alloc::boxed::Box;
use alloc::rc::Rc;
#[cfg(not(feature = "std"))]
use alloc::string::String;
use alloc::sync::Arc;
#[cfg(not(feature = "std"))]
use alloc::vec::Vec;

use crate::prelude::*;

use zerogc_derive::unsafe_gc_impl;

unsafe_gc_impl! {
    target => Vec<T>,
    params => [T],
    null_trace => { where T: NullTrace },
    NEEDS_TRACE => T::NEEDS_TRACE,
    NEEDS_DROP => true, // Internal memory
    collector_id => *,
    trace_template => |self, visitor| {
        // Delegate to slice
        visitor.#trace_func::<[T]>(#b**self as #b [T])
    },
    deserialize => unstable_horrible_hack,
}
unsafe_gc_impl! {
    target => Box<T>,
    params => [T],
    null_trace => { where T: NullTrace },
    NEEDS_TRACE => T::NEEDS_TRACE,
    NEEDS_DROP => true, // Internal memory
    collector_id => *,
    trace_template => |self, visitor| {
        visitor.#trace_func::<T>(#b **self)
    },
    deserialize => unstable_horrible_hack,
}
// We can only trace `Rc` and `Arc` if the inner type implements `TraceImmutable`
unsafe_gc_impl! {
    target => Rc<T>,
    params => [T: TraceImmutable],
    null_trace => { where T: NullTrace },
    NEEDS_TRACE => T::NEEDS_TRACE,
    NEEDS_DROP => true, // Internal memory
    collector_id => *,
    trace_template => |self, visitor| {
        // We must always visit immutable, since we have shared references
        visitor.trace_immutable::<T>(&**self)
    },
}
unsafe_gc_impl! {
    target => Arc<T>,
    params => [T: TraceImmutable],
    null_trace => { where T: NullTrace },
    NEEDS_TRACE => T::NEEDS_TRACE,
    NEEDS_DROP => true, // Internal memory
    collector_id => *,
    trace_template => |self, visitor| {
        // We must always visit immutable, since we have shared references
        visitor.trace_immutable::<T>(&**self)
    },
}
// String is a primitive with no internal references
unsafe_trace_primitive!(String);
