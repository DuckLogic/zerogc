//! A thread safe implementation of small object arenas
//!
//! Mostly uses atomics, but also a bit of locking.
use parking_lot::Mutex;
use std::cell::{Cell};
use std::ptr::NonNull;
use std::sync::atomic::{Ordering, AtomicUsize};
use crossbeam_utils::atomic::AtomicCell;

use super::{ArenaState, FreeList, MaybeFreeSlot, FREE_SLOT_MARKER, Chunk};
use super::nosync::SimpleChunk;

/// The (thread-safe) state of a specific arena in the allocator.
///
/// TODO: Support per-thread arena caching
pub struct SyncArenaState {
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

/// The free list, implemented as a lock-free linked list
#[derive(Default)]
pub struct AtomicFreeList {
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


pub struct AtomicChunk {
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

