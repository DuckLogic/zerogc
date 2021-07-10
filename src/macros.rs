/// Implement [Trace] for a dynamically dispatched trait object
///
/// This requires that the trait object extends [DynTrace].
///
/// ## Example
/// ```
/// # use zerogc::{Trace, DynTrace, trait_object_trace};
/// # use zerogc_derive::Trace;
/// # use zerogc::dummy_impl::{self, DummyCollectorId, Gc};
/// trait Foo: DynTrace {
///     fn method(&self) -> i32;
/// }
/// trait_object_trace!(impl Trace for dyn Foo);
/// fn foo<T: ?Sized + Trace + Foo>(t: &T) -> i32 {
///     assert_eq!(t.method(), 12);
///     t.method() * 2
/// }
/// fn bar(gc: Gc<'_, dyn Foo>) -> i32 {
///     foo(gc.value())
/// }
/// #[derive(Trace)]
/// # #[zerogc(collector_id(DummyCollectorId))]
/// struct Bar<'gc> {
///     val: Gc<'gc, i32>
/// }
/// impl<'gc> Foo for Bar<'gc> {
///     fn method(&self) -> i32 {
///        **self.val
///     }
/// }
/// let val = dummy_impl::leaked(12);
/// let gc: Gc<'_, Bar<'_>> = dummy_impl::leaked(Bar { val });
/// assert_eq!(bar(gc as Gc<'_, dyn Foo>), 24);
/// ```
///
/// ## Safety
/// This macro is completely safe.
#[macro_export]
macro_rules! trait_object_trace {
    (impl $(<$($lt:lifetime,)* $($param:ident),*>)? Trace for dyn $target:path $(where $($where_clause:tt)*)?;
        Branded<$branded_lt:lifetime> => $branded:ty,
        Erased<$erased_lt:lifetime> => $erased:ty) => {
        unsafe impl$(<$($lt,)* $($param:ident),*>)? $crate::GcSafe for dyn $target where Self: $crate::DynTrace, $($($where_clause)*)? {}
        unsafe impl$(<$($lt,)* $($param:ident),*>)? $crate::Trace for dyn $target where Self: $crate::DynTrace, $($($where_clause)*)? {
            /*
             * Insufficient compile-time information to know whether we need to be traced.
             *
             * Therefore, we make a conservative estimate
             */
            const NEEDS_TRACE: bool = true;
            // Likewise for `NEEDS_DROP`
            const NEEDS_DROP: bool = true;

            fn visit<V: $crate::GcVisitor>(&mut self, visitor: &mut V) -> Result<(), V::Err> {
               unimplemented!("Unable to use DynTrace outside of a Gc")
            }

            #[inline]
            unsafe fn visit_inside_gc<'actual_gc, V, Id>(gc: &mut $crate::Gc<'actual_gc, Self, Id>, visitor: &mut V) -> Result<(), V::Err>
                where V: $crate::GcVisitor, Id: $crate::CollectorId, Self: $crate::GcSafe + 'actual_gc {
                visitor.visit_trait_object(gc)
            }
        }
        unsafe impl<$branded_lt, $($($lt,)* $($param:ident,)*)? ActualId: $crate::CollectorId> $crate::GcRebrand<$branded_lt, ActualId> for dyn $target where Self: $crate::DynTrace, $($($where_clause)*)? {
            type Branded = $branded;
        }
        unsafe impl<$erased_lt, $($($lt,)* $($param:ident,)*)? ActualId: $crate::CollectorId> $crate::GcErase<$erased_lt, ActualId> for dyn $target where Self: $crate::DynTrace, $($($where_clause)*)? {
            type Erased = $erased;
        }

    }
}