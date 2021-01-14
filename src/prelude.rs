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
    GcSafe, GcErase, GcRebrand, Trace, TraceImmutable, NullTrace
};
// Hack traits
pub use crate::{GcBindHandle};
// Utils
pub use crate::AssumeNotTraced;
pub use crate::cell::GcCell;