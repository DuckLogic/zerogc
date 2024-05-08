//! The prelude for `zergogc`,
//! containing a set of commonly used
//! types and macros.
//!
//! This should really contain everything a garbage
//! collected program needs to use the API.

// Basic collector types
pub use crate::{Gc, GcHandle, GcSystem, GcVisitor, HandleCollectorId};
// Traits for user code to implement
pub use crate::cell::GcCell;
pub use crate::AssumeNotTraced;
pub use crate::CollectorId;
pub use crate::{GcRebrand, GcSafe, NullTrace, Trace, TraceImmutable, TrustedDrop};
