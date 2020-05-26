use std::cell::Cell;

use crate::{GcSafe, Trace, GcVisitor, NullTrace, TraceImmutable};

/// A `Cell` pointing to a garbage collected object.
///
/// Either this or a `GcRefCell` is needed in order to mutate a garbage collected object,
/// since garbage collected pointers are otherwise immutable after allocation.
/// Unlike a regular `Cell` this type implements `GarbageCollected`,
/// and may eventually have read/write barriers.
#[derive(Default, Clone, Debug)]
pub struct GcCell<T: Trace + Copy>(Cell<T>);
impl<T: Trace + Copy> GcCell<T> {
    #[inline]
    pub fn new(value: T) -> Self {
        GcCell(Cell::new(value))
    }
    #[inline]
    pub fn get_mut(&mut self) -> &mut T {
        self.0.get_mut()
    }
    #[inline]
    pub fn get(&self) -> T {
        self.0.get()
    }
    #[inline]
    pub fn set(&self, value: T) {
        self.0.set(value)
    }
}

unsafe impl<T: Trace + Copy> Trace for GcCell<T> {
    const NEEDS_TRACE: bool = T::NEEDS_TRACE;

    #[inline]
    fn visit<V: GcVisitor>(&mut self, visitor: &mut V) -> Result<(), V::Err> {
        visitor.visit(self.get_mut())
    }
}
/// Since a cell has interior mutablity, it can implement `TraceImmutable`
/// even if the interior type is only `Trace`
unsafe impl<T: GcSafe + Copy> TraceImmutable for GcCell<T> {
    #[inline]
    fn visit_immutable<V: GcVisitor>(&self, visitor: &mut V) -> Result<(), <V as GcVisitor>::Err> {
        let mut value = self.get();
        visitor.visit(&mut value)?;
        self.set(value);
        Ok(())
    }
}
unsafe impl<T: GcSafe + Copy + NullTrace> NullTrace for GcCell<T> {}
unsafe impl<T: GcSafe + Copy> GcSafe for GcCell<T> {}
