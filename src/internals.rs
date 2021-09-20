//! Internal traits intended only for implementations
//! of `zerogc`

use super::CollectorId;

use crate::array::GcArray;

/// `const` access to the `CollectorId`
pub unsafe trait ConstCollectorId: CollectorId {
   /// Resolve the length of the specified [GcArray](`crate::array::GcArray`)
    fn resolve_array_len_const<T>(repr: &GcArray<'_, T, Self>) -> usize;
}
