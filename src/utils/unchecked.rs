use std::cell::{Cell, RefCell};

/// Perform a panic in debug mode, and assume unreachable in release
#[inline]
pub unsafe fn debug_unreachable() -> ! {
    if cfg!(debug_assertions) {
        unreachable!()
    } else {
        ::unreachable::unreachable()
    }
}

unsafe trait UncheckedCell<T> {
    /// Borrow this cell, unsafely assuming it isn't currently being mutated.
    #[inline]
    unsafe fn unchecked_borrow(&self) -> &T {
        &*self.as_ptr()
    }
    #[inline]
    unsafe fn unchecked_borrow_mut(&mut self) -> &mut T {
        &mut *self.as_ptr()
    }
    #[inline]
    fn as_ptr(&self) -> *mut T;
}
unsafe impl<T> UncheckedCell<T> for Cell<T> {
    #[inline]
    fn as_ptr(&self) -> *mut T {
        self.as_ptr()
    }
}

unsafe impl<T> UncheckedCell<T> for RefCell<T> {
    #[inline]
    fn as_ptr(&self) -> *mut T {
        self.as_ptr()
    }
}