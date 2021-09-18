//! A horrible hack to pass `GcContext` back and forth to serde using thread locals.
use std::any::TypeId;
use std::ffi::c_void;
use std::cell::{UnsafeCell, RefCell, Cell};
use std::collections::HashMap;
use std::marker::PhantomData;
use std::ptr::NonNull;
use std::collections::hash_map::Entry;

use crate::prelude::*;
use crate::serde::GcDeserialize;
use serde::{Deserializer, Deserialize};


struct ContextHackState {
    current_ctx: UnsafeCell<Option<ContextHack>>,
    /// The number of active references to the context.
    ///
    /// If this is zero, then `state` should be `None`,
    /// otherwise it should be `Some`
    active_refs: Cell<usize>,
}
impl ContextHackState {
    const fn uninit() -> ContextHackState {
        ContextHackState {
            current_ctx: UnsafeCell::new(None),
            active_refs: Cell::new(0)
        }
    }
    #[inline]
    unsafe fn get_unchecked(&self) -> Option<&ContextHack> {
        if self.active_refs.get() == 0 {
            None
        } else {
            Some((&*self.current_ctx.get()).as_ref().unwrap_unchecked())
        }
    }
    unsafe fn lock_unchecked<'gc, C: GcContext>(&self) -> ContextHackGuard<'gc, C> {
        self.active_refs.set(self.active_refs.get() + 1);
        debug_assert_eq!(TypeId::of::<C::Id>(), self.get_unchecked().unwrap().collector_type_id);
        ContextHackGuard {
            state: NonNull::from(self),
            marker: PhantomData
        }
    }
    #[inline]
    unsafe fn release_lock(&self) -> bool {
        debug_assert!(self.active_refs.get() > 0);
        match self.active_refs.get() {
            1 => {
                self::unregister_context(self);
                true
            },
            0 => std::hint::unreachable_unchecked(),
            _ => {
                self.active_refs.set(self.active_refs.get() - 1);
                false
            }
        }
    }
}
/// A hack to store a dynamically typed [GcContext] in a thread-local.
struct ContextHack {
    collector_type_id: TypeId,
    ptr: NonNull<c_void>,
}
impl ContextHack {
    /// Cast this context into the specified type.
    ///
    /// Returns `None` if the id doesn't match
    #[inline]
    pub fn cast_as<Ctx: GcContext>(&self) ->  Option<&'_ Ctx> {
        if TypeId::of::<Ctx::Id>() == self.collector_type_id {
            Some(unsafe { &*(self.ptr.as_ptr() as *mut Ctx) })
        } else {
            None
        }
    }
}
thread_local! {
    static PRIMARY_DE_CONTEXT: ContextHackState = ContextHackState::uninit();
    static OTHER_DE_CONTEXT: RefCell<HashMap<TypeId, Box<ContextHackState>>> = RefCell::new(HashMap::new());
}

pub struct ContextHackGuard<'gc, C: GcContext> {
    state: NonNull<ContextHackState>,
    marker: PhantomData<&'gc C>,
}
impl<'gc, C: GcContext> ContextHackGuard<'gc, C> {
    /// Get the guard's underlying context.
    ///
    /// ## Safety
    /// Undefined behavior if this guard is dropped
    /// and then subsequently mutated.
    ///
    /// Undefined behavior if a safepoint occurs (although the immutable reference should prevent that)
    #[inline]
    pub unsafe fn get_unchecked(&self) -> &'gc C {
        &*self.state.as_ref().get_unchecked().unwrap_unchecked().ptr.cast::<C>().as_ptr()
    }
}
impl<'gc, C: GcContext> Drop for ContextHackGuard<'gc, C> {
    #[inline]
    fn drop(&mut self) {
        unsafe {
            self.state.as_ref().release_lock();
        }
    }
}
/// Temporarily places the specified gc context
/// in a thread-local, to allow deserialization with serde.
///
/// Panics if another context of the same type is
/// already in the process of being deserialized.
///
/// ## Safety
/// Undefined behavior if the context is mutated or ever used with the wrong
/// lifetime (although this is technically part of the [current_ctx] contract)
#[track_caller]
pub unsafe fn set_context<C: GcContext>(ctx: &C) -> ContextHackGuard<'_, C> {
    let guard = PRIMARY_DE_CONTEXT.with(|state| {
        match state.get_unchecked() {
            Some(other) => {
                if let Some(other) = other.cast_as::<C>() {
                    assert_eq!(other.id(), ctx.id(), "Multiple collectors of the same type");
                    Some(state.lock_unchecked())
                } else {
                    None
                }
            },
            None => {
                state.current_ctx.get().write(Some(ContextHack {
                    collector_type_id: TypeId::of::<C::Id>(),
                    ptr: NonNull::from(ctx).cast()
                }));
                Some(state.lock_unchecked())
            }
        }
    });
    guard.unwrap_or_else(|| _fallback_set_context(ctx))
}
#[cold]
unsafe fn _fallback_set_context<C: GcContext>(ctx: &C) -> ContextHackGuard<'_, C> {
    OTHER_DE_CONTEXT.with(|map| {
        let mut map = map.borrow_mut();
        match map.entry(TypeId::of::<C::Id>()) {
            Entry::Occupied(occupied) => {
                let other = occupied.get();
                assert_eq!(other.get_unchecked().unwrap().cast_as::<C>().unwrap().id(), ctx.id(), "Multiple collectors of the same type");
                other.lock_unchecked()
            },
            Entry::Vacant(entry) => {
                let res = entry.insert(Box::new(ContextHackState {
                    active_refs: Cell::new(0),
                    current_ctx: UnsafeCell::new(Some(ContextHack {
                        collector_type_id: TypeId::of::<C::Id>(),
                        ptr: NonNull::from(ctx).cast()
                    }))
                }));
                res.lock_unchecked()
            }
        }
    })
}
#[cold]
unsafe fn unregister_context(state: &ContextHackState) {
    let expected_ctx = state.get_unchecked().unwrap_unchecked();
    assert_eq!(state.active_refs.get(), 1);
    let needs_fallback_free = PRIMARY_DE_CONTEXT.with(|ctx| {
        if let Some(actual) = ctx.get_unchecked() {
            if actual.collector_type_id == expected_ctx.collector_type_id {
                debug_assert_eq!(actual.ptr.as_ptr(), expected_ctx as *const _ as *mut _);
                ctx.active_refs.set(0);
                ctx.current_ctx.get().write(None);
                return false; // don't search the fallback HashMap. We're freed the old fashioned way
            }
        }
        true // need to fallback to search HashMap
    });
    if needs_fallback_free {
        OTHER_DE_CONTEXT.with(|map| {
            let mut map = map.borrow_mut();
            let actual_state = map.remove(&expected_ctx.collector_type_id)
                .unwrap_or_else(|| unreachable!("Can't find collector in serde::hack state"));
            debug_assert_eq!(expected_ctx.collector_type_id, actual_state.get_unchecked().unwrap().collector_type_id);
            debug_assert_eq!(actual_state.get_unchecked().unwrap() as *const _, expected_ctx as *const _);
            actual_state.as_ref().active_refs.set(0);
            drop(actual_state);
        })
    }
}

