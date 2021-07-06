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

const DEBUG_INTERNAL_ALLOCATOR: bool = cfg!(zerogc_simple_debug_alloc);
mod debug {
    pub const PADDING: u32 = 0xDEADBEAF;
    pub const UNINIT: u32 = 0xCAFEBABE;
    pub const PADDING_TIMES: usize = 16;
    pub const PADDING_BYTES: usize = PADDING_TIMES * 4;
    pub unsafe fn pad_memory_block(ptr: *mut u8, size: usize) {
        assert!(super::DEBUG_INTERNAL_ALLOCATOR);
        let start = ptr.sub(PADDING_BYTES);
        for i in 0..PADDING_TIMES {
            (start as *mut u32).add(i).write(PADDING);
        }
        let end = ptr.add(size);
        for i in 0..PADDING_TIMES {
            (end as *mut u32).add(i).write(PADDING);
        }
    }
    pub unsafe fn mark_memory_uninit(ptr: *mut u8, size: usize) {
        assert!(super::DEBUG_INTERNAL_ALLOCATOR);
        let (blocks, leftover) = (size / 4, size % 4);
        for i in 0..blocks {
            (ptr as *mut u32).add(i).write(UNINIT);
        }
        let leftover_ptr = ptr.add(blocks * 4);
        debug_assert_eq!(leftover_ptr.wrapping_add(leftover), ptr.add(size));
        for i in 0..leftover {
            leftover_ptr.add(i).write(0xF0);
        }
    }
    pub unsafe fn assert_padded(ptr: *mut u8, size: usize) {
        assert!(super::DEBUG_INTERNAL_ALLOCATOR);
        let start = ptr.sub(PADDING_BYTES);
        let end = ptr.add(size);
        let start_padding = std::slice::from_raw_parts(
            start as *const u8 as *const u32,
            PADDING_TIMES
        );
        let region = std::slice::from_raw_parts(
            ptr as *const u8,
            size
        );
        let end_padding = std::slice::from_raw_parts(
            end as *const u8 as *const u32,
            PADDING_TIMES
        );
        let print_memory_region = || {
            use std::fmt::Write;
            let mut res = String::new();
            for &val in start_padding {
                write!(&mut res, "{:X}", val).unwrap();
            }
            res.push_str("||");
            for &b in region {
                write!(&mut res, "{:X}", b).unwrap();
            }
            res.push_str("||");
            for &val in end_padding {
                write!(&mut res, "{:X}", val).unwrap();
            }
            res
        };
        // Closest to farthest
        for (idx, &block) in start_padding.iter().rev().enumerate() {
            if block == PADDING { continue }
            assert_eq!(
                block, PADDING,
                "Unexpected start padding (offset -{}) w/ {}",
                idx * 4,
                print_memory_region()
            );
        }
        for (idx, &block) in end_padding.iter().enumerate() {
            if block == PADDING { continue }
            assert_eq!(
                block, PADDING,
                "Unexpected end padding (offset {}) w/ {}",
                idx * 4,
                print_memory_region()
            )
        }
    }
}
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

use crate::layout::{GcHeader, UnknownHeader};

#[inline]
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

/// A slot in the free list
#[repr(C)]
pub struct FreeSlot {
    /// Pointer to the previous free slot
    pub(crate) prev_free: Option<NonNull<FreeSlot>>,
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
    fn alloc(&self, element_size: usize) -> NonNull<UnknownHeader> {
        unsafe {
            let chunk = &*self.current_chunk().as_ptr();
            match chunk.try_alloc(element_size) {
                Some(header) => header.cast(),
                None => self.alloc_fallback(element_size)
            }
        }
    }

    #[cold]
    #[inline(never)]
    fn alloc_fallback(&self, element_size: usize) -> NonNull<UnknownHeader> {
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
                .cast::<UnknownHeader>()
        }
    }
}

/// The free list
///
/// This is a lock-free linked list
#[derive(Default)]
pub(crate) struct FreeList {
    next: AtomicCell<Option<NonNull<FreeSlot>>>
}
impl FreeList {
    unsafe fn add_free(&self, free: *mut UnknownHeader, size: usize) {
        if DEBUG_INTERNAL_ALLOCATOR {
            debug::assert_padded(free as *mut u8, size);
            debug::mark_memory_uninit(free as *mut u8, size);
        }
        let new_slot = free as *mut FreeSlot;
        let mut next = self.next.load();
        loop {
            (*new_slot).prev_free = next;
            match self.next.compare_exchange(next, Some(NonNull::new_unchecked(new_slot))) {
                Ok(_) => break,
                Err(actual_next) => {
                    next = actual_next;
                }
            }
        }
    }
    #[inline]
    fn take_free(&self) -> Option<NonNull<u8>> {
        loop {
            let next_free = match self.next.load() {
                Some(free) => free,
                None => return None, // Out of free space
            };
            // Update free pointer
            unsafe {
                if self.next.compare_exchange(
                    Some(next_free),
                    next_free.as_ref().prev_free
                ).is_err() { continue /* retry */ }
                return Some(next_free.cast())
            }
        }
    }
}

pub struct SmallArena {
    pub(crate) element_size: usize,
    state: ArenaState,
    free: FreeList,
}

impl SmallArena {
    pub(crate) unsafe fn add_free(&self, obj: *mut UnknownHeader) {
        self.free.add_free(obj, self.element_size)
    }
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
    pub(crate) fn alloc(&self) -> NonNull<UnknownHeader> {
        // Check the free list
        if let Some(free) = self.free.take_free() {
            free.cast()
        } else if DEBUG_INTERNAL_ALLOCATOR {
            let mem = self.state.alloc(self.element_size + debug::PADDING_BYTES * 2)
                .as_ptr() as *mut u8;
            unsafe {
                let mem = mem.add(debug::PADDING_BYTES);
                debug::pad_memory_block(mem, self.element_size);
                debug::mark_memory_uninit(mem, self.element_size);
                NonNull::new_unchecked(mem).cast()
            }
        } else {
            self.state.alloc(self.element_size)
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
    #[inline] // This should hopefully be constant folded away (layout is const)
    pub fn find(&self, layout: Layout) -> Option<&SmallArena> {
        if !fits_small_object(layout) {
            return None
        }
        // Divide round up
        let word_size = mem::size_of::<usize>();
        let num_words = (layout.size() + (word_size - 1))
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
