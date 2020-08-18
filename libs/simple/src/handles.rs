//! Implementation of [::zerogc::GcHandle]
//!
//! Inspired by [Mono's Lock free Gc Handles](https://www.mono-project.com/news/2016/08/16/lock-free-gc-handles/)
use std::ptr::NonNull;
use std::marker::PhantomData;
use std::sync::atomic::{self, AtomicPtr, AtomicUsize, Ordering};

use zerogc::{Trace, GcSafe, GcBindHandle, GcBrand, GcVisitor, NullTrace, TraceImmutable};
use crate::{DynTrace, GcType, Gc, SimpleCollectorContext, SimpleCollector, MarkVisitor, GcHeader, MarkState, WeakCollectorRef};

const INITIAL_HANDLE_CAPACITY: usize = 128;

/// Concurrent list of [GcHandle]s
///
/// Each bucket in the linked list is twice the size of
/// the previous one, ensuring amortized growth
///
/// The list can not be appended to while a collection
/// is in progress.
///
/// TODO: This list only grows. It never shrinks!!
pub(crate) struct GcHandleList {
    last_bucket: AtomicPtr<GcHandleBucket>,
    /// Pointer to the last free slot in the free list
    ///
    /// If the list is empty, this is null
    last_free_slot: AtomicPtr<HandleSlot>
}
impl GcHandleList {
    pub(crate) fn new() -> Self {
        use std::ptr::null_mut;
        GcHandleList {
            last_bucket: AtomicPtr::new(null_mut()),
            last_free_slot: AtomicPtr::new(null_mut()),
        }
    }
    unsafe fn append_free_slot(&self, slot: *mut HandleSlot) {
        // Verify it's actually free...
        debug_assert_eq!(
            (*slot).valid.value
                .load(Ordering::SeqCst),
            std::ptr::null_mut()
        );
        let mut last_free = self.last_free_slot
            .load(Ordering::Acquire);
        loop {
            /*
             * NOTE: Must update `prev_freed_slot`
             * BFEORE we write to the free-list.
             * Other threads must see a consistent state
             * the moment we're present in the free-list
             */
            (*slot).freed.prev_free_slot
                .store(last_free, Ordering::Release);
            let actual_last_free = self.last_free_slot.compare_and_swap(
                last_free, slot,
                Ordering::AcqRel
            );
            if actual_last_free == last_free {
                return // Success
            } else {
                last_free = actual_last_free;
            }
        }
    }
    /// Allocate a raw handle that points to the given value.
    ///
    /// ## Safety
    /// A collection may not currently be in progress.
    ///
    /// The returned handle must be fully initialized
    /// before the next collection begins.
    #[inline]
    pub(crate) unsafe fn alloc_raw_handle(&self, value: *mut ()) -> &GcRawHandle {
        // TODO: Should we weaken these orderings?
        let mut slot = self.last_free_slot.load(Ordering::Acquire);
        while !slot.is_null() {
            /*
             * NOTE: If another thread has raced us and already initialized
             * this free slot, this pointer could be nonsense.
             * That's okay - it's still initialized memory.
             */
            let prev = (*slot).freed.prev_free_slot.load(Ordering::Acquire);
            /*
             * If this CAS succeeds, we have ownership.
             * Otherwise another thread beat us and
             * we must try again.
             */
            let actual_slot = self.last_free_slot.compare_and_swap(
                slot, prev,
                Ordering::AcqRel
            );
            if actual_slot == slot {
                // Verify it's actually free...
                debug_assert_eq!(
                    (*slot).valid.value
                        .load(Ordering::SeqCst),
                    std::ptr::null_mut()
                );
                /*
                 * We own the slot, initialize it to point to
                 * the provided pointer. The user is responsible
                 * for any remaining initialization.
                 */
                (*slot).valid.value
                    .store(value, Ordering::Release);
                return &(*slot).valid;
            } else {
                // Try again
                slot = actual_slot;
            }
        }
        // Empty free list
        self.alloc_handle_fallback(value)
    }
    /// Fallback to creating more buckets
    /// if the free-list is empty
    #[cold]
    #[inline(never)]
    unsafe fn alloc_handle_fallback(&self, value: *mut ()) -> &GcRawHandle {
        let mut bucket = self.last_bucket.load(Ordering::Acquire);
        loop {
            // TODO: Should we be retrying the free-list?
            let new_size: usize;
            if bucket.is_null() {
                new_size = INITIAL_HANDLE_CAPACITY;
            } else {
                if let Some(slot) = (*bucket).blindly_alloc_slot(value) {
                    /*
                     * NOTE: The caller is responsible
                     * for finishing up initializing this.
                     * We've merely set the pointer to `value`
                     */
                    return &slot.valid;
                }
                // Double capacity for amortized growth
                new_size = (*bucket).slots.len() * 2;
            }
            match self.init_bucket(bucket, new_size) {
                /*
                 * We honestly don't care what thread
                 * allocated the new bucket. We just want
                 * to use it */
                Ok(new_bucket) | Err(new_bucket) => {
                    bucket = new_bucket as *const _
                        as *mut _;
                }
            }
        }
    }
    unsafe fn init_bucket(
        &self,
        prev_bucket: *mut GcHandleBucket,
        desired_size: usize
    ) -> Result<&GcHandleBucket, &GcHandleBucket> {
        let mut slots: Vec<HandleSlot> = Vec::with_capacity(desired_size);
        // Zero initialize slots - assuming this is safe
        slots.as_mut_ptr().write_bytes(0, desired_size);
        slots.set_len(desired_size);
        let allocated_bucket = Box::into_raw(Box::new(GcHandleBucket {
            slots: slots.into_boxed_slice(),
            last_alloc: AtomicUsize::new(0),
            prev: AtomicPtr::new(prev_bucket),
        }));
        let actual_bucket = self.last_bucket.compare_and_swap(
            prev_bucket, allocated_bucket, Ordering::SeqCst
        );
        if actual_bucket == prev_bucket {
            Ok(&*actual_bucket)
        } else {
            /*
             * Someone else beat us too creating the bucket.
             *
             * Free the bucket we've created and return
             * their bucket
             */
            drop(Box::from_raw(allocated_bucket));
            Err(&*actual_bucket)
        }
    }
}
unsafe impl DynTrace for GcHandleList {
    fn trace(&mut self, visitor: &mut MarkVisitor) {
        /*
         * TODO: This fence seems unnecessary since we should
         * already have exclusive access.....
         */
        atomic::fence(Ordering::Acquire);
        let mut bucket = self.last_bucket.load(Ordering::Relaxed);
        while !bucket.is_null() {
            unsafe {
                // We should have exclusive access!
                let slots = &mut *(*bucket).slots;
                for slot in slots {
                    if slot.is_valid(Ordering::Relaxed) {
                        slot.valid.mark_inner(&mut *visitor);
                    }
                }
                bucket = (*bucket).prev.load(Ordering::Relaxed);
            }
        }
        /*
         * Release any pending writes (for relocated pointers)
         * TODO: We should have exclusive access, like above!
         */
        atomic::fence(Ordering::Release);
    }
}
impl Drop for GcHandleList {
    fn drop(&mut self) {
        let mut bucket = self.last_bucket.load(Ordering::Acquire);
        while !bucket.is_null() {
            unsafe {
                drop(Box::from_raw(bucket));
                bucket = (*bucket).prev
                    .load(Ordering::Acquire);
            }
        }
    }
}
struct GcHandleBucket {
    slots: Box<[HandleSlot]>,
    /// A pointer to the last allocated slot
    ///
    /// This doesn't take into account freed slots,
    /// so should only be used as a fallback if
    /// the free-list is empty
    last_alloc: AtomicUsize,
    /// Pointer to the last bucket in
    /// the linked list
    ///
    /// This must be freed manually.
    /// Dropping this bucket doesn't drop the
    /// last one.
    prev: AtomicPtr<GcHandleBucket>
}
impl GcHandleBucket {
    /// Acquire a new raw handle from this bucket,
    /// or `None` if the bucket is believed to be empty
    ///
    /// This **doesn't reused freed values**, so should
    /// only be used as a fallback if the free-list is empty.
    /// 
    /// ## Safety
    /// See docs on [GcHandleList::alloc_raw_bucket]
    unsafe fn blindly_alloc_slot(&self, value: *mut ()) -> Option<&HandleSlot> {
        let last_alloc = self.last_alloc.load(Ordering::Relaxed);
        for (i, slot) in self.slots.iter().enumerate()
            .skip(last_alloc) {
            // TODO: All these fences must be horrible on ARM
            if slot.valid.value.compare_and_swap(
                std::ptr::null_mut(), value,
                Ordering::AcqRel
            ).is_null() {
                // We acquired ownership!
                self.last_alloc.fetch_max(i, Ordering::AcqRel);
                return Some(&*slot);
            }
        }
        None
    }
}
/// A slot in the array of handles
///
/// This may or may not be valid,
/// depending on whether the value pointer is null.
#[repr(C)]
pub union HandleSlot {
    freed: FreedHandleSlot,
    valid: GcRawHandle
}
impl HandleSlot {
    /// Load the current value of this pointer
    #[inline]
    fn is_valid(&self, ord: Ordering) -> bool {
        /*
         * Check if the value pointer is null
         * 
         * The pointer is present in both variants
         * of the enum.
         */
        unsafe { !self.valid.value.load(ord).is_null() }

    }
}
/// A handle that is free
#[repr(C)]
pub struct FreedHandleSlot {
    /// This corresponds to a [GcRawHandle::value]
    ///
    /// It must be null for this object to be truly invalid!
    _invalid_value: AtomicPtr<()>,
    /// The previous slot in the free list
    prev_free_slot: AtomicPtr<HandleSlot>
}
/// The underlying value of a handle.
///
/// These are reused
#[repr(C)]
pub struct GcRawHandle {
    /// Refers to the underlying value of this handle.
    ///
    /// If it's null, it's invalid, and is actually
    /// a [FreedHandle].
    ///
    /// The underlying value can only be safely accessed 
    /// if there isn't a collection in progress
    value: AtomicPtr<()>,
    /// I think this should be protected by the other atomic
    /// accesses. Regardless, I'll put it in an AtomicPtr anyways.
    // TODO: Encapsulate
    pub(crate) type_info: AtomicPtr<GcType>,
    /// The reference count to the handle
    ///
    /// If this is zero the value can be freed
    /// and this memory can be used.
    // TODO: Encapsulate
    pub(crate) refcnt: AtomicUsize
}
impl GcRawHandle {
    /// Trace this handle, assuming collection is in progress
    ///
    /// ## Safety
    /// - A collection must currently be in progress
    /// - It is assumed that the appropriate atomic fences (if any)
    ///   have already been applied (TODO: Don't we have exclusive access?)
    unsafe fn mark_inner(&self, visitor: &mut MarkVisitor) {
        let value = self.value.load(Ordering::Relaxed);
        if value.is_null() {
            debug_assert_eq!(
                self.refcnt.load(Ordering::Relaxed),
                0
            );
            return; // Nothing to trace
        }
        debug_assert_ne!(
            self.refcnt.load(Ordering::Relaxed),
            0
        );
        let type_info = &*self.type_info.load(Ordering::Relaxed);
        visitor.visit_raw_gc(
            &mut *GcHeader::from_value_ptr(value, type_info),
            |header, visitor| {
                // Mark grey
                header.update_raw_state(MarkState::Grey.
                    to_raw(visitor.inverted_mark));
                // Visit innards
                (type_info.trace_func)(header.value(), visitor);
                // Mark black
                header.update_raw_state(MarkState::Black.
                    to_raw(visitor.inverted_mark));
            }
        );
    }
}
pub struct GcHandle<T: GcSafe> {
    inner: NonNull<GcRawHandle>,
    collector: WeakCollectorRef,
    marker: PhantomData<*mut T>
}
impl<T: GcSafe> GcHandle<T> {
    pub(crate) unsafe fn new(
        inner: NonNull<GcRawHandle>,
        collector: WeakCollectorRef
    ) -> Self {
        GcHandle {
            inner, collector,
            marker: PhantomData
        }
    }
}
unsafe impl<T: GcSafe> ::zerogc::GcHandle<T> for GcHandle<T> {
    type Context = SimpleCollectorContext;
    #[cfg(feature = "sync")]
    fn use_critical<R>(&self, func: impl FnOnce(&T) -> R) -> R {
        self.collector.ensure_valid(|collector| unsafe {
            /*
             * This should be sufficient to ensure
             * the value won't be collected or relocated.
             *
             * Note that this is implemented using a read lock,
             * so recursive calls will deadlock.
             * This is preferable to using `recursive_read`,
             * since that could starve writers (collectors).
             */
            collector.as_ref().prevent_collection(|_state| {
                let value = self.inner.as_ref().value
                    .load(Ordering::Acquire) as *mut T;
                func(&*value)
            })
        })
    }
    #[cfg(not(feature = "sync"))]
    fn use_critical<R>(&self, _func: impl FnOnce(&T) -> R) -> R {
        unimplemented!("critical sections for single-collector impl")
    }
}
unsafe impl<'new_gc, T> GcBindHandle<'new_gc, T> for GcHandle<T>
    where T: GcSafe, T: GcBrand<'new_gc, SimpleCollector>,
        T::Branded: GcSafe {
    type System = SimpleCollector;
    type Bound = Gc<'new_gc, T::Branded>;
    #[inline]
    fn bind_to(&self, _context: &'new_gc Self::Context) -> Self::Bound {
        /*
         * We can safely assume the object will
         * be as valid as long as the context.
         * By binding it to the lifetime of the context,
         * we ensure that the user will have to properly
         * track it with safepoints from now on.
         *
         * Instead of dynamically tracking the root
         * with runtime-overhead,
         * its tracked like any other object normally
         * allocated from a context. It's zero-cost
         * from now until the next safepoint.
         */
        unsafe {
            let collector = self.collector.assume_valid();
            let inner = self.inner.as_ref();
            let value = inner.value.load(Ordering::Acquire)
                as *mut T as *mut T::Branded;
            debug_assert!(!value.is_null());
            Gc::from_raw(
                collector,
                NonNull::new_unchecked(value)
            )
        }
    }

}
unsafe impl<T: GcSafe> Trace for GcHandle<T> {
    /// See docs on reachability
    const NEEDS_TRACE: bool = false;
    #[inline(always)]
    fn visit<V>(&mut self, _visitor: &mut V) -> Result<(), V::Err>
        where V: zerogc::GcVisitor {
        Ok(())
    }
}
unsafe impl<T: GcSafe> TraceImmutable for GcHandle<T> {
    #[inline(always)]
    fn visit_immutable<V>(&self, _visitor: &mut V) -> Result<(), V::Err>
        where V: GcVisitor {
        Ok(())
    }
}
unsafe impl<T: GcSafe> NullTrace for GcHandle<T> {}
impl<T: GcSafe> Clone for GcHandle<T> {
    fn clone(&self) -> Self {
        // NOTE: Dead collector -> invalid handle
        let collector = self.collector
            .ensure_valid(|id| unsafe { id.weak_ref() });
        let inner = unsafe { self.inner.as_ref() };
        debug_assert!(!inner.value
            .load(Ordering::SeqCst)
            .is_null(),
            "Pointer is invalid"
        );
        loop {
            let old_refcnt = inner.refcnt.load(Ordering::Relaxed);
            assert_ne!(
                old_refcnt, isize::max_value() as usize,
                "Reference count overflow"
            );
            if inner.refcnt.compare_and_swap(
                old_refcnt, old_refcnt + 1,
                Ordering::AcqRel
            ) == old_refcnt { break };
        }
        GcHandle {
            inner: self.inner,
            collector, marker: PhantomData
        }
    }
}
impl<T: GcSafe> Drop for GcHandle<T> {
    fn drop(&mut self) {
        self.collector.try_ensure_valid(|id| {
            let collector = match id {
                None => {
                    /*
                     * The collector is dead.
                     * Our memory has already been freed
                     */
                    return;
                },
                Some(ref id) => unsafe { id.as_ref() },
            };
            let inner = unsafe { self.inner.as_ref() };
            debug_assert!(!inner.value
                .load(Ordering::SeqCst)
                .is_null(),
                          "Pointer already invalid"
            );
            let prev = inner.refcnt
                .fetch_sub(1, Ordering::AcqRel);
            match prev {
                0 => {
                    // This should be impossible.
                    eprintln!("GcHandle refcnt Underflow");
                    std::process::abort();
                },
                1 => {
                    // Free underlying memory
                },
                _ => {}, // Other references
            }
            // Mark the value as freed
            inner.value.store(
                std::ptr::null_mut(),
                Ordering::Release
            );
            unsafe {
                collector.handle_list.append_free_slot(
                    self.inner.as_ptr() as *mut HandleSlot
                );
            }
        });
    }
}
/// In order to send *references* between threads,
/// the underlying type must be sync.
///
/// This is the same reason that `Arc<T>: Send` requires `T: Sync`
unsafe impl<T: GcSafe + Sync> Send for GcHandle<T> {}

/// If the underlying type is Sync,
/// it's safe to share garbage collected references between threads.
///
/// The collector itself is always safe
unsafe impl<T: GcSafe + Sync> Sync for GcHandle<T> {}