use std::fmt::Debug;

use zerogc_derive::Trace;

use zerogc::{
    Gc, GcArray, GcSimpleAlloc, safepoint_recurse,
};
use zerogc::epsilon::{self, EpsilonCollectorId, EpsilonContext, EpsilonSystem};

#[derive(Trace)]
#[zerogc(collector_ids(EpsilonCollectorId))]
pub struct Test<'gc> {
    val: i32,
    rec: Option<Gc<'gc, Test<'gc>, EpsilonCollectorId>>
}

fn recurse<'gc>(ctx: &'gc EpsilonContext, val: i32, test: Gc<'gc, Test, EpsilonCollectorId>) {
    let res = ctx.alloc(Test {
        val: 52,
        rec: Some(test),
    });
    assert_eq!(res.rec.unwrap().val, val);
}

#[test]
fn simple() {
    let leaking = EpsilonSystem::leak();
    let ctx = leaking.new_context();
    assert_eq!(*ctx.alloc(14i32).value(), 14);
    assert_eq!(ctx.alloc_slice_copy(b"foo").as_slice(), b"foo");
    assert_eq!(ctx.alloc(Test {
        val: 42,
        rec: None
    }).val, 42);
}

#[test]
fn recursive() {
    let leaking = EpsilonSystem::leak();
    let mut ctx = leaking.new_context();
    let first = ctx.alloc(Test{
        val: 18,
        rec: None
    });
    safepoint_recurse!(ctx, first, |ctx, root| recurse(ctx, 18, root));
}

#[test]
fn static_alloc() {
    fn recurse<'gc, T: ?Sized + PartialEq + Debug>(
        _ctx: &'gc EpsilonContext,
        expected: &T,
        test: Gc<'gc, T, EpsilonCollectorId>
    ) {
        assert_eq!(test.value(), expected);
    } 
    fn recurse_array<'gc, T: PartialEq + Debug>(
        _ctx: &'gc EpsilonContext,
        expected: &[T],
        test: GcArray<'gc, T, EpsilonCollectorId>
    ) {
        assert_eq!(test.as_slice(), expected);
    } 
    const BAR: &i32 = &12;
    let sys = EpsilonSystem::leak();
    let ctx = sys.new_context();
    recurse(&ctx, BAR, epsilon::gc(BAR));
    const FOO: &[u8] = b"Do you wanna build a snowman?";
    let array: GcArray<u8, EpsilonCollectorId>
        = epsilon::gc_array(FOO);
    recurse_array(&ctx, &*FOO, array);
}



