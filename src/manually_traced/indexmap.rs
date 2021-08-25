use indexmap::{IndexMap, IndexSet};

use crate::prelude::*;

use zerogc_derive::unsafe_gc_impl;

unsafe_gc_impl! {
    target => IndexMap<K, V>,
    params => [K: TraceImmutable, V],
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
    }
}


unsafe_gc_impl! {
    target => IndexSet<T>,
    params => [T: TraceImmutable],
    null_trace => { where T: NullTrace },
    NEEDS_TRACE => T::NEEDS_TRACE,
    NEEDS_DROP => true, // Internal memory
    collector_id => *,
    visit => |self, visitor| {
        for val in self.iter() {
            visitor.visit_immutable::<T>(val)?;
        }
        Ok(())
    }
}
