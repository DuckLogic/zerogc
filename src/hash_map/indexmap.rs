//! Contains the implementation of [GcIndexMap]

use core::mem;
use core::hash::{Hash, Hasher, BuildHasher};
use core::borrow::Borrow;

use hashbrown::raw::RawTable;

use zerogc_derive::{NullTrace, Trace, unsafe_gc_impl};

use crate::SimpleAllocCollectorId;
use crate::prelude::*;

/// A garbage collected hashmap that preserves insertion order.
///
/// This is based off [indexmap::IndexMap](https://docs.rs/indexmap/1.7.0/indexmap/map/struct.IndexMap.html),
/// both in API design and in implementation.
///
/// Like a [GcVec], there can only be one owner at a time,
/// simplifying mutability checking.
pub struct GcIndexMap<'gc, K: GcSafe<'gc, Id>, V: GcSafe<'gc, Id>, Id: SimpleAllocCollectorId, S: BuildHasher = super::DefaultHasher> {
    /// indices mapping from the entry hash to its index
    ///
    /// NOTE: This uses `std::alloc` instead of the garbage collector to allocate memory.
    /// This is necessary because of the possibility of relocating pointers.....
    ///
    /// The unfortunate downside is that allocating from `std::alloc` is slightly
    /// slower than allocating from a typical gc (which often uses bump-pointer allocation).
    indices: RawTable<usize>,
    /// an ordered, garbage collected vector of entries,
    /// in the original insertion order.
    entries: GcVec<'gc, Bucket<K, V>, Id>,
    /// The hasher used to hash elements
    hasher: S
}
unsafe impl<'gc, K: GcSafe<'gc, Id>, V: GcSafe<'gc, Id>, Id: SimpleAllocCollectorId, S: BuildHasher>
    crate::ImplicitWriteBarrier for GcIndexMap<'gc, K, V, Id, S> {}
impl<'gc, K: GcSafe<'gc, Id>, V: GcSafe<'gc, Id>, Id: SimpleAllocCollectorId, S: BuildHasher> GcIndexMap<'gc, K, V, Id, S> {
    /// Return the number of entries in the map
    #[inline]
    pub fn len(&self) -> usize {
        self.entries.len()
    }
    /// Check if the map is empty.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
    /// Return a reference to the value associated with the specified key,
    /// or `None` if it isn't present in the map.
    pub fn get<Q: ?Sized>(&self, key: &Q) -> Option<&V>
        where K: Borrow<Q>, Q: Hash + Eq {
        self.get_index_of(key).map(|index| {
            &self.entries[index].value
        })
    }
    /// Return a mutable reference to the value associated with the specified key,
    /// or `None` if it isn't present in the map.
    pub fn get_mut<Q: ?Sized>(&mut self, key: &Q) -> Option<&mut V>
        where K: Borrow<Q>, Q: Hash + Eq {
        self.get_index_of(key).map(move |index| {
            &mut self.entries[index].value
        })
    }
    /// Remove the value associated with the specified key.
    ///
    /// This does **not** preserve ordering.
    /// It is equivalent to [Vec::swap_remove],
    /// or more specifically [IndexMap::swap_remove](https://docs.rs/indexmap/1.7.0/indexmap/map/struct.IndexMap.html#method.swap_remove).
    pub fn swap_remove<Q: ?Sized>(&mut self, key: &Q) -> Option<V>
        where K: Borrow<Q>, Q: Hash + Eq {
        let hash = self.hash(key);
        self.indices.remove_entry(hash.get(), equivalent(key, &self.entries)).map(|index| {
            let entry = self.entries.swap_remove(index);
            /*hash_builder
             * correct the index that points to the moved entry
             * It was at 'self.len()', now it's at
             */
            if let Some(entry) = self.entries.get(index) {
                let last = self.entries.len();
                *self.indices.get_mut(entry.hash.get(), move |&i| i == last)
                    .expect("index not found") = index;
            }
            entry.value
        })
    }
    /// Returns
    /// Insert a key value pair into the map, returning the previous value (if any(
    ///
    /// If the key already exists, this replaces the existing pair
    /// and returns the previous value.
    #[inline]
    pub fn insert(&mut self, key: K, value: V) -> Option<V> where K: Hash + Eq {
        self.insert_full(key, value).1
    }
    /// Insert a key value pair into the map, implicitly looking up its index.
    ///
    /// If the key already exists, this replaces the existing pair
    /// and returns the previous value.
    ///
    /// If the key doesn't already exist,
    /// this returns a new entry.
    pub fn insert_full(&mut self, key: K, value: V) -> (usize, Option<V>)
        where K: Hash + Eq {
        let hash = self.hash(&key);
        match self.indices.get(hash.get(), equivalent(&key, &*self.entries)) {
            Some(&i) => (i, Some(mem::replace(&mut self.entries[i].value, value))),
            None => (self.push(hash, key, value), None)
        }
    }
    /// Return the index of the item with the specified key.
    pub fn get_index_of<Q: ?Sized>(&self, key: &Q) -> Option<usize>
        where Q: Hash + Eq, K: Borrow<Q> {
        if self.is_empty() {
            None
        } else {
            let hash = self.hash(key);
            self.indices.get(hash.get(), equivalent(key, &*self.entries)).copied()
        }
    }
    fn hash<Q: ?Sized + Hash>(&self, value: &Q) -> HashValue {
        let mut h = self.hasher.build_hasher();
        value.hash(&mut h);
        HashValue(h.finish() as usize)
    }
    /// Append a new key-value pair, *without* checking whether it already exists.
    ///
    /// Return the pair's new index
    fn push(&mut self, hash: HashValue, key: K, value: V) -> usize {
        let i = self.entries.len();
        self.indices.insert(hash.get(), i, get_hash(&self.entries));
        self.entries.push(Bucket { key, value, hash });
        i
    }
    /// Iterate over the entries in the map (in order)
    #[inline]
    pub fn iter(&self) -> Iter<'_, K, V> {
        Iter(self.entries.iter())
    }
    /// Mutably iterate over the entries in the map (in order)
    #[inline]
    pub fn iter_mut(&mut self) -> IterMut<'_, K, V> {
        IterMut(self.entries.iter_mut())
    }
    /// Iterate over tke keys in the map (in order)
    #[inline]
    pub fn keys(&self) -> Keys<'_, K, V> {
        Keys(self.entries.iter())
    }
    /// Iterate over the values in the map (in order)
    #[inline]
    pub fn values(&self) -> Values<'_, K, V> {
        Values(self.entries.iter())
    }
    /// Mutably iterate over the values in the map (in order)
    #[inline]
    pub fn values_mut(&mut self) -> ValuesMut<'_, K, V> {
        ValuesMut(self.entries.iter_mut())
    }
}
macro_rules! define_iterator {
    (struct $name:ident {
        const NAME = $item_name:literal;
        type Item = $item:ty;
        type Wrapped = $wrapped:ident;
        map => |$bucket:ident| $map:expr
    }) => {
        #[doc = concat!("An iterator over the ", $item_name, " of a [GcIndexMap]")]
        pub struct $name<'a, K: 'a, V: 'a>(core::slice::$wrapped<'a, Bucket<K, V>>);
        impl<'a, K: 'a, V: 'a> Iterator for $name<'a, K, V> {
            type Item = (&'a K, &'a V);
            #[inline]
            fn next(&mut self) -> Option<Self::Item> {
                self.0.next().map(|$bucket| (&$bucket.key, &$bucket.value))
            }
            #[inline]
            fn size_hint(&self) -> (usize, Option<usize>) {
                self.0.size_hint()
            }
        }
        impl<'a, K, V> DoubleEndedIterator for $name<'a, K, V> {
            #[inline]
            fn next_back(&mut self) -> Option<Self::Item> {
                self.0.next_back().map(|$bucket| (&$bucket.key, &$bucket.value))
            }
        }
        impl<'a, K, V> core::iter::ExactSizeIterator for $name<'a, K, V> {}
        impl<'a, K, V> core::iter::FusedIterator for $name<'a, K, V> {}
    };
}

