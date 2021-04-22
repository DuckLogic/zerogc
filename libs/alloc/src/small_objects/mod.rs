#![allow(clippy::vec_box)] // We must Box<Chunk> for a stable address
use std::alloc::{Layout};
use std::mem;
use std::ptr::NonNull;
use std::mem::MaybeUninit;
use std::borrow::BorrowMut;
use std::slice::SliceIndex;

mod nosync;
#[cfg(feature = "sync")]
mod sync;

use crate::{AllocationError, AllocatedObject};
use crate::utils::OnceCell;

/// The minimum size of supported memory (in words)
///
/// Since the header takes at least one word,
/// its not really worth ever allocating less than this
const MINIMUM_WORDS: usize = 2;

/// The minimum size of items allocated in the arena
pub const MINIMUM_SIZE: usize = MINIMUM_WORDS * std::mem::size_of::<usize>();

/// The maximum words supported by small arenas
///
/// Past this we have to fallback to the global allocator
pub const MAXIMUM_SMALL_WORDS: usize = 32;
/// The alignment of elements in the arena
///
/// This is equivalent to a machine word.
pub const ARENA_ELEMENT_ALIGN: usize = mem::size_of::<usize>();

#[doc(hidden)]
pub trait Chunk {
    fn alloc(capacity: usize) -> Self;
    fn try_alloc(&self, amount: usize) -> Option<NonNull<u8>>;
    fn current(&self) -> *mut u8;
    fn start(&self) -> *mut u8;
    fn capacity(&self) -> usize;
    unsafe fn unchecked_reset(&self);
}

/// We use zero as our marker value.
///
/// This is based on the assumption that the first word
/// of the object is never zero. This is guarnenteed by
/// the [SmallArena] API contract.
/// 
/// ## Safety
/// The first field of any allocated memory block must never be zero,
/// as that is used to mark freed memory internally. This avoids
/// any unessicarry space overhead.
///
/// For garbage collectors and language implementors
/// this should not be a problem
/// 
/// Having `field1 = 0usize` is **undefined behavior**!
///
/// If client code can innocently writes `0usize` to
/// the first word of the allocated memory,
/// the allocator will assume the slot is freed,
/// even though the client is still using it.
/// Then we will hand it
/// off to other codes, allowing multiple-ownership (UB).
///
///
/// ### Original reasoning (for simple collector)
/// GcHeader::type_info` is the first field of the header
/// and it will never be null (its a reference).
/// Therefore this marker will never conflict with a valid header.
///
pub const FREE_SLOT_MARKER: usize = 0;
#[repr(C)]
pub struct FreeSlot {
    /// Marker for the slot, initialized to `FREE_SLOT_MARKER`
    pub marker: usize,
    /// Pointer to the previous free slot
    pub(crate) prev_free: Option<NonNull<MaybeFreeSlot>>,
}
#[doc(hidden)]
#[repr(C)]
pub union MaybeFreeSlot {
    pub free: FreeSlot,
    pub obj: [u8; MINIMUM_SIZE],
}

impl MaybeFreeSlot {
    #[inline]
    pub unsafe fn is_free(&self) -> bool {
        self.free.marker == FREE_SLOT_MARKER
    }
    #[inline]
    pub unsafe fn mark_free(&mut self, prev: Option<NonNull<MaybeFreeSlot>>) {
        debug_assert!(!self.is_free());
        self.free = FreeSlot {
            marker: FREE_SLOT_MARKER,
            prev_free: prev
        };
    }
}
pub const NUM_SMALL_ARENAS: usize = 15;
const INITIAL_SIZE: usize = 512;

/// The state of a specific arena in the allocator
///
/// This is a trait to abstract away the differences between thread-safe
/// and non-thread-safe implementations
#[doc(hidden)]
pub trait ArenaState {
    type Chunk: Chunk;
    type ChunkLock<'a>: std::ops::DerefMut<Target=Vec<Box<Self::Chunk>>> + 'a;
    fn new(chunks: Vec<Box<Self::Chunk>>) -> Self;
    fn lock_chunks(&self) -> Self::ChunkLock<'_>;
    fn current_chunk(&self) -> NonNull<Self::Chunk>;
    unsafe fn force_current_chunk(&self, ptr: NonNull<Self::Chunk>);

