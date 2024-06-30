//! Defines the [`Collect`] trait and implements it for several types

use std::ptr::NonNull;

use crate::context::CollectContext;
use crate::CollectorId;

mod collections;
#[doc(hidden)] // has an internal helper module
pub mod macros;
mod primitives;

pub unsafe trait Collect<Id: CollectorId> {
    type Collected<'newgc>: Collect<Id>;
    const NEEDS_COLLECT: bool;

    unsafe fn collect_inplace(target: NonNull<Self>, context: &mut CollectContext<'_, Id>);
}

pub unsafe trait NullCollect<Id: CollectorId>: Collect<Id> {}

//
// macros
//
