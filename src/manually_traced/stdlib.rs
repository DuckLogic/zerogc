//! Tracing implementations for the standard library
//!
//! Types that are in `libcore` and are `#![no_std]` should go in the core module,
//! but anything that requires the rest of the stdlib (including collections and allocations),
//! should go in this module.

use crate::{Trace, GcSafe, GcVisitor, TraceImmutable, NullTrace, GcBrand, GcSystem};
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::Arc;
use std::num::Wrapping;

// NOTE: Delegate to slice to avoid code duplication
unsafe_trace_deref!(Vec, target = { [T] }; T);
unsafe_trace_deref!(Box, target = T);
// We can only trace `Rc` and `Arc` if the inner type implements `TraceImmutable`
unsafe_trace_deref!(Rc, T; immut = required; |rc| &**rc);
unsafe_trace_deref!(Arc, T; immut = required; |arc| &**arc);
// We can trace `Wrapping` by simply tracing its interior
unsafe_trace_deref!(Wrapping, T; immut = false; |wrapping| &mut wrapping.0);
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
unsafe impl<'new_gc, S, K, V> GcBrand<'new_gc, S> for HashMap<K, V>
    where S: GcSystem, K: TraceImmutable + GcBrand<'new_gc, S>,
        V: GcBrand<'new_gc, S>,
        <K as GcBrand<'new_gc, S>>::Branded: TraceImmutable + Sized,
        <V as GcBrand<'new_gc, S>>::Branded: Sized {
    type Branded = HashMap<
        <K as GcBrand<'new_gc, S>>::Branded,
        <V as GcBrand<'new_gc, S>>::Branded
    >;
}
unsafe_immutable_trace_iterable!(HashSet<V>; element = { &V });
unsafe impl<V: TraceImmutable> Trace for HashSet<V> {
    const NEEDS_TRACE: bool = V::NEEDS_TRACE;

    fn visit<Visit: GcVisitor>(&mut self, visitor: &mut Visit) -> Result<(), Visit::Err> {
        for value in self.iter() {
            visitor.visit_immutable(value)?;
        }
        Ok(())
    }
}
unsafe_gc_brand!(HashSet, immut = required; V);

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
unsafe_gc_brand!(Option, T);