    #[deprecated(note = "Should be tracked at the allocator level...")]
    fn reserved_memory(&self) -> usize;
    fn add_reserved_memory(&self, amount: usize);
    fn subtract_reserved_memory(&self, amount: usize);
    #[deprecated(note = "Should be tracked at the allocator level...")]
    fn used_memory(&self) -> usize;
    fn add_used_memory(&self, amount: usize);
    fn subtract_used_memory(&self, amount: usize);

    #[inline]
    fn alloc(&self, element_size: usize) -> NonNull<u8> {
        unsafe {
            let chunk = &*self.current_chunk().as_ptr();
            match chunk.try_alloc(element_size) {
                Some(header) => header,
                None => self.alloc_fallback(element_size)
            }
        }
    }

    #[cold]
    #[inline(never)]
    fn alloc_fallback(&self, element_size: usize) -> NonNull<u8> {
        let mut chunks = self.lock_chunks();
        let chunks = chunks.borrow_mut();
        // Now that we hold the lock, check the current chunk again
        unsafe {
            if let Some(res) = self.current_chunk().as_ref()
                .try_alloc(element_size) {
                return res;
            }
        }
        // Double capacity to amortize growth
        let last_capacity = chunks.last().unwrap().capacity();
        let new_capacity = last_capacity * 2;
        chunks.push(Box::new(Self::Chunk::alloc(new_capacity)));
        self.add_reserved_memory(new_capacity);
        unsafe {
            self.force_current_chunk(NonNull::from(&**chunks.last().unwrap()));
            self.current_chunk().as_ref()
                .try_alloc(element_size).unwrap()
        }
    }
}

/// The free list
#[doc(hidden)]
pub trait FreeList: Default {
    /// Take a reference to the next free slot
    fn next_free(&self) -> Option<NonNull<MaybeFreeSlot>>;
    /// Replace the next slot in the free list,
    /// without regard for what is already in there.
    ///
    /// ## Safety
    /// The slot must be valid.
    unsafe fn replace_next_free(&self, next: Option<NonNull<MaybeFreeSlot>>) -> Option<NonNull<MaybeFreeSlot>>;
    /// Take an element from the free list, or `None` if it's currently empty
    fn take_free(&self) -> Option<NonNull<u8>>;
    /// Mark the specified slot as free
    unsafe fn insert_free(&self, slot: NonNull<u8>);
}

pub struct SmallArena<S: ArenaState, FL: FreeList> {
    pub(crate) element_size: usize,
    state: S,
    pub(crate) free: FL,
}
impl<S: ArenaState, FL: FreeList> SmallArena<S, FL> {
    #[cold] // Initialization is the slow path
    fn with_words(num_words: usize) -> Self {
        assert!(num_words >= MINIMUM_WORDS);
        let element_size = num_words * mem::size_of::<usize>();
        assert!(INITIAL_SIZE >= element_size * 2);
        let chunks = vec![Box::new(S::Chunk::alloc(INITIAL_SIZE))];
        SmallArena {
            state: S::new(chunks),
            element_size, free: FL::default(),
        }
    }
    #[inline]
    pub(crate) fn alloc(&self) -> NonNull<u8> {
        // Check the free list
        if let Some(free) = self.free.take_free() {
            self.state.add_used_memory(self.element_size);
            free
        } else {
            self.state.alloc(self.element_size)
        }
    }
    pub(crate) unsafe fn for_each<F: FnMut(*mut MaybeFreeSlot)>(&self, mut func: F) {
        let chunks = self.state.lock_chunks();
        for chunk in &*chunks {
            let mut ptr = chunk.start();
            let end = chunk.current();
            while ptr < end {
                func(ptr as *mut MaybeFreeSlot);
                ptr = ptr.add(self.element_size);
            }
        }
    }
    unsafe fn unchecked_reset(&self) -> usize {
        self.free.replace_next_free(None);
        let mut chunks = self.state.lock_chunks();
        let mut freed_memory = 0;
        for chunk in &mut *chunks {
            let mut ptr = chunk.start();
            let end = chunk.current();
            while ptr < end {
                let slot = ptr as *mut MaybeFreeSlot;
                if !(*slot).is_free() {
                    freed_memory += self.element_size;
                    // NOTE: Okay to lie about `prev` since it's dead anyways
                    (*slot).mark_free(None);
                }
                ptr = ptr.add(self.element_size);
            }
            chunk.unchecked_reset();
        }
        freed_memory
    }
}
macro_rules! arena_match {
    ($arenas:expr, $target:ident, max = $max:expr; $($size:pat => $num_words:literal @ $idx:expr),*) => {
        Ok(match $target {
            $($size => $arenas[$idx].get_or_init(|| {
                assert_eq!(SMALL_ARENA_SIZES[$idx], $num_words);
                SmallArena::with_words($num_words)
            }),)*
            size => {
                assert!($target > $max);
                const REASON: &str = &concat!("Size is larger than maximum small object ", stringify!($max));
                return Err(AllocationError::InvalidSize { size, cause: &REASON })
            }
        })
    };
}
const SMALL_ARENA_SIZES: [usize; NUM_SMALL_ARENAS] =  [
    2, 3, 4, 5, 6, 7, 8,
    10, 12, 14, 16,
    20, 24, 28, 32
];

