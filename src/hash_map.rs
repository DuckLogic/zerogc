//! A garbage collected HashMap implementation
//!
//! Right now, the only implementation
//! is [GcIndexMap]. It is a garbage collected
//! version of [indexmap::IndexMap](https://docs.rs/indexmap/1.7.0/indexmap/map/struct.IndexMap.html).
//!
//! In the future, unordered maps may be possible
//! (although they'll likely require much more work).
pub mod indexmap;

/// The default hasher for garbage collected maps.
pub type DefaultHasher = ahash::RandomState;

pub use self::indexmap::GcIndexMap;
