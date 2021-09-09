/// Implement [Trace] for a dynamically dispatched trait object
///
/// This requires that the trait object extends [DynTrace].
///
/// ## Example
/// ```
/// # use zerogc::{Trace, DynTrace, trait_object_trace};
/// # use zerogc_derive::Trace;
/// # use zerogc::epsilon::{self, EpsilonCollectorId, Gc};
/// # type OurSpecificId = EpsilonCollectorId;
/// trait Foo<'gc>: DynTrace<'gc, OurSpecificId> {
///     fn method(&self) -> i32;
/// }
/// trait_object_trace!(
///     impl<'gc,> Trace for dyn Foo<'gc>;
///     Branded<'new_gc> => (dyn Foo<'new_gc> + 'new_gc),
///     collector_id => OurSpecificId,
///     gc_lifetime => 'gc
/// );
/// fn foo<'gc, T: ?Sized + Trace + Foo<'gc>>(t: &T) -> i32 {
///     assert_eq!(t.method(), 12);
///     t.method() * 2
/// }
/// fn bar<'gc>(gc: Gc<'gc, dyn Foo<'gc> + 'gc>) -> i32 {
///     foo(gc.value())
/// }
/// #[derive(Trace)]
/// # #[zerogc(collector_ids(EpsilonCollectorId))]
/// struct Bar<'gc> {
///     val: Gc<'gc, i32>
/// }
/// impl<'gc> Foo<'gc> for Bar<'gc> {
///     fn method(&self) -> i32 {
///        **self.val
///     }
/// }
/// let val = epsilon::leaked(12);
/// let gc: Gc<'_, Bar<'_>> = epsilon::leaked(Bar { val });
/// assert_eq!(bar(gc as Gc<'_, dyn Foo>), 24);
/// ```
///
/// ## Safety
/// This macro is completely safe.
#[macro_export]
macro_rules! trait_object_trace {
    (impl $(<$($lt:lifetime,)* $($param:ident),*>)? Trace for dyn $target:path $(where $($where_clause:tt)*)?;
        Branded<$branded_lt:lifetime> => $branded:ty,
        collector_id => $collector_id:path,
        gc_lifetime => $gc_lt:lifetime) => {
        unsafe impl$(<$($lt,)* $($param:ident),*>)? $crate::TrustedDrop for (dyn $target + $gc_lt) where Self: $crate::DynTrace<$gc_lt, $collector_id>, $($($where_clause)*)? {}
        unsafe impl$(<$($lt,)* $($param:ident),*>)? $crate::GcSafe<$gc_lt, $collector_id> for (dyn $target + $gc_lt) where Self: $crate::DynTrace<$gc_lt, $collector_id>, $($($where_clause)*)? {}
        unsafe impl$(<$($lt,)* $($param:ident),*>)? $crate::Trace for (dyn $target + $gc_lt) where Self: $crate::DynTrace::<$gc_lt, $collector_id>, $($($where_clause)*)? {
            /*
             * Insufficient compile-time information to know whether we need to be traced.
             *
             * Therefore, we make a conservative estimate
             */
            const NEEDS_TRACE: bool = true;
            // Likewise for `NEEDS_DROP`
            const NEEDS_DROP: bool = true;

            fn trace<V: $crate::GcVisitor>(&mut self, visitor: &mut V) -> Result<(), V::Err> {
                unimplemented!("Unable to use DynTrace outside of a Gc")
            }

            #[inline]
            unsafe fn trace_inside_gc<'actual_gc, V, Id>(gc: &mut $crate::Gc<'actual_gc, Self, Id>, visitor: &mut V) -> Result<(), V::Err>
                where V: $crate::GcVisitor, Id: $crate::CollectorId, Self: $crate::GcSafe<'actual_gc, Id> {
                visitor.trace_trait_object(gc)
            }
        }
        unsafe impl<$branded_lt, $($($lt,)* $($param:ident,)*)?> $crate::GcRebrand<$branded_lt, $collector_id> for dyn $target $(where $($where_clause)*)? {
            type Branded = $branded;
        }
    }
}

/// Implement [Trace] and [TraceImmutable] as a no-op,
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

            #[inline]
            unsafe fn trace_inside_gc<'actual_gc, Visitor, ActualId>(
                gc: &mut $crate::Gc<'actual_gc, Self, ActualId>,
                visitor: &mut Visitor
            ) -> Result<(), Visitor::Err> where Visitor: zerogc::GcVisitor,
                ActualId: zerogc::CollectorId, Self: zerogc::GcSafe<'actual_gc, ActualId> + 'actual_gc {
                visitor.trace_gc(gc)
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