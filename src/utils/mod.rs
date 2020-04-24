use std::marker::PhantomData;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::cell::Cell;

pub mod math;
mod unchecked;

use self::math::{CheckedMath, OverflowError};
pub use self::unchecked::*;
use num_traits::NumCast;

pub struct IdCounter<T: CheckedMath> {
    current: Cell<T>
}
impl<T: CheckedMath> IdCounter<T> {
    #[inline]
    pub fn new() -> Self {
        IdCounter { current: Cell::new(T::zero()) }
    }
    #[inline]
    pub fn try_next(&self) -> Result<T, OverflowError> {
        let old_count = self.current.get();
        self.current.set(old_count.checked_add(&T::one()).ok_or(OverflowError)?);
        Ok(old_count)
    }
}
pub struct AtomicIdCounter<T: CheckedMath> {
    atomic_current: AtomicUsize,
    marker: PhantomData<T>
}
impl<T: CheckedMath> AtomicIdCounter<T> {
    #[inline]
    pub const fn new() -> Self {
        AtomicIdCounter {
            atomic_current: AtomicUsize::new(0),
            marker: PhantomData
        }
    }
    pub fn try_next(&self) -> Result<T, OverflowError> {
        loop {
            let old_count = self.atomic_current.load(Ordering::SeqCst);
            let new_count = NumCast::from(old_count.checked_add(1).ok_or(OverflowError)?)
                .ok_or(OverflowError)?;
            if self.atomic_current.compare_and_swap(old_count, new_count, Ordering::SeqCst) == old_count {
                return Ok(NumCast::from(old_count).unwrap());
            }
        }
    }
}
