//! Implements the [`Gc`]` smart pointer.

use core::cmp::Ordering;
use core::fmt::{self, Debug, Display, Formatter};
use core::hash::{Hash, Hasher};
use core::marker::PhantomData;
use core::ops::Deref;
use core::ptr::NonNull;

// nightly feature: Unsized coercion
#[cfg(feature = "nightly")]
use std::marker::Unsize;
#[cfg(feature = "nightly")]
use std::ops::CoerceUnsized;

use crate::system::{CollectorId, HandleCollectorId};
use crate::trace::barrier::GcDirectBarrier;
use crate::trace::{GcRebrand, GcSafe, GcVisitor, Trace, TrustedDrop};

/// A garbage collected pointer to a value.
///
/// This is the equivalent of a garbage collected smart-pointer.
/// It's so smart, you can even coerce it to a reference bound to the lifetime of the `GarbageCollectorRef`.
/// However, all those references are invalidated by the borrow checker as soon as
/// your reference to the collector reaches a safepoint.
/// The objects can only survive garbage collection if they live in this smart-pointer.
///
/// The smart pointer is simply a guarantee to the garbage collector
/// that this points to a garbage collected object with the correct header,
/// and not some arbitrary bits that you've decided to heap allocate.
///
/// ## Safety
/// A `Gc` can be safely transmuted back and forth from its corresponding pointer.
///
/// Unsafe code can rely on a pointer always dereferencing to the same value in between
/// safepoints. This is true even for copying/moving collectors.
///
/// ## Lifetime
/// The borrow does *not* refer to the value `&'gc T`.
/// Instead, it refers to the *context* `&'gc Id::Context`
///
/// This is necessary because `T` may have borrowed interior data
/// with a shorter lifetime `'a < 'gc`, making `&'gc T` invalid
/// (because that would imply 'gc: 'a, which is false).
///
/// This ownership can be thought of in terms of the following (simpler) system.
/// ```no_run
/// # trait GcSafe{}
/// # use core::marker::PhantomData;
/// struct GcContext {
///     values: Vec<Box<dyn GcSafe>>
/// }
/// struct Gc<'gc, T: GcSafe> {
///     index: usize,
///     marker: PhantomData<T>,
///     ctx: &'gc GcContext
/// }
/// ```
///
/// In this system, safepoints can be thought of mutations
/// that remove dead values from the `Vec`.
///
/// This ownership equivalency is also the justification for why
/// the `'gc` lifetime can be [covariant](https://doc.rust-lang.org/nomicon/subtyping.html#variance)
///
/// The only difference is that the real `Gc` structure
/// uses pointers instead of indices.
#[repr(transparent)]
pub struct Gc<'gc, T: ?Sized, Id: CollectorId> {
    /// The pointer to the garbage collected value.
    ///
    /// NOTE: The logical lifetime here is **not** `&'gc T`
    /// See the comments on 'Lifetime' for details.
    value: NonNull<T>,
    /// Marker struct used to statically identify the collector's type,
    /// and indicate that 'gc is a logical reference the system.
    ///
    /// The runtime instance of this value can be
    /// computed from the pointer itself: `NonNull<T>` -> `&CollectorId`
    collector_id: PhantomData<&'gc Id::System>,
}
impl<'gc, T: GcSafe<'gc, Id> + ?Sized, Id: CollectorId> Gc<'gc, T, Id> {
    /// Create a GC pointer from a raw pointer
    ///
    /// ## Safety
    /// Undefined behavior if the underlying pointer is not valid
    /// and doesn't correspond to the appropriate id.
    #[inline]
    pub unsafe fn from_raw(value: NonNull<T>) -> Self {
        Gc {
            collector_id: PhantomData,
            value,
        }
    }
    /// Create a [GcHandle] referencing this object,
    /// allowing it to be used without a context
    /// and referenced across safepoints.
    ///
    /// Requires that the collector [supports handles](`HandleCollectorId`)
    #[inline]
    pub fn create_handle(&self) -> Id::Handle<T::Branded>
    where
        Id: HandleCollectorId,
        T: GcRebrand<'static, Id>,
    {
        Id::create_handle(*self)
    }

    /// Get a reference to the system
    ///
    /// ## Safety
    /// This is based on the assumption that a [GcSystem] must outlive
    /// all of the pointers it owns.
    /// Although it could be restricted to the lifetime of the [CollectorId]
    /// (in theory that may have an internal pointer) it will still live for '&self'.
    #[inline]
    pub fn system(&self) -> &'_ Id::System {
        // This assumption is safe - see the docs
        unsafe { self.collector_id().assume_valid_system() }
    }
}
impl<'gc, T: ?Sized, Id: CollectorId> Gc<'gc, T, Id> {
    /// The value of the underlying pointer
    #[inline(always)]
    pub const fn value(&self) -> &'gc T {
        unsafe { *(&self.value as *const NonNull<T> as *const &'gc T) }
    }
    /// Cast this reference to a raw pointer
    ///
    /// ## Safety
    /// It's undefined behavior to mutate the
    /// value.
    /// The pointer is only valid as long as
    /// the reference is.
    #[inline]
    pub unsafe fn as_raw_ptr(&self) -> *mut T {
        self.value.as_ptr() as *const T as *mut T
    }

    /// Get a reference to the collector's id
    ///
    /// The underlying collector it points to is not necessarily always valid
    #[inline]
    pub fn collector_id(&self) -> &'_ Id {
        Id::from_gc_ptr(self)
    }
}

