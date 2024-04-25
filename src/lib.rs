#![feature(
    inline_const, // will be stabilized in next release
)]
use std::fmt::Debug;

pub mod collect;
pub mod context;
mod gcptr;
pub(crate) mod utils;

pub use self::collect::{Collect, NullCollect};
pub use self::context::{CollectContext, CollectorId, GarbageCollector};
