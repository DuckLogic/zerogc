//! Miscellaneous macros for `zerogc`

/// Implement [Trace](`crate::Trace`) and [TraceImmutable](`crate::TraceImmutable`) as a no-op,
/// based on the fact that a type implements [NullTrace](crate::NullTrace)
///
/// ## Safety
/// Because this verifies that `Self: NullTrace`, it is known there
/// are no values inside that need to be traced (allowing a no-op trace)
///
/// In other words, the unsafety is delegated to NullTrace
#[macro_export]
macro_rules! impl_trace_for_nulltrace {
    (impl $(<$($lt:lifetime,)* $($param:ident),*>)? Trace for $target:path $(where $($where_clause:tt)*)?) => {
        unsafe impl$(<$($lt,)* $($param),*>)? $crate::Trace for $target $(where $($where_clause)*)? {
            const NEEDS_TRACE: bool = false;
            // Since we don't need a dummy-drop, we can just delegate to std
            const NEEDS_DROP: bool = core::mem::needs_drop::<Self>();

            #[inline]
            fn trace<V: $crate::GcVisitor>(&mut self, _visitor: &mut V) -> Result<(), V::Err> {
                <Self as $crate::NullTrace>::verify_null_trace();
                Ok(())
            }
        }
        unsafe impl$(<$($lt,)* $($param),*>)? $crate::TraceImmutable for $target $(where $($where_clause)*)? {
            #[inline]
            fn trace_immutable<V: $crate::GcVisitor>(&self, _visitor: &mut V) -> Result<(), V::Err> {
                Ok(())
            }
        }
    }
}

/// Implement [NullTrace](`crate::NullTrace`) for a type that lives for `'static`
///
/// ## Safety
/// Because the type is `'static`, it can't possibly
/// have any `Gc` pointers inside it.
///
/// Therefore this macro is safe.
#[macro_export]
macro_rules! impl_nulltrace_for_static {
    ($target:path) => ($crate::impl_nulltrace_for_static!($target, params => []););
    ($target:path, params => [$($param:tt)*] $(where $($where_tk:tt)*)?) => {
        zerogc_derive::unsafe_gc_impl!(
            target => $target,
            params => [$($param)*],
            bounds => {
                GcSafe => { where Self: 'static, $($($where_tk)*)* },
                Trace => { where Self: 'static, $($($where_tk)*)* },
                TraceImmutable => { where Self: 'static, $($($where_tk)*)* },
                GcRebrand => { where Self: 'static, $($($where_tk)*)* },
                TrustedDrop => { where Self: 'static, $($($where_tk)*)* }
            },
            null_trace => { where Self: 'static, $($($where_tk)*)* },
            branded_type => Self,
            NEEDS_TRACE => false,
            NEEDS_DROP => core::mem::needs_drop::<Self>(),
            trace_template => |self, visitor| { Ok(()) },
            collector_id => *,
        );
    };
}
