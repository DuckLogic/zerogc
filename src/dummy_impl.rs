//! Dummy collector implementation for testing

use crate::{CollectorId, GcContext, GcSafe, GcSimpleAlloc, GcSystem, GcVisitor, NullTrace, Trace, TraceImmutable, GcArray, TrustedDrop};
use std::ptr::NonNull;
use std::mem::MaybeUninit;

/// Fake a [Gc] that points to the specified value
///
/// This will never actually be collected
/// and will always be valid
pub fn gc<'gc, T: GcSafe<'gc, DummyCollectorId> + 'gc>(ptr: &'gc T) -> Gc<'gc, T> {
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
pub fn leaked<'gc, T: GcSafe<'gc, DummyCollectorId> + 'static>(value: T) -> Gc<'gc, T> {
    gc(Box::leak(Box::new(value)))
}

/// An fake [garbage collected pointer](`crate::Gc`)
/// that uses the dummy collector system
///
/// This never actually collects any garbage
pub type Gc<'gc, T> = crate::Gc<'gc, T, DummyCollectorId>;

/// A dummy implementation of [`crate::GcSystem`]
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

    unsafe fn unchecked_safepoint<T: Trace>(&self, _value: &mut &mut T) {
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


/// A dummy implementation of [GcSystem]
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
unsafe impl GcSimpleAlloc for DummyContext {
    unsafe fn alloc_uninit<'gc, T>(&'gc self) -> (Self::Id, *mut T) where T: GcSafe<'gc, DummyCollectorId> + 'gc {
        (DummyCollectorId { _priv: () }, Box::leak(Box::<T>::new_uninit()) as *mut MaybeUninit<T> as *mut T)
    }

    fn alloc<'gc, T>(&'gc self, value: T) -> crate::Gc<'gc, T, Self::Id>
        where T: GcSafe<'gc, Self::Id> + 'gc {
        gc(Box::leak(Box::new(value)))
    }

    unsafe fn alloc_uninit_slice<'gc, T>(&'gc self, len: usize) -> (Self::Id, *mut T)
        where T: GcSafe<'gc, Self::Id> + 'gc {
        let mut res: Vec<T> = Vec::with_capacity(len);
        res.set_len(len);
        let res = Vec::leak(res);
        (DummyCollectorId { _priv: () }, res.as_ptr() as *mut T)
    }

    fn alloc_vec<'gc, T>(&'gc self) -> crate::vec::GcVec<'gc, T, Self>
        where T: GcSafe<'gc, Self::Id> + 'gc {
        todo!("Vec alloc")
    }

    fn alloc_vec_with_capacity<'gc, T>(&'gc self, _capacity: usize) -> crate::vec::GcVec<'gc, T, Self>
        where T: GcSafe<'gc, Self::Id> + 'gc {
        todo!("Vec alloc")
    }
}

/// The id for a [dummy gc pointer](Gc)
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct DummyCollectorId {
    _priv: ()
}
unsafe impl TrustedDrop for DummyCollectorId {}
unsafe impl<'other_gc, OtherId: CollectorId> GcSafe<'other_gc, OtherId> for DummyCollectorId {}
unsafe impl Trace for DummyCollectorId {
    const NEEDS_TRACE: bool = false;
    const NEEDS_DROP: bool = false;

    fn visit<V: GcVisitor>(&mut self, _visitor: &mut V) -> Result<(), <V as GcVisitor>::Err> {
        Ok(())
    }

    unsafe fn visit_inside_gc<'gc, V, Id>(gc: &mut crate::Gc<'gc, Self, Id>, visitor: &mut V) -> Result<(), V::Err> where V: GcVisitor, Id: CollectorId, Self: GcSafe<'gc, Id> + 'gc {
        visitor.visit_gc(gc)
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
    type RawVecRepr<'gc> = crate::vec::repr::Unsupported<'gc, Self>;

    #[inline]
    fn from_gc_ptr<'a, 'gc, T>(_gc: &'a Gc<'gc, T>) -> &'a Self where T: GcSafe<'gc, Self> + ?Sized + 'gc, 'gc: 'a {
        const ID: DummyCollectorId = DummyCollectorId { _priv: () };
        &ID
    }

    fn resolve_array_len<'gc, T>(_array: GcArray<'gc, T, Self>) -> usize where T: GcSafe<'gc, Self> + 'gc {
        todo!()
    }

    fn resolve_array_id<'a, 'gc, T>(_gc: &'a GcArray<'gc, T, Self>) -> &'a Self where T: GcSafe<'gc, Self> + 'gc, 'gc: 'a {
        todo!()
    }


    unsafe fn gc_write_barrier<'gc, T, V>(
        _owner: &Gc<'gc, T>,
        _value: &Gc<'gc, V>,
        _field_offset: usize
    ) where T: GcSafe<'gc, Self> + ?Sized + 'gc, V: GcSafe<'gc, Self> + ?Sized + 'gc {}

    unsafe fn assume_valid_system(&self) -> &Self::System {
        unimplemented!()
    }
}