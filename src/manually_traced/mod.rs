//! Includes manual `GarbageCollected` implementations for various types.
//!
//! Since unsafe code and third-party libraries can't have an automatically derived `GarbageCollected` implementation,
//! we need to manually provide them here.
//! This is done for all stdlib types and some feature gated external libraries.
#![doc(hidden)] // This is unstable

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
/// ````
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
/// This delegates to `unsafe_gc_brand` to provide the `GcBrand` implementation,
/// so that could also trigger undefined behavior.
#[macro_export]
macro_rules! unsafe_trace_lock {
    ($target:ident, target = $target_type:ident; |$get_mut:ident| $get_mut_expr:expr, |$lock:ident| $acquire_guard:expr) => {
        unsafe_gc_brand!($target, $target_type);
        unsafe impl<$target_type: Trace> Trace for $target<$target_type> {
            const NEEDS_TRACE: bool = T::NEEDS_TRACE;
            #[inline]
            fn visit<V: GcVisitor>(&mut self, visitor: &mut V) -> Result<(), V::Err> {
                let $get_mut = self;
                let value: &mut $target_type = $get_mut_expr;
                visitor.visit(value)
            }
        }
        unsafe impl<$target_type: Trace> $crate::TraceImmutable for $target<$target_type> {
            #[inline]
            fn visit_immutable<V: GcVisitor>(&self, visitor: &mut V) -> Result<(), V::Err> {
                if !Self::NEEDS_TRACE { return Ok(()) };
                // We can immutably visit a lock by acquiring it
                let $lock = self;
                #[allow(unused_mut)]
                let mut guard = $acquire_guard;
                let guard_value = &mut *guard;
                visitor.visit(guard_value)
            }
        }
        unsafe impl<$target_type: $crate::NullTrace> $crate::NullTrace for $target<$target_type> {}
        unsafe impl<$target_type: $crate::GcSafe> $crate::GcSafe for $target<$target_type> {}
    };
}

