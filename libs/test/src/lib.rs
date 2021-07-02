#![feature(
    arbitrary_self_types, // Unfortunately this is required for methods on Gc refs
    generic_associated_types, // We need more abstraction
)]
use zerogc::{
    GcContext, GcSystem, GcSimpleAlloc, GcSafe, CollectorId, GcHandleSystem, GcErase,
    GcRebrand, GcHandle, GcBindHandle
};
use slog::Logger;

pub mod examples;

pub trait TestContext: GcContext + GcSimpleAlloc {
}

pub trait GcTest: GcSystem + GcHandleSystem {
    type Ctx: TestContext<Id=Self::Id>;
    fn create_collector(logger: Logger) -> Self;
    fn into_context(self) -> Self::Ctx;
}
pub trait GcSyncTest: Sync + GcTest {
    fn create_context(&self) -> Self::Ctx;
}

