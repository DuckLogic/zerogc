use zerogc::{Gc, CollectorId, Trace, GcSafe};

use zerogc_derive::Trace;

#[derive(Trace)]
#[zerogc(ignore_params(Id))]
pub struct Basic<'gc, Id: CollectorId> {
    parent: Option<Gc<'gc, Basic<'gc, Id>, Id>>,
    children: Vec<Gc<'gc, Basic<'gc, Id>, Id>>,
    value: String
}

#[derive(Copy, Clone, Trace)]
#[zerogc(copy, ignore_params(Id))]
pub struct BasicCopy<'gc, Id: CollectorId> {
    test: i32,
    value: i32,
    basic: Option<Gc<'gc, Basic<'gc, Id>, Id>>
}

fn assert_copy<T: Copy>() {}
fn check_id<'gc, Id: CollectorId>() {
    assert_copy::<BasicCopy<'gc, Id>>();
    assert_copy::<Gc<'gc, BasicCopy<'gc, Id>, Id>>();
    assert_copy::<Gc<'gc, Basic<'gc, Id>, Id>>();
    assert_copy::<Option<Gc<'gc, Basic<'gc, Id>, Id>>>();

    assert!(<Basic<'gc, Id> as GcSafe>::NEEDS_DROP);
}

#[test]
fn basic() {
    let _b = Basic::<dummy::DummyCollectorId> {
        value: String::new(),
        parent: None,
        children: vec![]
    };
    assert!(<Basic::<dummy::DummyCollectorId> as Trace>::NEEDS_TRACE);
    assert!(<BasicCopy::<dummy::DummyCollectorId> as Trace>::NEEDS_TRACE);
    assert!(<Basic::<dummy::DummyCollectorId> as GcSafe>::NEEDS_DROP);
    assert!(!<BasicCopy::<dummy::DummyCollectorId> as GcSafe>::NEEDS_DROP);
    assert_copy::<BasicCopy::<dummy::DummyCollectorId>>();

    check_id::<dummy::DummyCollectorId>();
}

mod dummy {
    use zerogc::{
        Gc, Trace, GcSafe, GcSystem, GcContext, CollectorId,
        NullTrace, TraceImmutable, GcVisitor
    };

    pub struct DummyContext {}
    unsafe impl GcContext for DummyContext {
        type System = DummySystem;
        type Id = DummyCollectorId;

        unsafe fn basic_safepoint<T: Trace>(&mut self, _value: &mut &mut T) {
            unimplemented!()
        }

        unsafe fn freeze(&mut self) {
            unimplemented!()
        }

        unsafe fn unfreeze(&mut self) {
            unimplemented!()
        }

        unsafe fn recurse_context<T, F, R>(&self, _value: &mut &mut T, _func: F) -> R where T: Trace, F: for<'gc> FnOnce(&'gc mut Self, &'gc mut T) -> R {
            unimplemented!()
        }
    }


    pub struct DummySystem {}
    unsafe impl GcSystem for DummySystem {
        type Id = DummyCollectorId;
        type Context = DummyContext;
    }

    #[derive(Copy, Clone, Debug, Eq, PartialEq)]
    pub struct DummyCollectorId {}
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

        unsafe fn gc_write_barrier<'gc, T, V>(
            _owner: &Gc<'gc, T, Self>,
            _value: &Gc<'gc, V, Self>,
            _field_offset: usize
        ) where T: GcSafe + ?Sized + 'gc, V: GcSafe + ?Sized + 'gc {}

        unsafe fn assume_valid_system(&self) -> &Self::System {
            unimplemented!()
        }
    }
}