/// Unsafely implement `GarbageCollected` for the specified type,
/// by converting the specified wrapper type in order to trace the underlying objects.
///
/// In addition to smart pointers, it can be used for newtype structs like `Wrapping` or `NonZero`,
/// as long as the types can properly traced by just reading their underlying value.
/// However, it's slightly easier to invoke if you implement `Deref`,
/// since the expression is inferred based on the target type.
/// However, you will always need to explicitly indicate the dereferencing output type,
/// in order to be eplicit about the intended operation.
///
/// This macro is usually only useful for unsafe wrappers like `NonZero` and smart pointers like `Rc` and `Box` who use raw pointers internally,
/// since raw pointers can't be automatically traced without additional type information.
/// Otherwise, it's best to use an automatically derived implementation since that's always safe.
/// However, using this macro is always better than a manual implementation, since it makes your intent clearer.
///
/// ## Usage
/// ````
/// // Easy to use for wrappers that `Deref` to their type parameter
/// unsafe_trace_deref!(Box, target = T);
/// unsafe_trace_deref!(Rc, target = T);
/// unsafe_trace_deref!(Arc, target = T);
/// // The `Deref::Output` type can be declared separately from the type paremters
/// unsafe_trace_deref!(Vec, target = { [T] }, T);
/// /*
///  * You can use an arbitrary expression to acquire the underlying value,
///  * and the type doesn't need to implement `Deref` at all
///  */
/// unsafe_trace_deref!(Cell, T; |cell| cell.get());
/// unsafe_trace_deref!(Wrapping, T; |wrapping| &wrapping.0);
/// unsafe_trace_deref!(NonZero, T; |nonzero| &nonzero.get());
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
/// This delegates to `unsafe_gc_brand` to provide the [GcBrand] implementation,
/// so that could also trigger undefined behavior.
#[macro_export]
macro_rules! unsafe_trace_deref {
    ($target:ident, target = $target_type:ident) => {
        unsafe_trace_deref!($target, target = $target_type; $target_type);
    };
    ($target:ident, target = $target_type:ident; $($param:ident),*) => {
        unsafe_trace_deref!($target, target = { $target_type }; $($param),*);
    };
    ($target:ident, target = { $target_type:ty }; $($param:ident),*) => {
        unsafe impl<$($param: TraceImmutable),*> $crate::TraceImmutable for $target<$($param),*> {
            #[inline]
            fn visit_immutable<V: GcVisitor>(&self, visitor: &mut V) -> Result<(), V::Err> {
                let extracted: &$target_type = &**self;
                visitor.visit_immutable(extracted)
            }
        }
        unsafe_trace_deref!($target, $($param),*; immut = false; |value| {
            // I wish we had type ascription -_-
            let dereferenced: &mut $target_type = &mut **value;
            dereferenced
        });
    };
    ($target:ident, $($param:ident),*; immut = required; |$value:ident| $extract:expr) => {
        unsafe_gc_brand!($target, immut = required; $($param),*);
        unsafe impl<$($param),*> Trace for $target<$($param),*>
            where $($param: TraceImmutable),* {

            const NEEDS_TRACE: bool = $($param::NEEDS_TRACE || )* false;
            #[inline]
            fn visit<V: GcVisitor>(&mut self, visitor: &mut V) -> Result<(), V::Err> {
                let extracted = {
                    let $value = self;
                    $extract
                };
                visitor.visit_immutable(extracted)
            }
        }
        unsafe impl<$($param),*> TraceImmutable for $target<$($param),*>
            where $($param: TraceImmutable),* {

            #[inline]
            fn visit_immutable<V: GcVisitor>(&self, visitor: &mut V) -> Result<(), V::Err> {
                let extracted = {
                    let $value = self;
                    $extract
                };
                visitor.visit_immutable(extracted)
            }
        }
        unsafe impl<$($param: NullTrace),*> NullTrace for $target<$($param),*> {}
        /// We trust ourselves to not do anything bad as long as our paramaters don't
        unsafe impl<$($param),*> GcSafe for $target<$($param),*>
            where $($param: GcSafe + TraceImmutable),*  {
            const NEEDS_DROP: bool = std::mem::needs_drop::<Self>();
        }
    };
    ($target:ident, $($param:ident),*; immut = false; |$value:ident| $extract:expr) => {
        unsafe_gc_brand!($target, $($param),*);
        unsafe impl<$($param),*> Trace for $target<$($param),*>
            where $($param: Trace),* {

            const NEEDS_TRACE: bool = $($param::NEEDS_TRACE || )* false;
            #[inline]
            fn visit<V: GcVisitor>(&mut self, visitor: &mut V) -> Result<(), V::Err> {
                let extracted = {
                    let $value = self;
                    $extract
                };
                visitor.visit(extracted)
            }
        }
        unsafe impl<$($param: NullTrace),*> NullTrace for $target<$($param),*> {}
        /// We trust ourselves to not do anything bad as long as our paramaters don't
        unsafe impl<$($param),*> GcSafe for $target<$($param),*>
            where $($param: GcSafe),*  {
            const NEEDS_DROP: bool = std::mem::needs_drop::<Self>();
        }
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
/// ````
/// unsafe_trace_iterable!(Vec, element = T);
/// unsafe_trace_iterable!(HashMap, element = { (&K, &V) }; K, V);
/// unsafe_trace_iterable!(HashSet, element = T);
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
/// This delegates to `unsafe_gc_brand!` to provide the [GcBrand] implementation,
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
/// This delegates to `unsafe_gc_brand!` to provide the [GcBrand] implementation,
/// but that will never cause undefined behavior unless you
/// already have garbage collected pointers inside
/// (which are already undefined behavior for tracing).
#[macro_export]
macro_rules! unsafe_trace_primitive {
    ($target:ty) => {
        unsafe_gc_brand!($target);
        unsafe impl Trace for $target {
            const NEEDS_TRACE: bool = false;
            #[inline(always)] // This method does nothing and is always a win to inline
            fn visit<V: $crate::GcVisitor>(&mut self, _visitor: &mut V) -> Result<(), V::Err> {
                Ok(())
            }
        }
        unsafe impl $crate::TraceImmutable for $target {
            #[inline(always)]
            fn visit_immutable<V: $crate::GcVisitor>(&self, _visitor: &mut V) -> Result<(), V::Err> {
                Ok(())
            }
        }
        unsafe impl $crate::NullTrace for $target {}
        /// No drop/custom behavior -> GcSafe
        unsafe impl GcSafe for $target {
            const NEEDS_DROP: bool = std::mem::needs_drop::<$target>();
        }
        unsafe impl<'gc, OwningRef> $crate::GcDirectBarrier<'gc, OwningRef> for $target {
            #[inline(always)]
            unsafe fn write_barrier(
                &self, _owner: &OwningRef, _field_offset: usize,
            ) {
                /* NOP */
            }
        }
    };
}


/// Unsafely assume that the generic implementation of [GcBrand] is valid,
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
/// which are all properly bounded and required to implement [GcBrand] correctly.
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
macro_rules! unsafe_gc_brand {
    ($target:tt) => {
        unsafe impl<'new_gc, S: $crate::GcSystem> $crate::GcBrand<'new_gc, S> for $target {
            type Branded = Self;
        }
    };
    ($target:ident, $($param:ident),+) => {
        unsafe impl<'new_gc, S, $($param),*> $crate::GcBrand<'new_gc, S> for $target<$($param),*>
            where S: crate::GcSystem, $($param: $crate::GcBrand<'new_gc, S>,)*
                  $(<$param as $crate::GcBrand<'new_gc, S>>::Branded: Trace,)* {
            type Branded = $target<$(<$param as $crate::GcBrand<'new_gc, S>>::Branded),*>;
        }
    };
    ($target:tt, immut = required; $($param:ident),+) => {
        unsafe impl<'new_gc, S, $($param),*> $crate::GcBrand<'new_gc, S> for $target<$($param),*>
            where S: crate::GcSystem, $($param: $crate::GcBrand<'new_gc, S> + TraceImmutable,)*
                  $(<$param as $crate::GcBrand<'new_gc, S>>::Branded: TraceImmutable,)* {
            type Branded = $target<$(<$param as $crate::GcBrand<'new_gc, S>>::Branded),*>;
        }
    }
}

mod core;
mod stdlib;
#[cfg(feature = "indexmap")]
mod indexmap;
