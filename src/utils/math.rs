use num_traits::{PrimInt, NumCast};
use std::fmt::Debug;

/// Internal utility trait for performing checked arithmetic on primitive integers.
///
/// Prefered over `num_traits` since it's easier to propagate overflow,
/// and we have some utility methods like `ceil_divide`.
/// I would gladly accept a PR to use them instead,
/// if one of their methods elegantly covers my use cases.
pub trait CheckedMath: Copy + NumCast + PrimInt + Debug {
    #[inline]
    fn add(self, other: Self) -> Result<Self, OverflowError> {
        self.checked_add(&other).ok_or(OverflowError)
    }
    #[inline]
    fn sub(self, other: Self) -> Result<Self, OverflowError> {
        self.checked_sub(&other).ok_or(OverflowError)
    }

    /// Divide this number by `divisor`, rounding the result upwards
    #[inline]
    fn ceil_divide(self, divisor: Self) -> Self {
        /*
         * Increasing `target` by `divisor-1` will always increase the result of division,
         * unless `target` is a perfect multiple of `divisor` in which case it'll give the exact result.
         * For example, `(5 + (10 - 1)) / 10` properly rounds up to `1`.
         * Taken from https://stackoverflow.com/questions/17944/how-to-round-up-the-result-of-integer-division
         */
        (self + (divisor - Self::one())) / divisor
    }

    /// Round this number up to the next multiple of `increment`,
    /// returning an error on overflow and panicking on division by zero
    #[inline]
    fn round_up_checked(self, increment: Self) -> Result<Self, OverflowError> {
        assert_ne!(increment, Self::zero(), "Division by zero!");
        // Exact same addition described in `ceil_divide`
        let rounded_up = self.checked_add(&(increment - Self::one()))
            .ok_or(OverflowError)?;
        /*
         * This is the 'remainder' of what we would get by the addition in `ceil_divide`,
         * and we `rounded_up - rounding_remainder` value in order to properly ceil.
         * This properly handles the case where we're an exact multiple of `increment`,
         * since in that case `rounding_remainder` is exactly `increment - 1`,
         * and `rounded_up - (increment - 1)` cancels out via simple algebra.
         * For example when calling `checked_round_up(10, 10)`,
         * `rounded_up = (10 + (10 - 1))` and becomes `19`.
         * Then, rounding_remainder becomes `19 % 10` and becomes 9.
         * Subtracting that from `rounded_up` becomes `10` once again.
         */
        let rounding_remainder = rounded_up % increment;
        Ok(rounded_up - rounding_remainder)
    }
}


impl CheckedMath for u16 {}
impl CheckedMath for u32 {}
impl CheckedMath for u64 {}
impl CheckedMath for usize {}
/// Dedicated error that indicates that some math has encountered unexpected arithmetic overflow.
///
/// A dedicated error type for arithmetic overflow not only better represents intended meaning,
/// but allows us to use the full power of rust's error handling system.
/// The beauty of using an error for arithmetic overflow is it can be easily propagated with `?`,
/// and we can give much better error messages.
/// Error conversion is also much cleaner,
/// since instead of using `From<NoneError>`, you can use `From<OverflowError>`.
///
/// Hopefully `num_traits` will eventually decide this is the best option and we can scrap all this.
#[derive(Debug)]
pub struct OverflowError;
