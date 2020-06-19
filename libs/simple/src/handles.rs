//! Implementation of [::zerogc::GcHandle]
//!
//! Inspired by [Mono's Lock free Gc Handles](https://www.mono-project.com/news/2016/08/16/lock-free-gc-handles/)
use std::ptr::NonNull;
use std::marker::PhantomData;
use std::ffi::c_void;
use std::sync::atomic::{self, AtomicPtr, AtomicUsize, Ordering};

use zerogc::GcSafe;
use crate::{DynTrace, GcType};

const INITIAL_HANDLE_CAPACITY: usize = 128;

/// Concurrent list of [GcHandle]s
///
/// Each bucket in the linked list is twice the size of
/// the previous one, ensuring amortized growth
///
/// The list can not be appended to while a collection
/// is in progress.
pub(crate) struct GcHandleList {
    last_bucket: AtomicPtr<GcHandleBucket>,
}
impl GcHandleList {
    /// Allocate a raw handle that points to the given value.
    ///
    /// ## Safety
    /// A collection may not currently be in progress.
    ///
    /// The returned handle must be fully initialized
    /// before the next collection begins.
    pub(crate) unsafe fn alloc_raw_handle(&self, value: *mut ()) -> &GcRawHandle {
        let mut bucket = self.last_bucket.load(Ordering::Acquire);
        loop {
            let new_size: usize;
            if bucket.is_null() {
                new_size = INITIAL_HANDLE_CAPACITY;
            } else {
                if let Some(handle) = (*bucket).acquire_raw_handle(value) {
                    return handle;
                }
                // Double capacity for amortized growth
                new_size = (*bucket).elements.len() * 2;
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
        let mut elements: Vec<GcRawHandle> = Vec::with_capacity(desired_size);
        // Zero initialize handles - assuming this is safe
        elements.as_mut_ptr().write_bytes(0, desired_size);
        elements.set_len(desired_size);
        let allocated_bucket = Box::into_raw(Box::new(GcHandleBucket {
            elements: elements.into_boxed_slice(),
            last_used: AtomicUsize::new(0),
            prev: AtomicPtr::new(prev_bucket),
        }));
        let actual_bucket = self.last_bucket.compare_and_swap(
            prev_bucket, allocated_bucket, Ordering::SeqCst
        );
        if actual_bucket == allocated_bucket {
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
    /// Iterate over all the handles in this list,
    /// assuming exclusive access
    unsafe fn for_each_handle_mut(
        &self, mut func: impl FnMut(&mut GcRawHandle)
    ) {
        let mut bucket = self.last_bucket.load(Ordering::Acquire);
        while !bucket.is_null() {
            let elements =  &mut *(*bucket).elements;
            // We should have exclusive access!
            for element in elements {
                func(element);
            }
            bucket = (*bucket).prev.load(Ordering::Acquire);
        }
    }
}
impl DynTrace for GcHandleList {
    fn trace(&mut self, visitor: &mut crate::MarkVisitor) {
        unsafe { self.for_each_handle_mut(|element| {
            element.trace(visitor)
        }) }
    }
}
impl Drop for GcHandleList {
    fn drop(&mut self) {
        let mut bucket = self.last_bucket.load(Ordering::Acquire);
        while !bucket.is_null() {
            unsafe {
                bucket = (*bucket).prev
                    .load(Ordering::Acquire);
                drop(Box::from_raw(bucket));
            }
        }
    }

}
struct GcHandleBucket {
    elements: Box<[GcRawHandle]>,
    last_used: AtomicUsize,
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
    /// ## Safety
    /// See docs on [GcHandleList::alloc_raw_bucket]
    unsafe fn acquire_raw_handle(&self, value: *mut ()) -> Option<&GcRawHandle> {
        let len = self.elements.len();
        let last_used = self.last_used.load(Ordering::Relaxed);
        for (i, raw) in self.elements.iter().enumerate().skip(last_used + 1) {
            // TODO: All these fences must be horrible on ARM
            if raw.value.compare_and_swap(
                std::ptr::null_mut(), value,
                Ordering::AcqRel
            ).is_null() {
                // We acquired ownership!
                self.last_used.fetch_max(i, Ordering::AcqRel);
                return Some(&*raw);
            }
        }
        None
    }
}
/// The underlying value of a handle,
/// which may or may not be valid.
///
/// These are reused
pub struct GcRawHandle {
    /// Refers to the underlying value of this handle.
    ///
    /// If it's null, it's invalid and free to be reused.
    ///
    /// The underlying value can only be safely accessed 
    /// if there isn't a collection in progress
    value: AtomicPtr<()>,
    /// I think this should be protected by the other atomic
    /// accesses. Regardless, I'll put it in an AtomicPtr anyways.
    type_info: AtomicPtr<GcType>,
    /// The reference count to the handle
    ///
    /// If this is zero the value can be freed
    /// and this memory can be used.
    refcnt: AtomicUsize
}
impl DynTrace for GcRawHandle {
    #[inline]
    fn trace(&mut self, visitor: &mut crate::MarkVisitor) {
        /*
         * TODO: Can we somehow hoist this fence?
         * Shouldn't the collector already have
         * exclusive access?
         */
        atomic::fence(Ordering::Acquire);
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
        let type_info = self.type_info.load(Ordering::Relaxed); 
        unsafe {
            ((*type_info).trace_func)(
                value as *mut c_void,
                visitor
            );
        }
    }

}
pub struct GcHandle<T: GcSafe> {
    inner: NonNull<GcRawHandle>,
    marker: PhantomData<*mut T>
}
impl<T: GcSafe> Clone for GcHandle<T> {
    fn clone(&self) -> Self {
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
            marker: PhantomData
        }
    }
}
impl<T: GcSafe> Drop for GcHandle<T> {
    fn drop(&mut self) {
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
        // Finally free the value
        inner.value.store(
            std::ptr::null_mut(),
            Ordering::Release
        );
    }
}