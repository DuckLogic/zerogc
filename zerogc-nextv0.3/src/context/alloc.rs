use std::alloc::Layout;
use std::cell::{Cell, RefCell};
use std::collections::hash_map::Entry;
use std::collections::HashMap;
use std::ptr::NonNull;

use allocator_api2::alloc::{AllocError, Allocator};

pub struct CountingAlloc<A: Allocator> {
    alloc: A,
    allocated_bytes: Cell<usize>,
}
impl<A: Allocator> CountingAlloc<A> {
    #[inline]
    pub fn new(alloc: A) -> Self {
        CountingAlloc {
            alloc,
            allocated_bytes: Cell::new(0),
        }
    }

    #[inline]
    pub fn as_inner(&self) -> &A {
        &self.alloc
    }

    #[inline]
    pub fn as_inner_mut(&mut self) -> &mut A {
        &mut self.alloc
    }

    #[inline]
    pub fn allocated_bytes(&self) -> usize {
        self.allocated_bytes.get()
    }
}

unsafe impl<A: Allocator> Allocator for CountingAlloc<A> {
    #[inline]
    fn allocate(&self, layout: Layout) -> Result<NonNull<[u8]>, AllocError> {
        let res = self.as_inner().allocate(layout)?;
        self.allocated_bytes
            .set(self.allocated_bytes.get() + res.len());
        Ok(res)
    }

    #[inline]
    unsafe fn deallocate(&self, ptr: NonNull<u8>, layout: Layout) {
        self.as_inner().deallocate(ptr, layout);
        self.allocated_bytes
            .set(self.allocated_bytes.get() - layout.size());
    }
}

#[derive(Debug, Eq, PartialEq)]
struct AllocObject {
    ptr: NonNull<u8>,
    layout: Layout,
}

/// An arena allocator that only supports freeing objects in bulk.
///
/// This emulates [`bumpalo::Bump`],
/// but allocates each object individually for better tracking.
pub struct ArenaAlloc<A: Allocator> {
    alloc: A,
    allocated_objects: RefCell<Vec<AllocObject>>,
}
impl<A: Allocator> ArenaAlloc<A> {
    pub fn new(alloc: A) -> Self {
        ArenaAlloc {
            alloc,
            allocated_objects: Default::default(),
        }
    }

    pub unsafe fn reset(&mut self) {
        let objects = self.allocated_objects.get_mut();
        for obj in objects.iter() {
            self.alloc.deallocate(obj.ptr, obj.layout);
        }
        objects.clear();
    }
}
unsafe impl<A: Allocator> Allocator for ArenaAlloc<A> {
    fn allocate(&self, layout: Layout) -> Result<NonNull<[u8]>, AllocError> {
        let res = self.alloc.allocate(layout)?;

        self.allocated_objects.borrow_mut().push(AllocObject {
            ptr: res.cast(),
            layout: Layout::from_size_align(res.len(), layout.align()).unwrap(),
        });
        Ok(res)
    }

    unsafe fn deallocate(&self, ptr: NonNull<u8>, layout: Layout) {
        // dealloc is nop
    }
}
impl<A: Allocator> Drop for ArenaAlloc<A> {
    fn drop(&mut self) {
        unsafe {
            self.reset();
        }
    }
}
