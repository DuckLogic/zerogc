use std::cell::{Cell, RefCell, Ref, RefMut};
use std::ops::{Deref, DerefMut};

use crate::{GcSafe, Trace, GcVisitor};

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
    unsafe fn visit<V: GcVisitor>(&self, visitor: &mut V) -> Result<(), V::Err> {
        visitor.visit(&self.get());
    }
}
unsafe impl<T: GcSafe + Copy> GcSafe for GcCell<T> {}
/// A `RefCell` pointing to a garbage collected object.
///
/// Either this or a `GcCell` is needed in order to mutate a garbage collected object,
/// since garbage collected pointers are otherwise immutable after allocation.
/// If a generational garbage collection algorithm is ever implemented,
/// mutating this would be significantly slower than `GcCell`,
/// since the collector would always have to completely retrace the mutated object.
/// Unlike a regular `RefCell` this type implements `GarbageCollected`,
/// and may eventually have read/write barriers.
#[derive(Default, Clone, Debug)]
pub struct GcRefCell<T: Trace>(RefCell<T>);
impl<T: Trace> GcRefCell<T> {
    #[inline]
    pub fn new(value: T) -> Self {
        GcRefCell(RefCell::new(value))
    }
    #[inline]
    pub fn get_mut(&mut self) -> &mut T {
        self.0.get_mut()
    }
    #[inline]
    pub fn borrow_mut(&self) -> GcRefMut<T> {
        GcRefMut(self.0.borrow_mut())
    }
    #[inline]
    pub fn borrow(&self) -> GcRef<T> {
        GcRef(self.0.borrow())
    }

}
unsafe_trace_lock!(GcRefCell, target = T; |cell| cell.borrow());

pub struct GcRef<'a, T: 'a>(Ref<'a, T>);
impl<'a, T> Deref for GcRef<'a, T> {
    type Target = T;

    #[inline]
    fn deref(&self) -> &T {
        &self.0
    }
}


pub struct GcRefMut<'a, T: 'a>(RefMut<'a, T>);
impl<'a, T> Deref for GcRefMut<'a, T> {
    type Target = T;

    #[inline]
    fn deref(&self) -> &T {
        &self.0
    }
}
impl<'a, T> DerefMut for GcRefMut<'a, T> {
    #[inline]
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}
