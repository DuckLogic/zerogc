use arrayvec::{ArrayString, ArrayVec};

use crate::{GcRebrand, NullTrace};

use zerogc_derive::unsafe_gc_impl;

unsafe_gc_impl!(
    target => ArrayString<SIZE>,
    params => [const SIZE: usize],
    null_trace => always,
    NEEDS_TRACE => false,
    NEEDS_DROP => false,
    branded_type => ArrayString<SIZE>,
    trace_template => |self, visitor| { Ok(()) },
);

unsafe_gc_impl!(
    target => ArrayVec<T, SIZE>,
    params => [T, const SIZE: usize],
    null_trace => { where T: NullTrace },
    NEEDS_TRACE => <T as Trace>::NEEDS_TRACE,
    NEEDS_DROP => <T as Trace>::NEEDS_DROP,
    bounds => {
        GcRebrand => { where T: GcRebrand<'new_gc, Id>, T::Branded: Sized },
    },
    branded_type => ArrayVec<T::Branded, SIZE>,
    trace_template => |self, visitor| {
        for val in self.#iter() {
            visitor.#trace_func(val)?;
        }
        Ok(())
    },
);
