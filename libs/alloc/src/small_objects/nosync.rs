use std::cell::{Cell, RefCell};
use std::ptr::NonNull;

use super::{ArenaState, Chunk, FreeList, MaybeFreeSlot, FREE_SLOT_MARKER};

pub struct SimpleArenaState {
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


pub struct SimpleChunk {
    pub(super) start: *mut u8,
    pub(super) current: Cell<*mut u8>,
    pub(super) end: *mut u8
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


/// The free list, implemented as a linked list
#[derive(Default)]
pub struct SimpleFreeList {
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
