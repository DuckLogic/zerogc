//! Implementation of [::zerogc::GcHandle]
//!
//! Inspired by [Mono's Lock free Gc Handles](https://www.mono-project.com/news/2016/08/16/lock-free-gc-handles/)
use core::ptr::{self, NonNull, Pointee};
use core::marker::PhantomData;
use core::sync::atomic::{self, AtomicPtr, AtomicUsize, Ordering};
use core::mem::ManuallyDrop;

use alloc::boxed::Box;
use alloc::vec::Vec;

use zerogc::{GcRebrand, GcSafe, GcVisitor, HandleCollectorId, NullTrace, Trace, TraceImmutable, TrustedDrop};
use crate::{Gc, WeakCollectorRef, CollectorId, CollectorRef, CollectionManager};
use crate::collector::RawCollectorImpl;

const INITIAL_HANDLE_CAPACITY: usize = 64;

/// A [RawCollectorImpl] that supports handles
pub unsafe trait RawHandleImpl: RawCollectorImpl {
    /// Type information
    type TypeInfo: Sized;

    fn type_info_of<'gc, T: GcSafe<'gc, CollectorId<Self>>>() -> &'static Self::TypeInfo;

    fn resolve_type_info<'gc, T: ?Sized + GcSafe<'gc, CollectorId<Self>>>(
        gc: Gc<'gc, T, CollectorId<Self>>
    ) -> &'static Self::TypeInfo;

    fn handle_list(&self) -> &GcHandleList<Self>;
}

