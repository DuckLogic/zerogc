//! Includes manual `GarbageCollected` implementations for various types.
//!
//! Since unsafe code and third-party libraries can't have an automatically derived `GarbageCollected` implementation,
//! we need to manually provide them here.
//! This is done for all stdlib types and some feature gated external libraries.
#![doc(hidden)] // This is unstable

use crate::prelude::*;

use zerogc_derive::unsafe_gc_impl;

/// Unsafely implement `GarbageCollected` for the specified type,
/// by acquiring a 'lock' in order to trace the underlying value.
///
/// This is good for interior mutability types like `RefCell` and `Mutex` where you need to acquire a lock,
/// in order to safely view the interior.
/// Usually `unsafe_trace_deref!` is sufficient since it also lets you run
/// arbitrary code in order to 'convert' the macro to the necessary type,
/// and the only restriction is that the interior can be directly traced.
///
/// However, that isn't sufficient if you need to hold RAII guards (like `Ref` or `MutexGuard`s)
/// on the values you're tracing in addition to just accessing them.
/// For example, for `RefCell` you'd call `borrow` in order to acquire a `Ref` to the interior.
/// Although tracing garbage collection is already unsafe,
/// it's always completely undefined behavior to bypass the locking of `Mutex` and `RefCell`,
/// even if it's just for a 'little bit' since it may cause mutable references to alias.
/// It is currently the most powerful of the unsafe implementation macros,
/// since it lets you not only run an arbitrary expression like `unsafe_trace_deref!`,
/// but also acquire and hold a RAII guard object.
///
/// This macro is usually only useful for types like `Mutex` and `RefCell` who use raw pointers internally,
/// since raw pointers can't be automatically traced without additional type information.
/// Otherwise, it's best to use an automatically derived implementation since that's always safe.
/// However, using this macro is always better than a manual implementation, since it makes your intent clearer.
///
/// ## Usage
/// ````no_test
/// // You can use an arbitrary expression to acquire a lock's guard
/// unsafe_trace_lock!(RefCell, target = T, |cell| cell.borrow());
/// unsafe_trace_lock!(Mutex, target = T, |lock| lock.lock().unwrap());
/// unsafe_trace_lock!(RwLock, target = T, |lock| lock.lock().unwrap());
/// ````
///
/// ## Safety
/// Always prefer automatically derived implementations where possible,
/// since they're just as fast and can never cause undefined behavior.
/// This is basically an _unsafe automatically derived_ implementation,
/// to be used only when a safe automatically derived implementation isn't possible (like with `Vec`).
///
/// Undefined behavior if there could be additional garbage collected objects that are not reachable
/// by dereferencing the specified lock, since the macro only traces the item the lock dereferences to.
/// This usually isn't the case with most locks and would be somewhat rare,
/// but it's still a possibility that causes the macro to be unsafe.
///
///
/// This delegates to `unsafe_gc_brand` to provide the [GcRebrand] and [GcErase] implementation,
/// so that could also trigger undefined behavior.
#[macro_export]
macro_rules! unsafe_trace_lock {
    ($target:ident, target = $target_type:ident; |$get_mut:ident| $get_mut_expr:expr, |$lock:ident| $acquire_guard:expr) => {
        unsafe_impl_gc!(
            target => $target<$target_type>,
            params = [$target_type],
            null_trace => { where $target_type: NullTrace },
            NEEDS_TRACE => true,
            NEEDS_DROP => $target_type::NEEDS_DROP /* if our inner type needs a drop */
                || core::mem::needs_drop::<$target<()>>; // Or we have unconditional drop (std-mutex)
            trace_mut => |self, visitor| {
                let $get_mut = self;
                let value: &mut $target_type = $get_mut_expr;
                visitor.visit::<$target_type>(value)
            },
            trace_immutable => |self, visitor| {
                if !Self::NEEDS_TRACE { return Ok(()) };
                // We can immutably visit a lock by acquiring it
                let $lock = self;
                #[allow(unused_mut)]
                let mut guard = $acquire_guard;
                let guard_value = &mut *guard;
                visitor.visit(guard_value)
            }
        );
    };
}



