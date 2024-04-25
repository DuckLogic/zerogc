pub(crate) mod layout_helpers;

pub(crate) use self::layout_helpers::LayoutExt;
use crate::Collect;
use std::mem::ManuallyDrop;

#[inline(always)]
pub unsafe fn transmute_arbitrary<T, U>(val: T) -> U {
    if std::mem::size_of::<T>() != std::mem::size_of::<U>()
        || std::mem::align_of::<T>() != std::mem::align_of::<U>()
    {
        mismatch_transmute_size::<T, U>()
    }
    ManuallyDrop::into_inner(std::ptr::read::<ManuallyDrop<U>>(&ManuallyDrop::new(val)
        as *const ManuallyDrop<T>
        as *const ManuallyDrop<U>))
}

#[cold]
fn mismatch_transmute_size<T, U>() -> ! {
    let original_type_name = std::any::type_name::<T>();
    let collected_type_name = std::any::type_name::<U>();
    let sizes = [
        ("sizes", std::mem::size_of::<T>(), std::mem::size_of::<T>()),
        (
            "alignments",
            std::mem::align_of::<T>(),
            std::mem::align_of::<U>(),
        ),
    ];
    for (value_name, original_size, collected_size) in sizes {
        assert_eq!(
            original_size, collected_size,
            "Mismatched {} between T `{}` and U `{}`",
            value_name, original_type_name, collected_type_name
        );
    }
    unreachable!("sizes & alignments unexpectedly matched")
}
