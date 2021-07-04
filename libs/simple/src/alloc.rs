#![allow(clippy::vec_box)] // We must Box<Chunk> for a stable address
use std::alloc::Layout;
use std::mem;
use std::ptr::NonNull;
use std::mem::MaybeUninit;

#[cfg(feature = "sync")]
use once_cell::sync::OnceCell;
#[cfg(not(feature = "sync"))]
use once_cell::unsync::OnceCell;
#[cfg(feature = "sync")]
use parking_lot::Mutex;
#[cfg(not(feature = "sync"))]
use std::cell::RefCell;

use zerogc_context::utils::AtomicCell;

/// The minimum size of supported memory (in words)
///
/// Since the header takes at least one word,
/// its not really worth ever allocating less than this
pub const MINIMUM_WORDS: usize = 2;
/// The maximum words supported by small arenas
///
/// Past this we have to fallback to the global allocator
pub const MAXIMUM_SMALL_WORDS: usize = 32;
/// The alignment of elements in the arena
pub const ARENA_ELEMENT_ALIGN: usize = std::mem::align_of::<GcHeader>();

use crate::layout::{GcHeader};

[inline]
pub const fn fits_small_object(layout: Layout) -> bool {
    layout.size() <= MAXIMUM_SMALL_WORDS * std::mem::size_of::<usize>()
        && layout.align() <= ARENA_ELEMENT_ALIGN
}

pub(crate) struct Chunk {
    pub start: *mut u8,
    current: AtomicCell<*mut u8>,
    pub end: *mut u8
}
impl Chunk {
    fn alloc(capacity: usize) -> Box<Self> {
        assert!(capacity >= 1);
        let mut result = Vec::<u8>::with_capacity(capacity);
        let start = result.as_mut_ptr();
        std::mem::forget(result);
        let current = AtomicCell::new(start);
        Box::new(Chunk {
            start, current,
            end: unsafe { start.add(capacity) }
        })
    }

    #[inline]
    fn try_alloc(&self, amount: usize) -> Option<NonNull<u8>> {
        loop {
            let old_current = self.current.load();
            let remaining = self.end as usize - old_current as usize;
            if remaining >= amount {
                unsafe {
                    let updated = old_current.add(amount);
                    if self.current.compare_exchange(old_current, updated).is_ok() {
                        return Some(NonNull::new_unchecked(old_current))
                    } else {
                        continue
                    }
                }
            } else {
                return None
            }
        }
    }
    #[inline]
    fn current(&self) -> *mut u8 {
        self.current.load()
    }
    #[inline]
    fn capacity(&self) -> usize {
        self.end as usize - self.start as usize
    }
}
impl Drop for Chunk {
    fn drop(&mut self) {
        unsafe {
            drop(Vec::from_raw_parts(
                self.start, 0,
                self.capacity()
            ))
        }
    }
}

/// We use zero as our marker value.
///
/// `GcHeader::type_info` is the first field of the header
/// and it will never be null (its a reference).
/// Therefore this marker will never conflict with a valid header.
pub const FREE_SLOT_MARKER: usize = 0;
#[repr(C)]
pub struct FreeSlot {
    /// Marker for the slot, initialized to `FREE_SLOT_MARKER`
    pub marker: usize,
    /// Pointer to the previous free slot
    pub(crate) prev_free: Option<NonNull<MaybeFreeSlot>>,
}
#[repr(C)]
pub(crate) union MaybeFreeSlot {
    pub free: FreeSlot,
    pub header: GcHeader,
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

/// The current state of the allocator.
///
/// TODO: Support per-thread arena caching
struct ArenaState {
    /// We have to Box the chunk so that it'll remain valid
    /// even when we move it.
    ///
    /// This is required for thread safety.
    /// One thread could still be seeing an old chunk's location
    /// after it's been moved.
    #[cfg(feature = "sync")]
    chunks: Mutex<Vec<Box<Chunk>>>,
    /// List of chunks, not thread-safe
    ///
    /// We still box it however, as an extra check of safety.
    #[cfg(not(feature = "sync"))]
    chunks: RefCell<Vec<Box<Chunk>>>,
    /// Lockless access to the current chunk
    ///
    /// The pointers wont be invalidated,
    /// since the references are internally boxed.
    current_chunk: AtomicCell<NonNull<Chunk>>
}
impl ArenaState {
    fn new(chunks: Vec<Box<Chunk>>) -> Self {
        assert!(!chunks.is_empty());
        let current_chunk = NonNull::from(&**chunks.last().unwrap());
        let chunk_lock;
        #[cfg(feature = "sync")] {
            chunk_lock = Mutex::new(chunks);
        }
        #[cfg(not(feature = "sync"))] {
            chunk_lock = RefCell::new(chunks);
        }
        ArenaState {
            chunks: chunk_lock,
            current_chunk: AtomicCell::new(current_chunk)
        }
    }
    #[inline]
    #[cfg(feature = "sync")]
    fn lock_chunks(&self) -> ::parking_lot::MutexGuard<Vec<Box<Chunk>>> {
        self.chunks.lock()
    }
    #[inline]
    #[cfg(not(feature = "sync"))]
    fn lock_chunks(&self) -> ::std::cell::RefMut<Vec<Box<Chunk>>> {
        self.chunks.borrow_mut()
    }
    #[inline]
    fn current_chunk(&self) -> NonNull<Chunk> {
        self.current_chunk.load()
    }
    #[inline]
    unsafe fn force_current_chunk(&self, ptr: NonNull<Chunk>) {
        self.current_chunk.store(ptr);
    }
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
    fn alloc_fallback(&self, element_size: usize) -> NonNull<GcHeader> {
        let mut chunks = self.lock_chunks();
        // Now that we hold the lock, check the current chunk again
        unsafe {
            if let Some(header) = self.current_chunk().as_ref()
                .try_alloc(element_size) {
                return header.cast();
            }
        }
        // Double capacity to amortize growth
        let last_capacity = chunks.last().unwrap().capacity();
        chunks.push(Chunk::alloc(last_capacity * 2));
        unsafe {
            self.force_current_chunk(NonNull::from(&**chunks.last().unwrap()));
            self.current_chunk().as_ref()
                .try_alloc(element_size).unwrap()
                .cast::<u8>()
        }
    }
}

