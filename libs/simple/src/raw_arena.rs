use std::ptr::NonNull;
use std::cell::{Cell, RefCell, Ref};
use std::alloc::Layout;

#[derive(Debug)]
pub struct Chunk {
    data: Vec<u8>,
    /// The start of the data (our limit since we grow backwards)
    limit: NonNull<u8>,
    /// NOTE: This grows backwards (towards the front) make computing alignment easier
    current: Cell<NonNull<u8>>,
}
impl Chunk {
    pub fn alloc(capacity: usize) -> Chunk {
        assert!(capacity >= 1);
        let mut data = Vec::<u8>::with_capacity(capacity);
        let ptr = data.as_mut_ptr();
        unsafe {
            let limit = NonNull::new_unchecked(ptr);
            let current = NonNull::new_unchecked(ptr.add(capacity));
            Chunk { data, limit, current: Cell::new(current) }
        }
    }
    #[inline]
    pub fn capacity(&self) -> usize {
        self.data.capacity()
    }
    #[inline]
    fn back(&self) -> *mut u8 {
        unsafe { self.data.as_ptr().add(self.data.capacity()) as *mut u8 }
    }
    #[inline]
    pub fn used_bytes(&self) -> usize {
        let back = self.back() as usize;
        let current = self.current.get().as_ptr() as usize;
        back - current
    }
    /// Reset the chunk, without freeing any of the underlying memory
    pub unsafe fn reset(&self) {
        debug_assert_eq!(self.data.as_ptr(), self.limit.as_ptr());
        self.current.set(NonNull::new_unchecked(self.back()));
    }
    #[inline(always)]
    pub fn try_alloc_layout(&self, layout: Layout) -> Option<NonNull<u8>> {
        unsafe {
            // This chops off lower bits, rounding down
            debug_assert!(layout.align().is_power_of_two());
            let current = self.current.get().as_ptr();
            let limit = self.limit.as_ptr();
            debug_assert!(limit as usize <= current as usize);
            let ptr = (current as usize).checked_sub(layout.size())?;
            let aligned_ptr = (ptr & !(layout.align() - 1)) as *mut u8;
            if aligned_ptr >= limit {
                self.current.set(NonNull::new_unchecked(aligned_ptr));
                Some(NonNull::new_unchecked(aligned_ptr))
            } else {
                None
            }
        }
    }
    #[inline]
    pub fn contains(&self, ptr: *mut u8) -> bool {
        ptr >= self.limit.as_ptr() && ptr < self.back()
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
    #[inline]
    pub unsafe fn raw_chunks(&self) -> Ref<'_, [Chunk]> {
        Ref::map(self.chunks.borrow(), Vec::as_slice)
    }
    pub fn num_chunks(&self) -> usize {
        self.chunks.borrow().len()
    }
    #[inline]
    pub fn current_chunk_capacity(&self) -> usize {
        unsafe { self.current_chunk.get().as_ref().capacity() }
    }
    pub fn total_used(&self) -> usize {
        let chunks = self.chunks.borrow();
        chunks.iter().map(Chunk::used_bytes).sum()
    }
    #[inline(always)] // TODO: Is this *always* necessary?
    pub fn alloc<T>(&self, value: T) -> &mut T {
        unsafe {
            let layout = Layout::new::<T>();
            let ptr = self.alloc_layout(layout) as *mut T;
            ptr.write(value);
            &mut *ptr
        }
    }
    #[inline(always)]
    pub fn alloc_layout(&self, layout: Layout) -> *mut u8 {
        unsafe {
            let chunk = &*self.current_chunk.get().as_ptr();
            let ptr = match chunk.try_alloc_layout(layout) {
                Some(value) => value,
                None => self.alloc_fallback(layout),
            };
            ptr.as_ptr()
        }
    }
    #[inline(never)]
    #[cold]
    fn alloc_fallback(&self, layout: Layout) -> NonNull<u8> {
        self.create_raw_chunk(layout.size() + layout.align());
        unsafe {
            self.current_chunk.get().as_ref().try_alloc_layout(layout).unwrap()
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
    pub fn reset_single_chunk(&self, chunk: Chunk) {
        let v = vec![chunk];
        self.current_chunk.set(NonNull::from(v.last().unwrap()));
        self.chunks.replace(v);
    }
}

