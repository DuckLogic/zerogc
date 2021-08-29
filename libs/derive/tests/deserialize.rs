use zerogc_derive::{GcDeserialize, Trace};

use zerogc::SimpleAllocCollectorId;
use zerogc::prelude::*;
use zerogc::dummy_impl::{DummyCollectorId};

#[derive(Trace, GcDeserialize)]
#[zerogc(collector_ids(DummyCollectorId))]
struct BasicDeserialize<'gc> {
    test: Gc<'gc, String, DummyCollectorId>
}

#[derive(Trace, GcDeserialize)]
#[zerogc(collector_ids(Id))]
struct DeserializeParameterized<'gc, T: GcSafe<'gc, Id>, Id: SimpleAllocCollectorId> {
    test: Gc<'gc, Vec<T>, Id>
}