use std::ptr::NonNull;
use std::cell::{Cell, RefCell};
use crate::GcHeader;

#[derive(Debug)]
pub struct Chunk {
    data: Vec<u8>,
    /// The end the data (our limit since we grow forwards)
    limit: NonNull<u8>,
    /// NOTE: This grows upwards
    current: Cell<NonNull<u8>>,
}
impl Chunk {
    pub fn alloc(capacity: usize) -> Chunk {
        assert!(capacity >= 1);
        let mut data = Vec::<u8>::with_capacity(capacity);
        let ptr = data.as_mut_ptr();
        // TODO: Breaks encapsulation ^_^
        assert_eq!(ptr as usize & (std::mem::align_of::<GcHeader>() - 1), 0);
        unsafe {
            let limit = NonNull::new_unchecked(ptr.add(capacity));
            let current = NonNull::new_unchecked(ptr);
            Chunk { data, limit, current: Cell::new(current) }
        }
    }
    pub fn clear(&mut self) {
        unsafe {
            let ptr = self.data.as_mut_ptr();
            debug_assert_eq!(
                self.limit.as_ptr(),
                ptr.wrapping_add(self.data.capacity())
            );
            self.current.set(NonNull::new_unchecked(ptr));
        }
    }
    #[inline]
    pub fn start(&self) -> *mut u8 {
        self.data.as_ptr() as *mut u8
    }
    #[inline]
    pub fn capacity(&self) -> usize {
        self.data.capacity()
    }
    #[inline]
    pub fn used_bytes(&self) -> usize {
        self.current.get().as_ptr() as usize - self.start() as usize
    }
    #[inline(always)]
    pub fn try_alloc(&self, amount: usize) -> Option<NonNull<u8>> {
        unsafe {
            let current = self.current.get().as_ptr();
            let limit = self.limit.as_ptr();
            debug_assert!(current as usize <= limit as usize);
            let remaining_bytes = limit as usize - current as usize;
            if amount <= remaining_bytes {
                let updated = current.add(amount);
                self.current.set(NonNull::new_unchecked(updated));
                Some(NonNull::new_unchecked(current))
            } else {
                None
            }
        }
    }
    #[inline]
    pub fn is_used(&self, ptr: *mut u8) -> bool {
        ptr >= self.start() && ptr < self.current.get().as_ptr()
    }
    #[inline]
    pub fn contains(&self, ptr: *mut u8) -> bool {
        ptr >= self.start() && ptr < self.limit.as_ptr()
    }
}

pub struct Arena {
    chunks: RefCell<Vec<Chunk>>,
    current_chunk: Cell<NonNull<Chunk>>
}
impl Arena {
    pub fn new() -> Self {
        Arena::from_chunk(Chunk::alloc(2048))
    }
    pub fn from_chunk(chunk: Chunk) -> Self {
        assert!(chunk.capacity() >= 1);
        let chunks = vec![chunk];
        let current_chunk = NonNull::from(chunks.first().unwrap());
        Arena {
            chunks: RefCell::new(chunks),
            current_chunk: Cell::new(current_chunk)
        }
    }
    #[cfg(debug_assertions)]
    pub fn check_contains_ptr(&self, ptr: *mut u8) -> Option<usize> {
        self.chunks.borrow().iter().position(|c| c.is_used(ptr))
    }
    #[inline]
    pub fn current_chunk_capacity(&self) -> usize {
        unsafe { self.current_chunk.get().as_ref().capacity() }
    }
    pub fn total_used(&self) -> usize {
        let chunks = self.chunks.borrow();
        chunks.iter().map(Chunk::used_bytes).sum()
    }
    #[inline(always)]
    pub fn alloc_bytes(&self, amount: usize) -> *mut u8 {
        unsafe {
            let chunk = &*self.current_chunk.get().as_ptr();
            let ptr = match chunk.try_alloc(amount) {
                Some(value) => value,
                None => self.alloc_fallback(amount),
            };
            ptr.as_ptr()
        }
    }
    #[inline(never)]
    #[cold]
    fn alloc_fallback(&self, amount: usize) -> NonNull<u8> {
        self.create_raw_chunk(amount);
        unsafe {
            self.current_chunk.get().as_ref().try_alloc(amount).unwrap()
        }
    }
    pub fn create_raw_chunk(&self, min_size: usize) {
        let last_chunk_size = self.chunks.borrow().last().unwrap().data.capacity();
        self.create_raw_chunk_exact(std::cmp::max(
            last_chunk_size * 2, // we want doubling to ensure amortized growth
            min_size
        ));
    }
    fn create_raw_chunk_exact(&self, min_size: usize) {
        assert!(min_size > 1);
        let mut chunks = self.chunks.borrow_mut();
        self.current_chunk.set(NonNull::dangling()); // sanity
        chunks.push(Chunk::alloc(min_size));
        self.current_chunk.set(NonNull::from(chunks.last_mut().unwrap()));
    }
    pub unsafe fn replace_single_chunk(&self, chunk: Chunk) -> Chunk {
        let v = vec![chunk];
        self.current_chunk.set(NonNull::from(v.last().unwrap()));
        let mut old_chunks = self.chunks.replace(v);
        let mut chunk = old_chunks.pop().unwrap();
        chunk.clear();
        chunk
    }
}
