//! Implementations of `GarbageCollected` for the language's core types.
//!
//! This includes references, tuples, primitives, arrays, and everything else in `libcore`.
//!
//! `RefCell` and `Cell` are intentionally ignored and do not have implementations.
//! Some collectors may need write barriers to protect their internals.
use core::num::Wrapping;

use crate::prelude::*;
use crate::GcDirectBarrier;

use zerogc_derive::unsafe_gc_impl;

macro_rules! __rec_trace_tuple {
    ($($param:ident),*) => {
        // Nothing remaining
    };
    ($first_param:ident, $($param:ident),+) => {
        trace_tuple!($($param),*);
    };
}
macro_rules! trace_tuple {
    { $($param:ident),* } => {
        __rec_trace_tuple!($($param),* );
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
            branded_type => ( $(<$param as GcRebrand<'new_gc, Id>>::Branded,)* ),
            erased_type => ( $(<$param as GcErase<'min, Id>>::Erased,)* ),
            visit => |self, visitor| {
                ##[allow(non_snake_case)]
                let ($(ref #mutability $param,)*) = *self;
                $(visitor.#visit_func($param)?;)*
                Ok(())
            }
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
unsafe_trace_primitive!(&'static str);

trace_tuple! { A, B, C, D, E, F, G, H, I }

macro_rules! trace_array {
    ($size:tt) => {
        unsafe_gc_impl! {
            target => [T; $size],
            params => [T],
            null_trace => { where T: NullTrace },
            NEEDS_TRACE => T::NEEDS_TRACE,
            NEEDS_DROP => T::NEEDS_DROP,
            branded_type => [<T as GcRebrand<'new_gc, Id>>::Branded; $size],
            erased_type => [<T as GcErase<'min, Id>>::Erased; $size],
            visit => |self, visitor| {
                visitor.#visit_func(#b*self as #b [T])
            },
        }
    };
    { $($size:tt),* } => ($(trace_array!($size);)*)
}
trace_array! {
    0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20,
    24, 32, 48, 64, 100, 128, 256, 512, 1024, 2048, 4096
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
        /*
         * TODO: Right now we require `NullTrace`
         *
         * This is unfortunately required by our bounds, since we don't know
         * that `T::Branded` lives for &'a making `&'a T::Branded` invalid
         * as far as the compiler is concerned.
         *
         * Therefore the only solution is to preserve `&'a T` as-is,
         * which is only safe if `T: NullTrace`
         */
        GcRebrand => { where T: NullTrace, 'a: 'new_gc },
        GcErase => { where T: NullTrace, 'a: 'min }
    },
    branded_type => &'a T,
    erased_type => &'a T,
    null_trace => { where T: NullTrace },
    NEEDS_TRACE => T::NEEDS_TRACE,
    NEEDS_DROP => false, // We never need to be dropped
    visit => |self, visitor| {
        visitor.visit_immutable::<T>(&**self)
    }
}


/*
 * Implements tracing for mutable references.
 *
 * See also: Implementation for `&'a T`
 */
unsafe_gc_impl! {
    target => &'a mut T,
    params => ['a, T: 'a],
    bounds => {
        /*
         * TODO: Right now we require `NullTrace`
         *
         * This is the same reasoning as the requirements for `&'a T`.
         * See their comments for details.....
         */
        GcRebrand => { where T: NullTrace, 'a: 'new_gc },
        GcErase => { where T: NullTrace, 'a: 'min }
    },
    branded_type => &'a mut T,
    erased_type => &'a mut T,
    null_trace => { where T: NullTrace },
    NEEDS_TRACE => T::NEEDS_TRACE,
    NEEDS_DROP => false, // Although not `Copy`, mut references don't need to be dropped
    visit => |self, visitor| {
        visitor.#visit_func::<T>(#b **self)
    }
}



/*
 * Implements tracing for slices, by tracing all the objects they refer to.
 *
 * NOTE: We cannot currently implement `GcRebrand` + `GcErase`
 * because we are unsized :((
 */
unsafe_gc_impl! {
    target => [T],
    params => [T],
    bounds => {
        GcRebrand => never,
        GcErase => never
    },
    null_trace => { where T: NullTrace },
    NEEDS_TRACE => T::NEEDS_TRACE,
    NEEDS_DROP => ::core::mem::needs_drop::<T>(),
    visit => |self, visitor| {
        for val in self.#iter() {
            visitor.#visit_func(val)?;
        }
        Ok(())
    }
}

unsafe_gc_impl! {
    target => Option<T>,
    params => [T],
    null_trace => { where T: NullTrace },
    NEEDS_TRACE => T::NEEDS_TRACE,
    NEEDS_DROP => T::NEEDS_DROP,
    visit => |self, visitor| {
        match *self {
            None => Ok(()),
            Some(ref #mutability value) => visitor.#visit_func::<T>(value),
        }
    },
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
    visit => |self, visitor| {
        // We can trace `Wrapping` by simply tracing its interior
        visitor.#visit_func(#b self.0)
    }
}