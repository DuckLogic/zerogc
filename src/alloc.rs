use std::mem;
use std::ptr::NonNull;

use super::{GarbageCollected, CollectorId, GarbageCollectionSystem, GcMemoryError};

#[repr(C)]
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub struct GcHeader(u32);
impl GcHeader {
    #[inline]
    pub fn new(collector_id: CollectorId, mark: bool) -> Self {
        GcHeader((collector_id.0 as u32) << 16 | (if mark { 1 } else { 0 }))
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
impl<T: GarbageCollected> GcObject<T> {
    #[inline]
    pub fn new(header: GcHeader, value: T) -> Box<Self> {
        Box::new(GcObject { value, header })
    }
    #[inline]
    pub unsafe fn from_value_ptr(value: NonNull<T>) -> NonNull<GcObject<T>> {
        // Pointer arithmetic to retrieve the header of the object
        let ptr = (value.as_ptr() as *mut u8)
            .sub(mem::size_of::<GcHeader>());
        // Cast it back into a `GcObject`
        NonNull::new_unchecked(ptr as *mut GcObject<T>)
    }
    /// Erase the type information for this object (turning into an unsized type)
    ///
    /// This also erases all lifetime information to `'static`, making it unsafe
    #[inline]
    pub unsafe fn erased(self: Box<Self>) -> ErasedGcObject {
        let temp_lifetime = self as Box<GcObject<dyn DynGarbageCollected + '_>>;
        std::mem::transmute::<Box<GcObject<dyn DynGarbageCollected + '_>>, Box<GcObject<dyn DynGarbageCollected + 'static>>>(temp_lifetime)
    }
}
impl<T: ?Sized + GarbageCollected> GcObject<T> {
    #[inline]
    pub fn size(&self) -> usize {
        // Remember to keep this consistent with `GcHeap::size_of`
        mem::size_of_val(self)
    }
}
pub type ErasedGcObject = Box<GcObject<dyn DynGarbageCollected>>;

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
    pub fn try_alloc<T>(&mut self, header: GcHeader, value: T) -> Result<NonNull<T>, GcMemoryError>
        where T: GarbageCollected {
        if self.can_alloc(GcHeap::size_of::<T>()) {
            Ok(self.force_alloc(header, value)?)
        } else {
            Err(GcMemoryError::new(mem::size_of::<T>(), self.remaining()))
        }
    }
    #[inline]
    pub fn force_alloc<T>(&mut self, header: GcHeader, value: T) -> Result<NonNull<T>, GcMemoryError>
        where T: GarbageCollected {
        let object = GcObject::new(header, value);
        let ptr = NonNull::from(&object.value);
        let size = object.size();
        self.values.push(unsafe { object.erased() /* NOTE: Erases lifetimes */ });
        self.size += size;
        Ok(ptr)
    }
    pub fn sweep(&mut self, keep: GcHeader, destroy: GcHeader) {
        assert_eq!(keep.collector_id(), destroy.collector_id());
        assert_ne!(keep.mark(), destroy.mark());
        debug_assert_eq!(self.size, self.compute_size());
        let size = &mut self.size;
        self.values.retain(|object| {
            if object.header != keep {
                debug_assert_eq!(object.header, destroy);
                *size -= object.size();
                false
            } else {
                true
            }
        });
        debug_assert_eq!(self.size, self.compute_size());
    }
    fn compute_size(&self) -> usize {
        self.values.iter().map(|value| value.size()).sum()
    }
}

#[doc(hidden)]
pub unsafe trait DynGarbageCollected {
    unsafe fn raw_trace(&self, collector: &mut GarbageCollectionSystem);
}
unsafe impl<T: ?Sized + GarbageCollected> DynGarbageCollected for T {
    unsafe fn raw_trace(&self, collector: &mut GarbageCollectionSystem) {
        GarbageCollected::raw_trace(self, collector)
    }
}
unsafe impl<'a> GarbageCollected for dyn DynGarbageCollected + 'a {
    /// We must conservatively assume that we need to be traced
    const NEEDS_TRACE: bool = true;

    #[inline]
    unsafe fn raw_trace(&self, collector: &mut GarbageCollectionSystem) {
        (self as &dyn DynGarbageCollected).raw_trace(collector)
    }
}

