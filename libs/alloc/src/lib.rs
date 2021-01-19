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
)]
// Features required for 'small object' allocator
#![cfg_attr(feature = "small-objects", feature(
    new_uninit, // Needed to allocate fixed-size arrays via `Box<[SmallArena; NUM_ARENAS]>`
    cell_update, // Cell::update is just useful :)
    generic_associated_types, // Used to be generic over 'std::cell::MutRef' and `MutextGuard`
))]

use std::ptr::NonNull;
use std::alloc::Layout;

#[cfg(feature = "malloc")]
mod malloc;
#[cfg(feature = "small-objects")]
mod small_objects;

/// The most basic interface to allocation
///
/// ## Safety
/// The allocator must obey the API description and allocate chunks of
/// memory of the correct size.
pub unsafe trait SimpleAllocator {
    /// The minimum size of supported memory.
    ///
    /// Anything less than this is wasted space.
    ///
    /// Allocators are required to satisfy requests smaller than this,
    /// although they can just round up internally.
    const MIN_SIZE: usize = 0;
    /// The maximum size of objects supported by the allocator,
    /// or `None` if there is no inherent limitation
    const MAX_SIZE: Option<usize> = None;
    /// The maximum supported alignment supported by the allocator,
    /// or `None` if there is no inhernit limitation.
    const MAX_ALIGNMENT: Option<usize> = None;

    /// Allocate a chunk of memory
    /// whose size is not known at compile time.
    fn alloc(&self, layout: Layout) -> Result<AllocatedObject, AllocationError>;

    /// Allocate a chunk of memory
    /// whose layout is statically known in advance.
    ///
    /// This is likely faster than [SimpleAllocator::alloc], because it can statically
    /// determine which part of the allocator to use.
    #[inline]
    fn alloc_fixed<const SIZE: usize, const ALIGN: usize>(&self) -> Result<AllocatedObject, AllocationError> {
        self.alloc(Layout::from_size_align(SIZE, ALIGN).unwrap())
    }

    /// Free the specified object
    ///
    /// ## Safety
    /// Undefined behavior if the specified object is invalid,
    /// or came from a different allocator.
    unsafe fn free(&self, mem: AllocatedObject);

    /// Free the specified object, whose layout is statically known
    ///
    /// ## Safety
    /// Undefined behavior if the specified object is invalid,
    /// or came from a different allocator.
    #[inline]
    unsafe fn free_fixed<const SIZE: usize, const ALIGN: usize>(&self, mem: AllocatedObject) {
        debug_assert_eq!(mem.size(), SIZE);
        debug_assert_eq!(mem.align(), ALIGN);
        self.free(mem)
    }

    /// Returns the memory currently in use
    fn used_memory(&self) -> usize;

    /// Returns the total amount of memory currently reserved by this allocator.
    ///
    /// Not all of this memory is nessicarry being used by alloated objects,
    /// although it can't be used by other parts of the program.
    ///
    /// For the number of objects currently in use, see [Allocator::used_memory]
    #[deprecated(note = "Should this require computation or be a simple getter?")]
    fn reserved_memory(&self) -> usize;

    /// Release all the memory currently in use,
    /// marking it as unused.
    ///
    /// Returns the amont of memory freed.
    ///
    /// Like [Vec::clear], this method doesn't actually return anything to
    /// the operating system and continues to reserve it. It simply marks the memory as unused.
    ///
    /// It is equivalent to manually calling free on every object
    /// that is currently allocated.
    ///
    /// ## Used
    /// Undefined behavior if any of the freed memory is ever used again.
    unsafe fn unchecked_reset(&self) -> usize;
}

/// A chunk of allocated memory
///
/// This is a simple wrapper type
#[derive(Clone, Debug)]
#[must_use]
pub struct AllocatedObject {
    /// The allocated memory
    pub ptr: NonNull<u8>,
    /// The layout of the memory
    ///
    /// This may include more space than requested
    /// if the allocator had excess.
    pub layout: Layout
}

impl AllocatedObject {
    /// The size of the object
    pub const fn size(&self) -> usize {
        self.layout.size()
    }
    /// The alignment of the object
    pub const fn align(&self) -> usize {
        self.layout.align()
    }
}

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