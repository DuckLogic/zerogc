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
    deserialize => delegate
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
    deserialize => |ctx, deserializer| {
        use core::marker::PhantomData;
        use crate::CollectorId;
        use crate::serde::{GcDeserialize, GcDeserializeSeed};
        use serde::de::{Visitor, Error, SeqAccess};
        struct ArrayVecVisitor<
            'gc, 'de, Id: CollectorId,
            T: GcDeserialize<'gc, 'de, Id>,
            const SIZE: usize
        > {
            ctx: &'gc Id::Context,
            marker: PhantomData<fn(&'de ()) -> ArrayVec<T, SIZE>>
        }
        impl<
            'gc, 'de, Id: CollectorId,
            T: GcDeserialize<'gc, 'de, Id>,
            const SIZE: usize
        > Visitor<'de> for ArrayVecVisitor<'gc, 'de, Id, T, SIZE> {
            type Value = ArrayVec<T, SIZE>;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                write!(f, "a array with size <= {}", SIZE)
            }
            #[inline]
            fn visit_seq<A>(self, mut access: A) -> Result<Self::Value, A::Error>
                where A: SeqAccess<'de>, {
                let mut values = Self::Value::new();
                while let Some(value) = access.next_element_seed(
                    GcDeserializeSeed::new(self.ctx)
                )? {
                    match values.try_push(value) {
                        Ok(()) => {},
                        Err(_) => {
                            return Err(A::Error::invalid_length(SIZE + 1, &self))
                        }
                    }
                }
                Ok(values)
            }
        }
        let visitor: ArrayVecVisitor<Id, T, SIZE> = ArrayVecVisitor { ctx, marker: PhantomData };
        deserializer.deserialize_seq(visitor)
    }
);
