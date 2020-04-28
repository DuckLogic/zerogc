//! Implementations of `GarbageCollected` for the language's core types.
//!
//! This includes references, tuples, primitives, arrays, and everything else in `libcore`.
//!
//! `RefCell` and `Cell` are intentionally ignored and do not have implementations,
//! since you need to use their `GcRefCell` and `GcCell` counterparts.

use crate::{GarbageCollected, GarbageCollectionSystem};

macro_rules! trace_tuple {
    { $($param:ident)* } => {
        unsafe impl<$($param),*> GarbageCollected for ($($param,)*)
            where $($param: GarbageCollected),* {
            /*
             * HACK: Macros don't allow using `||` as separator,
             * so we use it as a terminator, causing there to be an illegal trailing `||`.
             * However, the redundant `||` is okay if we put a trailing false at the end,
             * since `a || false` is always `a`.
             * This also correctly handles the empty unit tuple by making it false
             */
            const NEEDS_TRACE: bool = $($param::NEEDS_TRACE || )* false;
            #[inline]
            unsafe fn raw_trace(&self, #[allow(unused)] collector: &mut GarbageCollectionSystem) {
                #[allow(non_snake_case)]
                let ($(ref $param,)*) = *self;
                $(collector.trace::<$param>($param);)*
            }
        }
        unsafe impl<$($param),*> $crate::safepoints::GcErase for ($($param,)*)
            where $($param: $crate::safepoints::GcErase),* {
            type Erased = ($($param::Erased,)*);

            #[inline]
            unsafe fn erase(self) -> Self::Erased {
                let erased = ::std::mem::transmute_copy(&self);
                std::mem::forget(self);
                erased
            }
        }
        unsafe impl<'gc, $($param),*> $crate::safepoints::GcUnErase<'gc> for ($($param,)*)
            where $($param: $crate::GarbageCollected + $crate::safepoints::GcUnErase<'gc>),* {
            type Corrected = ($($param::Corrected,)*);

            #[inline]
            unsafe fn unerase(self) -> Self::Corrected {
                let unerased = ::std::mem::transmute_copy(&self);
                std::mem::forget(self);
                unerased
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
        unsafe impl<T: GarbageCollected> GarbageCollected for [T; $size] {
            const NEEDS_TRACE: bool = T::NEEDS_TRACE;
            #[inline]
            unsafe fn raw_trace(&self, collector: &mut GarbageCollectionSystem) {
                collector.trace::<[T]>(self as &[T]);
            }
        }
    };
    { $($size:tt),* } => ($(trace_array!($size);)*)
}
trace_array! {
    0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20,
    24, 32, 48, 64, 100, 128, 256, 512, 1024, 2048, 4096
}

unsafe impl<'unm, T: GarbageCollected> GarbageCollected for &'unm [T] {
    const NEEDS_TRACE: bool = T::NEEDS_TRACE;
    #[inline]
    unsafe fn raw_trace(&self, collector: &mut GarbageCollectionSystem) {
        for element in *self {
            collector.trace(element);
        }
    }
}

/// Implements tracing for references, by tracing the objects they refer to.
///
/// However, the references can never be garbage collected themselves (and live across safepoints),
/// so `GcErase` isn't implemented for this type.
unsafe impl<'a, T: GarbageCollected> GarbageCollected for &'a T {
    const NEEDS_TRACE: bool = T::NEEDS_TRACE;
    #[inline]
    unsafe fn raw_trace(&self, collector: &mut GarbageCollectionSystem) {
        T::trace(*self, collector)
    }
}

unsafe impl<'a, T: GarbageCollected> GarbageCollected for &'a mut T {
    const NEEDS_TRACE: bool = T::NEEDS_TRACE;
    #[inline]
    unsafe fn raw_trace(&self, collector: &mut GarbageCollectionSystem) {
        T::trace(*self, collector);
    }
}
/// Implements tracing for slices, by tracing all the objects they refer to.
unsafe impl<T: GarbageCollected> GarbageCollected for [T] {
    const NEEDS_TRACE: bool = T::NEEDS_TRACE;

    #[inline]
    unsafe fn raw_trace(&self, collector: &mut GarbageCollectionSystem) {
        for value in self {
            collector.trace(value)
        }
    }
}
