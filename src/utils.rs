use std::mem::ManuallyDrop;

pub(crate) mod bumpalo_raw;
mod layout_helpers;

pub use self::layout_helpers::{Alignment, LayoutExt};

/// Transmute one type into another,
/// without doing compile-time checks for sizes.
///
/// The difference between [`mem::transmute`] is that this function
/// does not attempt to check sizes at compile time.
/// In the regular [`mem::transmute`] function, an compile-time error occurs
/// if the sizes can't be proved equal at compile time.
/// This function does `assert_eq!` at runtime instead.
///
/// In some contexts, the sizes of types are statically unknown,
/// so a runtime assertion is better than a static compile-time check.
///
/// ## Safety
/// See [`mem::transmute`] for full details on safety.
///
/// The sizes of the two types must exactly match,
/// but unlike [`mem::transmute`] this is not checked at compile time.
///
/// Because transmute is a by-value operation,
/// the alignment of the transmuted values themselves is not a concern.
#[inline(always)]
#[track_caller] // for the case where sizes don't match
pub unsafe fn transmute_arbitrary<Src, Dst>(val: Src) -> Dst {
    let size_matches = const { std::mem::size_of::<Src>() == std::mem::size_of::<Dst>() };
    if size_matches {
        let src: ManuallyDrop<Src> = ManuallyDrop::new(val);
        std::mem::transmute_copy(&src as &Src)
    } else {
        mismatch_transmute_sizes(
            TransmuteTypeInfo::new::<Src>(),
            TransmuteTypeInfo::new::<Dst>(),
        )
    }
}

struct TransmuteTypeInfo {
    size: usize,
    type_name: &'static str,
}
impl TransmuteTypeInfo {
    #[inline]
    pub fn new<T>() -> Self {
        TransmuteTypeInfo {
            size: std::mem::size_of::<T>(),
            type_name: std::any::type_name::<T>(),
        }
    }
}

#[cold]
#[track_caller]
fn mismatch_transmute_sizes(src: TransmuteTypeInfo, dst: TransmuteTypeInfo) -> ! {
    assert_eq!(
        src.size, dst.size,
        "Mismatched size between Src `{}` and Dst `{}`",
        src.type_name, dst.type_name
    );
    unreachable!() // sizes actually match
}
