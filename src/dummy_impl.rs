//! Dummy collector implementation for testing

use crate::{
    Trace, TraceImmutable, GcVisitor, NullTrace, CollectorId,
    GcSafe, GcSystem, GcContext,
};
use std::ptr::NonNull;

/// Fake a [Gc] that points to the specified value
///
/// This will never actually be collected
/// and will always be valid
pub fn gc<'gc, T: GcSafe + 'static>(ptr: &'static T) -> Gc<'gc, T> {
    unsafe {
        Gc::from_raw(
            DummyCollectorId { _priv: () },
            NonNull::from(ptr)
        )
    }
}

/// Allocate a [(fake) Gc](Gc) that points to the specified
/// value and leak it.
///
/// Since collection is unimplemented,
/// this intentionally leaks memory.
pub fn leaked<'gc, T: GcSafe + 'static>(value: T) -> Gc<'gc, T> {
    gc(Box::leak(Box::new(value)))
}

/// An fake [garbage collected pointer](::zerogc::Gc)
/// that uses the dummy collector system
///
/// This never actually collects any garbage
pub type Gc<'gc, T> = crate::Gc<'gc, T, DummyCollectorId>;

/// A dummy implementation of [crate::GcSystem]
/// which is useful for testing
///
/// This just blindly allocates memory and doesn't
/// actually do any collection.
pub struct DummyContext {
    _priv: ()
}
unsafe impl GcContext for DummyContext {
    type System = DummySystem;
    type Id = DummyCollectorId;

    unsafe fn basic_safepoint<T: Trace>(&mut self, _value: &mut &mut T) {
        // safepoints are a nop since there is nothing to track
    }

    unsafe fn freeze(&mut self) {
        unimplemented!()
    }

    unsafe fn unfreeze(&mut self) {
        unimplemented!()
    }

    unsafe fn recurse_context<T, F, R>(&self, value: &mut &mut T, func: F) -> R
        where T: Trace, F: for<'gc> FnOnce(&'gc mut Self, &'gc mut T) -> R {
        // safepoints are a nop since there is nothing to track
        let mut child = DummyContext { _priv: () };
        func(&mut child, &mut *value)
    }
}


/// A dummy implementation of [::zerogc::GcSystem]
/// which is useful for testing
///
/// All methods panic and this should never be used
/// in actual code.
#[derive(Default)]
pub struct DummySystem {
    _priv: ()
}
impl DummySystem {
    /// Create a new fake system for testing
    pub fn new() -> Self {
        DummySystem::default()
    }

    /// Create a [DummyContext]
    ///
    /// There are few restrictions on this
    /// because it doesn't actually do anything
    pub fn new_context(&self) -> DummyContext {
        DummyContext {
            _priv: ()
        }
    }
}
unsafe impl GcSystem for DummySystem {
    type Id = DummyCollectorId;
    type Context = DummyContext;
}

/// The id for a [dummy gc pointer](Gc)
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct DummyCollectorId {
    _priv: ()
}
unsafe impl Trace for DummyCollectorId {
    const NEEDS_TRACE: bool = false;

    fn visit<V: GcVisitor>(&mut self, _visitor: &mut V) -> Result<(), <V as GcVisitor>::Err> {
        Ok(())
    }
}
unsafe impl TraceImmutable for DummyCollectorId {
    fn visit_immutable<V: GcVisitor>(&self, _visitor: &mut V) -> Result<(), V::Err> {
        Ok(())
    }
}

unsafe impl NullTrace for DummyCollectorId {}
unsafe impl CollectorId for DummyCollectorId {
    type System = DummySystem;

    #[inline]
    fn from_gc_ptr<'a, 'gc, T>(_gc: &'a Gc<'gc, T>) -> &'a Self where T: GcSafe + ?Sized + 'gc, 'gc: 'a {
        const ID: DummyCollectorId = DummyCollectorId { _priv: () };
        &ID
    }

    unsafe fn gc_write_barrier<'gc, T, V>(
        _owner: &Gc<'gc, T>,
        _value: &Gc<'gc, V>,
        _field_offset: usize
    ) where T: GcSafe + ?Sized + 'gc, V: GcSafe + ?Sized + 'gc {}

    unsafe fn assume_valid_system(&self) -> &Self::System {
        unimplemented!()
    }
}