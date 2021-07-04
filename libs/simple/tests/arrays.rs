use slog::Logger;

use zerogc::GcSimpleAlloc;
use zerogc::safepoint;
use zerogc_derive::Trace;

use zerogc_simple::{SimpleCollector, GcArray, Gc, CollectorId as SimpleCollectorId, GcConfig};

fn test_collector() -> SimpleCollector {
    let mut config = GcConfig::default();
    config.always_force_collect = true; // Force collections for predictability
    SimpleCollector::with_config(
        config, Logger::root(::slog::Discard, ::slog::o!())
    )
}

#[derive(Trace, Copy, Clone, Debug)]
#[zerogc(copy, collector_id(SimpleCollectorId))]
struct Dummy<'gc> {
    val: usize,
    inner: Option<Gc<'gc, Dummy<'gc>>>
}

#[test]
fn int_array() {
    let collector = test_collector();
    let mut context = collector.into_context();
    let array1 = context.alloc_slice_fill_copy(5, 12u32);
    assert_eq!(*array1.value(), *vec![12u32; 5]);
    safepoint!(context, ());
    const TEXT: &[u8] = b"all cows eat grass";
    let array_text = context.alloc_slice_copy(TEXT);
    let array_none: GcArray<Option<usize>> = context.alloc_slice_none(12);
    for val in array_none.value() {
        assert_eq!(*val, None);
    }
    let array_text = safepoint!(context,  array_text);
    assert_eq!(array_text.0.value(), TEXT);
    let mut nested_trace = Vec::new();
    let mut last = None;
    for i in 0..16 {
        let obj = context.alloc(Dummy {
            val: i,
            inner: last
        });
        nested_trace.push(obj);
        last = Some(obj);
    }
    let nested_trace = context.alloc_slice_copy(nested_trace.as_slice());
    let nested_trace: GcArray<Gc<Dummy>> = safepoint!(context, nested_trace);
    for (idx, val) in nested_trace.value().iter().enumerate() {
        assert_eq!(val.val, idx, "Invalid val: {:?}", val);
        if let Some(last) = val.inner {
            assert_eq!(last.val, idx - 1);
        }
    }
}
