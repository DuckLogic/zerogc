//! An "epsilon" garbage collector, which never garbage collects or
//! frees memory until the garbage collector is dropped.
//!
//! Essentially, this is an arena allocator.
//!
//! Because it is backed by a simple arena allocator,
//! the [EpsilonSystem] is `!Sync`, and can't be used by multiple threads
//! at once (although references to it can be freely sent around once already allocated).
#![cfg(feature = "epsilon")]

mod alloc;
mod handle;
mod layout;

use crate::{CollectorId, GcContext, GcSafe, GcSimpleAlloc, GcSystem, Trace};
use std::alloc::Layout;
use std::cell::{Cell, OnceCell};
use std::ptr::NonNull;
use std::rc::Rc;

use self::alloc::EpsilonAlloc;
use self::layout::EpsilonRawVec;

/// The implementation of [EpsilonRawVec]
pub mod vec {
    pub use super::layout::EpsilonRawVec;
}

/// Coerce a reference into a [Gc] pointer.
///
/// This is only supported on the epsilon collector.
/// Because the epsilon collector never allocates,
/// it doesn't need to make a distinction between `Gc<T>` and `&T`.
///
/// This will never actually be collected
/// and will always be valid
///
/// TODO: Rename??
#[inline]
pub const fn gc<'gc, T: ?Sized + GcSafe<'gc, EpsilonCollectorId> + 'gc>(ptr: &'gc T) -> Gc<'gc, T> {
    /*
     * SAFETY: Epsilon never collects unless explicitly added to
     * the linked list of allocated objects.
     * Therefore any reference can be assumed to be a Gc ptr.
     */
    unsafe { std::mem::transmute::<&'gc T, crate::Gc<'gc, T, EpsilonCollectorId>>(ptr) }
}

/// Coerce a slice into a `GcArray`.
///
/// This is only supported on the epsilon collector.
/// Because the epsilon collector never collects,
/// it doesn't need to make a distinction between `GcArray<T>` and `&[T]`.
///
/// See also: [gc] for converting `&T` -> `Gc<T>`
#[inline]
pub const fn gc_array<'gc, T: GcSafe<'gc, EpsilonCollectorId> + 'gc>(
    slice: &'gc [T],
) -> GcArray<'gc, T> {
    /*
     * SAFETY: Epsilon uses the 'fat' representation for GcArrays.
     * That means that repr(GcArray) == repr(&[T]).
     *
     * Since we never collect, we are free to transmute
     * back and forth between them
     */
    unsafe { std::mem::transmute::<&'gc [T], crate::GcArray<'gc, T, EpsilonCollectorId>>(slice) }
}

/// Coerce a `&str` into a `GcString`
///
/// This is only supported on the epsilon collector,
/// because the epsilon collector never collects.
///
/// See also [gc_array] for converting `&[T]` -> `GcArray<T>`
#[inline]
pub const fn gc_str<'gc>(s: &'gc str) -> GcString<'gc> {
    /*
     * SAFETY: Epsilon uses the 'fat' representation for GcArrays.
     * This means that repr(GcArray) == repr(&[T])
     *
     * Because we already know the string is UTF8 encoded,
     * we can take advantage of the fact that repr(str) == repr(&[u8])
     * and repr(GcArray) == repr(GcString).
     * Instead of going `str -> &[T] -> GcArray -> GcString`
     * we can just go directly from `str -> GcString`
     */
    unsafe { std::mem::transmute::<&'gc str, crate::array::GcString<'gc, EpsilonCollectorId>>(s) }
}

/// Allocate a [(fake) Gc](Gc) that points to the specified
/// value and leak it.
///
/// Since collection is unimplemented,
/// this intentionally leaks memory.
pub fn leaked<'gc, T: GcSafe<'gc, EpsilonCollectorId> + 'static>(value: T) -> Gc<'gc, T> {
    gc(Box::leak(Box::new(value)))
}

