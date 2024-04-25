use crate::collect::{Collect, NullCollect};
use crate::context::CollectContext;
use crate::CollectorId;
use std::ptr::NonNull;

unsafe impl<Id: CollectorId, T: Collect<Id>> Collect<Id> for Vec<T> {
    type Collected<'newgc> = Vec<T::Collected<'newgc>>;
    const NEEDS_COLLECT: bool = T::NEEDS_COLLECT;

    #[inline]
    unsafe fn collect_inplace(target: NonNull<Self>, context: &mut CollectContext<'_, Id>) {
        if Self::NEEDS_COLLECT {
            for val in target.as_ref().iter() {
                T::collect_inplace(NonNull::from(val), context);
            }
        }
    }
}

unsafe impl<Id: CollectorId, T: NullCollect<Id>> NullCollect<Id> for Vec<T> {}