/// Marker trait that indicates whether the allocator should
/// be thread safe.
#[doc(hidden)]
pub trait AllocThreading {
    type Cell: Sized + OnceCell<SmallArena<Self::ArenaState, Self::FreeList>>;
    type FreeList: FreeList;
    type ArenaState: ArenaState;
}

/// Indicates that the allocator should be thread-safe
#[cfg(feature = "sync")]
pub struct ThreadSafe;

#[cfg(feature = "sync")]
impl AllocThreading for ThreadSafe {
    type Cell = ::once_cell::sync::OnceCell<SmallArena<Self::ArenaState, Self::FreeList>>;
    type FreeList = self::sync::AtomicFreeList;
    type ArenaState = self::sync::SyncArenaState;
}

/// Marks that the allocator should not be thread safe
pub struct SingleThreaded;
impl AllocThreading for SingleThreaded {
    type Cell = ::once_cell::unsync::OnceCell<SmallArena<Self::ArenaState, Self::FreeList>>;
    type FreeList = self::nosync::SimpleFreeList;
    type ArenaState = self::nosync::SimpleArenaState;
}


/// A fast and lightweight arena-allocator for small objects
///
/// This may use excessive memory. It is tuned for throughput.
/// Unfortunately, that means it never shrinks by default,
/// although it does reuse previously freed allocations.
///
/// ## Safety
/// The **first field** of the allocated memory must **never be zero**.
///
/// If that happens, then undefined behavior will occur.
///
/// Field zero is used as a magic marker for already freed memory.
/// See [FREE_SLOT_MARKER] for more information. 
pub struct SmallObjectAllocator<A: AllocThreading> {
    // NOTE: Internally boxed to avoid bloating main struct
    arenas: Box<[A::Cell; NUM_SMALL_ARENAS]>,
}
impl<T: AllocThreading> SmallObjectAllocator<T> {
    /// Create a new instance of the allocator,
    /// promising to uphold the special safety invariants
    ///
    /// ## Safety
    /// Must prevent the first word of allocated memory
    /// from ever being zero, as described in the structure
    /// documentations and [FREE_SLOT_MARKER].
    pub unsafe fn new() -> Self {
        // NOTE: Why does writing arrays have to be so difficult:?
        unsafe {
            let mut arenas: Box<[
                MaybeUninit<T::Cell>;
                NUM_SMALL_ARENAS
            ]> = Box::new_uninit().assume_init();
            for i in 0..NUM_SMALL_ARENAS {
                arenas[i].as_mut_ptr().write(T::Cell::new());
            }
            SmallObjectAllocator {
                // NOTE: This is done because I want to explicitly specify types
                arenas: mem::transmute::<
                    Box<[MaybeUninit<T::Cell>; NUM_SMALL_ARENAS]>,
                    Box<[T::Cell; NUM_SMALL_ARENAS]>
                >(arenas)
            }
        }
    }
    pub fn iter(&self) -> impl Iterator<Item=&SmallArena<T::ArenaState, T::FreeList>> + '_ {
        self.arenas.iter().filter_map(T::Cell::get)
    }
    #[inline]
    fn find<const SIZE: usize, const ALIGN: usize>(&self) -> Result<&SmallArena<T::ArenaState, T::FreeList>, AllocationError> {
        if ALIGN > ARENA_ELEMENT_ALIGN {
            return Err(AllocationError::UnsupportedAlignment { align: ALIGN })
        }
        // Divide round up
        let word_size = mem::size_of::<usize>();
        let num_words = (SIZE + (word_size - 1))
            / word_size;
        self.find_raw(num_words)
    }
    #[inline(always)] // We want this constant-folded away......
    fn find_raw(&self, num_words: usize) -> Result<&SmallArena<T::ArenaState, T::FreeList>, AllocationError> {
        arena_match!(
            self.arenas, num_words, max = 32;
            0..=2 => 2 @ 0,
            3 => 3 @ 1,
            4 => 4 @ 2,
            5 => 5 @ 3,
            6 => 6 @ 4,
            7 => 7 @ 5,
            8 => 8 @ 6,
            9..=10 => 10 @ 7,
            11..=12 => 12 @ 8,
            13..=14 => 14 @ 9,
            15..=16 => 16 @ 10,
            17..=20 => 20 @ 11,
            21..=24 => 24 @ 12,
            25..=28 => 28 @ 13,
            29..=32 => 32 @ 14
        )
    }
}
unsafe impl<T: AllocThreading> super::SimpleAllocator for SmallObjectAllocator<T> {
    const MIN_SIZE: usize = MINIMUM_SIZE;
    const MAX_SIZE: Option<usize> = Some(MAXIMUM_SMALL_WORDS * std::mem::size_of::<usize>());
    const MAX_ALIGNMENT: Option<usize> = Some(ARENA_ELEMENT_ALIGN);