define_iterator!(struct Iter {
    const NAME = "entries";
    type Item = (&'a K, &'a V);
    type Wrapped = Iter;
    map => |bucket| (&bucket.key, &bucket.value)
});
define_iterator!(struct Keys {
    const NAME = "keys";
    type Item = &'a K;
    type Wrapped = Iter;
    map => |bucket| &bucket.key
});
define_iterator!(struct Values {
    const NAME = "valuesj";
    type Item = &'a V;
    type Wrapped = Iter;
    map => |bucket| &bucket.value
});
define_iterator!(struct ValuesMut {
    const NAME = "mutable values";
    type Item = &'a mut V;
    type Wrapped = IterMut;
    map => |bucket| &mut bucket.value
});
define_iterator!(struct IterMut {
    const NAME = "mutable entries";
    type Item = (&'a K, &'a mut V);
    type Wrapped = IterMut;
    map => |bucket| (&bucket.key, &mut bucket.value)
});

unsafe_gc_impl!(
    target => GcIndexMap<'gc, K, V, Id, S>,
    params => ['gc, K: GcSafe<'gc, Id>, V: GcSafe<'gc, Id>, Id: SimpleAllocCollectorId, S: BuildHasher],
    bounds => {
        GcSafe => { where S: 'static },
        Trace => { where S: 'static },
        TraceImmutable => never,
        TrustedDrop => { where K: TrustedDrop, V: TrustedDrop, S: 'static },
        GcRebrand => {
            where K: GcRebrand<'new_gc, Id>, V: GcRebrand<'new_gc, Id>, S: 'static, K::Branded: Sized, V::Branded: Sized }
    },
    branded_type => GcIndexMap<'new_gc, K::Branded, V::Branded, Id, S>,
    NEEDS_TRACE => true,
    NEEDS_DROP => core::mem::needs_drop::<Self>(),
    null_trace => never,
    trace_template => |self, visitor| {
        for entry in self.entries.#iter() {
            visitor.#trace_func(entry)?;
        }
        Ok(())
    },
    collector_id => Id
);

#[inline]
fn equivalent<'a, K, V, Q: ?Sized>(
    key: &'a Q, entries: &'a [Bucket<K, V>]
) -> impl Fn(&usize) -> bool + 'a where Q: Hash + Eq, K: Borrow<Q> {
    move |&other_index| entries[other_index].key.borrow() == key
}

#[inline]
fn get_hash<K, V>(entries: &[Bucket<K, V>]) -> impl Fn(&usize) -> u64 + '_ {
    move |&i| entries[i].hash.get()
}

#[derive(Copy, Clone, Debug, PartialEq, NullTrace)]
struct HashValue(usize);
impl HashValue {
    #[inline(always)]
    fn get(self) -> u64 {
        self.0 as u64
    }
}

#[derive(Copy, Clone, Debug, Trace)]
#[zerogc(unsafe_skip_drop)]
struct Bucket<K, V> {
    hash: HashValue,
    key: K,
    value: V
}