/// Unsafely implement `ImmutableTrace` for the specified iterable type,
/// by iterating over the type to trace all objects.
///
/// You still have to implement the regular `Trace` and `GcSafe` traits by hand.
///
/// This macro is only useful for unsafe collections like `Vec` and `HashMap` who use raw pointers internally,
/// since raw pointers can't have automatically derived tracing implementations.
/// Otherwise, it's best to use an automatically derived implementation since that's always safe.
/// In order to prevent ambiguity, this always requires the type of the element being traced.
///
/// ## Usage
/// ````no_test
/// unsafe_trace_iterable!(Vec, element = T);
/// unsafe_trace_iterable!(HashMap, element = { (&K, &V) }; K, V);
/// unsafe_trace_iterable!(HashSet, element = T);
///
/// assert!(!<Vec<i32> as Trace>::NEEDS_TRACE);
/// assert!(<Vec<dummy_impl::Gc<'static, i32>> as Trace>::NEEDS_TRACE);
/// assert!(<Vec<i32> as GcSafe>::NEEDS_DROP);
/// ````
///
/// ## Safety
/// Always prefer automatically derived implementations where possible,
/// since they're just as fast and can never cause undefined behavior.
/// This is basically an _unsafe automatically derived_ implementation,
/// to be used only when a safe automatically derived implementation isn't possible (like with `Vec`).
///
/// Undefined behavior if there could be garbage collected objects that are not reachable via iteration,
/// since the macro only traces objects it can iterate over,
/// and the garbage collector will free objects that haven't been traced.
/// This usually isn't the case with collections and would be somewhat rare,
/// but it's still a possibility that causes the macro to be unsafe.
///
///
/// This delegates to `unsafe_gc_brand!` to provide the [GcRebrand] and [GcErase] implementation,
/// so that could also trigger undefined behavior.
#[macro_export]
macro_rules! unsafe_immutable_trace_iterable {
    ($target:ident, element = $element_type:ident) => {
        unsafe_trace_iterable!($target, element = &$element_type; $element_type);
    };
    ($target:ident<$($param:ident),*>; element = { $element_type:ty }) => {
        unsafe impl<$($param),*> TraceImmutable for $target<$($param),*>
            where $($param: TraceImmutable),* {
            fn visit_immutable<Visit: GcVisitor>(&self, visitor: &mut Visit) -> Result<(), Visit::Err> {
                if !Self::NEEDS_TRACE { return Ok(()) };
                let iter = IntoIterator::into_iter(self);
                for element in iter {
                    let element: $element_type = element;
                    visitor.visit_immutable(&element)?;
                }
                Ok(())
            }
        }
        unsafe impl<$($param: $crate::NullTrace),*> NullTrace for $target<$($param),*> {}
    };
}


/// Unsafely implement `GarbageCollected` for the specified type,
/// by assuming it's a 'primitive' and never needs to be traced.
///
/// The fact that this type is a 'primitive' means it's assumed to have no type parameters,
/// and that there's nothing .
/// This macro is only useful for unsafe types like `String` who use raw pointers internally,
/// since raw pointers can't be automatically traced without additional type information.
///
/// ## Safety
/// Always prefer automatically derived implementations where possible,
/// since they're just as fast and can never cause undefined behavior.
/// This is basically an _unsafe automatically derived_ implementation,
/// to be used only when a safe automatically derived implementation isn't possible (like with `String`).
///
/// Undefined behavior only if there are garbage collected pointers in the type's interior,
/// since the implementation assumes there's nothing to trace in the first place.
///
/// This delegates to `unsafe_gc_brand!` to provide the [GcRebrand] and [GcErase] implementation,
/// but that will never cause undefined behavior unless you
/// already have garbage collected pointers inside
/// (which are already undefined behavior for tracing).
#[macro_export]
macro_rules! unsafe_trace_primitive {
    ($target:ty) => {
        unsafe_gc_impl! {
            target => $target,
            params => [],
            null_trace => always,
            NEEDS_TRACE => false,
            NEEDS_DROP => core::mem::needs_drop::<$target>(),
            visit => |self, visitor| { /* nop */ }
        }
        unsafe impl<'gc, OwningRef> $crate::GcDirectBarrier<'gc, OwningRef> for $target {
            #[inline(always)]
            unsafe fn write_barrier(
                &self, _owner: &OwningRef, _field_offset: usize,
            ) {
                /*
                 * TODO: We don't have any GC fields,
                 * so what does it mean to have a write barrier?
                 */
                /* NOP */
            }
        }
    };
}


