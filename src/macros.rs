/// Implement [Trace] for a dynamically dispatched trait object
///
/// This requires that the trait object extends [DynTrace].
///
/// ## Example
/// ```
/// # use zerogc::{Trace, DynTrace, trait_object_trace};
/// # use zerogc_derive::Trace;
/// # use zerogc::dummy_impl::{self, DummyCollectorId, Gc};
/// trait Foo<'gc>: DynTrace + 'gc {
///     fn method(&self) -> i32;
/// }
/// trait_object_trace!(impl<'gc,> Trace for dyn Foo<'gc>; Branded<'new_gc> => dyn Foo<'new_gc> + 'new_gc, Erased<'min> => dyn Foo<'min> + 'min);
/// fn foo<'gc, T: ?Sized + Trace + Foo<'gc>>(t: &T) -> i32 {
///     assert_eq!(t.method(), 12);
///     t.method() * 2
/// }
/// fn bar<'gc>(gc: Gc<'gc, dyn Foo<'gc> + 'gc>) -> i32 {
///     foo(gc.value())
/// }
/// #[derive(Trace)]
/// # #[zerogc(collector_id(DummyCollectorId))]
/// struct Bar<'gc> {
///     val: Gc<'gc, i32>
/// }
/// impl<'gc> Foo<'gc> for Bar<'gc> {
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

/// Get the offset of the specified field within a structure
#[macro_export]
macro_rules! field_offset {
    ($target:path, $($field:ident).+) => {{
        const OFFSET: usize = {
            let uninit = core::mem::MaybeUninit::<$target>::uninit();
            unsafe { ((core::ptr::addr_of!((*uninit.as_ptr())$(.$field)*)) as *const u8)
                .offset_from(uninit.as_ptr() as *const u8) as usize }
        };
        OFFSET
    }};
}
