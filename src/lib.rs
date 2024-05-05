extern crate core;

pub mod collect;
pub mod context;
mod gcptr;
pub(crate) mod utils;

pub use self::collect::{Collect, NullCollect};
pub use self::context::{CollectContext, CollectorId, GarbageCollector};

pub use self::gcptr::Gc;
