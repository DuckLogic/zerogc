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

/// An allocator that supports freeing objects in bulk via the [`GroupAlloc::clear`] method.
pub struct GroupAlloc<A: Allocator> {
    alloc: A,
    allocated_objects: RefCell<HashMap<NonNull<u8>, AllocObject>>,
}
impl<A: Allocator> GroupAlloc<A> {
    pub fn new(alloc: A) -> Self {
        GroupAlloc {
            alloc,
            allocated_objects: Default::default(),
        }
    }

    pub unsafe fn reset(&self) {
        let mut objects = self.allocated_objects.borrow_mut();
        for (&ptr, obj) in objects.iter() {
            assert_eq!(ptr, obj.ptr);
            self.alloc.deallocate(ptr, obj.layout);
        }
        objects.clear();
    }
}
unsafe impl<A: Allocator> Allocator for GroupAlloc<A> {
    fn allocate(&self, layout: Layout) -> Result<NonNull<[u8]>, AllocError> {
        let res = self.alloc.allocate(layout)?;
        let mut objects = self.allocated_objects.borrow_mut();
        match objects.entry(res.cast::<u8>()) {
            Entry::Occupied(_) => panic!("Duplicate entries for {res:?}"),
            Entry::Vacant(entry) => {
                entry.insert(AllocObject {
                    ptr: res.cast(),
                    layout: Layout::from_size_align(res.len(), layout.align()).unwrap(),
                });
            }
        }
        Ok(res)
    }

    unsafe fn deallocate(&self, ptr: NonNull<u8>, layout: Layout) {
        let mut allocated = self.allocated_objects.borrow_mut();
        match allocated.entry(ptr) {
            Entry::Occupied(entry) => {
                assert_eq!(*entry.get(), AllocObject { ptr, layout });
                entry.remove();
            }
            Entry::Vacant(_) => panic!("Missing entry for {ptr:?} w/ {layout:?}"),
        }
        drop(allocated); // release guard
        self.alloc.deallocate(ptr, layout);
    }
}
impl<A: Allocator> Drop for GroupAlloc<A> {
    fn drop(&mut self) {
        unsafe {
            self.reset();
        }
    }
}
