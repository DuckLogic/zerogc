//! Tracing implementations for the standard library
//!
//! Types that are in `libcore` and are `#![no_std]` should go in the core module,
//! but anything that requires the rest of the stdlib (including collections and allocations),
//! should go in this module.
use std::collections::{HashMap, HashSet};
#[cfg(feature = "serde1")]
use std::hash::{Hash, BuildHasher};
#[cfg(feature = "serde1")]
use std::marker::PhantomData;

use zerogc_derive::unsafe_gc_impl;

#[cfg(feature="serde1")]
use crate::serde::GcDeserialize;
use crate::prelude::*;


unsafe_gc_impl! {
    target => HashMap<K, V, S>,
    params => [K: TraceImmutable, V, S: 'static],
    bounds => {
        /*
         * We require S: 'static so that we know S: NullTrace
         */
        Trace => { where K: TraceImmutable, V: Trace, S: 'static },
        TraceImmutable => { where K: TraceImmutable, V: TraceImmutable, S: 'static },
        TrustedDrop => { where K: TrustedDrop, V: TrustedDrop, S: 'static },
        GcSafe => { where K: TraceImmutable + GcSafe<'gc, Id>, V: GcSafe<'gc, Id>, S: 'static },
        GcDeserialize => { where K: GcDeserialize<'gc, 'deserialize, Id> + Eq + Hash,
            V: GcDeserialize<'gc, 'deserialize, Id>, S: Default + BuildHasher }
    },
    null_trace => { where K: NullTrace, V: NullTrace, S: NullTrace },
    NEEDS_TRACE => K::NEEDS_TRACE || V::NEEDS_TRACE,
    NEEDS_DROP => true, // Internal memory
    collector_id => *,
    visit => |self, visitor| {
        for (key, value) in self.#iter() {
            visitor.visit_immutable::<K>(key)?;
            visitor.#visit_func::<V>(value)?;
        }
        // NOTE: Because S: 'static, we can assume S: NullTrace
        Ok(())
    },
    deserialize => |ctx, deserializer| {
        use serde::de::{Visitor, MapAccess};
        use crate::serde::GcDeserializeSeed;
        struct MapVisitor<
            'gc, 'de, Id: CollectorId,
            K: GcDeserialize<'gc, 'de, Id>,
            V: GcDeserialize<'gc, 'de, Id>,
            S: BuildHasher + Default
        > {
            ctx: &'gc <Id::System as GcSystem>::Context,
            marker: PhantomData<fn(&'de (), S) -> (K, V)>
        }
        impl<'gc, 'de, Id: CollectorId,
            K: Eq + Hash + GcDeserialize<'gc, 'de, Id>,
            V: GcDeserialize<'gc, 'de, Id>,
            S: BuildHasher + Default
        > Visitor<'de> for MapVisitor<'gc, 'de, Id, K, V, S> {
            type Value = HashMap<K, V, S>;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("a map")
            }
            #[inline]
            fn visit_map<A>(self, mut access: A) -> Result<Self::Value, A::Error>
                where A: MapAccess<'de>, {
                let mut values = HashMap::<K, V, S>::with_capacity_and_hasher(
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
    },
}


unsafe_gc_impl! {
    target => HashSet<T, S>,
    params => [T: TraceImmutable, S: 'static],
    bounds => {
        /*
         * We require S: 'static so that we know S: NullTrace
         */
        Trace => { where T: TraceImmutable, S: 'static },
        TraceImmutable => { where T: TraceImmutable, S: 'static },
        TrustedDrop => { where T: TrustedDrop, S: 'static },
        GcSafe => { where T: TraceImmutable + GcSafe<'gc, Id>, S: 'static },
        GcDeserialize => { where T: GcDeserialize<'gc, 'deserialize, Id> + Eq + Hash, S: BuildHasher + Default }
    },
    null_trace => { where T: NullTrace, S: 'static },
    NEEDS_TRACE => T::NEEDS_TRACE,
    NEEDS_DROP => true, // Internal memory
    collector_id => *,
    visit => |self, visitor| {
        for val in self.iter() {
            visitor.visit_immutable::<T>(val)?;
        }       
        // NOTE: Because S: 'static, we can assume S: NullTrace
        Ok(())
    },
    deserialize => |ctx, deserializer| {
        use serde::de::{Visitor, SeqAccess};
        use crate::serde::GcDeserializeSeed;
        struct SetVisitor<
            'gc, 'de, Id: CollectorId,
            T: GcDeserialize<'gc, 'de, Id>,
            S: BuildHasher + Default
        > {
            ctx: &'gc <Id::System as GcSystem>::Context,
            marker: PhantomData<fn(&'de (), S) -> T>
        }
        impl<'gc, 'de, Id: CollectorId,
            T: Eq + Hash + GcDeserialize<'gc, 'de, Id>,
            S: BuildHasher + Default
        > Visitor<'de> for SetVisitor<'gc, 'de, Id, T, S> {
            type Value = HashSet<T, S>;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("a set")
            }
            #[inline]
            fn visit_seq<A>(self, mut access: A) -> Result<Self::Value, A::Error>
                where A: SeqAccess<'de>, {
                let mut values = HashSet::<T, S>::with_capacity_and_hasher(
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
    },
}
