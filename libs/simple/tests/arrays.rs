use slog::Logger;

use zerogc::GcSimpleAlloc;
use zerogc::safepoint;
use zerogc_derive::Trace;

use zerogc_simple::{SimpleCollector, GcArray, GcVec, Gc, CollectorId as SimpleCollectorId, GcConfig};

fn test_collector() -> SimpleCollector {
    let mut config = GcConfig::default();
    config.always_force_collect = true; // Force collections for predictability
    SimpleCollector::with_config(
        config, Logger::root(::slog::Discard, ::slog::o!())
    )
}

#[derive(Trace, Copy, Clone, Debug)]
#[zerogc(copy, collector_ids(SimpleCollectorId))]
struct Dummy<'gc> {
    val: usize,
    inner: Option<Gc<'gc, Dummy<'gc>>>
}

#[test]
fn array() {
    let collector = test_collector();
    let mut context = collector.into_context();
    let array1 = context.alloc_slice_fill_copy(5, 12u32);
    assert_eq!(*array1.as_slice(), *vec![12u32; 5]);
    safepoint!(context, ());
    const TEXT: &[u8] = b"all cows eat grass";
    let array_text = context.alloc_slice_copy(TEXT);
    let array_none: GcArray<Option<usize>> = context.alloc_slice_none(12);
    for val in array_none.as_slice() {
        assert_eq!(*val, None);
    }
    let array_text = safepoint!(context,  array_text);
    assert_eq!(array_text.as_slice(), TEXT);
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
    for (idx, val) in nested_trace.as_slice().iter().enumerate() {
        assert_eq!(val.val, idx, "Invalid val: {:?}", val);
        if let Some(last) = val.inner {
            assert_eq!(last.val, idx - 1);
        }
    }
}

#[test]
fn vec() {
    let collector = test_collector();
    let mut context = collector.into_context();
    let mut vec1 = context.alloc_vec();
    for _ in 0..5 {
        vec1.push(12u32);
    }
    assert_eq!(*vec1.as_slice(), *vec![12u32; 5]);
    drop(vec1);
    safepoint!(context, ());
    const TEXT: &[u8] = b"all cows eat grass";
    let mut vec_text = context.alloc_vec();
    vec_text.extend_from_slice(TEXT);
    let mut vec_none: GcVec<Option<usize>> = context.alloc_vec_with_capacity(12);
    for _ in 0..12 {
        vec_none.push(None);
    }
    for val in vec_none.iter() {
        assert_eq!(*val, None);
    }
    drop(vec_none);
    let vec_text: GcVec<u8> = GcVec { raw: safepoint!(context, vec_text.into_raw()), context: &context };
    assert_eq!(vec_text.as_slice(), TEXT);
    let mut nested_trace: GcVec<Gc<Dummy>> = context.alloc_vec_with_capacity(3);
    let mut last = None;
    for i in 0..16 {
        let obj = context.alloc(Dummy {
            val: i,
            inner: last
        });
        nested_trace.push(obj);
        last = Some(obj);
    }
    drop(vec_text);
    let nested_trace: GcVec<Gc<Dummy>> = GcVec{ raw: safepoint!(context, nested_trace.into_raw()), context: &context };
    for (idx, val) in nested_trace.iter().enumerate() {
        assert_eq!(val.val, idx, "Invalid val: {:?}", val);
        if let Some(last) = val.inner {
            assert_eq!(last.val, idx - 1);
        }
    }
}