/// A [garbage collected pointer](`crate::Gc`)
/// that uses the [episolon collector](EpsilonSystem)
///
/// **WARNING**: This never actually collects any garbage
pub type Gc<'gc, T> = crate::Gc<'gc, T, EpsilonCollectorId>;
/// A [garbage collected array](`crate::array::GcArray`)
/// that uses the [epsilon collector](EpsilonSystem)
///
/// **WARNING**: This never actually collects any garbage.
pub type GcArray<'gc, T> = crate::array::GcArray<'gc, T, EpsilonCollectorId>;
/// A [garbage collected array](`crate::vec::GcVec`)
/// that uses the [epsilon collector](EpsilonSystem)
///
/// **WARNING**: This never actually collects any garbage.
pub type GcVec<'gc, T> = crate::vec::GcVec<'gc, T, EpsilonContext>;
/// A [garbage collected string](`crate::array::GcString`)
/// that uses the epsilon collector.
///
/// **WARNING**: This never actually collects any garbage
pub type GcString<'gc> = crate::array::GcString<'gc, EpsilonCollectorId>;

/// A never-collecting garbage collector context.
///
/// **WARNING**: This never actually collects any garbage.
pub struct EpsilonContext {
    state: NonNull<State>,
    root: bool,
}
unsafe impl GcContext for EpsilonContext {
    type System = EpsilonSystem;
    type Id = EpsilonCollectorId;

    #[inline]
    unsafe fn unchecked_safepoint<T: Trace>(&self, _value: &mut &mut T) {
        // safepoints are a nop in our system
    }

    unsafe fn freeze(&mut self) {
        unimplemented!()
    }

    unsafe fn unfreeze(&mut self) {
        unimplemented!()
    }

    #[inline]
    unsafe fn recurse_context<T, F, R>(&self, value: &mut &mut T, func: F) -> R
    where
        T: Trace,
        F: for<'gc> FnOnce(&'gc mut Self, &'gc mut T) -> R,
    {
        // safepoints are a nop since there is nothing to track
        let mut child = EpsilonContext {
            state: self.state,
            root: false,
        };
        func(&mut child, &mut *value)
    }

    #[inline]
    fn system(&self) -> &'_ Self::System {
        // Pointer to a pointer
        unsafe {
            NonNull::<NonNull<State>>::from(&self.state)
                .cast::<EpsilonSystem>()
                .as_ref()
        }
    }

    #[inline]
    fn id(&self) -> Self::Id {
        EpsilonCollectorId { _priv: () }
    }
}
impl Drop for EpsilonContext {
    #[inline]
    fn drop(&mut self) {
        unsafe {
            if self.root {
                drop(Rc::from_raw(self.state.as_ptr()))
            }
        }
    }
}

struct State {
    alloc: alloc::Default,
    /// The head of the linked-list of allocated objects.
    head: Cell<Option<NonNull<layout::EpsilonHeader>>>,
    empty_vec: OnceCell<NonNull<layout::EpsilonVecHeader>>,
}
impl State {
    #[inline]
    unsafe fn push_state(&self, mut header: NonNull<layout::EpsilonHeader>) {
        header.as_mut().next = self.head.get();
        self.head.set(Some(header));
    }
}
impl Drop for State {
    fn drop(&mut self) {
        let mut ptr = self.head.get();
        unsafe {
            while let Some(header) = ptr {
                let header_layout = layout::EpsilonHeader::LAYOUT;
                let desired_align = header.as_ref().type_info.layout.align();
                let padding = header_layout.padding_needed_for(desired_align);
                let value_ptr = (header.as_ptr() as *const u8)
                    .add(header_layout.size())
                    .add(padding);
                if let Some(drop_func) = header.as_ref().type_info.drop_func {
                    (drop_func)(value_ptr as *const _ as *mut _);
                }
                let next = header.as_ref().next;
                if self::alloc::Default::NEEDS_EXPLICIT_FREE {
                    let value_layout = header.as_ref().determine_layout();
                    let original_header = NonNull::new_unchecked(
                        header
                            .cast::<u8>()
                            .as_ptr()
                            .sub(header.as_ref().type_info.layout.common_header_offset()),
                    );
                    let header_size =
                        value_ptr.cast::<u8>().offset_from(original_header.as_ptr()) as usize;
                    let combined_layout = Layout::from_size_align_unchecked(
                        value_layout.size() + header_size,
                        value_layout
                            .align()
                            .max(layout::EpsilonHeader::LAYOUT.align()),
                    );
                    self.alloc.free_alloc(original_header, combined_layout);
                }
                ptr = next;
            }
        }
    }
}

/// A dummy implementation of [GcSystem]
/// which is useful for testing
///
/// **WARNING**: This never actually collects any memory.
pub struct EpsilonSystem {
    /// The raw state of the system
    state: NonNull<State>,
}
impl EpsilonSystem {
    #[inline]
    fn from_state(state: Rc<State>) -> EpsilonSystem {
        EpsilonSystem {
            state: unsafe { NonNull::new_unchecked(Rc::into_raw(state) as *mut _) },
        }
    }

