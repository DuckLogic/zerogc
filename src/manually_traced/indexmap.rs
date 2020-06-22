use indexmap::{IndexMap, IndexSet};

use crate::prelude::*;
use std::hash::Hash;

// Maps

unsafe impl<K, V> TraceImmutable for IndexMap<K, V>
    where K: TraceImmutable + Eq + Hash, V: TraceImmutable {
    fn visit_immutable<Visit: GcVisitor>(&self, visitor: &mut Visit) -> Result<(), Visit::Err> {
        if !Self::NEEDS_TRACE { return Ok(()); };
        for (key, value) in self.iter() {
            visitor.visit_immutable(key)?;
            visitor.visit_immutable(value)?;
        }
        Ok(())
    }
}
unsafe impl<K, V> NullTrace for IndexMap<K, V>
    where K: NullTrace + Eq + Hash, V: NullTrace {}
unsafe impl<K, V> Trace for IndexMap<K, V>
    where K: TraceImmutable + Eq + Hash, V: Trace {
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
unsafe impl<K, V> GcSafe for IndexMap<K, V> where
    K: GcSafe + TraceImmutable + Eq + Hash, V: GcSafe {
    const NEEDS_DROP: bool = true; // IndexMap has internal memory
}
unsafe impl<'new_gc, S, K, V> GcBrand<'new_gc, S> for IndexMap<K, V>
    where S: GcSystem, K: TraceImmutable + Eq + Hash + GcBrand<'new_gc, S>,
          V: GcBrand<'new_gc, S>,
          <K as GcBrand<'new_gc, S>>::Branded: TraceImmutable + Eq + Hash + Sized,
          <V as GcBrand<'new_gc, S>>::Branded: Sized {
    type Branded = IndexMap<
        <K as GcBrand<'new_gc, S>>::Branded,
        <V as GcBrand<'new_gc, S>>::Branded
    >;
}

// Sets

unsafe impl<T> TraceImmutable for IndexSet<T>
    where T: TraceImmutable + Eq + Hash {
    fn visit_immutable<Visit: GcVisitor>(&self, visitor: &mut Visit) -> Result<(), Visit::Err> {
        if !Self::NEEDS_TRACE { return Ok(()); };
        for element in self.iter() {
            visitor.visit_immutable(element)?;
        }
        Ok(())
    }
}
unsafe impl<V> Trace for IndexSet<V>
    where V: TraceImmutable + Eq + Hash {
    const NEEDS_TRACE: bool = V::NEEDS_TRACE;

    fn visit<Visit: GcVisitor>(&mut self, visitor: &mut Visit) -> Result<(), Visit::Err> {
        for value in self.iter() {
            visitor.visit_immutable(value)?;
        }
        Ok(())
    }
}
unsafe impl<'new_gc, S, V> GcBrand<'new_gc, S> for IndexSet<V>
    where S: GcSystem, V: GcBrand<'new_gc, S> + TraceImmutable + Eq + Hash,
          <V as GcBrand<'new_gc, S>>::Branded: TraceImmutable + Eq + Hash, {
    type Branded = IndexSet<<V as GcBrand<'new_gc, S>>::Branded>;
}