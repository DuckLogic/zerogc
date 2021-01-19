#![allow(clippy::vec_box)] // We must Box<Chunk> for a stable address
use std::alloc::{Layout, Global};
use std::mem;
use std::ptr::NonNull;
use std::mem::MaybeUninit;

#[cfg(feature = "sync")]
use parking_lot::Mutex;
use std::cell::{RefCell, Cell};

use crate::{AllocationError, AllocatedObject};
#[cfg(feature = "sync")]
use crossbeam_utils::atomic::AtomicCell;
#[cfg(feature = "sync")]
use std::sync::atomic::AtomicUsize;
use std::borrow::BorrowMut;
use std::sync::atomic::Ordering;
use std::slice::SliceIndex;

/// The minimum size of supported memory (in words)
///
/// Since the header takes at least one word,
/// its not really worth ever allocating less than this
pub const MINIMUM_WORDS: usize = 2;
/// The minimum size of items allocated in the arena
const MINIMUM_SIZE: usize = MINIMUM_WORDS * std::mem::size_of::<usize>();
/// The maximum words supported by small arenas
///
/// Past this we have to fallback to the global allocator
pub const MAXIMUM_SMALL_WORDS: usize = 32;
/// The alignment of elements in the arena
///
/// This is equivalent to a machine word.
pub const ARENA_ELEMENT_ALIGN: usize = mem::size_of::<usize>();

pub(crate) trait Chunk {
    fn alloc(capacity: usize) -> Self;
    fn try_alloc(&self, amount: usize) -> Option<NonNull<u8>>;
    fn current(&self) -> *mut u8;
    fn start(&self) -> *mut u8;
    fn capacity(&self) -> usize;
    unsafe fn unchecked_reset(&self);
}
pub(crate) struct SimpleChunk {
    start: *mut u8,
    current: Cell<*mut u8>,
    end: *mut u8
}
impl Chunk for SimpleChunk {
    fn alloc(capacity: usize) -> Self {
        assert!(capacity >= 1);
        let mut result = Vec::<u8>::with_capacity(capacity);
        let start = result.as_mut_ptr();
        std::mem::forget(result);
        let current = Cell::new(start);
        SimpleChunk {
            start, current,
            end: unsafe { start.add(capacity) }
        }
    }

    #[inline]
    fn try_alloc(&self, amount: usize) -> Option<NonNull<u8>> {
        let old_current = self.current.get();
        let remaining = self.end as usize - old_current as usize;
        if remaining >= amount {
            unsafe {
                self.current.set(old_current.add(amount));
                Some(NonNull::new_unchecked(old_current))
            }
        } else {
            None
        }
    }

    #[inline]
    fn current(&self) -> *mut u8 {
        self.current.get()
    }
    #[inline]
    fn start(&self) -> *mut u8 {
        self.start
    }
    #[inline]
    fn capacity(&self) -> usize {
        self.end as usize - self.start as usize
    }

    #[inline]
    unsafe fn unchecked_reset(&self) {
        self.current.set(self.start);
    }
}