    #[inline]
    fn clone_rc(&self) -> Rc<State> {
        unsafe {
            Rc::increment_strong_count(self.state.as_ptr());
            Rc::from_raw(self.state.as_ptr())
        }
    }
    /// Create a new epsilon collector, which intentionally leaks memory
    #[inline]
    pub fn leak() -> Self {
        EpsilonSystem::from_state(Rc::new(State {
            alloc: self::alloc::Default::new(),
            head: Cell::new(None),
            empty_vec: OnceCell::new(),
        }))
    }

    #[inline]
    fn state(&self) -> &'_ State {
        unsafe { self.state.as_ref() }
    }

    /// Create a new [EpsilonContext]
    ///
    /// There are few restrictions on this
    /// because it doesn't actually do anything
    #[inline]
    pub fn new_context(&self) -> EpsilonContext {
        EpsilonContext {
            state: unsafe { NonNull::new_unchecked(Rc::into_raw(self.clone_rc()) as *mut _) },
            root: true,
        }
    }
}
impl Drop for EpsilonSystem {
    #[inline]
    fn drop(&mut self) {
        unsafe { Rc::decrement_strong_count(self.state.as_ptr()) }
    }
}
unsafe impl GcSystem for EpsilonSystem {
    type Id = EpsilonCollectorId;
    type Context = EpsilonContext;
}
unsafe impl GcSimpleAlloc for EpsilonContext {
    #[inline]
    unsafe fn alloc_uninit<'gc, T>(&'gc self) -> *mut T
    where
        T: GcSafe<'gc, EpsilonCollectorId>,
    {
        let tp = self::layout::TypeInfo::of::<T>();
        let needs_header = self::alloc::Default::NEEDS_EXPLICIT_FREE || !tp.may_ignore();
        let ptr = if needs_header {
            let (overall_layout, offset) = self::layout::EpsilonHeader::LAYOUT
                .extend(Layout::new::<T>())
                .unwrap();
            let mem = self.system().state().alloc.alloc_layout(overall_layout);
            let header = mem.cast::<self::layout::EpsilonHeader>();
            header.as_ptr().write(self::layout::EpsilonHeader {
                type_info: tp,
                next: None,
            });
            self.system().state().push_state(header);
            mem.as_ptr().add(offset)
        } else {
            self.system()
                .state()
                .alloc
                .alloc_layout(Layout::new::<T>())
                .as_ptr()
        };
        ptr.cast()
    }

    #[inline]
    fn alloc<'gc, T>(&'gc self, value: T) -> crate::Gc<'gc, T, Self::Id>
    where
        T: GcSafe<'gc, Self::Id>,
    {
        unsafe {
            let ptr = self.alloc_uninit::<T>();
            ptr.write(value);
            Gc::from_raw(NonNull::new_unchecked(ptr))
        }
    }

    #[inline]
    unsafe fn alloc_uninit_slice<'gc, T>(&'gc self, len: usize) -> *mut T
    where
        T: GcSafe<'gc, Self::Id>,
    {
        let type_info = self::layout::TypeInfo::of_array::<T>();
        let (overall_layout, offset) = Layout::new::<self::layout::EpsilonArrayHeader>()
            .extend(Layout::array::<T>(len).unwrap())
            .unwrap();
        let mem = self.system().state().alloc.alloc_layout(overall_layout);
        let header = mem.cast::<self::layout::EpsilonArrayHeader>();
        header.as_ptr().write(self::layout::EpsilonArrayHeader {
            common_header: self::layout::EpsilonHeader {
                type_info,
                next: None,
            },
            len,
        });
        self.system()
            .state()
            .push_state(NonNull::from(&header.as_ref().common_header));
        mem.as_ptr().add(offset).cast()
    }

    #[inline]
    fn alloc_raw_vec_with_capacity<'gc, T>(&'gc self, capacity: usize) -> EpsilonRawVec<'gc, T>
    where
        T: GcSafe<'gc, Self::Id>,
    {
        if capacity == 0 {
            if let Some(&empty_ptr) = self.system().state().empty_vec.get() {
                return unsafe { self::layout::EpsilonRawVec::from_raw_parts(empty_ptr, self) };
            }
        }
        let type_info = layout::TypeInfo::of_vec::<T>();
        let (overall_layout, offset) = Layout::new::<layout::EpsilonVecHeader>()
            .extend(Layout::array::<T>(capacity).unwrap())
            .unwrap();
        let mem = self.system().state().alloc.alloc_layout(overall_layout);
        unsafe {
            let header = mem.cast::<self::layout::EpsilonVecHeader>();
            header.as_ptr().write(self::layout::EpsilonVecHeader {
                common_header: self::layout::EpsilonHeader {
                    type_info,
                    next: None,
                },
                len: Cell::new(0),
                capacity,
            });
            self.system()
                .state()
                .push_state(NonNull::from(&header.as_ref().common_header));
            let value_ptr = mem.as_ptr().add(offset).cast::<T>();
            let raw = self::layout::EpsilonRawVec::from_raw_parts(header, self);
            debug_assert_eq!(raw.as_ptr(), value_ptr);
            raw
        }
    }
}

