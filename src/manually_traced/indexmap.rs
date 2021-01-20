use indexmap::{IndexMap, IndexSet};

use crate::prelude::*;
use std::hash::Hash;
use crate::CollectorId;

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
    #[inline]
    fn visit_dyn_immutable(&self, visitor: &mut GcDynVisitor) -> Result<(), GcDynVisitError> {
        self.visit_immutable::<GcDynVisitor>(visitor)
    }
}
unsafe impl<K, V> NullTrace for IndexMap<K, V>
    where K: NullTrace + Eq + Hash, V: NullTrace {}
unsafe impl<K, V> Trace for IndexMap<K, V>
    where K: TraceImmutable + Eq + Hash, V: Trace {
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
    K: GcSafe + TraceImmutable + Eq + Hash, V: GcSafe {}
unsafe impl<'new_gc, Id, K, V> GcRebrand<'new_gc, Id> for IndexMap<K, V>
    where Id: CollectorId, K: TraceImmutable + Eq + Hash + GcRebrand<'new_gc, Id>,
          V: GcRebrand<'new_gc, Id>,
          <K as GcRebrand<'new_gc, Id>>::Branded: TraceImmutable + Eq + Hash + Sized,
          <V as GcRebrand<'new_gc, Id>>::Branded: Sized {
    type Branded = IndexMap<
        <K as GcRebrand<'new_gc, Id>>::Branded,
        <V as GcRebrand<'new_gc, Id>>::Branded
    >;
}

unsafe impl<K, V> GcTypeInfo for IndexMap<K, V>
    where K: TraceImmutable + Eq + Hash, V: Trace {
    /// IndexMap has internal memory
    const NEEDS_TRACE: bool = K::NEEDS_TRACE | V::NEEDS_TRACE;
    const NEEDS_DROP: bool = true;
}
unsafe impl<'a, Id, K, V> GcErase<'a, Id> for IndexMap<K, V>
    where Id: CollectorId, K: TraceImmutable + Eq + Hash + GcErase<'a, Id>,
          V: GcErase<'a, Id>,
          <K as GcErase<'a, Id>>::Erased: TraceImmutable + Eq + Hash + Sized,
          <V as GcErase<'a, Id>>::Erased: Sized {
    type Erased = IndexMap<
        <K as GcErase<'a, Id>>::Erased,
        <V as GcErase<'a, Id>>::Erased
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

    #[inline]
    fn visit_dyn_immutable(&self, visitor: &mut GcDynVisitor) -> Result<(), GcDynVisitError> {
        self.visit_immutable(visitor)
    }
}
unsafe impl<V> Trace for IndexSet<V>
    where V: TraceImmutable + Eq + Hash {
    fn visit<Visit: GcVisitor>(&mut self, visitor: &mut Visit) -> Result<(), Visit::Err> {
        for value in self.iter() {
            visitor.visit_immutable(value)?;
        }
        Ok(())
    }
}
unsafe impl<'new_gc, Id, V> GcRebrand<'new_gc, Id> for IndexSet<V>
    where Id: CollectorId, V: GcRebrand<'new_gc, Id> + TraceImmutable + Eq + Hash,
          <V as GcRebrand<'new_gc, Id>>::Branded: TraceImmutable + Eq + Hash, {
    type Branded = IndexSet<<V as GcRebrand<'new_gc, Id>>::Branded>;
}
unsafe impl<'a, Id, V> GcErase<'a, Id> for IndexSet<V>
    where Id: CollectorId, V: GcErase<'a, Id> + TraceImmutable + Eq + Hash,
          <V as GcErase<'a, Id>>::Erased: TraceImmutable + Eq + Hash, {
    type Erased = IndexSet<<V as GcErase<'a, Id>>::Erased>;
}
unsafe impl<V> GcTypeInfo for IndexSet<V>
    where V: TraceImmutable + Eq + Hash {
    const NEEDS_TRACE: bool = V::NEEDS_TRACE;
    /// IndexSet has internal memory
    const NEEDS_DROP: bool = true;
}
