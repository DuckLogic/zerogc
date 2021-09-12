//! Internal traits intended only for implementations
//! of `zerogc`

use super::CollectorId;

/// `const` access to the `CollectorId`
pub unsafe trait ConstCollectorId: CollectorId {
   /// Resolve the length of the specified [GcArray]
    fn resolve_array_len_const<T>(repr: &Self::ArrayRepr<'_, T>) -> usize;
}
