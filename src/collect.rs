//! Defines the [`Collect`] trait and implements it for several types

mod collections;
pub mod macros;
mod primitives;

use crate::context::CollectContext;
use crate::CollectorId;
use std::mem::ManuallyDrop;
use std::ptr::NonNull;

pub unsafe trait Collect<Id: CollectorId> {
    type Collected<'newgc>: Collect<Id>;
    const NEEDS_COLLECT: bool;

    unsafe fn collect_inplace(target: NonNull<Self>, context: &mut CollectContext<'_, Id>);
}

pub unsafe trait NullCollect<Id: CollectorId>: Collect<Id> {}

//
// macros
//
