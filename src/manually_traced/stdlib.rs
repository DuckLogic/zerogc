//! Tracing implementations for the standard library
//!
//! Types that are in `libcore` and are `#![no_std]` should go in the core module,
//! but anything that requires the rest of the stdlib (including collections and allocations),
//! should go in this module.
use std::collections::{HashMap, HashSet};

use zerogc_derive::unsafe_gc_impl;

use crate::prelude::*;


unsafe_gc_impl! {
    target => HashMap<K, V>,
    params => [K: TraceImmutable, V],
    null_trace => { where K: NullTrace, V: NullTrace },
    NEEDS_TRACE => K::NEEDS_TRACE || V::NEEDS_TRACE,
    NEEDS_DROP => true, // Internal memory
    visit => |self, visitor| {
        for (key, value) in self.#iter() {
            visitor.visit_immutable::<K>(key)?;
            visitor.#visit_func::<V>(value)?;
        }
        Ok(())
    }
}


unsafe_gc_impl! {
    target => HashSet<T>,
    params => [T: TraceImmutable],
    null_trace => { where T: NullTrace },
    NEEDS_TRACE => T::NEEDS_TRACE,
    NEEDS_DROP => true, // Internal memory
    visit => |self, visitor| {
        for val in self.iter() {
            visitor.visit_immutable::<T>(val)?;
        }
        Ok(())
    }
}
