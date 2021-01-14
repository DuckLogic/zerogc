use zerogc::{Gc, CollectorId, Trace, GcSafe, NullTrace, dummy_impl};

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


#[derive(Copy, Clone, Trace)]
#[zerogc(copy, ignore_params(Id))]
pub enum BasicEnum<'gc, Id: CollectorId> {
    Unit,
    Tuple(i32),
    First {
        all: i32
    },
    Second {
        you: Gc<'gc, String, Id>,
        need: bool
    },
    Third {
        is: (),
        love: Gc<'gc, BasicEnum<'gc, Id>, Id>
    },
    Fifth(
        Gc<'gc, BasicEnum<'gc, Id>, Id>,
        Gc<'gc, Basic<'gc, Id>, Id>
    )
}

fn assert_copy<T: Copy>() {}
fn assert_null_trace<T: NullTrace>() {}
fn check_id<'gc, Id: CollectorId>() {
    assert_copy::<BasicCopy<'gc, Id>>();
    assert_copy::<Gc<'gc, BasicCopy<'gc, Id>, Id>>();
    assert_copy::<Gc<'gc, Basic<'gc, Id>, Id>>();
    assert_copy::<Option<Gc<'gc, Basic<'gc, Id>, Id>>>();

    assert!(<Basic<'gc, Id> as GcSafe>::NEEDS_DROP);
}

#[derive(Trace)]
#[zerogc(nop_trace)]
#[allow(unused)]
struct NopTrace {
    s: String,
    i: i32,
    wow: Box<NopTrace>
}

#[cfg(disabled)] // TODO: Fixup
#[derive(Trace)]
#[zerogc(nop_trace, ignore_lifetimes("'a"), ignore_params(T))]
#[allow(unused)]
struct LifetimeTrace<'a, T: GcSafe> {
    s: String,
    i: i32,
    wow: Box<NopTrace>,
    other: &'a LifetimeTrace<'a, T>,
    generic: Box<T>
}


#[test]
fn basic() {
    let _b = Basic::<dummy_impl::DummyCollectorId> {
        value: String::new(),
        parent: None,
        children: vec![]
    };
    assert!(<Basic::<dummy_impl::DummyCollectorId> as Trace>::NEEDS_TRACE);
    assert!(<BasicCopy::<dummy_impl::DummyCollectorId> as Trace>::NEEDS_TRACE);
    assert!(<Basic::<dummy_impl::DummyCollectorId> as GcSafe>::NEEDS_DROP);
    assert!(!<BasicCopy::<dummy_impl::DummyCollectorId> as GcSafe>::NEEDS_DROP);
    assert_copy::<BasicCopy::<dummy_impl::DummyCollectorId>>();
    assert_null_trace::<NopTrace>();
    assert!(!<NopTrace as Trace>::NEEDS_TRACE);

    check_id::<dummy_impl::DummyCollectorId>();
}
