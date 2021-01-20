//! Implementations of `GarbageCollected` for the language's core types.
//!
//! This includes references, tuples, primitives, arrays, and everything else in `libcore`.
//!
//! `RefCell` and `Cell` are intentionally ignored and do not have implementations.
//! Some collectors may need write barriers to protect their internals.
use core::num::Wrapping;

use crate::prelude::*;
use crate::{GcDirectBarrier, CollectorId, DynTrace};

macro_rules! trace_tuple {
    { $($param:ident)* } => {
        unsafe impl<$($param),*> Trace for ($($param,)*)
            where $($param: Trace),* {
            #[inline]
            fn visit<Visit: $crate::GcVisitor>(&mut self, #[allow(unused)] visitor: &mut Visit) -> Result<(), Visit::Err> {
                #[allow(non_snake_case)]
                let ($(ref mut $param,)*) = *self;
                $(visitor.visit::<$param>($param)?;)*
                Ok(())
            }
        }
        unsafe impl<$($param),*> GcTypeInfo for ($($param,)*)
            where $($param: Trace),* {
            /*
             * HACK: Macros don't allow using `||` as separator,
             * so we use it as a terminator, causing there to be an illegal trailing `||`.
             * However, the redundant `||` is okay if we put a trailing false at the end,
             * since `a || false` is always `a`.
             * This also correctly handles the empty unit tuple by making it false
             */
            const NEEDS_TRACE: bool = $($param::NEEDS_TRACE || )* false;
            const NEEDS_DROP: bool = $(<$param as GcTypeInfo>::NEEDS_DROP ||)* false;
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
            #[inline]
            fn visit_dyn_immutable(&self, visitor: &mut GcDynVisitor) -> Result<(), GcDynVisitError> {
                self.visit_immutable(visitor)
            }
        }
        unsafe impl<$($param: NullTrace),*> NullTrace for ($($param,)*) {}
        unsafe impl<'new_gc, Id, $($param),*> $crate::GcRebrand<'new_gc, Id> for ($($param,)*)
            where Id: $crate::CollectorId, $($param: $crate::GcRebrand<'new_gc,     Id>,)*
                 $(<$param as $crate::GcRebrand<'new_gc, Id>>::Branded: Sized,)* {
            type Branded = ($(<$param as $crate::GcRebrand<'new_gc, Id>>::Branded,)*);
        }
        unsafe impl<'a, Id, $($param),*> $crate::GcErase<'a, Id> for ($($param,)*)
            where Id: $crate::CollectorId, $($param: $crate::GcErase<'a, Id>,)*
                 $(<$param as $crate::GcErase<'a, Id>>::Erased: Sized,)* {
            type Erased = ($(<$param as $crate::GcErase<'a, Id>>::Erased,)*);
        }
        unsafe impl<$($param: GcSafe),*> GcSafe for ($($param,)*) {}
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
            #[inline]
            fn visit<V: $crate::GcVisitor>(&mut self, visitor: &mut V) -> Result<(), V::Err> {
                visitor.visit::<[T]>(self as &mut [T])
            }
        }
        unsafe impl<T: Trace> GcTypeInfo for [T; $size] {
            const NEEDS_TRACE: bool = T::NEEDS_TRACE;
            const NEEDS_DROP: bool = T::NEEDS_DROP;
        }
        unsafe impl<T: $crate::TraceImmutable> $crate::TraceImmutable for [T; $size] {
            #[inline]
            fn visit_immutable<V: $crate::GcVisitor>(&self, visitor: &mut V) -> Result<(), V::Err> {
                visitor.visit_immutable::<[T]>(self as &[T])
            }
            #[inline]
            fn visit_dyn_immutable(&self, visitor: &mut GcDynVisitor) -> Result<(), GcDynVisitError> {
                visitor.visit_immutable::<[T]>(self as &[T])
            }
        }
        unsafe impl<T: $crate::NullTrace> $crate::NullTrace for [T; $size] {}
        unsafe impl<T: GcSafe> GcSafe for [T; $size] {}
        unsafe impl<'new_gc, Id, T> $crate::GcRebrand<'new_gc, Id> for [T; $size]
            where Id: CollectorId, T: GcRebrand<'new_gc, Id>,
                  <T as GcRebrand<'new_gc, Id>>::Branded: Sized {
            type Branded = [<T as GcRebrand<'new_gc, Id>>::Branded; $size];
        }
        unsafe impl<'a, Id, T> $crate::GcErase<'a, Id> for [T; $size]
            where Id: CollectorId, T: GcErase<'a, Id>,
                  <T as GcErase<'a, Id>>::Erased: Sized {
            type Erased = [<T as GcErase<'a, Id>>::Erased; $size];
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

    #[inline]
    fn visit_dyn_immutable(&self, visitor: &mut GcDynVisitor) -> Result<(), GcDynVisitError> {
        visitor.visit_immutable::<T>(*self)
    }
}
unsafe impl<'a, T: NullTrace> NullTrace for &'a T {}
unsafe impl<'a, T: GcSafe + TraceImmutable> GcSafe for &'a T {}
unsafe impl<'a, T: TraceImmutable> GcTypeInfo for &'a T {
    const NEEDS_TRACE: bool = T::NEEDS_TRACE;
    const NEEDS_DROP: bool = false; // References never need to be dropped :)
}
/// TODO: Right now we require `NullTrace`
///
/// This is unfortunately required by our bounds, since we don't know
///  that `T::Branded` lives for &'a making `&'a T::Branded` invalid
///  as far as the compiler is concerned.
///
/// Therefore the only solution is to preserve `&'a T` as-is,
/// which is only safe if `T: NullTrace`
unsafe impl<'a, 'new_gc, Id, T> GcRebrand<'new_gc, Id> for &'a T
    where Id: CollectorId, T: NullTrace, 'a: 'new_gc {
    type Branded = &'a T;
}
/// See impl of `GcRebrand` for why we require `T: NullTrace`
unsafe impl<'a, Id, T> GcErase<'a, Id> for &'a T
    where Id: CollectorId, T: NullTrace {
    type Erased = &'a T;
}

/// Implements tracing for mutable references.
unsafe impl<'a, T: Trace> Trace for &'a mut T {
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

    #[inline]
    fn visit_dyn_immutable(&self, visitor: &mut GcDynVisitor) -> Result<(), GcDynVisitError> {
        visitor.visit_immutable::<T>(&**self)
    }
}
unsafe impl<'a, T: NullTrace> NullTrace for &'a mut T {}
unsafe impl<'a, T: GcSafe> GcSafe for &'a mut T {}
unsafe impl<'a, T: Trace> GcTypeInfo for &'a mut T {
    const NEEDS_TRACE: bool = T::NEEDS_TRACE;
    /// Although not [Copy], mutable references
    /// never need to be dropped
    const NEEDS_DROP: bool = false;
}
/// TODO: We currently require NullTrace for `T`
unsafe impl<'a, 'new_gc, Id, T> GcRebrand<'new_gc, Id> for &'a mut T
    where Id: CollectorId, T: NullTrace, 'a: 'new_gc {
    type Branded = &'a mut T;
}
/// TODO: We currently require NullTrace for `T`
unsafe impl<'a, Id, T> GcErase<'a, Id> for &'a mut T
    where Id: CollectorId, T: NullTrace {
    type Erased = &'a mut T;
}

/// Implements tracing for slices, by tracing all
/// the objects they refer to.
unsafe impl<T: Trace> Trace for [T] {
    #[inline]
    fn visit<V: GcVisitor>(&mut self, visitor: &mut V) -> Result<(), V::Err> {
        if !T::NEEDS_TRACE { return Ok(()) };
        for value in self {
            visitor.visit(value)?;
        }
        Ok(())
    }
}
unsafe impl<T: Trace> DynTrace for [T] {
    fn visit_dyn(&mut self, visitor: &mut GcDynVisitor) -> Result<(), GcDynVisitError> {
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

    #[inline]
    fn visit_dyn_immutable(&self, visitor: &mut GcDynVisitor) -> Result<(), GcDynVisitError> {
        self.visit_immutable::<GcDynVisitor>(visitor)
    }
}
unsafe impl<T: NullTrace> NullTrace for [T] {}
unsafe impl<T: GcSafe> GcSafe for [T] {}
unsafe impl<T: Trace> GcTypeInfo for [T] {
    const NEEDS_TRACE: bool = T::NEEDS_TRACE;
    const NEEDS_DROP: bool = T::NEEDS_DROP;
}

unsafe impl<T: Trace> Trace for Option<T> {
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

    #[inline]
    fn visit_dyn_immutable(&self, visitor: &mut GcDynVisitor) -> Result<(), GcDynVisitError> {
        self.visit_immutable(visitor)
    }
}
unsafe impl<T: NullTrace> NullTrace for Option<T> {}
unsafe impl<T: GcSafe> GcSafe for Option<T> {}
unsafe impl<T: Trace> GcTypeInfo for Option<T> {
    const NEEDS_TRACE: bool = T::NEEDS_TRACE;
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

    fn visit_dyn_immutable(&self, visitor: &mut GcDynVisitor) -> Result<(), GcDynVisitError> {
        self.visit_immutable(visitor)
    }
}