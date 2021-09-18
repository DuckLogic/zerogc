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
use std::collections::{HashMap, HashSet};
use std::marker::PhantomData;
use std::hash::{BuildHasher, Hash};

use serde::Serialize;
use serde::de::{self, Deserializer, Visitor, DeserializeSeed, MapAccess, SeqAccess};
use serde::ser::SerializeSeq;

#[cfg(feature = "indexmap")]
use indexmap::{IndexMap, IndexSet};

use crate::array::{GcArray, GcString};
use crate::prelude::*;

#[doc(hidden)]
#[macro_use]
pub mod hack;

/// An implementation of [serde::Deserialize] that requires a [GcContext] for allocation.
///
/// The type must be [GcSafe], so that it can actually be allocated.
pub trait GcDeserialize<'gc, 'de, Id: CollectorId>: GcSafe<'gc, Id> + Sized {
    /// Deserialize the value given the specified context
    fn deserialize_gc<D: Deserializer<'de>>(ctx: &'gc Id::Context, deserializer: D) -> Result<Self, D::Error>;
}

impl<'gc, 'de, Id: CollectorId, T: GcDeserialize<'gc, 'de, Id>> GcDeserialize<'gc, 'de, Id> for Gc<'gc, T, Id>
    where Id::Context: GcSimpleAlloc {
    #[inline]
    fn deserialize_gc<D: Deserializer<'de>>(ctx: &'gc Id::Context, deserializer: D) -> Result<Self, D::Error> {
        Ok(ctx.alloc(T::deserialize_gc(ctx, deserializer)?))
    }
}


impl<'gc, 'de, Id: CollectorId, T: GcDeserialize<'gc, 'de, Id>> GcDeserialize<'gc, 'de, Id> for GcArray<'gc, T, Id>
    where Id::Context: GcSimpleAlloc {
    fn deserialize_gc<D: Deserializer<'de>>(ctx: &'gc Id::Context, deserializer: D) -> Result<Self, D::Error> {
        Ok(ctx.alloc_array_from_vec(
            Vec::<T>::deserialize_gc(ctx, deserializer)?
        ))
    }
}


impl<'gc, 'de, Id: CollectorId> GcDeserialize<'gc, 'de, Id> for GcString<'gc, Id>
    where Id::Context: GcSimpleAlloc {
    fn deserialize_gc<D: Deserializer<'de>>(ctx: &'gc Id::Context, deserializer: D) -> Result<Self, D::Error> {
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

impl<'gc, 'de, T, Id: CollectorId> GcDeserialize<'gc, 'de, Id> for PhantomData<T> {
    fn deserialize_gc<D: Deserializer<'de>>(_ctx: &'gc Id::Context, _deserializer: D) -> Result<Self, D::Error> {
        Ok(PhantomData)
    }
}

impl<'gc, 'de, Id: CollectorId> GcDeserialize<'gc, 'de, Id> for () {
    fn deserialize_gc<D: Deserializer<'de>>(_ctx: &'gc Id::Context, deserializer: D) -> Result<Self, D::Error> {

        struct UnitVisitor;
        impl<'de> Visitor<'de> for UnitVisitor {
            type Value = ();
            fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
                formatter.write_str("a unit tuple")
            }
            fn visit_unit<E>(self) -> Result<Self::Value, E> where
                    E: de::Error, {
                Ok(())
            }
        }
        deserializer.deserialize_unit(UnitVisitor)
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
            fn deserialize_gc<D: serde::Deserializer<$de>>(_ctx: &$gc <$id as $crate::CollectorId>::Context, deserializer: D) -> Result<Self, <D as serde::Deserializer<$de>>::Error> {
                <Self as serde::Deserialize<$de>>::deserialize(deserializer)
            }
        }
    };
}


/// An implementation of [serde::de::DeserializeSeed] that wraps [GcDeserialize]
pub struct GcDeserializeSeed<'gc, 'de, Id: CollectorId, T: GcDeserialize<'gc, 'de, Id>> {
    context: &'gc Id::Context,
    marker: PhantomData<fn(&'de ()) -> T>
}
impl<'de, 'gc, Id: CollectorId, T: GcDeserialize<'gc, 'de, Id>> GcDeserializeSeed<'gc, 'de, Id, T> {
    /// Create a new wrapper for the specified context
    #[inline]
    pub fn new(context: &'gc Id::Context) -> Self {
        GcDeserializeSeed { context, marker: PhantomData }
    }
}
impl<'de, 'gc, Id: CollectorId, T: GcDeserialize<'gc, 'de, Id>> DeserializeSeed<'de> for GcDeserializeSeed<'gc, 'de, Id, T> {
    type Value = T;

    fn deserialize<D>(self, deserializer: D) -> Result<Self::Value, D::Error> where D: Deserializer<'de> {
        T::deserialize_gc(self.context, deserializer)
    }
}

