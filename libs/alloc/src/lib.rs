//! A set of modular allocators for zerogc.
//!
//! ## Current implementations:
//!
//! ### "Small object" allocator
//! Allocates separate memory-arenas for each size of object.
//! Only supports "small" objects less than a certain size.
//!
//! When objects are freed, they are added to an internal free list.
//! This free list is checked before allocating any more chunks of memory
//! from the operating system.
//!
//! ### Malloc allocator
//! Allocates memory directly using the [standard allocator API](std::alloc)
//!
//! This is excellent for debugging.
#![deny(missing_docs)]
// Features that we always use
#![feature(
    alloc_layout_extra, // Needed to compute Layout of objects
    const_alloc_layout, // We want to const-fold away layout computations
    allocator_api, // We allocate via the standard API
    slice_ptr_get, // We want to have NonNull::as_mut
    untagged_unions, // This should already be stable....
    const_panic, // Used to verify sizes
)]
// Features required for 'small object' allocator
#![cfg_attr(feature = "small-objects", feature(
    new_uninit, // Needed to allocate fixed-size arrays via `Box<[SmallArena; NUM_ARENAS]>`
    cell_update, // Cell::update is just useful :)
))]

use std::ptr::NonNull;
use std::alloc::Layout;

pub mod free_lists;

/// An error caused when allocating a chunk of memory
///
/// This indicates a recoverable error, not a developer error.
/// Invalid usages will cause panics.
#[derive(Clone, Debug)]
pub enum AllocationError {
    /// Indicates that there was insufficient memory to allocate
    /// from the standard library
    StdError {
        /// The underlying cause of the allocation failure
        cause: std::alloc::AllocError,
        /// The layout that failed to allocate
        ///
        /// This may include internal metadata,
        /// so it may end up being larger than actually requested.
        layout: Layout
    },
    /// Indicates that the specified size is invalid
    InvalidSize {
        /// The requested layout
        size: usize,
        /// The reason the size is invalid
        cause: &'static &'static str
    },
    /// Indicates that the specified size is unsupported
    UnsupportedAlignment {
        /// The requested alignment
        align: usize
    }
}
impl AllocationError {
    /// Treat this error as a fatal error and panic with an appropriate message
    ///
    /// By default, rust allocations tend to panic on failure so
    /// this should be fairly common.
    ///
    /// This is analogous to [std::alloc::handle_alloc_error] from the
    /// standard allocator API.
    #[cold]
    pub fn consider_fatal(&self) -> ! {
        match *self {
            AllocationError::StdError { cause: _, layout } => {
                std::alloc::handle_alloc_error(layout)
            },
            AllocationError::InvalidSize { size, cause } => {
                panic!("Invalid size {}: {}", size, cause);
            },
            AllocationError::UnsupportedAlignment { align } => {
                panic!("Unsupported alignment: {}", align);
            }
        }
    }
}