/// Double-indirection is completely safe
unsafe impl<'gc, T: ?Sized + GcSafe<'gc, Id>, Id: CollectorId> TrustedDrop for Gc<'gc, T, Id> {}
unsafe impl<'gc, T: ?Sized + GcSafe<'gc, Id>, Id: CollectorId> GcSafe<'gc, Id> for Gc<'gc, T, Id> {
    #[inline]
    unsafe fn trace_inside_gc<V>(gc: &mut Gc<'gc, Self, Id>, visitor: &mut V) -> Result<(), V::Err>
    where
        V: GcVisitor,
    {
        // Double indirection is fine. It's just a `Sized` type
        visitor.trace_gc(gc)
    }
}
/// Rebrand
unsafe impl<'gc, 'new_gc, T, Id> GcRebrand<'new_gc, Id> for Gc<'gc, T, Id>
where
    T: GcSafe<'gc, Id> + ?Sized + GcRebrand<'new_gc, Id>,
    Id: CollectorId,
    Self: Trace,
{
    type Branded = Gc<'new_gc, T::Branded, Id>;
}
unsafe impl<'gc, T: ?Sized + GcSafe<'gc, Id>, Id: CollectorId> Trace for Gc<'gc, T, Id> {
    // We always need tracing....
    const NEEDS_TRACE: bool = true;
    // we never need to be dropped because we are `Copy`
    const NEEDS_DROP: bool = false;

