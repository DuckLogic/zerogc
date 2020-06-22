//! Serde support
//!
//! Beware of cyclic object graphs!
//!
//! Deserializes using [serde::de::DeserializeSeed]
use std::marker::PhantomData;

use serde::de::{DeserializeSeed, Deserialize, Deserializer};
use serde::ser::{Serialize, Serializer};

use zerogc::prelude::*;
use crate::{Gc, SimpleCollectorContext};

//
// Implementations for Gc
//

pub struct GcDeserializeVisitor<'gc, T> {
    pub context: &'gc SimpleCollectorContext,
    pub marker: PhantomData<fn() -> T>
}
impl<'gc, T> From<&'gc SimpleCollectorContext> for GcDeserializeVisitor<'gc, T> {
    #[inline]
    fn from(context: &'gc SimpleCollectorContext) -> Self {
        GcDeserializeVisitor { context, marker: PhantomData }
    }
}
impl<'gc, 'de, T> DeserializeSeed<'de> for GcDeserializeVisitor<'gc, T>
    where T: GcSafe + 'gc, T: Deserialize<'de> {
    type Value = Gc<'gc, T>;

    fn deserialize<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
        where D: Deserializer<'de> {
        let value = T::deserialize(deserializer)?;
        Ok(self.context.alloc(value))
    }
}

impl<'gc, T> Serialize for Gc<'gc, T>
    where T: GcSafe + Serialize + 'gc {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error> where
        S: Serializer {
        self.value().serialize(serializer)
    }
}