/// The free list
///
/// This is a lock-free linked list
#[derive(Default)]
pub(crate) struct FreeList {
    next: AtomicCell<Option<NonNull<MaybeFreeSlot>>>
}
impl FreeList {
    #[inline]
    pub(crate) fn next_free(&self) -> Option<NonNull<MaybeFreeSlot>> {
        self.next.load()
    }
    #[inline]
    pub(crate) unsafe fn set_next_free(&self, next: Option<NonNull<MaybeFreeSlot>>) {
        self.next.store(next)
    }
    #[inline]
    fn take_free(&self) -> Option<NonNull<GcHeader>> {
        loop {
            let next_free = match self.next.load() {
                Some(free) => free,
                None => return None, // Out of free space
            };
            // Update free pointer
            unsafe {
                if self.next.compare_exchange(
                    Some(next_free),
                    next_free.as_ref().free.prev_free
                ).is_err() { continue /* retry */ }
                debug_assert_eq!(
                    next_free.as_ref().free.marker,
                    FREE_SLOT_MARKER
                );
                return Some(NonNull::from(&next_free.as_ref().header))
            }
        }
    }
}

pub struct SmallArena {
    pub(crate) element_size: usize,
    state: ArenaState,
    pub(crate) free: FreeList
}
impl SmallArena {
    #[cold] // Initialization is the slow path
    fn with_words(num_words: usize) -> SmallArena {
        assert!(num_words >= MINIMUM_WORDS);
        let element_size = num_words * mem::size_of::<usize>();
        assert!(INITIAL_SIZE >= element_size * 2);
        let chunks = vec![Chunk::alloc(INITIAL_SIZE)];
        SmallArena {
            state: ArenaState::new(chunks),
            element_size, free: Default::default(),
        }
    }
    #[inline]
    pub(crate) fn alloc(&self) -> NonNull<u8> {
        // Check the free list
        if let Some(free) = self.free.take_free() {
            let res = free.as_ptr().sub(match free.as_ref().type_info.layout {
                Layout::new
            })
        } else {
            self.state.alloc(self.element_size)
        }
    }
    pub(crate) unsafe fn for_each<F: FnMut(*mut MaybeFreeSlot)>(&self, mut func: F) {
        let chunks = self.state.lock_chunks();
        for chunk in &*chunks {
            let mut ptr = chunk.start;
            let end = chunk.current();
            while ptr < end {
                func(ptr as *mut MaybeFreeSlot);
                ptr = ptr.add(self.element_size);
            }
        }
    }
}
macro_rules! arena_match {
    ($arenas:expr, $target:ident, max = $max:expr; $($size:pat => $num_words:literal @ $idx:expr),*) => {
        Some(match $target {
            $($size => $arenas[$idx].get_or_init(|| {
                assert_eq!(SMALL_ARENA_SIZES[$idx], $num_words);
                SmallArena::with_words($num_words)
            }),)*
            _ => {
                assert!($target > $max);
                return None
            }
        })
    };
}
const SMALL_ARENA_SIZES: [usize; NUM_SMALL_ARENAS] =  [
    2, 3, 4, 5, 6, 7, 8,
    10, 12, 14, 16,
    20, 24, 28, 32
];
pub struct SmallArenaList {
    // NOTE: Internally boxed to avoid bloating main struct
    arenas: Box<[OnceCell<SmallArena>; NUM_SMALL_ARENAS]>
}
impl SmallArenaList {
    pub fn new() -> Self {
        // NOTE: Why does writing arrays have to be so difficult:?
        unsafe {
            let mut arenas: Box<[
                MaybeUninit<OnceCell<SmallArena>>;
                NUM_SMALL_ARENAS
            ]> = Box::new_uninit().assume_init();
            for i in 0..NUM_SMALL_ARENAS {
                arenas[i].as_mut_ptr().write(OnceCell::new());
            }
            SmallArenaList {
                // NOTE: This is done because I want to explicitly specify types
                arenas: mem::transmute::<
                    Box<[MaybeUninit<OnceCell<SmallArena>>; NUM_SMALL_ARENAS]>,
                    Box<[OnceCell<SmallArena>; NUM_SMALL_ARENAS]>
                >(arenas)
            }
        }
    }
    pub fn iter(&self) -> impl Iterator<Item=&SmallArena> + '_ {
        self.arenas.iter().filter_map(OnceCell::get)
    }
    #[inline] // This should hopefully be constant folded away (layout is const)
    pub fn find(&self, layout: Layout) -> Option<&SmallArena> {
        if !fits_small_object(layout) {
            return None
        }
        // Divide round up
        let word_size = mem::size_of::<usize>();
        let num_words = (small_object_size(layout) + (word_size - 1))
            / word_size;
        self.find_raw(num_words)
    }
    #[inline] // We want this constant-folded away......
    fn find_raw(&self, num_words: usize) -> Option<&SmallArena> {
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
