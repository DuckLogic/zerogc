use std::mem;
use std::ptr::Shared;
use std::any::Any;

use super::{GarbageCollected, CollectorId, GarbageCollector, GcMemoryError};

#[repr(C)]
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub struct GcHeader(u32);
impl GcHeader {
    #[inline]
    pub fn new(collector_id: CollectorId, mark: bool) -> Self {
        GcHeader(collector_id.0 << 16 | (if mark { 1 } else { 0 }))
    }
    /// The header's lowest mark bit.
    ///
    /// During the first collection this bit is set if the object needs to be kept,
    /// so everything else doesn't have the bit set and and is destroyed.
    /// However, in the next collection we flip the meaning of this bit,
    /// so that everything with the bit set is destroyed instead of kept.
    /// This avoids needing to search through the surviving objects and reset the mark bit to `0`,
    /// since anything that survived the first collection would've been `1`.
    ///
    /// This is just as simple to implement, since it's just an xor to invert the bit
    /// instead of just an or.
    /// We also just change allocation to default to an `expected_header` field,
    /// so all newly allocated objects will have the bit we expect.
    #[inline]
    pub fn mark(self) -> bool {
        (self.0 & 1) != 0
    }
    /// Create a new `GcHeader`, with the mark bit flipped/inverted
    #[inline]
    pub fn flipped_mark(self) -> GcHeader {
        GcHeader(self.0 ^ 1)
    }
    #[inline]
    pub fn verify_collector(self, collector: u16) {
        assert_eq!(self.collector_id(), collector, "Wrong garbage collector!")
    }
    /// The integer id of the collector,
    /// filling the top 16 bits of the header.
    #[inline]
    pub fn collector_id(self) -> u16 {
        (self.0 >> 16) as u16
    }
}
#[repr(C)]
#[derive(Copy, Clone)]
pub struct GcObject<T: ?Sized + GarbageCollected> {
    pub header: GcHeader,
    pub value: T
}
impl<T: ?Sized + GarbageCollected> GcObject<T> {
    #[inline]
    pub fn size(&self) -> usize {
        // Remember to keep this consistent with `GcHeap::size_of`
        mem::size_of_val(self)
    }
    #[inline]
    pub fn new(header: GcHeader, value: T) -> Box<Self> {
        Box::new(GcObject { value, header })
    }
    #[inline]
    pub unsafe fn from_value_ptr(value: Shared<T>) -> Shared<GcObject<T>> {
        // Pointer arithmetic to retrieve the header of the object
        let ptr = (value.as_ptr() as *mut u8)
            .offset(-(mem::size_of::<GcHeader>() as isize));
        // Cast it back into a `GcObject`
        Shared::from((ptr as *mut GcObject<T>))
    }

    #[inline]
    pub fn erased(self: Box<Self>) -> ErasedGcObject {
        self // This works because of coersion
    }

}
pub type ErasedGcObject = Box<GcObject<AnyGcAssumed>>;

pub struct GcHeap {
    values: Vec<ErasedGcObject>,
    /// The current limit on the number of bytes of allocated memory,
    /// causing an OOM if you attempt to go past this point.
    pub limit: usize,
    /// The size of the currently allocated memory.
    size: usize,
}
impl GcHeap {
    /// The garbage collected size of the specified object
    #[inline]
    pub const fn size_of<T>() -> usize {
        mem::size_of::<GcHeader>() + mem::size_of::<T>()
    }
    #[inline]
    pub fn with_capacity(capacity: usize) -> Self {
        GcHeap {
            values: Vec::with_capacity(capacity / 32),
            size: 0,
            limit: capacity,
        }
    }
    /// Give the size of the currently allocated memory
    #[inline]
    pub fn current_size(&self) -> usize {
        debug_assert!(self.size <= self.limit, "Size {} exceeds limit {}", self.size, self.limit);
        self.size
    }
    /// The available memory remaining before the `limit` is reached
    #[inline]
    pub fn remaining(&self) -> usize {
        self.limit - self.current_size()
    }
    #[inline]
    pub fn can_alloc(&self, requested: usize) -> bool {
        requested <= self.remaining()
    }
    #[inline]
    pub fn try_alloc<T>(&mut self, header: GcHeader, value: T) -> Result<Shared<T>, GcMemoryError> {
        if self.can_alloc(GcHeap::size_of::<T>()) {
            let object = GcObject::new(header, value);
            let ptr = Shared::from(&object.value);
            self.values.push(object.erased());
            self.size += object.size();
            Ok(ptr)
        } else {
            Err(GcMemoryError::new(mem::size_of::<T>(), self.remaining()))
        }
    }
    pub fn sweep(&mut self, keep: GcHeader, destroy: GcHeader) -> bool {
        assert_eq!(keep.collector_id(), destroy.collector_id());
        assert_ne!(keep.mark(), destroy.mark());
        let mut size = &mut self.size;
        debug_assert_eq!(self.size, self.compute_size());
        self.values.retain(|object| {
            if object.header != keep {
                debug_assert_eq!(object.header, destroy);
                *size -= object.size();
                true
            } else {
                false
            }
        });
        debug_assert_eq!(self.size, self.compute_size());
    }
    fn compute_size(&self) -> usize {
        self.values.iter().map(|value| value.size()).sum()
    }
}


/// Wrapper type for an `Any` trait object,
/// indicating we're unsafely assuming it implements `GarbageCollected`.
///
/// The type should never actually be traced, and undefined behavior will occur if it does.
pub struct AnyGcAssumed(Any);
unsafe impl GarbageCollected for AnyGcAssumed {
    /// It is undefined behavior to even consider invoking the trace function.
    const NEEDS_TRACE: bool = true;

    #[inline]
    unsafe fn raw_trace(&self, collector: &mut GarbageCollector) {
        unreachable!()
    }
}

