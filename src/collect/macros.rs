#[macro_export]
macro_rules! static_null_trace {
    ($($target:ident),*) => {
        $($crate::static_null_trace!(@single $target);)*
    };
    (@single $target:ident) => {
        unsafe impl<Id: $crate::CollectorId> $crate::Collect<Id> for $target {
            type Collected<'newgc> = Self;
            const NEEDS_COLLECT: bool = {
                $crate::collect::macros::helpers::assert_static_lifetime::<Self>();
                false
            };

            #[inline(always)] // does nothing
            unsafe fn collect_inplace(_target: std::ptr::NonNull<Self>, _context: &mut crate::context::CollectContext<'_, Id>) {}
        }
        unsafe impl<Id: $crate::CollectorId> $crate::NullCollect<Id> for $target {}
    };
}

#[doc(hidden)]
pub mod helpers {
    pub const fn assert_static_lifetime<T: ?Sized + 'static>() {}
}