macro_rules! impl_for_map {
    ($target:ident <K, V, S> $(where $($bounds:tt)*)?) => {
        impl<'gc, 'de, Id: CollectorId,
            K: Eq + Hash + GcDeserialize<'gc, 'de, Id>,
            V: GcDeserialize<'gc, 'de, Id>,
            S: BuildHasher + Default
        > GcDeserialize<'gc, 'de, Id> for $target<K, V, S> $(where $($bounds)*)* {
            fn deserialize_gc<D: Deserializer<'de>>(ctx: &'gc Id::Context, deserializer: D) -> Result<Self, D::Error> {
                struct MapVisitor<
                    'gc, 'de, Id: CollectorId,
                    K: GcDeserialize<'gc, 'de, Id>,
                    V: GcDeserialize<'gc, 'de, Id>,
                    S: BuildHasher + Default
                > {
                    ctx: &'gc Id::Context,
                    marker: PhantomData<(&'de S, K, V)>
                }
                impl<'gc, 'de, Id: CollectorId,
                    K: Eq + Hash + GcDeserialize<'gc, 'de, Id>,
                    V: GcDeserialize<'gc, 'de, Id>,
                    S: BuildHasher + Default
                > Visitor<'de> for MapVisitor<'gc, 'de, Id, K, V, S> {
                    type Value = $target<K, V, S>;
                    fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                        f.write_str(concat!("a ", stringify!($target)))
                    }
                    #[inline]
                    fn visit_map<A>(self, mut access: A) -> Result<Self::Value, A::Error>
                        where A: MapAccess<'de>, {
                        let mut values = $target::<K, V, S>::with_capacity_and_hasher(
                            access.size_hint().unwrap_or(0).min(1024),
                            S::default()
                        );
                        while let Some((key, value)) = access.next_entry_seed(
                            GcDeserializeSeed::new(self.ctx),
                            GcDeserializeSeed::new(self.ctx)
                        )? {
                            values.insert(key, value);
                        }

                        Ok(values)
                    }
                }
                let visitor: MapVisitor<Id, K, V, S> = MapVisitor { ctx, marker: PhantomData };
                deserializer.deserialize_map(visitor)
            }
        }
    };
}

macro_rules! impl_for_set {
    ($target:ident <T, S> $(where $($bounds:tt)*)?) => {
        impl<'gc, 'de, Id: CollectorId,
            T: Eq + Hash + GcDeserialize<'gc, 'de, Id>,
            S: BuildHasher + Default
        > GcDeserialize<'gc, 'de, Id> for $target<T, S> $(where $($bounds)*)* {
            fn deserialize_gc<D: Deserializer<'de>>(ctx: &'gc Id::Context, deserializer: D) -> Result<Self, D::Error> {
                struct SetVisitor<
                    'gc, 'de, Id: CollectorId,
                    T: GcDeserialize<'gc, 'de, Id>,
                    S: BuildHasher + Default
                > {
                    ctx: &'gc Id::Context,
                    marker: PhantomData<fn(&'de (), S) -> T>
                }
                impl<'gc, 'de, Id: CollectorId,
                    T: Eq + Hash + GcDeserialize<'gc, 'de, Id>,
                    S: BuildHasher + Default
                > Visitor<'de> for SetVisitor<'gc, 'de, Id, T, S> {
                    type Value = $target<T, S>;
                    fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                        f.write_str(concat!("a ", stringify!($target)))
                    }
                    #[inline]
                    fn visit_seq<A>(self, mut access: A) -> Result<Self::Value, A::Error>
                        where A: SeqAccess<'de>, {
                        let mut values = $target::<T, S>::with_capacity_and_hasher(
                            access.size_hint().unwrap_or(0).min(1024),
                            S::default()
                        );
                        while let Some(value) = access.next_element_seed(
                            GcDeserializeSeed::new(self.ctx)
                        )? {
                            values.insert(value);
                        }

                        Ok(values)
                    }
                }
                let visitor: SetVisitor<Id, T, S> = SetVisitor { ctx, marker: PhantomData };
                deserializer.deserialize_seq(visitor)
            }
        }
    };
}


impl_for_map!(HashMap<K, V, S> where K: TraceImmutable, S: 'static);
impl_for_set!(HashSet<T, S> where T: TraceImmutable, S: 'static);
#[cfg(feature = "indexmap")]
impl_for_map!(IndexMap<K, V, S> where K: GcSafe<'gc, Id>, S: 'static);
#[cfg(feature = "indexmap")]
impl_for_set!(IndexSet<T, S> where T: TraceImmutable, S: 'static);

#[cfg(test)]
mod test {
    use crate::epsilon::{EpsilonSystem, EpsilonCollectorId};
    use super::*;
    #[test]
    #[cfg(feature = "indexmap")]
    fn indexmap() {
        let system = EpsilonSystem::leak();
        let ctx = system.new_context();
        const INPUT: &str = r##"{"foo": "bar", "eats": "turds"}"##;
        let mut deser = serde_json::Deserializer::from_str(INPUT);
        let s = |s: &'static str| String::from(s);
        assert_eq!(
            <IndexMap<String, Gc<String, EpsilonCollectorId>> as GcDeserialize<EpsilonCollectorId>>::deserialize_gc(&ctx, &mut deser).unwrap(),
            indexmap::indexmap!(
                s("foo") => ctx.alloc(s("bar")),
                s("eats") => ctx.alloc(s("turds"))
            )
        );
        let mut deser = serde_json::Deserializer::from_str(INPUT);
        assert_eq!(
            <IndexMap<String, Gc<String, EpsilonCollectorId>, fnv::FnvBuildHasher> as GcDeserialize<EpsilonCollectorId>>::deserialize_gc(&ctx, &mut deser).unwrap(),
            indexmap::indexmap!(
                s("foo") => ctx.alloc(s("bar")),
                s("eats") => ctx.alloc(s("turds"))
            )
        );
    }
    #[test]
    fn gc() {
        let system = EpsilonSystem::leak();
        let ctx = system.new_context();
        let mut deser = serde_json::Deserializer::from_str(r#"128"#);
        assert_eq!(
            <Gc<i32, EpsilonCollectorId> as GcDeserialize<EpsilonCollectorId>>::deserialize_gc(&ctx, &mut deser).unwrap(),
            ctx.alloc(128)
        );
    }
}
