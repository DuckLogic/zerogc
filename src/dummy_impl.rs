//! Dummy collector implementation for testing

use zerogc_derive::unsafe_gc_impl;

use crate::{
    Trace, TraceImmutable, GcVisitor, NullTrace, CollectorId,
    GcSafe, GcSystem, GcContext, GcRef, GcRebrand, GcErase
};
use std::ptr::NonNull;
use std::marker::PhantomData;

/// Fake a [Gc] that points to the specified value
///
/// This will never actually be collected
/// and will always be valid
pub fn gc<'gc, T: GcSafe + 'static>(ptr: &'static T) -> DummyGc<'gc, T> {
    unsafe {
        DummyGc::from_raw(
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
pub fn leaked<'gc, T: GcSafe + 'static>(value: T) -> DummyGc<'gc, T> {
    gc(Box::leak(Box::new(value)))
}

/// An fake [garbage collected pointer](::zerogc::Gc)
/// that uses the dummy collector system
///
/// **WARNING:** This never actually collects any garbage.
/// This is **only for testing purposes**.
#[repr(C)]
pub struct DummyGc<'gc, T> {
    ptr: NonNull<T>,
    id: DummyCollectorId,
    marker: PhantomData<&'gc T>,
}
unsafe impl<'gc, T: GcSafe + 'gc> GcRef<'gc, T> for DummyGc<'gc, T> {
    type Id = DummyCollectorId;
    fn collector_id(&self) -> Self::Id {
        self.id
    }
    unsafe fn from_raw(id: DummyCollectorId, ptr: NonNull<T>) -> Self {
        DummyGc { ptr, id, marker: PhantomData }
    }
    fn value(&self) -> &'gc T {
        unsafe { &*self.ptr.as_ptr() }
    }
    unsafe fn as_raw_ptr(&self) -> *mut T {
        self.ptr.as_ptr()
    }
    fn system(&self) -> &'_ <Self::Id as CollectorId>::System {
        // This assumption is safe - see the docs
        unsafe { self.id.assume_valid_system() }
    }
}
impl<'gc, T: GcSafe + 'gc> Copy for DummyGc<'gc, T> {}
impl<'gc, T: GcSafe + 'gc> Clone for DummyGc<'gc, T> {
    fn clone(&self) -> Self {
        *self
    }
}
unsafe_gc_impl!(
    target => DummyGc<'gc, T>,
    params => ['gc, T: GcSafe + 'gc],
    null_trace => never,
    bounds => {
        TraceImmutable => never,
        GcRebrand => { where T: GcRebrand<'new_gc, Id>, T::Branded: GcSafe },
        GcErase => { where T: GcErase<'min, Id>, T::Erased: GcSafe }
    },
    branded_type => DummyGc<'new_gc, T::Branded>,
    erased_type => DummyGc<'min, T::Erased>,
    NEEDS_TRACE => true,
    NEEDS_DROP => false, // Copy
    trace_mut => |self, visitor| {
        unsafe { visitor.visit_gc(self) }
    }
);
impl<'gc, T: GcSafe + 'gc> std::ops::Deref for DummyGc<'gc, T> {
    type Target = &'gc T;
    fn deref(&self) -> &&'gc T {
        unsafe { &*(&self.ptr as *const NonNull<T> as *const &T) }
    }
}

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

    unsafe fn assume_valid_system(&self) -> &Self::System {
        unimplemented!()
    }
}