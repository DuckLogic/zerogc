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

use crate::array::{GcArray, GcString};
use serde::{Serialize, de::{self, Deserializer, DeserializeSeed}, ser::SerializeSeq};

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


impl<'gc, 'de, Id: CollectorId, T: GcDeserialize<'gc, 'de, Id>> GcDeserialize<'gc, 'de, Id> for GcArray<'gc, T, Id>
    where <Id::System as GcSystem>::Context: GcSimpleAlloc {
    fn deserialize_gc<D: Deserializer<'de>>(ctx: &'gc <Id::System as GcSystem>::Context, deserializer: D) -> Result<Self, D::Error> {
        Ok(ctx.alloc_array_from_vec(
            Vec::<T>::deserialize_gc(ctx, deserializer)?
        ))
    }
}


impl<'gc, 'de, Id: CollectorId> GcDeserialize<'gc, 'de, Id> for GcString<'gc, Id>
    where <Id::System as GcSystem>::Context: GcSimpleAlloc {
    fn deserialize_gc<D: Deserializer<'de>>(ctx: &'gc <Id::System as GcSystem>::Context, deserializer: D) -> Result<Self, D::Error> {
        struct GcStrVisitor<'gc, A: GcSimpleAlloc> {
            ctx: &'gc A
        }
        impl<'de, 'gc, A: GcSimpleAlloc> de::Visitor<'de> for GcStrVisitor<'gc, A> {
            type Value = GcString<'gc, A::Id>;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("a string")
            }
            fn visit_str<E>(self, v: &str) -> Result<Self::Value, E> where E: de::Error, {
                Ok(self.ctx.alloc_str(v))
            }
        }
        deserializer.deserialize_str(GcStrVisitor { ctx })
    }
}

impl<'gc, T: Serialize, Id: CollectorId> Serialize for Gc<'gc, T, Id> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>  where
        S: serde::Serializer {
        self.value().serialize(serializer)
    }
}


impl<'gc, T: Serialize, Id: CollectorId> Serialize for GcArray<'gc, T, Id> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>  where
        S: serde::Serializer {
        let mut seq = serializer.serialize_seq(Some(self.len()))?;
        for val in self.as_slice().iter() {
            seq.serialize_element(val)?;
        }
        seq.end()
    }
}


impl<'gc, Id: CollectorId> Serialize for GcString<'gc, Id> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>  where
        S: serde::Serializer {
        serializer.serialize_str(self.as_str())
    }
}


/// Implement [GcDeserialize] for a type by delegating to its [serde::Deserialize] implementation.
///
/// This should only be used for types that can never have gc pointers inside of them (or if you don't care to support that).
#[macro_export]
macro_rules! impl_delegating_deserialize {
    (impl GcDeserialize for $target:path) => (
        $crate::impl_delegating_deserialize!(impl <'gc, 'de, Id> GcDeserialize<'gc, 'de, Id> for $target where Id: zerogc::CollectorId);
    );
    (impl $(<$($lt:lifetime,)* $($param:ident),*>)? GcDeserialize<$gc:lifetime, $de:lifetime, $id:ident> for $target:path $(where $($where_clause:tt)*)?) => {
        impl$(<$($lt,)* $($param),*>)? $crate::serde::GcDeserialize<$gc, $de, $id> for $target
            where Self: Deserialize<$de> + $(, $($where_clause)*)?{
            fn deserialize_gc<D: serde::Deserializer<$de>>(_ctx: <<$id as $crate::CollectorId>::System as $crate::GcSystem>::Context, deserializer: D) -> Result<Self, <D as serde::Deserializer<$de>>::Error> {
                <Self as serde::Deserialize<$de>>::deserialize(deserializer)
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
