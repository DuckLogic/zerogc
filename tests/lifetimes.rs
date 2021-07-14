use zerogc::dummy_impl::{Gc, DummyContext, DummyCollectorId, leaked, DummySystem};
use zerogc_derive::Trace;
use zerogc::GcSimpleAlloc;
use zerogc::{safepoint, safepoint_recurse};

#[derive(Trace)]
#[zerogc(collector_id(DummyCollectorId))]
struct ComplexRoot<'gc> {
    usage: Gc<'gc, u32>
}

fn complex_lifetime_usage<'gc>(ctx: &'gc mut DummyContext, root: ComplexRoot<'gc>, new_val: u32) -> ComplexRoot<'gc> {
    let mut root = safepoint!(ctx, root);
    root.usage = ctx.alloc(new_val);
    root.usage
}

#[test]
fn lifetime_usage() {
    let system = DummySystem::new();
    let mut ctx = system.new_context();
    let root = ComplexRoot { usage: ctx.alloc(31) };
    let (root, result_val) = safepoint_recurse!(ctx, root, @managed_result, |sub_ctx, root| {
        complex_lifetime_usage(sub_ctx, root, 12)
    });
    assert_eq!(*root.usage.value(), 12);
    assert_eq!(result_val, 12);

}