/// Get the current context for deserialization
///
/// ## Safety
/// The inferred lifetime must be correct.
#[track_caller]
pub unsafe fn current_ctx<'gc, Id: CollectorId>() -> ContextHackGuard<'gc, Id::Context> {
    PRIMARY_DE_CONTEXT.with(|state| {
        match state.get_unchecked() {
            Some(hack) if hack.collector_type_id == TypeId::of::<Id>() => {
                Some(state.lock_unchecked())
            },
            _ => None
        }
    }).unwrap_or_else(|| _fallback_current_ctx::<'gc, Id>())
}
#[cold]
#[track_caller]
unsafe fn _fallback_current_ctx<'gc, Id: CollectorId>() -> ContextHackGuard<'gc, Id::Context> {
    OTHER_DE_CONTEXT.with(|map| {
        let map = map.borrow();
        let state = map.get(&TypeId::of::<Id>())
            .unwrap_or_else(|| unreachable!("Can't find collector for {} in serde::hack state", std::any::type_name::<Id>()));
        state.lock_unchecked()
    })
}

/// Wrapper function to deserialize the specified value via the "hack",
/// getting the current context via [current_ctx]
///
/// ## Safety
/// The contract of [current_de_ctx] must be upheld.
/// In other words, the current context must've been set by [set_context] and have the appropriate lifetime
#[track_caller]
pub unsafe fn unchecked_deserialize_hack<'gc, 'de, D: Deserializer<'de>, Id: CollectorId, T: GcDeserialize<'gc, 'de, Id>>(
    deserializer: D
) -> Result<T, D::Error> {
    let guard = current_ctx::<'gc, Id>();
    T::deserialize_gc(guard.get_unchecked(), deserializer)
}

#[repr(transparent)]
#[derive(Eq, Hash, Debug, PartialEq, Clone)]
pub struct DeserializeHackWrapper<T, Id>(T, PhantomData<Id>);

impl<'gc, 'de, Id: CollectorId, T: GcDeserialize<'gc, 'de, Id>> Deserialize<'de> for DeserializeHackWrapper<T, Id> {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error> where D: Deserializer<'de> {
        let guard = unsafe { current_ctx::<'gc, Id>() };
        Ok(DeserializeHackWrapper(T::deserialize_gc(unsafe { guard.get_unchecked() }, deserializer)?, PhantomData))
    }
}

/// Transmute between two types whose sizes may not be equal at compile time
///
/// ## Safety
/// All the usual cavets of [std::mem::transmute] apply.
///
/// However, the sizes aren't verified
/// to be the same size at compile time.
///
/// It is undefined behavior to invoke this function with types whose sizes
/// don't match at runtime.
/// However, in the current implementation, this will cause a panic.
#[inline]
pub unsafe fn transmute_mismatched<T, U>(src: T) -> U {
    assert_eq!(
        std::mem::size_of::<T>(), std::mem::size_of::<U>(),
        "UB: Mismatched sizes for {} and {}",
        std::any::type_name::<T>(), std::any::type_name::<U>()
    );
    let src = std::mem::ManuallyDrop::new(src);
    std::mem::transmute_copy::<T, U>(&*src)
}