//! An "epsilon" garbage collector, which never garbage collects or
//! frees memory until the garbage collector is dropped.
//!
//! Essentially, this is an arena allocator.
//!
//! Because it is backed by a simple arena allocator,
//! the [EpsilonSystem] is `!Sync`, and can't be used by multiple threads
//! at once (although references to it can be freely sent around once already allocated).
#![cfg(feature = "epsilon")]

mod layout;
mod alloc;

use crate::{CollectorId, GcContext, GcSafe, GcSimpleAlloc, GcSystem, GcVisitor, NullTrace, Trace, TraceImmutable, TrustedDrop, internals::ConstCollectorId};
use std::ptr::NonNull;
use std::alloc::Layout;
use std::rc::Rc;
use std::cell::Cell;
use std::lazy::OnceCell;

use self::alloc::{EpsilonAlloc};

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
pub const fn gc<'gc, T: GcSafe<'gc, EpsilonCollectorId> + 'gc>(ptr: &'gc T) -> Gc<'gc, T> {
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
pub const fn gc_array<'gc, T: GcSafe<'gc, EpsilonCollectorId> + 'gc>(slice: &'gc [T]) -> GcArray<'gc, T> {
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
    root: bool
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
        where T: Trace, F: for<'gc> FnOnce(&'gc mut Self, &'gc mut T) -> R {
        // safepoints are a nop since there is nothing to track
        let mut child = EpsilonContext { state: self.state, root: false };
        func(&mut child, &mut *value)
    }

    #[inline]
    fn system(&self) -> &'_ Self::System {
        // Pointer to a pointer
        unsafe { NonNull::<NonNull<State>>::from(&self.state)
            .cast::<EpsilonSystem>().as_ref() }
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
    empty_vec: OnceCell<NonNull<layout::EpsilonVecRepr>>
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
                    let original_header = NonNull::new_unchecked(header.cast::<u8>()
                        .as_ptr()
                        .sub(header.as_ref().type_info.layout.common_header_offset()));
                    let header_size = value_ptr.cast::<u8>()
                        .offset_from(original_header.as_ptr()) as usize;
                    let combined_layout = Layout::from_size_align_unchecked(
                        value_layout.size() + header_size,
                        value_layout.align().max(layout::EpsilonHeader::LAYOUT.align())
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
    state: NonNull<State>
}
impl EpsilonSystem {
    #[inline]
    fn from_state(state: Rc<State>) -> EpsilonSystem {
        EpsilonSystem {
            state: unsafe { NonNull::new_unchecked(Rc::into_raw(state) as *mut _) }
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
            empty_vec: OnceCell::new()
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
            state: unsafe { NonNull::new_unchecked(Rc::into_raw(self.clone_rc()) as *mut _ ) },
            root: true
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
    unsafe fn alloc_uninit<'gc, T>(&'gc self) -> (Self::Id, *mut T) where T: GcSafe<'gc, EpsilonCollectorId> + 'gc {
        let id = self.id();
        let tp = self::layout::TypeInfo::of::<T>();
        let needs_header = self::alloc::Default::NEEDS_EXPLICIT_FREE
            || !tp.may_ignore();
        let ptr = if needs_header {
            let (overall_layout, offset) = self::layout::EpsilonHeader::LAYOUT
                .extend(Layout::new::<T>()).unwrap();
            let mem = self.system().state().alloc.alloc_layout(overall_layout);
            let header = mem.cast::<self::layout::EpsilonHeader>();
            header.as_ptr().write(self::layout::EpsilonHeader {
                type_info: tp,
                next: None
            });
            self.system().state().push_state(header);
            mem.as_ptr().add(offset)
        } else {
            self.system().state().alloc.alloc_layout(Layout::new::<T>()).as_ptr()
        };
        (id, ptr.cast())
    }

    #[inline]
    fn alloc<'gc, T>(&'gc self, value: T) -> crate::Gc<'gc, T, Self::Id>
        where T: GcSafe<'gc, Self::Id> + 'gc {
        unsafe {
            let (_id, ptr) = self.alloc_uninit::<T>();
            ptr.write(value);
            Gc::from_raw(NonNull::new_unchecked(ptr))
        }
    }