/// Concurrent list of [GcHandle]s
///
/// Each bucket in the linked list is twice the size of
/// the previous one, ensuring amortized growth
///
/// The list can not be appended to while a collection
/// is in progress.
///
/// TODO: This list only grows. It never shrinks!!
pub struct GcHandleList<C: RawHandleImpl> {
    last_bucket: AtomicPtr<GcHandleBucket<C>>,
    /// Pointer to the last free slot in the free list
    ///
    /// If the list is empty, this is null
    last_free_slot: AtomicPtr<HandleSlot<C>>
}
#[allow(clippy::new_without_default)]
impl<C: RawHandleImpl> GcHandleList<C> {
    pub fn new() -> Self {
        use core::ptr::null_mut;
        GcHandleList {
            last_bucket: AtomicPtr::new(null_mut()),
            last_free_slot: AtomicPtr::new(null_mut()),
        }
    }
    /// Append the specified slot to this list
    ///
    /// The specified slot must be logically owned
    /// and not already part of this list
    unsafe fn append_free_slot(&self, slot: *mut HandleSlot<C>) {
        // Verify it's actually free...
        debug_assert_eq!(
            (*slot).valid.value
                .load(Ordering::SeqCst),
            ptr::null_mut()
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
            /*
             * We really dont want surprise failures because we're going to
             * have to redo the above store if that happens.
             * Likewise we want acquire ordering so we don't fail unnecessarily
             * on retry.
             * In theory this is premature optimization, but I really want to
             * make this as straightforward as possible.
             * Maybe we should look into the efficiency of this on ARM?
             */
            match self.last_free_slot.compare_exchange(
                last_free, slot,
                Ordering::AcqRel,
                Ordering::Acquire
            ) {
                Ok(actual) => {
                    debug_assert_eq!(actual, last_free);
                    return; // Success
                },
                Err(actual) => {
                    last_free = actual;
                }
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
    pub(crate) unsafe fn alloc_raw_handle(&self, value: *mut ()) -> &GcRawHandle<C> {
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
             *
             * Avoid relaxed ordering and compare_exchange_weak
             * to make this straightforward.
             */
            match self.last_free_slot.compare_exchange(
                slot, prev,
                Ordering::AcqRel,
                Ordering::Acquire
            ) {
                Ok(actual_slot) => {
                    debug_assert_eq!(actual_slot, slot);
                    // Verify it's actually free...
                    debug_assert_eq!(
                        (*slot).valid.value
                            .load(Ordering::SeqCst),
                        ptr::null_mut()
                    );
                    /*
                     * We own the slot, initialize it to point to
                     * the provided pointer. The user is responsible
                     * for any remaining initialization.
                     */
                    (*slot).valid.value
                        .store(value, Ordering::Release);
                    return &(*slot).valid;
                },
                Err(actual_slot) => {
                    // Try again
                    slot = actual_slot;
                }
            }
        }
        // Empty free list
        self.alloc_handle_fallback(value)
    }
    /// Fallback to creating more buckets
    /// if the free-list is empty
    #[cold]
    #[inline(never)]
    unsafe fn alloc_handle_fallback(&self, value: *mut ()) -> &GcRawHandle<C> {
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
        prev_bucket: *mut GcHandleBucket<C>,
        desired_size: usize
    ) -> Result<&GcHandleBucket<C>, &GcHandleBucket<C>> {
        let mut slots: Vec<HandleSlot<C>> = Vec::with_capacity(desired_size);
        // Zero initialize slots - assuming this is safe
        slots.as_mut_ptr().write_bytes(0, desired_size);
        slots.set_len(desired_size);
        let allocated_bucket = Box::into_raw(Box::new(GcHandleBucket {
            slots: slots.into_boxed_slice(),
            last_alloc: AtomicUsize::new(0),
            prev: AtomicPtr::new(prev_bucket),
        }));
        match self.last_bucket.compare_exchange(
            prev_bucket, allocated_bucket,
            Ordering::SeqCst,
            Ordering::SeqCst
        ) {
            Ok(actual_bucket) => {
                assert_eq!(actual_bucket, prev_bucket);
                Ok(&*actual_bucket)
            },
            Err(actual_bucket) => {
                /*
                 * Someone else beat us to creating the bucket.
                 *
                 * Free the bucket we've created and return
                 * their bucket
                 */
                drop(Box::from_raw(allocated_bucket));
                Err(&*actual_bucket)
            }
        }
    }
    /// Trace the [GcHandle] using the specified closure.
    ///
    /// ## Safety
    /// Assumes the visitor function is well behaved.
    ///
    /// TODO: Can the 'unsafe' be removed?
    /// I think we were only using it for 'trace_inner'
    /// because it assumes the fences were already applied.
    /// Now that's behind a layer of abstraction,
    /// the unsafety has technically been moved to the caller.
    pub unsafe fn trace<F, E>(&mut self, mut visitor: F) -> Result<(), E>
        where F: FnMut(*mut (), &C::TypeInfo) -> Result<(), E> {
        /*
         * TODO: This fence seems unnecessary since we should
         * already have exclusive access.....
         */
        atomic::fence(Ordering::Acquire);
        let mut bucket = self.last_bucket.load(Ordering::Relaxed);
        while !bucket.is_null() {
            // We should have exclusive access!
            let slots = &mut *(*bucket).slots;
            for slot in slots {
                if slot.is_valid(Ordering::Relaxed) {
                    slot.valid.trace_inner(&mut visitor)?;
                }
            }
            bucket = (*bucket).prev.load(Ordering::Relaxed);
        }
        /*
         * Release any pending writes (for relocated pointers)
         * TODO: We should have exclusive access, like above!
         */
        atomic::fence(Ordering::Release);
        Ok(())
    }
}
impl<C: RawHandleImpl> Drop for GcHandleList<C> {
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
struct GcHandleBucket<C: RawHandleImpl> {
    slots: Box<[HandleSlot<C>]>,
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
    prev: AtomicPtr<GcHandleBucket<C>>
}
impl<C: RawHandleImpl> GcHandleBucket<C> {
    /// Acquire a new raw handle from this bucket,
    /// or `None` if the bucket is believed to be empty
    ///
    /// This **doesn't reused freed values**, so should
    /// only be used as a fallback if the free-list is empty.
    ///
    /// ## Safety
    /// See docs on [GcHandleList::alloc_raw_bucket]
    unsafe fn blindly_alloc_slot(&self, value: *mut ()) -> Option<&HandleSlot<C>> {
        let last_alloc = self.last_alloc.load(Ordering::Relaxed);
        for (i, slot) in self.slots.iter().enumerate()
            .skip(last_alloc) {
            // TODO: All these fences must be horrible on ARM
            if slot.valid.value.compare_exchange(
                ptr::null_mut(), value,
                Ordering::AcqRel,
                Ordering::Relaxed
            ).is_ok() {
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
pub union HandleSlot<C: RawHandleImpl> {
    freed: ManuallyDrop<FreedHandleSlot<C>>,
    valid: ManuallyDrop<GcRawHandle<C>>
}
impl<C: RawHandleImpl> HandleSlot<C> {
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
pub struct FreedHandleSlot<C: RawHandleImpl> {
    /// This corresponds to a [GcRawHandle::value]
    ///
    /// It must be null for this object to be truly invalid!
    _invalid_value: AtomicPtr<()>,
    /// The previous slot in the free list
    prev_free_slot: AtomicPtr<HandleSlot<C>>
}

/// The underlying value of a handle.
///
/// These are reused
#[repr(C)]
pub struct GcRawHandle<C: RawHandleImpl> {
    /// Refers to the underlying value of this handle.
    ///
    /// If it's null, it's invalid, and is actually
    /// a freed handle
    ///
    /// The underlying value can only be safely accessed
    /// if there isn't a collection in progress
    value: AtomicPtr<()>,
    /// I think this should be protected by the other atomic
    /// accesses. Regardless, I'll put it in an AtomicPtr anyways.
    // TODO: Encapsulate
    pub(crate) type_info: AtomicPtr<C::TypeInfo>,
    /// The reference count to the handle
    ///
    /// If this is zero the value can be freed
    /// and this memory can be used.
    // TODO: Encapsulate
    pub(crate) refcnt: AtomicUsize
}
impl<C: RawHandleImpl> GcRawHandle<C> {
    /// Trace this handle, assuming collection is in progress
    ///
    /// ## Safety
    /// - Trace function must be reasonable
    /// - A collection must currently be in progress
    /// - It is assumed that the appropriate atomic fences (if any)
    ///   have already been applied (TODO: Don't we have exclusive access?)
    unsafe fn trace_inner<F, E>(&self, trace: &mut F) -> Result<(), E>
        where F: FnMut(*mut (), &C::TypeInfo) -> Result<(), E> {
        let value = self.value.load(Ordering::Relaxed);
        if value.is_null() {
            debug_assert_eq!(
                self.refcnt.load(Ordering::Relaxed),
                0
            );
            return Ok(()); // Nothing to trace
        }
        debug_assert_ne!(
            self.refcnt.load(Ordering::Relaxed),
            0
        );
        let type_info = &*self.type_info.load(Ordering::Relaxed);
        trace(value, type_info)
    }
}
pub struct GcHandle<T: ?Sized + GcSafe<'static, CollectorId<C>>, C: RawHandleImpl> {
    inner: NonNull<GcRawHandle<C>>,
    collector: WeakCollectorRef<C>,
    /// The pointer metadata for the type.
    ///
    /// Assumed to be immutable
    /// and not change 
    /// SAFETY:
    /// 1. slices - Length never changes
    /// 2. dyn pointers - Never needs 
    metadata: <T as Pointee>::Metadata,
    marker: PhantomData<*mut T>
}
impl<T: ?Sized + GcSafe<'static, CollectorId<C>>, C: RawHandleImpl> GcHandle<T, C> {
    #[inline]
    pub(crate) unsafe fn new(
        inner: NonNull<GcRawHandle<C>>,
        collector: WeakCollectorRef<C>,
        metadata: <T as Pointee>::Metadata
    ) -> Self {
        GcHandle {
            inner, collector, metadata,
            marker: PhantomData
        }
    }
    #[inline]
    unsafe fn assume_valid(&self) -> *mut T {
        ptr::from_raw_parts_mut(
            self.inner.as_ref().value
                .load(Ordering::Acquire) as *mut (),
            self.metadata
        )
    }
}
unsafe impl<T: ?Sized + GcSafe<'static, CollectorId<C>>, C: RawHandleImpl> ::zerogc::GcHandle<T> for GcHandle<T, C> {
    type System = CollectorRef<C>;
    type Id = CollectorId<C>;

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
            C::Manager::prevent_collection(collector.as_ref(), || {
                let value = self.assume_valid();
                func(&*value)
            })
        })
    }
    #[inline]
    fn bind_to<'new_gc>(
        &self,
        context: &'new_gc <Self::System as zerogc::GcSystem>::Context
    ) -> Gc<'new_gc, <T as GcRebrand<'new_gc, Self::Id>>::Branded, Self::Id>
        where T: GcRebrand<'new_gc, Self::Id> {
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
            assert_eq!(
                collector.as_ref() as *const C,
                context.collector() as *const C,
                "Collectors mismatch"
            );
            /*
             * NOTE: Can't use regular pointer-cast
             * because of potentially mismatched vtables.
             */
            let value = crate::utils::transmute_mismatched::<
                *mut T,
                *mut T::Branded
            >(self.assume_valid());
            debug_assert!(!value.is_null());
            Gc::from_raw(NonNull::new_unchecked(value))
        }
    }
}
unsafe impl<T: ?Sized + GcSafe<'static, CollectorId<C>>, C: RawHandleImpl> Trace for GcHandle<T, C> {
    /// See docs on reachability
    const NEEDS_TRACE: bool = false;
    const NEEDS_DROP: bool = true;
    #[inline(always)]
    fn trace<V>(&mut self, _visitor: &mut V) -> Result<(), V::Err>
        where V: zerogc::GcVisitor {
        Ok(())
    }
}
unsafe impl<T: ?Sized + GcSafe<'static, CollectorId<C>>, C: RawHandleImpl> TraceImmutable for GcHandle<T, C> {
    #[inline(always)]
    fn trace_immutable<V>(&self, _visitor: &mut V) -> Result<(), V::Err>
        where V: GcVisitor {
        Ok(())
    }
}
unsafe impl<T: ?Sized + GcSafe<'static, CollectorId<C>>, C: RawHandleImpl> NullTrace for GcHandle<T, C> {}
unsafe impl<'gc, T: ?Sized + GcSafe<'static, CollectorId<C>>, C: RawHandleImpl>
    GcSafe<'gc, CollectorId<C>> for GcHandle<T, C> {
    #[inline]
    unsafe fn trace_inside_gc<V>(gc: &mut Gc<'gc, Self, CollectorId<C>>, visitor: &mut V) -> Result<(), V::Err>
        where V: GcVisitor {
        // Fine to stuff inside a pointer. We're a `Sized` type
        visitor.trace_gc(gc)
    }
}
unsafe impl<T: ?Sized + GcSafe<'static, CollectorId<C>>, C: RawHandleImpl> TrustedDrop for GcHandle<T, C> {}
impl<T: ?Sized + GcSafe<'static, CollectorId<C>>, C: RawHandleImpl> Clone for GcHandle<T, C> {
    fn clone(&self) -> Self {
        // NOTE: Dead collector -> invalid handle
        let collector = self.collector
            .ensure_valid(|id| unsafe { id.weak_ref() });
        let inner = unsafe { self.inner.as_ref() };
        debug_assert!(
            !inner.value
                .load(Ordering::SeqCst)
                .is_null(),
            "Pointer is invalid"
        );
        let mut old_refcnt = inner.refcnt.load(Ordering::Relaxed);
        loop {
            assert_ne!(
                old_refcnt, isize::max_value() as usize,
                "Reference count overflow"
            );
            /*
             * NOTE: Relaxed is sufficient for failure since we have no
             * expectations about the new state. Weak exchange is okay
             * since we retry in a loop.
             *
             * NOTE: We do **not** use fetch_add because we are afraid
             * of refcount overflow. We should possibly consider it
             */
            match inner.refcnt.compare_exchange_weak(
                old_refcnt, old_refcnt + 1,
                Ordering::AcqRel,
                Ordering::Relaxed
            ) {
                Ok(_) => break,
                Err(val) => {
                    old_refcnt = val;
                }
            }
        }
        GcHandle {
            inner: self.inner,
            metadata: self.metadata,
            collector, marker: PhantomData
        }
    }
}
impl<T: ?Sized + GcSafe<'static, CollectorId<C>>, C: RawHandleImpl> Drop for GcHandle<T, C> {
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
                    /*
                     * This should be impossible.
                     *
                     * I believe it's undefined behavior!
                     */
                    panic!("UB: GcHandle refcnt overflow")
                },
                1 => {
                    // Free underlying memory
                },
                _ => {}, // Other references
            }
            // Mark the value as freed
            inner.value.store(
                ptr::null_mut(),
                Ordering::Release
            );
            unsafe {
                collector.handle_list().append_free_slot(
                    self.inner.as_ptr() as *mut HandleSlot<C>
                );
            }
        });
    }
}
/// In order to send *references* between threads,
/// the underlying type must be sync.
///
/// This is the same reason that `Arc<T>: Send` requires `T: Sync`
///
/// Requires that the collector is thread-safe.
unsafe impl<T: ?Sized + GcSafe<'static, CollectorId<C>> + Sync, C: RawHandleImpl + Sync> Send for GcHandle<T, C> {}