impl Drop for SimpleChunk {
    fn drop(&mut self) {
        unsafe {
            drop(Vec::from_raw_parts(
                self.start, 0,
                self.capacity()
            ))
        }
    }
}
pub(crate) struct AtomicChunk {
    start: *mut u8,
    current: AtomicCell<*mut u8>,
    end: *mut u8
}
impl Chunk for AtomicChunk {
    fn alloc(capacity: usize) -> Self {
        let simple = SimpleChunk::alloc(capacity);
        AtomicChunk {
            start: simple.start,
            current: AtomicCell::new(simple.current.get()),
            end: simple.end
        }
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
    fn start(&self) -> *mut u8 {
        self.start
    }
    #[inline]
    fn capacity(&self) -> usize {
        self.end as usize - self.start as usize
    }

    #[inline]
    unsafe fn unchecked_reset(&self) {
        self.current.store(self.start);
    }
}
impl Drop for AtomicChunk {
    fn drop(&mut self) {
        drop(SimpleChunk {
            start: self.start,
            current: Cell::new(self.current.load()),
            end: self.end
        })
    }
}

/// We use zero as our marker value.
///
/// `GcHeader::type_info` is the first field of the header
/// and it will never be null (its a reference).
/// Therefore this marker will never conflict with a valid header.
///
/*
 * TODO: THIS IS UNSAFE!!! We know longer can assume that field #1 is GcHeader.type_info
 * Client code can innocently write `0usize` to the first field of the object and we will
 * assume the slot is freed, even though the client is still using it. Then we will hand it
 * off to other codes, allowing multiple-ownership (UB).
 */
#[deprecated(note = "Serious safety bug: We're can no longer assume field #1 is nonzero")]
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
trait ArenaState {
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
/// The (thread-safe) state of a specific arena in the allocator.
///
/// TODO: Support per-thread arena caching
#[cfg(feature = "sync")]
struct SyncArenaState {
    /// We have to Box the chunk so that it'll remain valid
    /// even when we move it.
    ///
    /// This is required for thread safety.
    /// One thread could still be seeing an old chunk's location
    /// after it's been moved.
    chunks: Mutex<Vec<Box<AtomicChunk>>>,
    /// Lockless access to the current chunk
    ///
    /// The pointers wont be invalidated,
    /// since the references are internally boxed.
    current_chunk: AtomicCell<NonNull<AtomicChunk>>,
    /// The amount of reserved memory, in bytes
    ///
    /// This will be automatically updated whenever we
    /// allocate a new chunk of memory.
    reserved_memory: AtomicUsize,
    /// The amount of used memory, given to allocator clients.
    ///
    /// Whenever memory is allocated from chunks directly,
    /// this is increased appropriately.
    ///
    /// However, it is possible that other internal uses (like a free list)
    /// could change this externally.
    used_memory: AtomicUsize
}
#[cfg(feature = "sync")]
impl ArenaState for SyncArenaState {
    type Chunk = AtomicChunk;
    type ChunkLock<'a> = ::parking_lot::MutexGuard<'a, Vec<Box<AtomicChunk>>>;
    fn new(chunks: Vec<Box<AtomicChunk>>) -> Self {
        assert!(!chunks.is_empty());
        let current_chunk = NonNull::from(&**chunks.last().unwrap());
        let reserved_memory = AtomicUsize::new(chunks.iter()
            .map(|chunk| chunk.capacity())
            .sum());
        let chunk_lock = Mutex::new(chunks);
        SyncArenaState {
            chunks: chunk_lock,
            current_chunk: AtomicCell::new(current_chunk),
            reserved_memory, used_memory: AtomicUsize::new(0)
        }
    }
    #[inline]
    #[cfg(feature = "sync")]
    fn lock_chunks(&self) -> Self::ChunkLock<'_> {
        self.chunks.lock()
    }
    #[inline]
    fn current_chunk(&self) -> NonNull<AtomicChunk> {
        self.current_chunk.load()
    }
    #[inline]
    unsafe fn force_current_chunk(&self, ptr: NonNull<AtomicChunk>) {
        self.current_chunk.store(ptr);
    }

    #[inline]
    fn reserved_memory(&self) -> usize {
        self.reserved_memory.load(Ordering::Acquire)
    }

    #[inline]
    fn add_reserved_memory(&self, amount: usize) {
        // Should never overflow a usize
        self.reserved_memory.fetch_add(amount, Ordering::AcqRel);
    }

    #[inline]
    fn subtract_reserved_memory(&self, amount: usize) {
        // Should never underflow a usize unless we incorrectly free
        let old = self.reserved_memory.fetch_sub(amount, Ordering::AcqRel);
        debug_assert!(old >= amount, "Underflow {} - {}", old, amount);
    }

    #[inline]
    fn used_memory(&self) -> usize {
        self.used_memory.load(Ordering::Acquire)
    }

    #[inline]
    fn add_used_memory(&self, amount: usize) {
        // we will never allocate more memory than can fit in a usize
        self.used_memory.fetch_add(amount, Ordering::AcqRel);
    }

