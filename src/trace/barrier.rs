//! Traits controlling write barriers

pub use crate::trace::{NullTrace, Trace};

/// Indicates that a mutable reference to a type
/// is safe to use without triggering a write barrier.
///
/// This means one of either two things:
/// 1. This type doesn't need any write barriers
/// 2. Mutating this type implicitly triggers a write barrier.
///
/// This is the bound for `RefCell<T>`. Since a RefCell doesn't explicitly trigger write barriers,
/// a `RefCell` can only be used with `T` if either:
/// 1. `T` doesn't need any write barriers or
/// 2. `T` implicitly triggers write barriers on any mutation
pub unsafe trait ImplicitWriteBarrier {}
unsafe impl<T: NullTrace> ImplicitWriteBarrier for T {}

/// Safely trigger a write barrier before
/// writing to a garbage collected value.
///
/// The value must be in managed memory,
/// a *direct* part of a garbage collected object.
/// Write barriers (and writes) must include a reference
/// to its owning object.
///
/// ## Safety
/// It is undefined behavior to forget to trigger a write barrier.
///
/// Field offsets are unchecked. They must refer to the correct
/// offset (in bytes).
///
/// ### Indirection
/// This trait only support "direct" writes,
/// where the destination field is inline with the source object.
///
/// For example it's correct to implement `GcDirectWrite<Value=A> for (A, B)`,
/// since since `A` is inline with the owning tuple.
///
/// It is **incorrect** to implement `GcDirectWrite<Value=T> for Vec<T>`,
/// since it `T` is indirectly referred to by the vector.
/// There's no "field offset" we can use to get from `*mut Vec` -> `*mut T`.
///
/// The only exception to this rule is [Gc] itself.
/// GcRef can freely implement [GcDirectBarrier] for any (and all values),
/// even though it's just a pointer.
/// It's the final destination of all write barriers and is expected
/// to internally handle the indirection.
pub unsafe trait GcDirectBarrier<'gc, OwningRef>: Trace {
    /// Trigger a write barrier,
    /// before writing to one of the owning object's managed fields
    ///
    /// It is undefined behavior to mutate a garbage collected field
    /// without inserting a write barrier before it.
    ///
    /// Generational, concurrent and incremental GCs need this to maintain
    /// the tricolor invariant.
    ///
    /// ## Safety
    /// The specified field offset must point to a valid field
    /// in the source object.
    ///
    /// The type of this value must match the appropriate field
    unsafe fn write_barrier(&self, owner: &OwningRef, field_offset: usize);
}
