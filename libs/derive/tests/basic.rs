#![feature(
    arbitrary_self_types, // Used for `zerogc(mutable)`
)]
use zerogc::{Gc, CollectorId, Trace, GcSafe, NullTrace, epsilon::{self, EpsilonCollectorId}};

use zerogc::cell::GcCell;
use std::marker::PhantomData;
use std::fmt::Debug;

#[derive(Trace)]
#[zerogc(collector_ids(EpsilonCollectorId))]
pub struct SpecificCollector<'gc> {
    gc: Gc<'gc, i32, EpsilonCollectorId>,
    rec: Gc<'gc, SpecificCollector<'gc>, EpsilonCollectorId>,
    #[zerogc(mutable)]
    cell: GcCell<Gc<'gc, SpecificCollector<'gc>, EpsilonCollectorId>>
}

#[derive(Trace)]
#[zerogc(collector_ids(Id))]
pub struct Basic<'gc, Id: CollectorId> {
    parent: Option<Gc<'gc, Basic<'gc, Id>, Id>>,
    children: Vec<Gc<'gc, Basic<'gc, Id>, Id>>,
    value: String,
    #[zerogc(mutable)]
    cell: GcCell<Option<Gc<'gc, Basic<'gc, Id>, Id>>>
}

#[derive(Copy, Clone, Trace)]
#[zerogc(copy, collector_ids(Id))]
pub struct BasicCopy<'gc, Id: CollectorId> {
    test: i32,
    value: i32,
    basic: Option<Gc<'gc, Basic<'gc, Id>, Id>>
}


#[derive(Copy, Clone, Trace)]
#[zerogc(copy, collector_ids(Id))]
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

/// A testing type that has no destructor
///
/// It doesn't implement copy, but shouldn't need
/// to be dropped.
///
/// We shouldn't need to annotate with `unsafe_skip_drop`.
/// The dummy destructor shouldn't be generated because of `#[zerogc(nop_trace)]`
#[derive(NullTrace)]
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
#[zerogc(unsafe_skip_drop, collector_ids(EpsilonCollectorId))]
#[allow(unused)]
struct UnsafeSkipped<'gc> {
    s: &'static str,
    i: i32,
    #[zerogc(unsafe_skip_trace)]
    wow: Gc<'gc, i32, EpsilonCollectorId>,
    #[zerogc(unsafe_skip_trace)]
    not_impld: NotImplTrace
}


/// A type that doesn't implement `Trace`
struct NotImplTrace;

#[derive(Trace)]
#[zerogc(ignore_lifetimes("'a"), immutable, collector_ids(EpsilonCollectorId))]
#[allow(unused)]
struct LifetimeTrace<'a: 'gc, 'gc, T: GcSafe<'gc, EpsilonCollectorId> + 'a> {
    s: String,
    i: i32,
    wow: Box<NopTrace>,
    other: &'a u32,
    generic: Box<T>,
    marker: PhantomData<&'gc ()>
}

#[derive(Trace)]
#[zerogc(copy, collector_ids(Id), ignore_params(T))]
struct IgnoredParam<'gc, T: Debug + 'gc, Id: CollectorId> {
    gc: Gc<'gc, IgnoredParam<'gc, T, Id>, Id>,
    param: PhantomData<fn() -> T>,
}

#[test]
fn basic<'gc>() {
    let _b = Basic::<epsilon::EpsilonCollectorId> {
        value: String::new(),
        parent: None,
        children: vec![],
        cell: GcCell::new(None)
    };
    assert!(<Basic::<epsilon::EpsilonCollectorId> as Trace>::NEEDS_TRACE);
    assert!(<BasicCopy::<epsilon::EpsilonCollectorId> as Trace>::NEEDS_TRACE);
    assert!(<Basic::<epsilon::EpsilonCollectorId> as Trace>::NEEDS_DROP);
    assert!(!<BasicCopy::<epsilon::EpsilonCollectorId> as Trace>::NEEDS_DROP);
    assert_copy::<BasicCopy::<epsilon::EpsilonCollectorId>>();
    assert_null_trace::<NopTrace>();
    assert!(!<NopTrace as Trace>::NEEDS_TRACE);

    check_id::<epsilon::EpsilonCollectorId>();

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
