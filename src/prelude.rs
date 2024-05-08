//! The prelude for `zergogc`,
//! containing a set of commonly used
//! types and macros.
//!
//! This should really contain everything a garbage
//! collected program needs to use the API.

// macros
pub use crate::{freeze_context, safepoint, safepoint_recurse, unfreeze_context};
// Basic collector types
pub use crate::{Gc, GcContext, GcHandle, GcSimpleAlloc, GcSystem, GcVisitor, HandleCollectorId};
// Traits for user code to implement
pub use crate::cell::GcCell;
pub use crate::AssumeNotTraced;
pub use crate::CollectorId;
pub use crate::{GcRebrand, GcSafe, NullTrace, Trace, TraceImmutable, TrustedDrop};
