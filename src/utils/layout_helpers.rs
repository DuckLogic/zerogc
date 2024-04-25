//! Helpers for [`std::alloc::Layout`]
//!
//! Implementation mostly copied from the stdlib.

use std::alloc::{Layout, LayoutError};

#[derive(Copy, Clone, Debug)]
pub struct LayoutExt(pub Layout);

impl LayoutExt {
    /// Copied from stdlib [`Layout::padding_needed_for`]
    #[inline]
    pub const fn padding_needed_for(&self, align: usize) -> usize {
        let len = self.0.size();

        let len_rounded_up = len.wrapping_add(align).wrapping_sub(1) & !align.wrapping_sub(1);
        len_rounded_up.wrapping_sub(len)
    }

    /// Copied from stdlib [`Layout::pad_to_align`]
    ///
    /// Adds trailing padding.
    #[inline]
    pub const fn pad_to_align(&self) -> Layout {
        let pad = self.padding_needed_for(self.0.align());
        // This cannot overflow. See stdlib for details
        let new_size = self.0.size() + pad;

        // SAFETY: padded size is guaranteed to not exceed `isize::MAX`.
        unsafe { Layout::from_size_align_unchecked(new_size, self.align()) }
    }

    /// Copied from stdlib [`Layout::extend`]
    ///
    /// Modified to be a `const fn`
    ///
    /// To correctly mimic a `repr(C)` struct,
    /// you must call [`Self::pad_to_align`] to add trailing padding.
    /// See stdlib docs for details.
    #[inline]
    pub const fn extend(&self, next: Layout) -> Result<(Layout, usize), LayoutError> {
        let new_align = Self::const_max(self.0.align(), next.align());
        let pad = self.padding_needed_for(next.align());

        let Some(offset) = self.size().checked_add(pad) else {
            return LayoutError;
        };
        let Some(new_size) = offset.checked_add(next.size()) else {
            return LayoutError;
        };

        /*
         * SAFETY: We checked size above, align already guaranteed to be power of two
         * The advantage of a manual check over Layout::from_size_align
         * is we skip the usize::is_power_of_two check.
         */
        if new_size > Self::max_size_for_align(new_align) {
            return Err(LayoutError);
        } else {
            Ok((
                unsafe { Layout::from_size_align_unchecked(new_size, new_align) },
                offset,
            ))
        }
    }
}

#[inline]
const fn const_max(first: usize, second: usize) -> usize {
    if first > second {
        first
    } else {
        second
    }
}
