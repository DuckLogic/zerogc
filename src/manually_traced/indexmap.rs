use indexmap::{IndexMap, IndexSet};

use crate::prelude::*;

use zerogc_derive::unsafe_gc_impl;

unsafe_gc_impl! {
    target => IndexMap<K, V, S>,
    params => [K: TraceImmutable, V, S: 'static],
    bounds => {
        Trace => { where K: TraceImmutable, V: Trace, S: 'static },
        TraceImmutable => { where K: TraceImmutable, V: TraceImmutable, S: 'static },
        TrustedDrop => { where K: TrustedDrop, V: TrustedDrop, S: 'static },
        GcSafe => { where K: GcSafe<'gc, Id>, V: GcSafe<'gc, Id>, S: 'static },
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
        // NOTE: S: 'static implies S: NullTrace
        Ok(())
    },
}


unsafe_gc_impl! {
    target => IndexSet<T, S>,
    params => [T: TraceImmutable, S: 'static],
    null_trace => { where T: NullTrace },
    bounds => {
        Trace => { where T: TraceImmutable, S: 'static },
        TraceImmutable => { where T: TraceImmutable, S: 'static },
        TrustedDrop => { where T: TrustedDrop, S: 'static },
        GcSafe => { where T: GcSafe<'gc, Id>, S: 'static },
    },
    NEEDS_TRACE => T::NEEDS_TRACE,
    NEEDS_DROP => true, // Internal memory
    collector_id => *,
    visit => |self, visitor| {
        for val in self.iter() {
            visitor.visit_immutable::<T>(val)?;
        }
        Ok(())
    },
}
