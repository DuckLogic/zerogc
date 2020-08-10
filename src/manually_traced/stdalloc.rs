//! Implementations for types in the standard `alloc` crate
//!
//! These can be used in `#![no_std]` crates without requiring
//! the entire standard library.
use alloc::rc::Rc;
use alloc::sync::Arc;
use alloc::vec::Vec;
use alloc::boxed::Box;

use crate::prelude::*;

// NOTE: Delegate to slice to avoid code duplication
unsafe_trace_deref!(Vec, target = { [T] }; T);
unsafe_trace_deref!(Box, target = T);
// We can only trace `Rc` and `Arc` if the inner type implements `TraceImmutable`
unsafe_trace_deref!(Rc, T; immut = required; |rc| &**rc);
unsafe_trace_deref!(Arc, T; immut = required; |arc| &**arc);
