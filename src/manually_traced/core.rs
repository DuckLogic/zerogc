//! Implementations of `GarbageCollected` for the language's core types.
//!
//! This includes references, tuples, primitives, arrays, and everything else in `libcore`.
//!
//! `RefCell` and `Cell` require `T: NullTrace` and do not have implementations for other types.
//! This is because some collectors may need write barriers to protect their internals.
use core::num::Wrapping;
use core::marker::PhantomData;
use core::cell::{RefCell, Cell};

use crate::prelude::*;
use crate::GcDirectBarrier;

use zerogc_derive::unsafe_gc_impl;

macro_rules! trace_tuple {
    { $single_param:ident } => {
        trace_tuple_impl!();
        trace_tuple_impl!($single_param);
        #[cfg(feature = "serde1")]
        deser_tuple_impl!($single_param);
    };
    { $first_param:ident, $($param:ident),* } => {
        trace_tuple! { $($param),* }
        trace_tuple_impl!( $first_param, $($param),*);
        #[cfg(feature = "serde1")]
        deser_tuple_impl!($first_param, $($param),*);
    };
}

macro_rules! trace_tuple_impl {
    {  $($param:ident),* } => {
        unsafe_gc_impl! {
            target => ( $($param,)* ),
            params => [$($param),*],
            null_trace => { where $($param: NullTrace,)* i32: Sized },
            /*
             * HACK: Macros don't allow using `||` as separator,
             * so we use it as a terminator, causing there to be an illegal trailing `||`.
             * However, the redundant `||` is okay if we put a trailing false at the end,
             * since `a || false` is always `a`.
             * This also correctly handles the empty unit tuple by making it false
             */
            NEEDS_TRACE => $($param::NEEDS_TRACE || )* false,
            NEEDS_DROP => $($param::NEEDS_DROP || )* false,
            bounds => {
                GcRebrand => { where $($param: GcRebrand<'new_gc, Id>,)* $($param::Branded: Sized),* },
            },
            branded_type => ( $(<$param as GcRebrand<'new_gc, Id>>::Branded,)* ),
            trace_template => |self, visitor| {
                ##[allow(non_snake_case)]
                let ($(ref #mutability $param,)*) = *self;
                $(visitor.#trace_func($param)?;)*
                Ok(())
            },
            collector_id => *
        }
        unsafe impl<'gc, OwningRef, $($param),*> $crate::GcDirectBarrier<'gc, OwningRef> for ($($param,)*)
            where $($param: $crate::GcDirectBarrier<'gc, OwningRef>),* {
            #[inline]
            unsafe fn write_barrier(
                &self, #[allow(unused)] owner: &OwningRef,
                #[allow(unused)] start_offset: usize
            ) {
                /*
                 * We are implementing gc **direct** write.
                 * This is safe because all the tuple's values
                 * are stored inline. We calculate the pointer offset
                 * using arithmetic.
                 */
                #[allow(non_snake_case)]
                let ($(ref $param,)*) = *self;
                $({
                    let field_offset = ($param as *const $param as usize)
                        - (self as *const Self as usize);
                    $param.write_barrier(owner, field_offset + start_offset);
                };)*
            }
        }
    };
    { $first_param:ident, $($param:ident)* ; gc_impl => { $($impls:tt)* }} => {
        trace_tuple!($first_param:ident, $($param)*);
        unsafe_gc_impl! {
            $($impls)*
        }
        unsafe impl<'gc, OwningRef, $($param),*> $crate::GcDirectBarrier<'gc, OwningRef> for ($($param,)*)
            where $($param: $crate::GcDirectBarrier<'gc, OwningRef>),* {
            #[inline]
            unsafe fn write_barrier(
                &self, #[allow(unused)] owner: &OwningRef,
                #[allow(unused)] start_offset: usize
            ) {
                /*
                 * We are implementing gc **direct** write.
                 * This is safe because all the tuple's values
                 * are stored inline. We calculate the pointer offset
                 * using arithmetic.
                 */
                #[allow(non_snake_case)]
                let ($(ref $param,)*) = *self;
                $({
                    let field_offset = ($param as *const $param as usize)
                        - (self as *const Self as usize);
                    $param.write_barrier(owner, field_offset + start_offset);
                };)*
            }
        }
    };
}

#[cfg(feature = "serde1")]
macro_rules! deser_tuple_impl {
    ( $($param:ident),+ ) => {

        impl<'gc, 'de, Id: $crate::CollectorId, $($param),*> $crate::serde::GcDeserialize<'gc, 'de, Id> for ($($param,)*)
            where $($param: $crate::serde::GcDeserialize<'gc, 'de, Id>),* {
            #[allow(non_snake_case, unused)]
            fn deserialize_gc<Deser: serde::Deserializer<'de>>(
                ctx: &'gc <Id as $crate::CollectorId>::Context,
                deser: Deser
            ) -> Result<Self, <Deser as serde::Deserializer<'de>>::Error> {
                use serde::de::{Visitor, Error, SeqAccess};
                use std::marker::PhantomData;
                use $crate::{CollectorId, GcSystem};
                struct TupleVisitor<'gc, 'de, Id: $crate::CollectorId, $($param: $crate::serde::GcDeserialize<'gc, 'de, Id>),*> {
                    ctx: &'gc Id::Context,
                    marker: PhantomData<(&'de (), ( $($param,)*) )>
                }
                impl<'gc, 'de, Id: CollectorId, $($param: $crate::serde::GcDeserialize<'gc, 'de, Id>),*>
                     Visitor<'de> for TupleVisitor<'gc, 'de, Id, $($param),*> {
                    type Value = ($($param,)*);
                    fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                        let mut count = 0;
                        $(
                            let $param = ();
                            count += 1;
                        )*
                        write!(f, "a tuple of len {}", count)
                    }
                    fn visit_seq<SeqAcc: SeqAccess<'de>>(self, mut seq: SeqAcc) -> Result<Self::Value, SeqAcc::Error> {
                        let mut idx = 0;
                        $(
                            let $param = match seq.next_element_seed($crate::serde::GcDeserializeSeed::new(self.ctx))? {
                                Some(value) => value,
                                None => return Err(Error::invalid_length(idx, &self))
                            };
                            idx += 1;
                        )*
                        Ok(($($param,)*))
                    }
                }
                let mut len = 0;
                $(
                    let _hack = PhantomData::<$param>;
                    len += 1;
                )*
                deser.deserialize_tuple(len, TupleVisitor {
                    marker: PhantomData, ctx
                })
            }
        }
    };
}

unsafe_trace_primitive!(i8);
unsafe_trace_primitive!(i16);
unsafe_trace_primitive!(i32);
unsafe_trace_primitive!(i64);
unsafe_trace_primitive!(isize);
unsafe_trace_primitive!(u8);
unsafe_trace_primitive!(u16);
unsafe_trace_primitive!(u32);
unsafe_trace_primitive!(u64);
unsafe_trace_primitive!(usize);
unsafe_trace_primitive!(f32);
unsafe_trace_primitive!(f64);
unsafe_trace_primitive!(bool);
unsafe_trace_primitive!(char);
// TODO: Get proper support for unsized types (issue #15)
unsafe_trace_primitive!(&'static str; @);

unsafe_gc_impl! {
    target => PhantomData<T>,
    params => [T],
    bounds => {
        Trace => always,
        TraceImmutable => always,
        TrustedDrop => always,
        GcSafe => always,
        GcRebrand => always,
    },
    branded_type => Self,
    null_trace => always,
    NEEDS_TRACE => false,
    collector_id => *,
    NEEDS_DROP => core::mem::needs_drop::<Self>(),
    trace_template => |self, visitor| { /* nop */ Ok(()) }
}

trace_tuple! { A, B, C, D, E, F, G, H, I }

unsafe_gc_impl! {
    target => [T; SIZE],
    params => [T, const SIZE: usize],
    null_trace => { where T: NullTrace },
    bounds => {
        GcRebrand => { where T: GcRebrand<'new_gc, Id>, T::Branded: Sized },
    },
    NEEDS_TRACE => T::NEEDS_TRACE,
    NEEDS_DROP => T::NEEDS_DROP,
    branded_type => [<T as GcRebrand<'new_gc, Id>>::Branded; SIZE],
    collector_id => *,
    trace_template => |self, visitor| {
        visitor.#trace_func(#b*self as #b [T])
    },
}

/*
 * Implements tracing for references.
 *
 * The underlying data must support `TraceImmutable` since we
 * only have an immutable reference.
 */
unsafe_gc_impl! {
    target => &'a T,
    params => ['a, T: 'a],
    bounds => {
        Trace => { where T: TraceImmutable },
        TraceImmutable => { where T: TraceImmutable },
        TrustedDrop => { where T: TraceImmutable /* NOTE: We are Copy, so dont' have any drop to trust */ },
        GcSafe => { where T: TraceImmutable + GcSafe<'gc, Id> },
        /*
         * TODO: Right now we require `NullTrace`. Can we weaken this?
         *
         * This is unfortunately required by our bounds, since we don't know
         * that `T::Branded` lives for &'a making `&'a T::Branded` invalid
         * as far as the compiler is concerned.
         *
         * Therefore the only solution is to preserve `&'a T` as-is,
         * which is only safe if `T: NullTrace`
         */
        GcRebrand => { where T: NullTrace + GcSafe<'new_gc, Id> },
    },
    branded_type => &'a T,
    null_trace => { where T: NullTrace },
    NEEDS_TRACE => T::NEEDS_TRACE,
    NEEDS_DROP => false, // We never need to be dropped
    collector_id => *,
    trace_template => |self, visitor| {
        visitor.trace_immutable::<T>(&**self)
    }
}


unsafe_gc_impl!(
    target => Cell<T>,
    params => [T: NullTrace],
    bounds => {
        GcRebrand => { where T: NullTrace + GcSafe<'new_gc, Id> },
    },
    branded_type => Self,
    null_trace => always,
    NEEDS_TRACE => false,
    NEEDS_DROP => T::NEEDS_DROP,
    collector_id => *,
    trace_template => |self, visitor| {
        Ok(()) /* nop */
    }
);
unsafe_gc_impl!(
    target => RefCell<T>,
    params => [T: NullTrace],
    bounds => {
        GcRebrand => { where T: GcSafe<'new_gc, Id> + NullTrace }
    },
    branded_type => Self,
    null_trace => always,
    NEEDS_TRACE => false,
    NEEDS_DROP => T::NEEDS_DROP,
    collector_id => *,
    trace_template => |self, visitor| {
        Ok(()) /* nop */
    }
);


/*
 * Implements tracing for mutable references.
 *
 * See also: Implementation for `&'a T`
 */
unsafe_gc_impl! {
    target => &'a mut T,
    params => ['a, T: 'a],
    bounds => {
        GcSafe => { where T: GcSafe<'gc, Id> },
        /*
         * TODO: Right now we require `NullTrace`
         *
         * This is the same reasoning as the requirements for `&'a T`.
         * See their comments for details.....
         */
        GcRebrand => { where T: NullTrace + GcSafe<'new_gc, Id>, },
    },
    branded_type => &'a mut T,
    null_trace => { where T: NullTrace },
    NEEDS_TRACE => T::NEEDS_TRACE,
    NEEDS_DROP => false, // Although not `Copy`, mut references don't need to be dropped
    collector_id => *,
    trace_template => |self, visitor| {
        visitor.#trace_func::<T>(#b **self)
    }
}



/*
 * Implements tracing for slices, by tracing all the objects they refer to.
 */
unsafe_gc_impl! {
    target => [T],
    params => [T],
    bounds => {
        GcRebrand => never,
        visit_inside_gc => where Visitor: crate::GcVisitor
    },
    null_trace => { where T: NullTrace },
    NEEDS_TRACE => T::NEEDS_TRACE,
    NEEDS_DROP => ::core::mem::needs_drop::<T>(),
    trace_template => |self, visitor| {
        for val in self.#iter() {
            visitor.#trace_func(val)?;
        }
        Ok(())
    },
    collector_id => *,
    visit_inside_gc => |gc, visitor| {
        todo!("Visit Gc<[T]> instead of GcArray<T>")
    }
}

unsafe_gc_impl! {
    target => Option<T>,
    params => [T],
    null_trace => { where T: NullTrace },
    NEEDS_TRACE => T::NEEDS_TRACE,
    NEEDS_DROP => T::NEEDS_DROP,
    collector_id => *,
    trace_template => |self, visitor| {
        match *self {
            None => Ok(()),
            Some(ref #mutability value) => visitor.#trace_func::<T>(value),
        }
    },
    deserialize => unstable_horrible_hack,
}
unsafe impl<'gc, OwningRef, V> GcDirectBarrier<'gc, OwningRef> for Option<V>
    where V: GcDirectBarrier<'gc, OwningRef> {
    #[inline]
    unsafe fn write_barrier(&self, owner: &OwningRef, start_offset: usize) {
        // Implementing direct write is safe because we store our value inline
        match *self {
            None => {
                /* Nothing to trigger the barrier for :) */
                // TODO: Is this unreachable code?
            },
            Some(ref value) => {
                /*
                 * We must manually compute the offset
                 * Null pointer-optimized types will have offset of zero,
                 * while other types may not
                 */
                let value_offset = (value as *const V as usize) -
                    (self as *const Self as usize);
                value.write_barrier(owner, start_offset + value_offset)
            },
        }
    }
}

unsafe_gc_impl! {
    target => Wrapping<T>,
    params => [T],
    null_trace => { where T: NullTrace },
    NEEDS_TRACE => T::NEEDS_TRACE,
    NEEDS_DROP => T::NEEDS_DROP,
    trace_template => |self, visitor| {
        // We can trace `Wrapping` by simply tracing its interior
        visitor.#trace_func(#b self.0)
    },
    collector_id => *,
    deserialize => unstable_horrible_hack,
}

#[cfg(test)]
mod test {
    use crate::epsilon::{EpsilonCollectorId, Gc};
    use zerogc_derive::Trace;
    use crate::prelude::*;
    use std::marker::PhantomData;

    #[test]
    fn test_null_trace<'gc>() {
        assert!(!<Option<i32> as Trace>::NEEDS_TRACE);
        assert!(!<Option<(i32, char)> as Trace>::NEEDS_TRACE);
        // PhantomData is NullTrace regardless of inside
        assert!(!<PhantomData<Gc<'gc, i32>> as Trace>::NEEDS_TRACE);

    }
    #[derive(Trace)]
    #[zerogc(collector_ids(EpsilonCollectorId))]
    struct Rec<'gc> {
        inner: Gc<'gc, Rec<'gc>>,
        inner_tuple: (Gc<'gc, Rec<'gc>>, Gc<'gc, Option<i32>>),
        inner_opt: Option<Gc<'gc, Rec<'gc>>>,
        inner_opt_tuple: Option<(Gc<'gc, Rec<'gc>>, Gc<'gc, char>)>,
    }
    #[test]
    fn test_trace<'gc>() {
        assert!(<Option<Gc<'gc, i32>> as Trace>::NEEDS_TRACE);
        assert!(<Option<(Gc<'gc, i32>, char)> as Trace>::NEEDS_TRACE);
        assert!(<Rec<'gc> as Trace>::NEEDS_TRACE);
        assert!(<Gc<'gc, Rec<'gc>> as Trace>::NEEDS_TRACE);
    }
}
