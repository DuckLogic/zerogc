use std::hash::Hash;

use indexmap::{IndexMap, IndexSet};

#[cfg(feature="serde1")]
use crate::serde::GcDeserialize;
use crate::prelude::*;

use zerogc_derive::unsafe_gc_impl;

unsafe_gc_impl! {
    target => IndexMap<K, V>,
    params => [K: TraceImmutable, V],
    bounds => {
        GcDeserialize => { where K: GcDeserialize<'gc, 'deserialize, Id> + Eq + Hash, V: GcDeserialize<'gc, 'deserialize, Id> }
    },
    null_trace => { where K: NullTrace, V: NullTrace },
    NEEDS_TRACE => K::NEEDS_TRACE || V::NEEDS_TRACE,
    NEEDS_DROP => true, // Internal memory
    collector_id => *,
    visit => |self, visitor| {
        for (key, value) in self.#iter() {
            visitor.visit_immutable::<K>(key)?;
            visitor.#visit_func::<V>(value)?;
        }
        Ok(())
    },
    deserialize => unstable_horrible_hack,
}


unsafe_gc_impl! {
    target => IndexSet<T>,
    params => [T: TraceImmutable],
    bounds => {
        GcDeserialize => { where T: GcDeserialize<'gc, 'deserialize, Id> + Eq + Hash }
    }
    null_trace => { where T: NullTrace },
    NEEDS_TRACE => T::NEEDS_TRACE,
    NEEDS_DROP => true, // Internal memory
    collector_id => *,
    visit => |self, visitor| {
        for val in self.iter() {
            visitor.visit_immutable::<T>(val)?;
        }
        Ok(())
    },
    deserialize => unstable_horrible_hack,
}