    #[inline]
    fn subtract_used_memory(&self, amount: usize) {
        let old = self.used_memory.fetch_sub(amount, Ordering::AcqRel);
        // this is possible if we incorrectly free
        debug_assert!(amount >= old, "Underflow {} - {}", old, amount);
    }
}

struct SimpleArenaState {
    /// List of chunks, not thread-safe
    ///
    /// We don't *need* to box it like the thread-safe state does.
    /// We still do it anyways, as an extra check of safety.
    chunks: RefCell<Vec<Box<SimpleChunk>>>,
    /// Pointer to the current chunk
    ///
    /// The pointers wont be invalidated,
    /// since the references are internally boxed.
    current_chunk: Cell<NonNull<SimpleChunk>>,
    reserved_memory: Cell<usize>,
    used_memory: Cell<usize>
}
impl ArenaState for SimpleArenaState {
    type Chunk = SimpleChunk;
    type ChunkLock<'a> = ::std::cell::RefMut<'a, Vec<Box<SimpleChunk>>>;
    fn new(chunks: Vec<Box<SimpleChunk>>) -> Self {
        assert!(!chunks.is_empty());
        let current_chunk = NonNull::from(&**chunks.last().unwrap());
        let reserved_memory = Cell::new(chunks.iter().map(|chunk| chunk.capacity()).sum());
        let chunk_lock = RefCell::new(chunks);
        SimpleArenaState {
            chunks: chunk_lock,
            current_chunk: Cell::new(current_chunk),
            reserved_memory, used_memory: Cell::new(0)
        }
    }
    #[inline]
    fn lock_chunks(&self) -> Self::ChunkLock<'_> {
        self.chunks.borrow_mut()
    }
    #[inline]
    fn current_chunk(&self) -> NonNull<SimpleChunk> {
        self.current_chunk.get()
    }
    #[inline]
    unsafe fn force_current_chunk(&self, ptr: NonNull<SimpleChunk>) {
        self.current_chunk.set(ptr);
    }

    #[inline]
    fn reserved_memory(&self) -> usize {
        self.reserved_memory.get()
    }

    #[inline]
    fn add_reserved_memory(&self, amount: usize) {
        self.reserved_memory.update(|i| i + amount);
    }

    #[inline]
    fn subtract_reserved_memory(&self, amount: usize) {
        self.reserved_memory.update(|old| {
            debug_assert!(old >= amount, "Underflow {} - {}", old, amount);
            old - amount
        });
    }

    #[inline]
    fn used_memory(&self) -> usize {
        self.used_memory.get()
    }

    #[inline]
    fn add_used_memory(&self, amount: usize) {
        self.used_memory.update(|i| i + amount);
    }

    #[inline]
    fn subtract_used_memory(&self, amount: usize) {
        self.used_memory.update(|old| {
            debug_assert!(old >= amount, "Underflow {} - {}", old, amount);
            old - amount
        });
    }
}

/// The free list
pub(crate) trait FreeList: Default {
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

/// The free list, implemented as a lock-free linked list
#[derive(Default)]
pub(crate) struct AtomicFreeList {
    next: AtomicCell<Option<NonNull<MaybeFreeSlot>>>
}
impl FreeList for AtomicFreeList {
    #[inline]
    fn next_free(&self) -> Option<NonNull<MaybeFreeSlot>> {
        self.next.load()
    }
    #[inline]
    unsafe fn replace_next_free(&self, next: Option<NonNull<MaybeFreeSlot>>) -> Option<NonNull<MaybeFreeSlot>> {
        self.next.swap(next)
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
                    next_free.as_ref().free.prev_free
                ).is_err() { continue /* retry */ }
                debug_assert_eq!(
                    next_free.as_ref().free.marker,
                    FREE_SLOT_MARKER
                );
                return Some(NonNull::from(&next_free.as_ref().obj).cast())
            }
        }
    }
    #[inline]
    unsafe fn insert_free(&self, free: NonNull<u8>) {
        let mut free = free.cast::<MaybeFreeSlot>();
        let mut next_free = self.next.load();
        loop {
            // Update free pointer
            free.as_mut().free.prev_free = next_free;
            free.as_mut().free.marker = FREE_SLOT_MARKER;
            match self.next.compare_exchange(
                next_free,
                Some(free)
            ) {
                Ok(_) => return,
                Err(actual_next) => {
                    next_free = actual_next;
                }
            }
        }
    }
}


