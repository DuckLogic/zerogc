use zerogc_simple::{SimpleCollector, GcConfig};
use std::sync::atomic::AtomicBool;
use slog::Logger;
use zerogc::GcSimpleAlloc;

fn test_collector() -> SimpleCollector {
    let mut config = GcConfig::default();
    config.always_force_collect = true; // Force collections for predictability
    SimpleCollector::with_config(
        config, Logger::root(::slog::Discard, ::slog::o!())
    )
}

#[test]
fn int_array() {
    let collector = test_collector();
    let context = collector.into_context();
    let res = context.alloc_slice_copy(12u32, 5);
    assert_eq!(*res.value(), *vec![12u32; 5]);
}