/// If the underlying type is Sync,
/// it's safe to share garbage collected references between threads.
///
/// Requires that the collector is thread-safe.
unsafe impl<T: ?Sized + GcSafe<'static, CollectorId<C>> + Sync, C: RawHandleImpl + Sync> Sync for GcHandle<T, C> {}

/// We support handles
unsafe impl<C> HandleCollectorId for CollectorId<C>
    where C: RawHandleImpl {
    type Handle<T: GcSafe<'static, Self> + ?Sized> = GcHandle<T, C>;


    #[inline]
    fn create_handle<'gc, T>(gc: Gc<'gc, T, CollectorId<C>>) -> Self::Handle<T::Branded> 
        where T: ?Sized + GcSafe<'gc, Self> + GcRebrand<'static, Self>, T::Branded: GcSafe<'static, Self> {
        unsafe {
            let collector = gc.collector_id();
            let value = gc.as_raw_ptr();
            let raw = collector.as_ref().handle_list()
                .alloc_raw_handle(value as *mut ());
            /*
             * WARN: Undefined Behavior
             * if we don't finish initializing
             * the handle!!!
             */
            raw.type_info.store(
                C::resolve_type_info(gc)
                    as *const C::TypeInfo
                    as *mut C::TypeInfo,
                Ordering::Release
            );
            raw.refcnt.store(1, Ordering::Release);
            let weak_collector = collector.weak_ref();
            let metadata = crate::utils::transmute_mismatched::<
                <T as Pointee>::Metadata,
                <T::Branded as Pointee>::Metadata
            >(ptr::metadata(value));
            GcHandle::new(NonNull::from(raw), weak_collector, metadata)
        }
    }
}