    #[inline]
    unsafe fn alloc_uninit_slice<'gc, T>(&'gc self, len: usize) -> (Self::Id, *mut T)
        where T: GcSafe<'gc, Self::Id> + 'gc {
        let id = self.id();
        let type_info = self::layout::TypeInfo::of_array::<T>();
        let (overall_layout, offset) = Layout::new::<self::layout::EpsilonArrayHeader>()
            .extend(Layout::array::<T>(len).unwrap())
            .unwrap();
        let mem = self.system().state().alloc.alloc_layout(overall_layout);
        let header = mem.cast::<self::layout::EpsilonArrayHeader>();
        header.as_ptr().write(self::layout::EpsilonArrayHeader {
            common_header: self::layout::EpsilonHeader {
                type_info,
                next: None
            },
            len
        });
        self.system().state().push_state(NonNull::from(&header.as_ref().common_header));
        (id, mem.as_ptr().add(offset).cast())
    }

    #[inline]
    fn alloc_vec<'gc, T>(&'gc self) -> crate::vec::GcVec<'gc, T, Self>
        where T: GcSafe<'gc, Self::Id> + 'gc {
        let ptr = self.system().state().empty_vec.get_or_init(|| unsafe {
            NonNull::new_unchecked(self.alloc_vec_with_capacity::<'gc, ()>(0).as_repr().as_raw_ptr())
        }).as_ptr();
        crate::vec::GcVec {
            context: self,
            raw: unsafe { crate::vec::GcRawVec::from_repr(crate::Gc::from_raw(NonNull::new_unchecked(ptr))) }
        }
    }

    #[inline]
    fn alloc_vec_with_capacity<'gc, T>(&'gc self, capacity: usize) -> crate::vec::GcVec<'gc, T, Self>
        where T: GcSafe<'gc, Self::Id> + 'gc {
        if capacity == 0 {
            if let Some(&empty_ptr) = self.system().state().empty_vec.get() {
                return crate::vec::GcVec {
                    context: self,
                    raw: unsafe { crate::vec::GcRawVec::from_repr(crate::Gc::from_raw(empty_ptr)) }
                }
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
                    next: None
                },
                len: Cell::new(0),
                capacity
            });
            self.system().state().push_state(NonNull::from(&header.as_ref().common_header));
            let ptr = mem.as_ptr().add(offset).cast::<layout::EpsilonVecRepr>();
            crate::vec::GcVec {
                context: self,
                raw: crate::vec::GcRawVec::from_repr(crate::Gc::from_raw(NonNull::new_unchecked(ptr)))
            }
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
    _priv: ()
}
unsafe impl TrustedDrop for EpsilonCollectorId {}
unsafe impl<'other_gc, OtherId: CollectorId> GcSafe<'other_gc, OtherId> for EpsilonCollectorId {}
unsafe impl Trace for EpsilonCollectorId {
    const NEEDS_TRACE: bool = false;
    const NEEDS_DROP: bool = false;

    #[inline]
    fn trace<V: GcVisitor>(&mut self, _visitor: &mut V) -> Result<(), <V as GcVisitor>::Err> {
        Ok(())
    }

    unsafe fn trace_inside_gc<'gc, V, Id>(gc: &mut crate::Gc<'gc, Self, Id>, visitor: &mut V) -> Result<(), V::Err> where V: GcVisitor, Id: CollectorId, Self: GcSafe<'gc, Id> + 'gc {
        visitor.trace_gc(gc)
    }
}
unsafe impl TraceImmutable for EpsilonCollectorId {
    #[inline]
    fn trace_immutable<V: GcVisitor>(&self, _visitor: &mut V) -> Result<(), V::Err> {
        Ok(())
    }
}

unsafe impl NullTrace for EpsilonCollectorId {}
unsafe impl const ConstCollectorId for EpsilonCollectorId {
    #[inline]
    fn resolve_array_len_const<'gc, T: 'gc>(repr: &Self::ArrayRepr<'gc, T>) -> usize {
        repr.len()
    }
}
unsafe impl CollectorId for EpsilonCollectorId {
    type System = EpsilonSystem;
    type RawVecRepr<'gc> = self::layout::EpsilonVecRepr;
    /// We use fat-pointers for arrays,
    /// so that we can transmute from `&'static [T]` -> `GcArray`
    type ArrayRepr<'gc, T: 'gc> = zerogc::array::repr::FatArrayRepr<'gc, T, Self>;

    #[inline]
    fn from_gc_ptr<'a, 'gc, T>(_gc: &'a Gc<'gc, T>) -> &'a Self where T: ?Sized + 'gc, 'gc: 'a {
        const ID: EpsilonCollectorId = EpsilonCollectorId { _priv: () };
        &ID
    }

    #[inline]
    fn resolve_array_id<'a, 'gc, T>(_array: &'a Self::ArrayRepr<'gc, T>) -> &'a Self where T: 'gc, 'gc: 'a {
        const ID: EpsilonCollectorId = EpsilonCollectorId { _priv: () };
        &ID
    }

    #[inline]
    fn resolve_array_len<'gc, T: 'gc>(repr: &Self::ArrayRepr<'gc, T>) -> usize {
        repr.len()
    }


    #[inline]
    unsafe fn gc_write_barrier<'gc, T, V>(
        _owner: &Gc<'gc, T>,
        _value: &Gc<'gc, V>,
        _field_offset: usize
    ) where T: GcSafe<'gc, Self> + ?Sized + 'gc, V: GcSafe<'gc, Self> + ?Sized + 'gc {}

    unsafe fn assume_valid_system(&self) -> &Self::System {
        /*
         * NOTE: Supporting this would lose our ability to go from `&'static T` -> `Gc<'gc, T, EpsilonCollectorId>
         * It would also necessitate a header for `Copy` objects.
         */
        unimplemented!("Unable to convert EpsilonCollectorId -> EpsilonSystem")
    }
}