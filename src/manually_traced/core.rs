//! Implementations of `GarbageCollected` for the language's core types.
//!
//! This includes references, tuples, primitives, arrays, and everything else in `libcore`.
//!
//! `RefCell` and `Cell` are intentionally ignored and do not have implementations.
//! Some collectors may need write barriers to protect their internals.
use core::num::Wrapping;

use crate::prelude::*;
use crate::{GcDirectBarrier, CollectorId};

macro_rules! trace_tuple {
    { $($param:ident)* } => {
        unsafe impl<$($param),*> Trace for ($($param,)*)
            where $($param: Trace),* {
            /*
             * HACK: Macros don't allow using `||` as separator,
             * so we use it as a terminator, causing there to be an illegal trailing `||`.
             * However, the redundant `||` is okay if we put a trailing false at the end,
             * since `a || false` is always `a`.
             * This also correctly handles the empty unit tuple by making it false
             */
            const NEEDS_TRACE: bool = $($param::NEEDS_TRACE || )* false;
            #[inline]
            fn visit<Visit: $crate::GcVisitor>(&mut self, #[allow(unused)] visitor: &mut Visit) -> Result<(), Visit::Err> {
                #[allow(non_snake_case)]
                let ($(ref mut $param,)*) = *self;
                $(visitor.visit::<$param>($param)?;)*
                Ok(())
            }
        }
        unsafe impl<$($param),*> TraceImmutable for ($($param,)*)
            where $($param: TraceImmutable),* {
            #[inline]
            fn visit_immutable<V: $crate::GcVisitor>(&self, #[allow(unused)] visitor: &mut V) -> Result<(), V::Err> {
                #[allow(non_snake_case)]
                let ($(ref $param,)*) = *self;
                $(visitor.visit_immutable::<$param>($param)?;)*
                Ok(())
            }
        }
        unsafe impl<$($param: NullTrace),*> NullTrace for ($($param,)*) {}
        unsafe impl<'new_gc, Id, $($param),*> $crate::GcBrand<'new_gc, Id> for ($($param,)*)
            where Id: $crate::CollectorId, $($param: $crate::GcBrand<'new_gc, Id>,)*
                 $(<$param as $crate::GcBrand<'new_gc, Id>>::Branded: Sized,)* {
            type Branded = ($(<$param as $crate::GcBrand<'new_gc, Id>>::Branded,)*);
        }
        unsafe impl<$($param: GcSafe),*> GcSafe for ($($param,)*) {
            const NEEDS_DROP: bool = false $(|| <$param as GcSafe>::NEEDS_DROP)*;
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

trace_tuple! {}
trace_tuple! { A }
trace_tuple! { A B }
trace_tuple! { A B C }
trace_tuple! { A B C D }
trace_tuple! { A B C D E }
trace_tuple! { A B C D E F }
trace_tuple! { A B C D E F G }
trace_tuple! { A B C D E F G H }
trace_tuple! { A B C D E F G H I }

macro_rules! trace_array {
    ($size:tt) => {
        unsafe impl<T: Trace> Trace for [T; $size] {
            const NEEDS_TRACE: bool = T::NEEDS_TRACE;
            #[inline]
            fn visit<V: $crate::GcVisitor>(&mut self, visitor: &mut V) -> Result<(), V::Err> {
                visitor.visit::<[T]>(self as &mut [T])
            }
        }
        unsafe impl<T: $crate::TraceImmutable> $crate::TraceImmutable for [T; $size] {
            #[inline]
            fn visit_immutable<V: $crate::GcVisitor>(&self, visitor: &mut V) -> Result<(), V::Err> {
                visitor.visit_immutable::<[T]>(self as &[T])
            }
        }
        unsafe impl<T: $crate::NullTrace> $crate::NullTrace for [T; $size] {}
        unsafe impl<T: GcSafe> GcSafe for [T; $size] {
            const NEEDS_DROP: bool = core::mem::needs_drop::<T>();
        }
        unsafe impl<'new_gc, Id, T> $crate::GcBrand<'new_gc, Id> for [T; $size]
            where Id: CollectorId, T: GcBrand<'new_gc, Id>,
                  <T as GcBrand<'new_gc, Id>>::Branded: Sized {
            type Branded = [<T as GcBrand<'new_gc, Id>>::Branded; $size];
        }
    };
    { $($size:tt),* } => ($(trace_array!($size);)*)
}
trace_array! {
    0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20,
    24, 32, 48, 64, 100, 128, 256, 512, 1024, 2048, 4096
}

/// Implements tracing for references.
///
/// The underlying data must support `TraceImmutable` since we
/// only have an immutable reference.
unsafe impl<'a, T: TraceImmutable> Trace for &'a T {
    const NEEDS_TRACE: bool = T::NEEDS_TRACE;
    #[inline(always)]
    fn visit<V: GcVisitor>(&mut self, visitor: &mut V) -> Result<(), V::Err> {
        visitor.visit_immutable::<T>(*self)
    }
}
unsafe impl<'a, T: TraceImmutable> TraceImmutable for &'a T {
    #[inline(always)]
    fn visit_immutable<V: GcVisitor>(&self, visitor: &mut V) -> Result<(), V::Err> {
        visitor.visit_immutable::<T>(*self)
    }
}
unsafe impl<'a, T: NullTrace> NullTrace for &'a T {}
unsafe impl<'a, T: GcSafe + TraceImmutable> GcSafe for &'a T {
    const NEEDS_DROP: bool = false; // References are safe :)
}
/// TODO: Right now we can only rebrand unmanaged types (NullTrace)
unsafe impl<'a: 'new_gc, 'new_gc, Id: CollectorId, T: NullTrace> GcBrand<'new_gc, Id> for &'a T {
    type Branded = Self;
}

/// Implements tracing for mutable references.
unsafe impl<'a, T: Trace> Trace for &'a mut T {
    const NEEDS_TRACE: bool = T::NEEDS_TRACE;
    #[inline(always)]
    fn visit<V: GcVisitor>(&mut self, visitor: &mut V) -> Result<(), V::Err> {
        visitor.visit::<T>(*self)
    }
}
unsafe impl<'a, T: TraceImmutable> TraceImmutable for &'a mut T {
    #[inline(always)]
    fn visit_immutable<V: GcVisitor>(&self, visitor: &mut V) -> Result<(), V::Err> {
        visitor.visit_immutable::<T>(&**self)
    }
}
unsafe impl<'a, T: NullTrace> NullTrace for &'a mut T {}
unsafe impl<'a, T: GcSafe> GcSafe for &'a mut T {
    const NEEDS_DROP: bool = false; // Referenes are Copy
}
/// TODO: Right now we can only rebrand unmanaged types (NullTrace)
unsafe impl<'a, 'new_gc, Id, T> GcBrand<'new_gc, Id> for &'a mut T
    where 'a: 'new_gc, Id: CollectorId, T: GcBrand<'new_gc, Id> {
    type Branded = &'new_gc mut <T as GcBrand<'new_gc, Id>>::Branded;
}

/// Implements tracing for slices, by tracing all the objects they refer to.
unsafe impl<T: Trace> Trace for [T] {
    const NEEDS_TRACE: bool = T::NEEDS_TRACE ;

    #[inline]
    fn visit<V: GcVisitor>(&mut self, visitor: &mut V) -> Result<(), V::Err> {
        if !T::NEEDS_TRACE { return Ok(()) };
        for value in self {
            visitor.visit(value)?;
        }
        Ok(())
    }
}
unsafe impl<T: TraceImmutable> TraceImmutable for [T] {
    #[inline]
    fn visit_immutable<V: GcVisitor>(&self, visitor: &mut V) -> Result<(), <V as GcVisitor>::Err> {
        if !T::NEEDS_TRACE { return Ok(()) };
        for value in self {
            visitor.visit_immutable(value)?;
        }
        Ok(())
    }
}
unsafe impl<T: NullTrace> NullTrace for [T] {}
unsafe impl<T: GcSafe> GcSafe for [T] {
    const NEEDS_DROP: bool = core::mem::needs_drop::<T>();
}

unsafe impl<T: Trace> Trace for Option<T> {
    const NEEDS_TRACE: bool = T::NEEDS_TRACE;

    #[inline]
    fn visit<V: GcVisitor>(&mut self, visitor: &mut V) -> Result<(), V::Err> {
        match *self {
            None => Ok(()),
            Some(ref mut value) => visitor.visit(value),
        }
    }
}
unsafe impl<T: TraceImmutable> TraceImmutable for Option<T> {
    #[inline]
    fn visit_immutable<V: GcVisitor>(&self, visitor: &mut V) -> Result<(), <V as GcVisitor>::Err> {
        match *self {
            None => Ok(()),
            Some(ref value) => visitor.visit_immutable(value),
        }
    }
}
unsafe impl<T: NullTrace> NullTrace for Option<T> {}
unsafe impl<T: GcSafe> GcSafe for Option<T> {
    const NEEDS_DROP: bool = T::NEEDS_DROP;
}
unsafe impl<'gc, OwningRef, V> GcDirectBarrier<'gc, OwningRef> for Option<V>
    where V: GcDirectBarrier<'gc, OwningRef> {
    #[inline]
    unsafe fn write_barrier(&self, owner: &OwningRef, start_offset: usize) {
        // Implementing direct write is safe because we store our value inline
        match *self {
            None => { /* Nothing to trigger the barrier for :) */ },
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
unsafe_gc_brand!(Option, T);

// We can trace `Wrapping` by simply tracing its interior
unsafe_trace_deref!(Wrapping, T; immut = false; |wrapping| &mut wrapping.0);
unsafe impl<T: TraceImmutable> TraceImmutable for Wrapping<T> {
    #[inline]
    fn visit_immutable<V: GcVisitor>(&self, visitor: &mut V) -> Result<(), V::Err> {
        visitor.visit_immutable(&self.0)
    }
}