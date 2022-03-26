use std::rc::Rc;
use std::ptr::NonNull;

use crate::{HandleCollectorId, prelude::*};

use super::{EpsilonCollectorId, EpsilonContext, EpsilonSystem, State};

pub struct GcHandle<T: ?Sized + GcSafe<'static, EpsilonCollectorId>> {
    /// The reference to the state,
    /// which keeps our data alive
    state: Rc<State>,
    ptr: *const T
}
impl<T: ?Sized + GcSafe<'static, EpsilonCollectorId>> Clone for GcHandle<T> {
    fn clone(&self) -> Self {
        GcHandle {
            state: Rc::clone(&self.state),
            ptr: self.ptr
        }
    }
}
unsafe impl<T: ?Sized + GcSafe<'static, EpsilonCollectorId>> zerogc::GcHandle<T> for GcHandle<T> {
    type System = EpsilonSystem;
    type Id = EpsilonCollectorId;

    #[inline]
    fn use_critical<R>(&self, func: impl FnOnce(&T) -> R) -> R {
        func(unsafe { &*self.ptr })
    }

    fn bind_to<'new_gc>(
        &self,
        context: &'new_gc EpsilonContext
    ) -> Gc<'new_gc, T::Branded, Self::Id>
        where T: GcRebrand<'new_gc, Self::Id> {
        // TODO: Does the simple collector assert the ids are equal?
        assert_eq!(context.state.as_ptr() as *const State, &*self.state as *const State);
        unsafe {
            Gc::from_raw(NonNull::new_unchecked(
                std::mem::transmute_copy::<*const T, *const T::Branded>(&self.ptr) as *mut T::Branded
            ))
        }
    }
}
zerogc_derive::unsafe_gc_impl!(
    target => GcHandle<T>,
    params => [T: GcSafe<'static, EpsilonCollectorId>],
    bounds => {
        Trace => { where T: ?Sized },
        TraceImmutable => { where T: ?Sized },
        TrustedDrop => { where T: ?Sized },
        GcSafe => { where T: ?Sized },
    },
    null_trace => { where T: ?Sized },
    NEEDS_DROP => true,
    NEEDS_TRACE => false,
    branded_type => Self,
    trace_template => |self, visitor| { Ok(()) }
);

unsafe impl HandleCollectorId for EpsilonCollectorId {
    type Handle<T> 
        where T: GcSafe<'static, Self> + ?Sized = GcHandle<T>;

    fn create_handle<'gc, T>(_gc: Gc<'gc, T, Self>) -> Self::Handle<T::Branded>
        where T: GcSafe<'gc, Self> + GcRebrand<'static, Self> + ?Sized {
        unimplemented!("epsilon collector can't convert Gc -> GcContext")
    }
}
