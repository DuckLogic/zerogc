//! Tracing implementations for the standard library
//!
//! Types that are in `libcore` and are `#![no_std]` should go in the core module,
//! but anything that requires the rest of the stdlib (including collections and allocations),
//! should go in this module.
use crate::prelude::*;

use std::collections::{HashMap, HashSet};
use crate::CollectorId;

unsafe_immutable_trace_iterable!(HashMap<K, V>; element = { (&K, &V) });
unsafe impl<K: TraceImmutable, V: Trace> Trace for HashMap<K, V> {
    const NEEDS_TRACE: bool = K::NEEDS_TRACE || V::NEEDS_TRACE;

    fn visit<Visit: GcVisitor>(&mut self, visitor: &mut Visit) -> Result<(), Visit::Err> {
        if !Self::NEEDS_TRACE { return Ok(()); };
        for (key, value) in self.iter_mut() {
            visitor.visit_immutable(key)?;
            visitor.visit(value)?;
        }
        Ok(())
    }
}
unsafe impl<K: GcSafe + TraceImmutable, V: GcSafe> GcSafe for HashMap<K, V> {
    const NEEDS_DROP: bool = true; // HashMap has internal memory
}
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
        if !Self::NEEDS_TRACE { return Ok(()); };
        for value in self.iter() {
            visitor.visit_immutable(value)?;
        }
        Ok(())
    }
}
unsafe_gc_brand!(HashSet, immut = required; V);
