use std::ptr::NonNull;
use std::cell::{Cell, RefCell, Ref};
use std::alloc::Layout;

pub struct Chunk {
    data: Vec<u8>,
    /// The back of the data (our limit since we grow backwards)
    limit: NonNull<u8>,
    /// NOTE: This grows backwards to make computing alignment easier
    current: Cell<NonNull<u8>>,
}
impl Chunk {
    fn alloc(capacity: usize) -> Chunk {
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
    pub fn used_bytes(&self) -> usize {
        let limit = self.limit.as_ptr() as usize;
        let current = self.current.get().as_ptr() as usize;
        debug_assert!(current > limit);
        current - limit
    }
    /// Reset the chunk, without freeing any of the underlying memory
    pub unsafe fn reset(&self) {
        let capacity = self.capacity();
        let back = self.data.as_ptr().add(capacity);
        self.current.set(NonNull::new_unchecked(back as *mut u8));
    }
    #[inline(always)]
    fn try_alloc_layout(&self, layout: Layout) -> Option<NonNull<u8>> {
        unsafe {
            // This chops off lower bits, rounding down
            debug_assert!(layout.align().is_power_of_two());
            let current = self.current.as_ptr();
            let limit = self.limit.as_ptr();
            debug_assert!(limit as usize <= current as usize);
            let aligned_ptr = (current as usize & !(layout.align() - 1)) as *mut u8;
            // NOTE: aligned_ptr cloud in theory be out of bounds (so we use wrapping_offset_of)
            let remaining_len = aligned_ptr as isize - limit as isize;
            debug_assert!(remaining_len + layout.align() as isize >= 0);
            // NOTE: If layout.size() overflows isize this test will implicitly fail
            if remaining_len >= layout.size() as isize {
                self.current.set(NonNull::new_unchecked(aligned_ptr.sub(layout.size())));
                Some(NonNull::new_unchecked(aligned_ptr))
            } else {
                None
            }
        }
    }
}

pub struct Arena {
    chunks: RefCell<Vec<Chunk>>,
    current_chunk: Cell<NonNull<Chunk>>
}
impl Arena {
    pub fn new() -> Self {
        let chunks = vec![Chunk::alloc(2048)];
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
        let mut chunks = self.chunks.borrow_mut();
        self.current_chunk.set(NonNull::dangling()); // sanity
        let last_chunk_size = chunks.last().unwrap().data.capacity();
        let result_size = std::cmp::max(
            last_chunk_size * 2, // we want doubling to ensure amortized growth
            min_size
        );
        chunks.push(Chunk::alloc(result_size));
        self.current_chunk.set(NonNull::from(chunks.last_mut().unwrap()));
    }
    pub unsafe fn free_old_chunks(&self) {
        let mut chunks = self.chunks.borrow_mut();
        // Always preserve the latest chunk
        self.current_chunk.set(NonNull::dangling());
        let last_chunk = chunks.pop().unwrap();
        chunks.retain(|chunk| chunk.used_bytes() > 0);
        chunks.push(last_chunk);
        self.current_chunk.set(NonNull::from(chunks.last_mut().unwrap()));
    }
}

