//! Implementations for types in the standard `alloc` crate
//!
//! These can be used in `#![no_std]` crates without requiring
//! the entire standard library.
use alloc::rc::Rc;
use alloc::sync::Arc;
use alloc::vec::Vec;
use alloc::boxed::Box;
use alloc::string::String;

use crate::prelude::*;

// NOTE: Delegate to slice to avoid code duplication
unsafe_trace_deref!(Vec, target = { [T] }; T);
unsafe_impl_gc! {
    target => Vec<T>,
    params => [T],
    null_trace => { where T: NullTrace },
    NEEDS_TRACE => T::NEEDS_TRACE,
    NEEDS_DROP => true, // Internal memory
    visit => |$visit:expr, $target:ident| {
        // Delegate to slice
        visit(**$target as [T]);
    }
}
unsafe_impl_gc! {
    target => Box<T>,
    params => [T],
    null_trace => { where T: NullTrace },
    NEEDS_TRACE => T::NEEDS_TRACE,
    NEEDS_DROP => true, // Internal memory
    visit => |$visit:expr, $target:ident| {
        // Delegate to slice
        visit(**$target);
    }
}
unsafe_trace_deref!(Box, target = T);
// We can only trace `Rc` and `Arc` if the inner type implements `TraceImmutable`
unsafe_trace_deref!(Rc, T; immut = required; |rc| &**rc);
unsafe_trace_deref!(Arc, T; immut = required; |arc| &**arc);
// String is a primitive with no internal references
unsafe_trace_primitive!(String);
