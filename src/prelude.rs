//! The prelude for `zergogc`,
//! containing a set of commonly used
//! types and macros.
//!
//! This should really contain everything a garbage
//! collected program needs to use the API.

// macros
pub use crate::{
    safepoint,
    safepoint_recurse,
    freeze_context,
    unfreeze_context
};
// Basic collector types
pub use crate::{
    GcSystem, GcContext, GcSimpleAlloc,
    Gc, GcHandle, GcVisitor
};
// Traits for user code to implement
pub use crate::{
    GcSafe, GcRebrand, Trace, TraceImmutable, NullTrace, TrustedDrop
};
// TODO: Should this trait be auto-imported???
pub use crate::CollectorId;
// Utils
pub use crate::AssumeNotTraced;
pub use crate::cell::GcCell;