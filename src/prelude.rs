//! The prelude for `zergogc`,
//! containing a set of commonly used
//! types and macros.
//!
//! This should really contain everything a garbage
//! collected program needs to use the API.

// Basic collector types
pub use crate::system::GcSystem;
pub use crate::trace::{Gc, GcVisitor};
// Traits for user code to implement
pub use crate::cell::GcCell;
pub use crate::system::CollectorId;
pub use crate::trace::barrier::GcDirectBarrier;
pub use crate::trace::AssumeNotTraced;
pub use crate::trace::{GcRebrand, GcSafe, NullTrace, Trace, TraceImmutable, TrustedDrop};
