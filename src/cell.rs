use std::cell::Cell;

use crate::{GcSafe, Trace, GcVisitor, NullTrace, TraceImmutable, GcDirectBarrier,};

/// A `Cell` pointing to a garbage collected object.
///
/// This only supports mutating `NullTrace` types,
/// becuase garbage collected pointers need write barriers.
#[derive(Default, Clone, Debug)]
#[repr(transparent)]
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
    pub fn as_ptr(&self) -> *mut T {
        self.0.as_ptr()
    }
    #[inline]
    pub fn get(&self) -> T {
        self.0.get()
    }
}
impl<T: NullTrace + Copy> GcCell<T> {
    /// Change the interior of this type to the specified type
    ///
    /// The type must be `NullTrace` because garbage collected
    /// types need write barriers
    #[inline]
    pub fn set(&self, value: T) {
        self.0.set(value)
    }
}
unsafe impl<'gc, OwningRef, Value> GcDirectBarrier<'gc, OwningRef> for GcCell<Value>
    where Value: GcDirectBarrier<'gc, OwningRef> + Copy {
    #[inline]
    unsafe fn write_barrier(
        &self, owner: &OwningRef,
        field_offset: usize
    ) {
        // NOTE: We are direct write because `Value` is stored inline
        self.get().write_barrier(owner, field_offset)
    }
}
/// GcCell can only support mutating types that are `NullTrace`,
/// because garbage collected types need write barriers.
///
/// However, this is already enforced by the bounds of `GcCell::set`,
/// so we don't need to verify here.
/// In other words is possible to safely trace a `GcCell`
/// with a garbage collected type, as long as it is never mutated.
unsafe impl<T: Trace + Copy> Trace for GcCell<T> {
    const NEEDS_TRACE: bool = T::NEEDS_TRACE;

    #[inline]
    fn visit<V: GcVisitor>(&mut self, visitor: &mut V) -> Result<(), V::Err> {
        visitor.visit(self.get_mut())
    }
}
/// See Trace documentation on the safety of mutation
///
/// We require `NullTrace` in order to `set` our internals
unsafe impl<T: GcSafe + NullTrace + Copy> TraceImmutable for GcCell<T> {
    #[inline]
    fn visit_immutable<V: GcVisitor>(&self, visitor: &mut V) -> Result<(), <V as GcVisitor>::Err> {
        let mut value = self.get();
        visitor.visit(&mut value)?;
        self.set(value);
        Ok(())
    }
}
unsafe impl<T: GcSafe + Copy + NullTrace> NullTrace for GcCell<T> {}
unsafe impl<T: GcSafe + Copy> GcSafe for GcCell<T> {
    /// Since T is Copy, we shouldn't need to be dropped
    const NEEDS_DROP: bool = false;
}
