// Unstable features
#![cfg_attr(feature = "nightly", feature(
    coerce_unsized,  // RFC 0982 - DST coercion
    unsize,
))]
#![deny(missing_docs)]
#![allow(
    clippy::missing_safety_doc, // TODO: Add missing safety docs and make this #[deny(...)]
)]
#![cfg_attr(not(feature = "std"), no_std)]
//! Zero overhead tracing garbage collection for rust,
//! by abusing the borrow checker.
//!
//! ## Features
//! 1. Easy to use, since `Gc<T>` is `Copy` and coerces to a reference.
//! 2. Absolutely zero overhead when modifying pointers, since `Gc<T>` is `Copy`.
//! 3. Implementation agnostic API
//! 4. Unsafe code has complete freedom to manipulate garbage collected pointers, and it doesn't need to understand the distinction
//! 5. Uses rust's lifetime system to ensure all roots are known at explicit safepoints, without any runtime overhead.
//! 6. Collection can only happen with an explicit `safepoint` call and has no overhead between these calls,
//! 7. API supports moving objects (allowing copying/generational GCs)

#[cfg(feature = "alloc")]
extern crate alloc;
/*
 * Allows proc macros to access `::zeroc::$name`
 *
 * NOTE: I can't figure out a
 * way to emulate $crate in a way
 * that doesn't confuse integration
 * tests with the main crate
 */

extern crate self as zerogc;
/*
 * I want this library to use 'mostly' stable features,
 * unless there's good justification to use an unstable feature.
 */

#[macro_use]
mod manually_traced;
#[macro_use]
mod macros;
pub mod cell;
pub mod prelude;
pub mod system;
pub mod trace;

// core traits, used by macros
pub use self::system::{CollectorId, GcSystem};
pub use self::trace::barrier::*;
pub use self::trace::*;

/// Assert that a type implements Copy
///
/// Used by the derive code
#[doc(hidden)]
pub fn assert_copy<T: Copy>() {}
