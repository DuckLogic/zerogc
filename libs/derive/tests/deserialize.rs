use zerogc_derive::{GcDeserialize, NullTrace, Trace};

use zerogc::SimpleAllocCollectorId;
use zerogc::prelude::*;
use zerogc::epsilon::{EpsilonCollectorId};

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

#[derive(NullTrace, GcDeserialize)]
enum PlainEnum {

}