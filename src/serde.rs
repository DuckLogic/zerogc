//! Support for deserializing garbage collected types
//!
//! As long as you aren't worried about cycles, serialization is easy.
//! Just do `#[derive(Serialize)]` on your type.
//!
//! Deserialization is much harder, because allocating a [Gc] requires
//! access to [GcSimpleAlloc] and [serde::de::DeserializeSeed] can't be automatically derived.
//!
//! As a workaround, zerogc introduces a `GcDeserialize` type,
//! indicating an implementation of [serde::Deserialize]
//! that requires a [GcContext].
use std::marker::PhantomData;

use serde::de::{Deserializer, DeserializeSeed};

use crate::prelude::*;

#[doc(hidden)]
#[macro_use]
pub mod hack;

/// An implementation of [serde::Deserialize] that requires a [GcContext] for allocation.
///
/// The type must be [GcSafe], so that it can actually be allocated.
pub trait GcDeserialize<'gc, 'de, Id: CollectorId>: GcSafe<'gc, Id> + Sized {
    /// Deserialize the value given the specified context
    fn deserialize_gc<D: Deserializer<'de>>(ctx: &'gc <Id::System as GcSystem>::Context, deserializer: D) -> Result<Self, D::Error>;
}

impl<'gc, 'de, Id: CollectorId, T: GcDeserialize<'gc, 'de, Id>> GcDeserialize<'gc, 'de, Id> for Gc<'gc, T, Id>
    where <Id::System as GcSystem>::Context: GcSimpleAlloc {
    #[inline]
    fn deserialize_gc<D: Deserializer<'de>>(ctx: &'gc <Id::System as GcSystem>::Context, deserializer: D) -> Result<Self, D::Error> {
        Ok(ctx.alloc(T::deserialize_gc(ctx, deserializer)?))
    }
}

/// Implement [GcDeserialize] for a type by delegating to its [serde::Deserialize] implementation.
///
/// This should only be used for types that can never have gc pointers inside of them (or if you don't care to support that).
#[macro_export]
macro_rules! impl_delegating_deserialize {
    (impl $(<$($lt:lifetime,)* $($param:ident),*>)? GcDeserialize<$gc:lifetime, $de:lifetime, $id:ident> for $target:path $(where $($where_clause:tt)*)?) => {
        impl$(<$($lt,)* $($param),*>)? $crate::serde::GcDeserialize<$gc, $de, $id> for $target
            where Self: Deserialize<'deserialize> + $(, $($where_clause)*)?{
            fn deserialize_gc(_ctx: <Id::System as GcSystem>::Context, deserializer: D) -> Result<Self, D::Error> {
                <Self as serde::Deserialize<'deserialize>>::deserialize(deserializer)
            }
        }
    };
}


/// An implementation of [serde::de::DeserializeSeed] that wraps [GcDeserialize]
pub struct GcDeserializeSeed<'gc, 'de, Id: CollectorId, T: GcDeserialize<'gc, 'de, Id>> {
    context: &'gc <Id::System as GcSystem>::Context,
    marker: PhantomData<fn(&'de ()) -> T>
}
impl<'de, 'gc, Id: CollectorId, T: GcDeserialize<'gc, 'de, Id>> GcDeserializeSeed<'gc, 'de, Id, T> {
    /// Create a new wrapper for the specified context
    #[inline]
    pub fn new(context: &'gc <Id::System as GcSystem>::Context) -> Self {
        GcDeserializeSeed { context, marker: PhantomData }
    }
}
impl<'de, 'gc, Id: CollectorId, T: GcDeserialize<'gc, 'de, Id>> DeserializeSeed<'de> for GcDeserializeSeed<'gc, 'de, Id, T> {
    type Value = T;

    fn deserialize<D>(self, deserializer: D) -> Result<Self::Value, D::Error> where D: Deserializer<'de> {
        T::deserialize_gc(self.context, deserializer)
    }
}
