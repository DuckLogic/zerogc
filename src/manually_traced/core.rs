//! Implementations of `GarbageCollected` for the language's core types.
//!
//! This includes references, tuples, primitives, arrays, and everything else in `libcore`.
//!
//! `RefCell` and `Cell` are intentionally ignored and do not have implementations,
//! since you need to use their `GcRefCell` and `GcCell` counterparts.

use crate::{Trace, GcSafe, GcVisitor, NullTrace, GcBrand, CollectorId, TraceImmutable};

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
        unsafe impl<T: GcSafe> GcSafe for [T; $size] {}
        unsafe impl<'new_gc, Id: CollectorId, T> $crate::GcBrand<'new_gc, Id> for [T; $size]
            where T: CollectorId, T: GcBrand<'new_gc, Id>,
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
unsafe impl<'a, T: GcSafe + TraceImmutable> GcSafe for &'a T {}
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
unsafe impl<'a, T: GcSafe> GcSafe for &'a mut T {}
/// TODO: Right now we can only rebrand unmanaged types (NullTrace)
unsafe impl<'a: 'new_gc, 'new_gc, Id: CollectorId, T: NullTrace> GcBrand<'new_gc, Id> for &'a mut T {
    type Branded = Self;
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
unsafe impl<T: GcSafe> GcSafe for [T] {}