/// Unsafely assume that the generic implementation of [GcRebrand] and [GcErase] is valid,
/// if and only if it's valid for the generic lifetime and type parameters.
///
/// Always _prefer automatically derived implementations where possible_,
/// since they can never cause undefined behavior.
/// This macro is only necessary if you have raw pointers internally,
/// which can't have automatically derived safe implementations.
/// This is basically an _unsafe automatically derived_ implementation,
/// to be used only when a safe automatically derived implementation isn't possible (like with `Vec`).
///
/// This macro takes a varying number of parameters referring to the type's generic parameters,
/// which are all properly bounded and required to implement [GcRebrand] and [GcErase] correctly.
///
/// This macro can only cause undefined behavior if there are garbage collected pointers
/// that aren't included in the type parameter.
/// For example including `Gc<u32>` would be completely undefined behavior,
/// since we'd blindly erase its lifetime.
///
/// However, generally this macro provides the correct implementation
/// for straighrtforward wrapper/collection types.
/// Currently the only exception is when you have garbage collected lifetimes like `Gc`.
#[macro_export]
#[deprecated(note = "Use unsafe_impl_gc")]
macro_rules! unsafe_gc_brand {
    ($target:tt) => {
        unsafe impl<'new_gc, Id: $crate::CollectorId> $crate::GcRebrand<'new_gc, Id> for $target {
            type Branded = Self;
        }
        unsafe impl<'a, Id: $crate::CollectorId> $crate::GcErase<'a, Id> for $target {
            type Erased = Self;
        }
    };
    ($target:ident, $($param:ident),+) => {
        unsafe impl<'new_gc, Id, $($param),*> $crate::GcRebrand<'new_gc, Id> for $target<$($param),*>
            where Id: $crate::CollectorId, $($param: $crate::GcRebrand<'new_gc, Id>,)*
                  $(<$param as $crate::GcRebrand<'new_gc, Id>>::Branded: Trace,)* {
            type Branded = $target<$(<$param as $crate::GcRebrand<'new_gc, Id>>::Branded),*>;
        }
        unsafe impl<'a, Id, $($param),*> $crate::GcErase<'a, Id> for $target<$($param),*>
            where Id: $crate::CollectorId, $($param: $crate::GcErase<'a, Id>,)*
                  $(<$param as $crate::GcErase<'a, Id>>::Erased: Trace,)* {
            type Erased = $target<$(<$param as $crate::GcErase<'a, Id>>::Erased),*>;
        }
    };
    ($target:tt, immut = required; $($param:ident),+) => {
        unsafe impl<'new_gc, Id, $($param),*> $crate::GcRebrand<'new_gc, Id> for $target<$($param),*>
            where Id: $crate::CollectorId, $($param: $crate::GcRebrand<'new_gc, Id> + TraceImmutable,)*
                  $(<$param as $crate::GcRebrand<'new_gc, Id>>::Branded: TraceImmutable,)* {
            type Branded = $target<$(<$param as $crate::GcRebrand<'new_gc, Id>>::Branded),*>;
        }
        unsafe impl<'a, Id, $($param),*> $crate::GcErase<'a, Id> for $target<$($param),*>
            where Id: $crate::CollectorId, $($param: $crate::GcErase<'a, Id> + TraceImmutable,)*
                  $(<$param as $crate::GcErase<'a, Id>>::Erased: TraceImmutable,)* {
            type Erased = $target<$(<$param as $crate::GcErase<'a, Id>>::Erased),*>;
        }
    }
}

mod core;
#[cfg(any(feature = "alloc", feature = "std"))]
mod stdalloc;
#[cfg(feature = "std")]
mod stdlib;
#[cfg(feature = "indexmap")]
mod indexmap;
