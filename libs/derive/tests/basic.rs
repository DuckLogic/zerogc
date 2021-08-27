#![feature(
    arbitrary_self_types, // Used for `zerogc(mutable)`
)]
use zerogc::{Gc, CollectorId, Trace, GcSafe, NullTrace, dummy_impl::{self, DummyCollectorId}};

use zerogc_derive::{Trace, NullTrace};
use zerogc::cell::GcCell;
use std::marker::PhantomData;

#[derive(Trace)]
pub struct SpecificCollector<'gc> {
    gc: Gc<'gc, i32, DummyCollectorId>,
    rec: Gc<'gc, SpecificCollector<'gc>, DummyCollectorId>,
    #[zerogc(mutable)]
    cell: GcCell<Gc<'gc, SpecificCollector<'gc>, DummyCollectorId>>
}

#[derive(Trace)]
pub struct Basic<'gc, #[zerogc(collector_id)] Id: CollectorId> {
    parent: Option<Gc<'gc, Basic<'gc, Id>, Id>>,
    children: Vec<Gc<'gc, Basic<'gc, Id>, Id>>,
    value: String,
    #[zerogc(mutable)]
    cell: GcCell<Option<Gc<'gc, Basic<'gc, Id>, Id>>>
}

#[derive(Copy, Clone, Trace)]
#[zerogc(copy)]
pub struct BasicCopy<'gc, #[zerogc(collector_id)] Id: CollectorId> {
    test: i32,
    value: i32,
    basic: Option<Gc<'gc, Basic<'gc, Id>, Id>>
}


#[derive(Copy, Clone, Trace)]
#[zerogc(copy)]
pub enum BasicEnum<'gc, #[zerogc(collector_id)] Id: CollectorId> {
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

/// A testing type that has no destructor
///
/// It doesn't implement copy, but shouldn't need
/// to be dropped.
///
/// We shouldn't need to annotate with `unsafe_skip_drop`.
/// The dummy destructor shouldn't be generated because of `#[zerogc(nop_trace)]`
#[derive(Trace)]
#[zerogc(nop_trace)]
#[allow(unused)]
struct NoDestructorNullTrace {
    val: i32,
    s: &'static str,
    f: f32
}

fn assert_copy<T: Copy>() {}
fn assert_null_trace<T: NullTrace>() {}
fn check_id<'gc, Id: CollectorId>() {
    assert_copy::<BasicCopy<'gc, Id>>();
    assert_copy::<Gc<'gc, BasicCopy<'gc, Id>, Id>>();
    assert_copy::<Gc<'gc, Basic<'gc, Id>, Id>>();
    assert_copy::<Option<Gc<'gc, Basic<'gc, Id>, Id>>>();

    assert!(<Basic<'gc, Id> as Trace>::NEEDS_DROP);
}

#[derive(NullTrace)]
#[allow(unused)]
struct NopTrace {
    s: String,
    i: i32,
    wow: Box<NopTrace>
}

#[derive(Trace)]
#[zerogc(unsafe_skip_drop)]
#[allow(unused)]
struct UnsafeSkipped<'gc> {
    s: &'static str,
    i: i32,
    #[zerogc(unsafe_skip_trace)]
    wow: Gc<'gc, i32, DummyCollectorId>
}

#[derive(Trace)]
#[zerogc(nop_trace)]
#[allow(unused)]
struct LifetimeTrace<#[zerogc(ignore)] 'a, 'gc, #[zerogc(ignore)] T: GcSafe<'gc, DummyCollectorId> + 'a> {
    s: String,
    i: i32,
    wow: Box<NopTrace>,
    other: &'a LifetimeTrace<'a, 'gc, T>,
    generic: Box<T>,
    marker: PhantomData<&'gc ()>
}


#[test]
fn basic<'gc>() {
    let _b = Basic::<dummy_impl::DummyCollectorId> {
        value: String::new(),
        parent: None,
        children: vec![],
        cell: GcCell::new(None)
    };
    assert!(<Basic::<dummy_impl::DummyCollectorId> as Trace>::NEEDS_TRACE);
    assert!(<BasicCopy::<dummy_impl::DummyCollectorId> as Trace>::NEEDS_TRACE);
    assert!(<Basic::<dummy_impl::DummyCollectorId> as Trace>::NEEDS_DROP);
    assert!(!<BasicCopy::<dummy_impl::DummyCollectorId> as Trace>::NEEDS_DROP);
    assert_copy::<BasicCopy::<dummy_impl::DummyCollectorId>>();
    assert_null_trace::<NopTrace>();
    assert!(!<NopTrace as Trace>::NEEDS_TRACE);

    check_id::<dummy_impl::DummyCollectorId>();

    // We explicitly skipped the only trace field
    assert!(!<UnsafeSkipped<'gc> as Trace>::NEEDS_TRACE);
    /*
     * We (unsafely) claimed drop-safety (w/ `unsafe_skip_drop`),
     * so we shouldn't generate a destructor
     *
     * Trace::NEEDS_DROP should already be false (since we have no Drop fields),
     * however in this case `std::mem::needs_drop` is false since we have no dummy drop impl.
     */
    assert!(!<UnsafeSkipped<'gc> as Trace>::NEEDS_DROP);
    assert!(!std::mem::needs_drop::<UnsafeSkipped<'gc>>());

    /*
     * Ensure that `NoDestructorNullTrace` doesn't need to be dropped
     *
     * The `nop_trace` automatically implies `unsafe_skip_drop` (safely)
     */
    assert_null_trace::<NoDestructorNullTrace>();
    assert!(!<NoDestructorNullTrace as Trace>::NEEDS_DROP);
    assert!(!std::mem::needs_drop::<NoDestructorNullTrace>());

}