    #[inline] // TODO: Is this a good idea for unknown sizes?
    fn alloc(&self, layout: Layout) -> Result<AllocatedObject, AllocationError> {
        if layout.align() > ARENA_ELEMENT_ALIGN {
            return Err(AllocationError::UnsupportedAlignment { align: layout.align() })
        }
        let arena = self.find_raw(layout.size())?;
        let ptr = arena.alloc();
        Ok(AllocatedObject { layout, ptr })
    }

    #[inline]
    fn alloc_fixed<const SIZE: usize, const ALIGN: usize>(&self) -> Result<AllocatedObject, AllocationError> {
        if ALIGN > ARENA_ELEMENT_ALIGN {
            return Err(AllocationError::UnsupportedAlignment { align: ALIGN })
        }
        let layout = Layout::from_size_align(SIZE, ALIGN).unwrap();
        // NOTE: We *really* want this constant-folded
        let arena = self.find::<SIZE, ALIGN>()?;
        let ptr = arena.alloc();
        Ok(AllocatedObject { layout, ptr })
    }

    #[inline] // TODO: Is this a good idea for unknown sizes?
    unsafe fn free(&self, mem: AllocatedObject) {
        assert!(mem.align() <= ARENA_ELEMENT_ALIGN);
        let arena = self.find_raw(mem.size())
            .expect("Invalid layout"); // Passed invalid data to free -_-
        arena.free.insert_free(mem.ptr);
        arena.state.subtract_used_memory(arena.element_size);
    }

    fn used_memory(&self) -> usize {
        // TODO: This is O(N) the number of chunks
        self.arenas.iter().filter_map(T::Cell::get)
            .map(|arena| arena.state.used_memory()).sum()
    }

    fn reserved_memory(&self) -> usize {
        // TODO: This is O(N) the number of chunks
        self.arenas.iter().filter_map(T::Cell::get)
            .map(|arena| arena.state.reserved_memory()).sum()
    }

    unsafe fn unchecked_reset(&self) -> usize {
        let mut total_freed = 0;
        for arena in self.arenas.iter().filter_map(T::Cell::get) {
            total_freed += arena.unchecked_reset();
        }
        total_freed
    }
}