    #[inline]
    fn trace<V: GcVisitor>(&mut self, visitor: &mut V) -> Result<(), V::Err> {
        unsafe {
            // We're delegating with a valid pointer.
            T::trace_inside_gc(self, visitor)
        }
    }
}
impl<'gc, T: GcSafe<'gc, Id> + ?Sized, Id: CollectorId> Deref for Gc<'gc, T, Id> {
    type Target = T;

    #[inline(always)]
    fn deref(&self) -> &Self::Target {
        self.value()
    }
}
unsafe impl<'gc, O, V, Id> GcDirectBarrier<'gc, Gc<'gc, O, Id>> for Gc<'gc, V, Id>
where
    O: GcSafe<'gc, Id> + 'gc,
    V: GcSafe<'gc, Id> + 'gc,
    Id: CollectorId,
{
    #[inline(always)]
    unsafe fn write_barrier(&self, owner: &Gc<'gc, O, Id>, field_offset: usize) {
        Id::gc_write_barrier(owner, self, field_offset)
    }
}
// We can be copied freely :)
impl<'gc, T: ?Sized, Id: CollectorId> Copy for Gc<'gc, T, Id> {}
impl<'gc, T: ?Sized, Id: CollectorId> Clone for Gc<'gc, T, Id> {
    #[inline(always)]
    fn clone(&self) -> Self {
        *self
    }
}
// Delegating impls
impl<'gc, T: GcSafe<'gc, Id> + Hash, Id: CollectorId> Hash for Gc<'gc, T, Id> {
    #[inline]
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.value().hash(state)
    }
}
impl<'gc, T: GcSafe<'gc, Id> + PartialEq, Id: CollectorId> PartialEq for Gc<'gc, T, Id> {
    #[inline]
    fn eq(&self, other: &Self) -> bool {
        // NOTE: We compare by value, not identity
        self.value() == other.value()
    }
}
impl<'gc, T: GcSafe<'gc, Id> + Eq, Id: CollectorId> Eq for Gc<'gc, T, Id> {}
impl<'gc, T: GcSafe<'gc, Id> + PartialEq, Id: CollectorId> PartialEq<T> for Gc<'gc, T, Id> {
    #[inline]
    fn eq(&self, other: &T) -> bool {
        self.value() == other
    }
}
impl<'gc, T: GcSafe<'gc, Id> + PartialOrd, Id: CollectorId> PartialOrd for Gc<'gc, T, Id> {
    #[inline]
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        self.value().partial_cmp(other.value())
    }
}
impl<'gc, T: GcSafe<'gc, Id> + PartialOrd, Id: CollectorId> PartialOrd<T> for Gc<'gc, T, Id> {
    #[inline]
    fn partial_cmp(&self, other: &T) -> Option<Ordering> {
        self.value().partial_cmp(other)
    }
}
impl<'gc, T: GcSafe<'gc, Id> + Ord, Id: CollectorId> Ord for Gc<'gc, T, Id> {
    #[inline]
    fn cmp(&self, other: &Self) -> Ordering {
        self.value().cmp(other)
    }
}
impl<'gc, T: ?Sized + GcSafe<'gc, Id> + Debug, Id: CollectorId> Debug for Gc<'gc, T, Id> {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        if !f.alternate() {
            // Pretend we're a newtype by default
            f.debug_tuple("Gc").field(&self.value()).finish()
        } else {
            // Alternate spec reveals `collector_id`
            f.debug_struct("Gc")
                .field("collector_id", &self.collector_id)
                .field("value", &self.value())
                .finish()
        }
    }
}
impl<'gc, T: ?Sized + GcSafe<'gc, Id> + Display, Id: CollectorId> Display for Gc<'gc, T, Id> {
    fn fmt(&self, f: &mut Formatter) -> fmt::Result {
        Display::fmt(&self.value(), f)
    }
}

/// In order to send *references* between threads,
/// the underlying type must be sync.
///
/// This is the same reason that `Arc<T>: Send` requires `T: Sync`
unsafe impl<'gc, T, Id> Send for Gc<'gc, T, Id>
where
    T: GcSafe<'gc, Id> + ?Sized + Sync,
    Id: CollectorId + Sync,
{
}

/// If the underlying type is `Sync`, it's safe
/// to share garbage collected references between threads.
///
/// The safety of the collector itself depends on whether [CollectorId] is Sync.
/// If it is, the whole garbage collection implementation should be as well.
unsafe impl<'gc, T, Id> Sync for Gc<'gc, T, Id>
where
    T: GcSafe<'gc, Id> + ?Sized + Sync,
    Id: CollectorId + Sync,
{
}

#[cfg(feature = "nightly")] // nightly feature: CoerceUnsized
impl<'gc, T, U, Id> CoerceUnsized<Gc<'gc, U, Id>> for Gc<'gc, T, Id>
where
    T: ?Sized + GcSafe<'gc, Id> + Unsize<U>,
    U: ?Sized + GcSafe<'gc, Id>,
    Id: CollectorId,
{
}