/// The id for an [EpsilonSystem]
///
/// All epsilon collectors have the same id,
/// regardless of the system they were originally allocated from.
/// It is equivalent to [
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub struct EpsilonCollectorId {
    _priv: (),
}
crate::impl_nulltrace_for_static!(EpsilonCollectorId);
unsafe impl CollectorId for EpsilonCollectorId {
    type System = EpsilonSystem;
    type Context = EpsilonContext;
    type RawVec<'gc, T: GcSafe<'gc, Self>> = self::layout::EpsilonRawVec<'gc, T>;
    /// We use fat-pointers for arrays,
    /// so that we can transmute from `&'static [T]` -> `GcArray`
    type ArrayPtr = zerogc::array::repr::FatArrayPtr<Self>;

    #[inline]
    fn from_gc_ptr<'a, 'gc, T>(_gc: &'a Gc<'gc, T>) -> &'a Self
    where
        T: ?Sized,
        'gc: 'a,
    {
        const ID: EpsilonCollectorId = EpsilonCollectorId { _priv: () };
        &ID
    }

    #[inline]
    fn resolve_array_id<'a, 'gc, T>(_array: &'a GcArray<'gc, T>) -> &'a Self
    where
        'gc: 'a,
    {
        const ID: EpsilonCollectorId = EpsilonCollectorId { _priv: () };
        &ID
    }

    #[inline]
    fn resolve_array_len<T>(repr: &GcArray<'_, T>) -> usize {
        repr.len()
    }

    #[inline]
    unsafe fn gc_write_barrier<'gc, T, V>(
        _owner: &Gc<'gc, T>,
        _value: &Gc<'gc, V>,
        _field_offset: usize,
    ) where
        T: GcSafe<'gc, Self> + ?Sized,
        V: GcSafe<'gc, Self> + ?Sized,
    {
    }

    unsafe fn assume_valid_system(&self) -> &Self::System {
        /*
         * NOTE: Supporting this would lose our ability to go from `&'static T` -> `Gc<'gc, T, EpsilonCollectorId>
         * It would also necessitate a header for `Copy` objects.
         */
        unimplemented!("Unable to convert EpsilonCollectorId -> EpsilonSystem")
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::Trace;
    #[test]
    fn lifetime_variance<'a>() {
        #[derive(Trace, Copy, Clone)]
        #[zerogc(copy, collector_ids(EpsilonCollectorId))]
        enum ShouldBeVariant<'gc> {
            First(Gc<'gc, ShouldBeVariant<'gc>>),
            Second(u32),
            #[allow(unused)]
            Array(GcArray<'gc, ShouldBeVariant<'gc>>),
        }
        const STATIC: Gc<'static, u32> = gc(&32);
        const SECOND: &ShouldBeVariant<'static> = &ShouldBeVariant::Second(32);
        const FIRST_VAL: &ShouldBeVariant<'static> = &ShouldBeVariant::First(gc(SECOND));
        const FIRST: Gc<'static, ShouldBeVariant<'static>> = gc(FIRST_VAL);
        fn covariant<'a, T>(s: Gc<'static, T>) -> Gc<'a, T> {
            s as _
        }
        let s: Gc<'a, u32> = covariant(STATIC);
        assert_eq!(s.value(), &32);
        let k: Gc<'a, ShouldBeVariant<'a>> = covariant::<'a, ShouldBeVariant<'static>>(FIRST) as _;
        assert!(matches!(k.value(), ShouldBeVariant::First(_)));
    }
}
