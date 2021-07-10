#![feature(thread_local_const_init)]
use core::cell::Cell;

use zerogc::{Trace, safepoint, DynTrace, trait_object_trace, GcSimpleAlloc};
use zerogc_derive::Trace;

use zerogc_simple::{SimpleCollector, Gc, CollectorId as SimpleCollectorId, GcConfig};
use slog::Logger;

fn test_collector() -> SimpleCollector {
    let mut config = GcConfig::default();
    config.always_force_collect = true; // Force collections for predictability
    SimpleCollector::with_config(
        config, Logger::root(::slog::Discard, ::slog::o!())
    )
}

trait Foo<'gc>: 'gc + DynTrace {
    fn method(&self) -> i32;
    fn validate(&self);
}
trait_object_trace!(impl<'gc,> Trace for dyn Foo<'gc>; Branded<'new_gc> => dyn Foo<'new_gc>, Erased<'min> => dyn Foo<'min>);

fn foo<'gc, T: ?Sized + Trace + Foo<'gc>>(t: &T) -> i32 {
    assert_eq!(t.method(), 12);
    t.method() * 2
}
fn bar<'gc>(gc: Gc<'gc, dyn Foo + 'gc>) -> i32 {
    foo(gc.value())
}
#[derive(Trace)]
#[zerogc(collector_id(SimpleCollectorId), unsafe_skip_drop)]
struct Bar<'gc> {
    inner: Option<Gc<'gc, Bar<'gc>>>,
    val: Gc<'gc, i32>
}
impl<'gc> Foo<'gc> for Bar<'gc> {
    fn method(&self) -> i32 {
       **self.val
    }
    fn validate(&self) {
        assert_eq!(**self.val, 12);
        assert_eq!(**self.inner.unwrap().val, 4);
    }
}
impl<'gc> Drop for Bar<'gc> {
    fn drop(&mut self) {
        BAR_DROP_COUNT.with(|val| {
            val.set(val.get() + 1);
        })
    }
}

thread_local! {
    static BAR_DROP_COUNT: Cell<u32> = const { Cell::new(0) };
}
#[test]
fn foo_bar() {
    let collector = test_collector();
    let mut context = collector.into_context();
    let val = context.alloc(12);
    let inner = context.alloc(Bar { inner: None, val: context.alloc(4) });
    let gc: Gc<'_, dyn Foo<'_>> = context.alloc(Bar { inner: Some(inner), val });
    assert_eq!(bar(gc), 24);
    // Should be traced correctly
    let gc = safepoint!(context, gc);
    assert_eq!(BAR_DROP_COUNT.with(Cell::get),  0, "Expected Bar to be retained");
    gc.validate();
    // Trace inner, should end up dropping Bar
    safepoint!(context, ());
    assert_eq!(BAR_DROP_COUNT.with(Cell::get), 2, "Expected Bar to be dropped");
}