use indexmap::{IndexMap, IndexSet};

use crate::prelude::*;

use zerogc_derive::unsafe_gc_impl;

unsafe_gc_impl! {
    target => IndexMap<K, V, S>,
    params => [K, V, S: 'static],
    bounds => {
        Trace => { where K: Trace, V: Trace, S: 'static },
        TraceImmutable => { where K: TraceImmutable, V: TraceImmutable, S: 'static },
        TrustedDrop => { where K: TrustedDrop, V: TrustedDrop, S: 'static },
        GcSafe => { where K: GcSafe<'gc, Id>, V: GcSafe<'gc, Id>, S: 'static },
    },
    null_trace => { where K: NullTrace, V: NullTrace },
    NEEDS_TRACE => K::NEEDS_TRACE || V::NEEDS_TRACE,
    NEEDS_DROP => true, // Internal memory
    collector_id => *,
    trace_mut => |self, visitor| {
        for idx in 0..self.len() {
            let (key, value) = self.get_index_mut(idx).unwrap();
            visitor.trace::<K>(key)?;
            visitor.trace::<V>(value)?;
        }
        // NOTE: S: 'static implies S: NullTrace
        Ok(())
    },
    trace_immutable => |self, visitor| {
        for (key, value) in self.iter() {
            visitor.trace_immutable::<K>(key)?;
            visitor.trace_immutable::<V>(value)?;
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
    trace_template => |self, visitor| {
        for val in self.iter() {
            visitor.trace_immutable::<T>(val)?;
        }
        Ok(())
    },
}
