//! Implements a [GcCell] to allow mutating values
//! inside garbage collected objects.
//!
//! 
//! Normally garbage collected objects are immutable,
//! since their references are shared. It's typical
//! for collectors to want to trigger a write barrier
//! before writing to a field. All interior mutability
//! requires triggering appropriate write barriers,
//! which is unsafe.
//!
//! The `zerogc_derive` crate can generate setters
//! for fields that are wrapped in a [GcCell].
//! Just mark the field with `#[zerogc(mutable(public))]`
//! and it'll generate a safe wrapper.
use core::cell::Cell;

use crate::{GcSafe, Trace, GcVisitor, NullTrace, TraceImmutable, GcDirectBarrier, GcTypeInfo, GcDynVisitError, GcDynVisitor};

/// A `Cell` pointing to a garbage collected object.
///
/// This only supports mutating `NullTrace` types directly,
/// because garbage collected pointers need write barriers.
///
/// However, other types can be mutated through auto-generated
/// accessors on the type (using the [GcDirectBarrier] trait).
#[derive(Default, Clone, Debug, Ord, PartialOrd, Eq, PartialEq, Hash)]
#[repr(transparent)]
pub struct GcCell<T: Trace + Copy>(Cell<T>);
impl<T: Trace + Copy> GcCell<T> {
    /// Create a new cell
    #[inline]
    pub fn new(value: T) -> Self {
        GcCell(Cell::new(value))
    }
    /// Get a mutable reference to this cell's value
    ///
    /// This is safe because the `&mut self`
    /// guarentes exclusive access to the cell.
    #[inline]
    pub fn get_mut(&mut self) -> &mut T {
        self.0.get_mut()
    }
    /// Get a pointer to this cell's conent
    #[inline]
    pub fn as_ptr(&self) -> *mut T {
        self.0.as_ptr()
    }
    /// Get the current value of this cell
    #[inline]
    pub fn get(&self) -> T {
        self.0.get()
    }
}
impl<T: NullTrace + Copy> GcCell<T> {
    /// Change the interior of this type
    /// to the specified type
    ///
    /// The type must be `NullTrace` because
    /// garbage collected
    /// types need write barriers.
    #[inline]
    pub fn set(&self, value: T) {
        self.0.set(value)
    }
}
/// Trigger a write barrier on the inner value
///
/// We are a 'direct' write barrier because `Value` is stored inline
unsafe impl<'gc, OwningRef, Value> GcDirectBarrier<'gc, OwningRef> for GcCell<Value>
    where Value: GcDirectBarrier<'gc, OwningRef> + Copy {
    #[inline]
    unsafe fn write_barrier(
        &self, owner: &OwningRef,
        field_offset: usize
    ) {
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
    #[inline]
    fn visit<V: GcVisitor>(&mut self, visitor: &mut V) -> Result<(), V::Err> {
        visitor.visit(self.get_mut())
    }

    #[inline]
    fn visit_dyn(&mut self, visitor: &mut GcDynVisitor) -> Result<(), GcDynVisitError> {
        self.visit(visitor)
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

    #[inline]
    fn visit_dyn_immutable(&self, visitor: &mut GcDynVisitor) -> Result<(), GcDynVisitError> {
        self.visit_immutable(visitor)
    }
}
unsafe impl<T: GcSafe + Copy + NullTrace> NullTrace for GcCell<T> {}
unsafe impl<T: GcSafe + Copy> GcSafe for GcCell<T> {}
unsafe impl<T: Trace + Copy> GcTypeInfo for GcCell<T> {
    const NEEDS_TRACE: bool = T::NEEDS_TRACE;
    /// Since T is Copy, we shouldn't need to be dropped
    const NEEDS_DROP: bool = false;
}