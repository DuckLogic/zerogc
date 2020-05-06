//! Tracing implementations for the standard library
//!
//! Types that are in `libcore` and are `#![no_std]` should go in the core module,
//! but anything that requires the rest of the stdlib (including collections and allocations),
//! should go in this module.

use crate::{Trace, GcSafe, GcVisitor, TraceImmutable, NullTrace, GcBrand, CollectorId};
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::Arc;
use std::num::Wrapping;

// NOTE: Delegate to slice to avoid code duplication
unsafe_trace_deref!(Vec, target = { &[T] }; T);
unsafe_trace_deref!(Box, target = T);
unsafe_trace_deref!(Rc, target = T);
unsafe_trace_deref!(Arc, target = T);
// We can trace `Wrapping` by simply tracing its interior
unsafe_trace_deref!(Wrapping, T; immut = false; |wrapping| &wrapping.0);
unsafe impl<T: TraceImmutable> TraceImmutable for Wrapping<T> {
    #[inline]
    fn visit_immutable<V: GcVisitor>(&self, visitor: &mut V) -> Result<(), V::Err> {
        visitor.visit_immutable(&self.0)
    }
}

unsafe_immutable_trace_iterable!(HashMap<K, V>; element = { (&K, &V) });
unsafe impl<K: TraceImmutable, V: Trace> Trace for HashMap<K, V> {
    const NEEDS_TRACE: bool = K::NEEDS_TRACE || V::NEEDS_TRACE;

    fn visit<Visit: GcVisitor>(&mut self, visitor: &mut Visit) -> Result<(), Visit::Err> {
        for (key, value) in self.iter_mut() {
            visitor.visit_immutable(key)?;
            visitor.visit(value)?;
        }
        Ok(())
    }
}
unsafe impl<K: GcSafe + TraceImmutable, V: GcSafe> GcSafe for HashMap<K, V> {}
unsafe impl<'new_gc, Id, K, V> GcBrand<'new_gc, Id> for HashMap<K, V>
    where Id: CollectorId, K: TraceImmutable + GcBrand<'new_gc, Id>,
        V: GcBrand<'new_gc, Id>,
        <K as GcBrand<'new_gc, Id>>::Branded: TraceImmutable + Sized,
        <V as GcBrand<'new_gc, Id>>::Branded: Sized {
    type Branded = HashMap<
        <K as GcBrand<'new_gc, Id>>::Branded,
        <V as GcBrand<'new_gc, Id>>::Branded
    >;
}
unsafe_immutable_trace_iterable!(HashSet<V>; element = { &V });
unsafe impl<V: TraceImmutable> Trace for HashSet<V> {
    const NEEDS_TRACE: bool = V::NEEDS_TRACE;

    fn visit<Visit: GcVisitor>(&mut self, visitor: &mut Visit) -> Result<(), Visit::Err> {
        for value in self {
            visitor.visit_immutable(value)?;
        }
        Ok(())
    }
}
unsafe impl<T: GcSafe + TraceImmutable> GcSafe for HashSet<T> {}
unsafe impl<'new_gc, Id, V> GcBrand<'new_gc, Id> for HashSet<V>
    where Id: CollectorId, V: TraceImmutable + GcBrand<'new_gc, Id>,
        <V as GcBrand<'new_gc, Id>>::Branded: TraceImmutable + Sized {
    type Branded = HashSet<<V as GcBrand<'new_gc, Id>>::Branded>;
}

unsafe impl<T: Trace> Trace for Option<T> {
    const NEEDS_TRACE: bool = T::NEEDS_TRACE;

    #[inline]
    fn visit<V: GcVisitor>(&mut self, visitor: &mut V) -> Result<(), V::Err> {
        match *self {
            None => Ok(()),
            Some(ref value) => visitor.visit(value),
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
unsafe_gc_brand!(Option, T);