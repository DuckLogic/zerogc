//! Tracing implementations for the standard library
//!
//! Types that are in `libcore` and are `#![no_std]` should go in the core module,
//! but anything that requires the rest of the stdlib (including collections and allocations),
//! should go in this module.
use std::collections::{HashMap, HashSet};

use zerogc_derive::unsafe_gc_impl;

use crate::prelude::*;


unsafe_gc_impl! {
    target => HashMap<K, V, S>,
    params => [K: TraceImmutable, V, S: 'static],
    bounds => {
        /*
         * We require S: 'static so that we know S: NullTrace
         */
        Trace => { where K: TraceImmutable, V: Trace, S: 'static },
        TraceImmutable => { where K: TraceImmutable, V: TraceImmutable, S: 'static },
        TrustedDrop => { where K: TrustedDrop, V: TrustedDrop, S: 'static },
        GcSafe => { where K: TraceImmutable + GcSafe<'gc, Id>, V: GcSafe<'gc, Id>, S: 'static },
    },
    null_trace => { where K: NullTrace, V: NullTrace, S: NullTrace },
    NEEDS_TRACE => K::NEEDS_TRACE || V::NEEDS_TRACE,
    NEEDS_DROP => true, // Internal memory
    collector_id => *,
    trace_template => |self, visitor| {
        for (key, value) in self.#iter() {
            visitor.trace_immutable::<K>(key)?;
            visitor.#trace_func::<V>(value)?;
        }
        // NOTE: Because S: 'static, we can assume S: NullTrace
        Ok(())
    },
}


unsafe_gc_impl! {
    target => HashSet<T, S>,
    params => [T: TraceImmutable, S: 'static],
    bounds => {
        /*
         * We require S: 'static so that we know S: NullTrace
         */
        Trace => { where T: TraceImmutable, S: 'static },
        TraceImmutable => { where T: TraceImmutable, S: 'static },
        TrustedDrop => { where T: TrustedDrop, S: 'static },
        GcSafe => { where T: TraceImmutable + GcSafe<'gc, Id>, S: 'static },
    },
    null_trace => { where T: NullTrace, S: 'static },
    NEEDS_TRACE => T::NEEDS_TRACE,
    NEEDS_DROP => true, // Internal memory
    collector_id => *,
    trace_template => |self, visitor| {
        for val in self.iter() {
            visitor.trace_immutable::<T>(val)?;
        }       
        // NOTE: Because S: 'static, we can assume S: NullTrace
        Ok(())
    },
}