/// The free list, implemented as a linked list
#[derive(Default)]
pub(crate) struct SimpleFreeList {
    next: Cell<Option<NonNull<MaybeFreeSlot>>>
}
impl FreeList for SimpleFreeList {
    #[inline]
    fn next_free(&self) -> Option<NonNull<MaybeFreeSlot>> {
        self.next.get()
    }
    #[inline]
    unsafe fn replace_next_free(&self, next: Option<NonNull<MaybeFreeSlot>>) -> Option<NonNull<MaybeFreeSlot>>{
        self.next.replace(next)
    }
    #[inline]
    fn take_free(&self) -> Option<NonNull<u8>> {
        let next_free = match self.next.get() {
            Some(free) => free,
            None => return None, // Out of free space
        };
        // Update free pointer
        unsafe {
            debug_assert_eq!(
                Some(next_free),
                next_free.as_ref().free.prev_free
            );
            debug_assert_eq!(
                next_free.as_ref().free.marker,
                FREE_SLOT_MARKER
            );
            return Some(NonNull::from(&next_free.as_ref().obj).cast())
        }
    }
    #[inline]
    unsafe fn insert_free(&self, slot: NonNull<u8>) {
        let mut slot = slot.cast::<MaybeFreeSlot>();
        let next_free = self.next.get();
        if let Some(next_free) = next_free {
            debug_assert_eq!(next_free.as_ref().free.marker, FREE_SLOT_MARKER)
        }
        slot.as_mut().free.prev_free = next_free;
        slot.as_mut().free.marker = FREE_SLOT_MARKER;
        self.next.set(Some(slot))
    }
}

struct SmallArena<S: ArenaState, FL: FreeList> {
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
/// Indicates that the allocator should be thread-safe
#[cfg(feature = "sync")]
pub struct ThreadSafe;

/// Abstracts over thread safe/unsafe versions of [once_cell::sync::OnceCell]
trait OnceCell<T>: Sized {
    fn new() -> Self;
    fn get_or_init(&self, func: impl FnOnce() -> T) -> &T;
    fn get(&self) -> Option<&T>;
}
#[cfg(feature = "sync")]
impl<T> OnceCell<T> for once_cell::sync::OnceCell<T> {
    #[inline]
    fn new() -> Self {
        Default::default()
    }

    #[inline]
    fn get_or_init(&self, func: impl FnOnce() -> T) -> &T {
        once_cell::sync::OnceCell::get_or_init(self, func)
    }

    #[inline]
    fn get(&self) -> Option<&T> {
        once_cell::sync::OnceCell::get(self)
    }
}
impl<T> OnceCell<T> for once_cell::unsync::OnceCell<T> {
    #[inline]
    fn new() -> Self {
        Default::default()
    }

    #[inline]
    fn get_or_init(&self, func: impl FnOnce() -> T) -> &T {
        once_cell::unsync::OnceCell::get_or_init(self, func)
    }

    #[inline]
    fn get(&self) -> Option<&T> {
        once_cell::unsync::OnceCell::get(self)
    }
}

trait AllocThreading {
    type Cell: Sized + OnceCell<SmallArena<Self::ArenaState, Self::FreeList>>;
    type FreeList: FreeList;
    type ArenaState: ArenaState;
}

pub struct SingleThreaded;
pub struct SmallObjectAllocator<A: AllocThreading> {
    // NOTE: Internally boxed to avoid bloating main struct
    arenas: Box<[A::Cell; NUM_SMALL_ARENAS]>,
}
impl<T: AllocThreading> SmallObjectAllocator<T> {
    pub fn new() -> Self {
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
