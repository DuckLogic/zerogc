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
/// // You can even have a custom expression to acquire a guard's value
/// unsafe_trace_lock!(WeirdLock, T; |guard| guard.value(), |lock| lock.lock());
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
/// This delegates to `unsafe_erase` to provide the `GcErase` implementation,
/// so that could also trigger undefined behavior.
#[macro_export]
macro_rules! unsafe_trace_lock {
    ($target:ident, target = $target_type:ident; |$value:ident| $acquire_lock:expr) => {
        unsafe_trace_lock!(
            $target, target = $target_type; $target_type;
            |$value| $acquire_lock
        );
    };
    ($target:ident, target = $target_type:ident; $($param:ident),*; |$value:ident| $acquire_lock:expr) => {
        unsafe_trace_lock!(
            $target; $($param),*;
            |lock| {
                let value: &$target_type = &lock;
                value
            },
            |$value| $acquire_lock
        );
    };
    ($target:ident; $($param:ident),*; |$guard:ident| $guard_value:expr, |$lock:ident| $acquire_guard:expr) => {
        unsafe_erase!($target, $($param),*);
        unsafe impl<$($param),*> GarbageCollected for $target<$($param),*>
            where $($param: GarbageCollected),* {

            const NEEDS_TRACE: bool = $($param::NEEDS_TRACE || )* false;
            #[inline]
            unsafe fn raw_trace(&self, target: &mut GarbageCollector) {
                let $lock = self;
                #[allow(unused_mut)]
                let mut $guard = $acquire_guard;
                let guard_value = $guard_value;
                target.trace(guard_value);
            }
        }
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
/// This delegates to `unsafe_erase` to provide the `GcErase` implementation,
/// so that could also trigger undefined behavior.
#[macro_export]
macro_rules! unsafe_trace_deref {
    ($target:ident, target = $target_type:ident) => {
        unsafe_trace_deref!($target, target = $target_type; $target_type);
    };
    ($target:ident, target = $target_type:ident; $($param:ident),*) => {
        unsafe_trace_deref!($target, target = { &$target_type }; $($param),*);
    };
    ($target:ident, target = { $target_type:ty }; $($param:ident),*) => {
        unsafe_trace_deref!($target, $($param),*; |value| {
            // I wish we had type ascription -_-
            let dereferenced: $target_type = &**value;
            dereferenced
        });
    };
    ($target:ident, $($param:ident),*; |$value:ident| $extract:expr) => {
        unsafe_erase!($target, $($param),*);
        unsafe impl<$($param),*> GarbageCollected for $target<$($param),*>
            where $($param: GarbageCollected),* {

            const NEEDS_TRACE: bool = $($param::NEEDS_TRACE || )* false;
            #[inline]
            unsafe fn raw_trace(&self, target: &mut GarbageCollector) {
                let extracted = {
                    let $value = self;
                    $extract
                };
                target.trace(extracted)
            }
        }
    };
}


/// Unsafely implement `GarbageCollected` for the specified iterable type,
/// by iterating over the type to trace all objects.
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
/// This delegates to `unsafe_erase` to provide the `GcErase` implementation,
/// so that could also trigger undefined behavior.
#[macro_export]
macro_rules! unsafe_trace_iterable {
    ($target:ident, element = $element_type:ident) => {
        unsafe_trace_iterable!($target, element = &$element_type; $element_type);
    };
    ($target:ident<$($param:ident),*>; element = { $element_type:ty }) => {
        unsafe_erase!($target, $($param),*);
        unsafe impl<$($param),*> GarbageCollected for $target<$($param),*>
            where $($param: GarbageCollected),* {

            const NEEDS_TRACE: bool = $($param::NEEDS_TRACE || )* false;
            #[inline]
            unsafe fn raw_trace(&self, target: &mut GarbageCollector) {
                let iter = IntoIterator::into_iter(self);
                for element in iter {
                    let element: $element_type = element;
                    target.trace(&element)
                }
            }
        }
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
/// This delegates to `unsafe_erase` to provide the `GcErase` implementation,
/// but that will never cause undefined behavior unless you have garbage collected pointers inside
/// (which are already undefined behavior for tracing).
#[macro_export]
macro_rules! unsafe_trace_primitive {
    ($target:ty) => {
        unsafe_erase!($target);
        unsafe impl GarbageCollected for $target {
            const NEEDS_TRACE: bool = false;
            #[inline(always)] // This method does nothing and is always a win to inline
            unsafe fn raw_trace(&self, _collector: &mut GarbageCollector) {}
        }
    };
}

mod core;
mod stdlib;

