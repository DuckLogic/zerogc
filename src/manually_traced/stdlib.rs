//! Tracing implementations for the standard library
//!
//! Types that are in `libcore` and are `#![no_std]` should go in the core module,
//! but anything that requires the rest of the stdlib (including collections and allocations),
//! should go in this module.

use ::{GarbageCollected, GarbageCollector};
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;
use std::num::Wrapping;

unsafe_trace_deref!(Vec, target = { &[T] }; T);
unsafe_trace_iterable!(HashMap, element = { (&K, &V) }; K, V);
unsafe_trace_deref!(Box, target = T);
unsafe_trace_deref!(Rc, target = T);
unsafe_trace_deref!(Arc, target = T);
/// We can trace `Wrapping` by simply tracing its interior
unsafe_trace_deref!(Wrapping, T; |wrapping| &wrapping.0);
