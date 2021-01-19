use std::ptr::NonNull;
use crate::{SimpleAllocator, AllocatedObject, AllocationError};

use std::alloc::{Allocator, Global, Layout};
use std::sync::atomic::{AtomicUsize, Ordering};
use crossbeam_utils::atomic::AtomicCell;

/// The header of objects allocated in the std allocator
///
/// This is used to implement a linked list of active objects
#[repr(C)]
struct ObjHeader {
    prev: AtomicCell<Option<NonNull<ObjHeader>>>,
}

#[inline]
pub const fn header_padding(inner_type: Layout) -> usize {
    Layout::new::<ObjHeader>().padding_needed_for(inner_type.align())
}
#[inline]
pub const fn layout_including_header(inner_type: Layout) -> Layout {
    let mut required_align = std::mem::align_of::<ObjHeader>();
    if inner_type.align() > required_align {
        required_align = inner_type.align()
    }
    unsafe {
        Layout::from_size_align_unchecked(
            std::mem::size_of::<ObjHeader>()
                + header_padding(inner_type)
                + inner_type.size(),
            required_align
        )
    }
}

#[derive(Default)]
pub struct StdAllocator<A: Allocator = Global> {
    alloc: A,
    total_used: AtomicUsize,
    last: AtomicCell<Option<NonNull<ObjHeader>>>
}
impl<A: Allocator> StdAllocator<A> {
    #[inline]
    unsafe fn insert_header(&self, mut header: NonNull<ObjHeader>) {
        let mut last = self.last.load();
        loop {
            header.as_mut().prev.store(last);
            match self.last.compare_exchange(last, Some(header)) {
                Ok(_) => return,
                Err(actual_last) => {
                    last = actual_last;
                }
            }
        }
    }
    #[inline]
    unsafe fn remove_header(&self, header: NonNull<ObjHeader>) {
        unimplemented!()
    }
}
impl StdAllocator {
    pub fn new() -> Self {
        Self::default()
    }
}
unsafe impl<A: Allocator> SimpleAllocator for StdAllocator<A> {
    /// We use at least one word for linking
    const MIN_SIZE: usize = std::mem::size_of::<usize>();
    /// There is no *inherent* limitation on sizes in the allocator API
    const MAX_SIZE: Option<usize> = None;
    /// There is no *inherent* limitation on alignment in the allocator API
    const MAX_ALIGNMENT: Option<usize> = None;

    #[inline]
    fn alloc(&self, layout: Layout) -> Result<AllocatedObject, AllocationError> {
        let total_layout = layout_including_header(layout);
        let bytes = match self.alloc.allocate(total_layout) {
            Ok(bytes) => bytes,
            Err(cause) => {
                return Err(AllocationError::StdError { cause, layout: total_layout })
            }
        };
        unsafe {
            self.insert_header(bytes.cast::<ObjHeader>());
            let res = bytes.as_mut_ptr().add(std::mem::size_of::<ObjHeader>()
                + header_padding(layout));
            Ok(AllocatedObject {
                layout, ptr: NonNull::new_unchecked(res)
            })
        }
    }

    unsafe fn free(&self, mem: AllocatedObject) {
        unimplemented!("{:?}", mem)
    }

    #[inline]
    fn used_memory(&self) -> usize {
        self.total_used.load(Ordering::Acquire)
    }

    #[inline]
    fn reserved_memory(&self) -> usize {
        self.used_memory()
    }

    unsafe fn unchecked_reset(&self) -> usize {
        unimplemented!()
    }
}