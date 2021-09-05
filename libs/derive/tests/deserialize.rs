use std::marker::PhantomData;

use zerogc_derive::{GcDeserialize, NullTrace, Trace};

use zerogc::SimpleAllocCollectorId;
use zerogc::prelude::*;
use zerogc::epsilon::{EpsilonCollectorId};
use serde::Deserialize;

#[derive(Trace, GcDeserialize)]
#[zerogc(collector_ids(EpsilonCollectorId))]
struct BasicDeserialize<'gc> {
    test: Gc<'gc, String, EpsilonCollectorId>
}

#[derive(Trace, GcDeserialize)]
#[zerogc(collector_ids(Id))]
struct DeserializeParameterized<'gc, T: GcSafe<'gc, Id>, Id: SimpleAllocCollectorId> {
    test: Gc<'gc, Vec<T>, Id>
}

#[derive(NullTrace, GcDeserialize, Deserialize)]
#[zerogc(serde(delegate))]
#[allow(unused)]
struct DelegatingDeserialize {
    foo: String,
    bar: i32,
    doesnt: DoesntImplGcDeserialize,
}


#[derive(Trace, GcDeserialize)]
#[allow(unused)]
#[zerogc(collector_ids(Id))]
struct DeserializeWith<'gc, Id: CollectorId> {
    foo: String,
    #[zerogc(serde(delegate))]
    doesnt_gc_deser: DoesntImplGcDeserialize,
    #[zerogc(serde(deserialize_with = "but_its_a_unit", bound(deserialize = "")))]
    doesnt_deser_at_all: DoesntDeserAtAll,
    marker: PhantomData<&'gc Id>
}

#[derive(NullTrace, serde::Deserialize)]
#[allow(unused)]
struct DoesntImplGcDeserialize {
    foo: String
}

fn but_its_a_unit<'de, D: serde::Deserializer<'de>>(_deser: D) -> Result<DoesntDeserAtAll, D::Error> {
    Ok(DoesntDeserAtAll {})
}

#[derive(NullTrace)]
struct DoesntDeserAtAll {

}