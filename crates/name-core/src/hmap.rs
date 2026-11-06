// SPDX-License-Identifier: Apache-2.0

//! The tree's own hash map.
//!
//! The query engine's maps were `hashbrown`'s, and `hashbrown` was the only
//! crate the library tree linked. The maps that were never maps — a vector
//! indexed by a dense id, a sort and a scan, a sorted vector with a binary
//! search — are those shapes now, and the ones that really are maps are this:
//! an open-addressing table with the crate's own `Fx` hasher.
//!
//! It is deliberately small. It is not a general-purpose container: it is the
//! shape a handful of calls need, with the operations they need and none of
//! the ones they do not.
//!
//! `hashbrown` stays in the tree as the differential oracle this is measured
//! against (see the tests at the bottom). A dev-dependency does not enter a
//! binary, and the crate that replaces one is held to it until the oracle is
//! retired.
//!
//! The layout is a parallel pair: one byte of tag per slot, and the entry
//! itself. The tag is the top seven bits of the key's hash, so a probe walks
//! a cache line of tags before it touches a single entry — the same reason
//! `hashbrown` keeps a control array. A tag of zero is a free slot and 255 is
//! a tombstone; the tags in between are the hash's own.

use alloc::vec::Vec;
use core::borrow::Borrow;
use core::hash::{BuildHasher, Hash};

use crate::name_index::Fx;

/// A free slot: the probe chain ends here.
const EMPTY: u8 = 0;
/// A slot that was used and emptied: the chain goes on past it.
const TOMB: u8 = 255;

/// Where a key is: in a slot, or in one that is free.
enum Place {
    /// The slot the key is in.
    Hit(usize),
    /// A slot the key can go into, and whether that slot was a tombstone.
    Free(usize, bool),
}

/// The tree's own hash map: open addressing with linear probing.
///
/// The keys are hashed with the crate's own `Fx` hasher — the query path
/// wants a cheap hash, and `std`'s is SipHash — and the hash is then put
/// through a finalizer, because `Fx`'s multiply leaves the low bits weakly
/// mixed and the bucket index is taken from them.
pub struct Map<K, V> {
    /// The tag of every slot: free, tombstone, or the key's own.
    tags: Vec<u8>,
    /// The entries, parallel to `tags`.
    slots: Vec<Option<(K, V)>>,
    /// The entries in the map.
    len: usize,
    /// The slots that were emptied and not reused yet.
    dead: usize,
}

impl<K: Hash + Eq, V> Default for Map<K, V> {
    fn default() -> Self {
        Map::new()
    }
}

impl<K: Hash + Eq, V> Map<K, V> {
    /// A map with nothing in it.
    pub fn new() -> Self {
        Map { tags: Vec::new(), slots: Vec::new(), len: 0, dead: 0 }
    }

    /// A map with room for `n` entries.
    pub fn with_capacity(n: usize) -> Self {
        let mut m = Map::new();
        m.reserve(n);
        m
    }

    /// The entries in the map.
    // Part of the map's contract even when a build needs no count of its own:
    // the tests hold this in-house map against `hashbrown` with it.
    #[allow(dead_code)]
    pub fn len(&self) -> usize {
        self.len
    }

    /// The value of `key`, if the map holds one.
    pub fn get<Q>(&self, key: &Q) -> Option<&V>
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        self.find(key).map(|at| match &self.slots[at] {
            Some((_, v)) => v,
            None => unreachable!("a found slot holds an entry"),
        })
    }

    /// Put `value` under `key`, and hand back what was there.
    pub fn insert(&mut self, key: K, value: V) -> Option<V> {
        self.reserve(1);
        let hash = hash_of(&key);
        match self.place(hash, &key) {
            Place::Hit(at) => {
                let old = self.slots[at].take();
                self.slots[at] = Some((key, value));
                old.map(|(_, v)| v)
            }
            Place::Free(at, tomb) => {
                // The slot takes an entry either way; a tombstone that is
                // reused is one tombstone fewer.
                self.len += 1;
                if tomb {
                    self.dead -= 1;
                }
                self.tags[at] = tag_of(hash);
                self.slots[at] = Some((key, value));
                None
            }
        }
    }

    /// The value under `key`, making it if it is not there.
    ///
    /// `make` runs only when the key is absent, and once.
    pub fn get_or_insert_with(&mut self, key: K, make: impl FnOnce() -> V) -> &mut V {
        self.reserve(1);
        let hash = hash_of(&key);
        match self.place(hash, &key) {
            Place::Hit(at) => &mut self.slots[at].as_mut().expect("the slot holds an entry").1,
            Place::Free(at, tomb) => {
                // The slot takes an entry either way; a tombstone that is
                // reused is one tombstone fewer.
                self.len += 1;
                if tomb {
                    self.dead -= 1;
                }
                self.tags[at] = tag_of(hash);
                self.slots[at] = Some((key, make()));
                &mut self.slots[at].as_mut().expect("the slot was just filled").1
            }
        }
    }

    /// Take the value under `key` out of the map.
    pub fn remove<Q>(&mut self, key: &Q) -> Option<V>
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        let at = self.find(key)?;
        let old = self.slots[at].take();
        self.tags[at] = TOMB;
        self.len -= 1;
        self.dead += 1;
        old.map(|(_, v)| v)
    }

    /// The entries of the map, taking the map apart.
    pub fn into_entries(self) -> impl Iterator<Item = (K, V)> {
        self.slots.into_iter().flatten()
    }

    /// The slot `key` is in, or `None` if it is not in the map.
    fn find<Q>(&self, key: &Q) -> Option<usize>
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        if self.tags.is_empty() {
            return None;
        }
        match self.place(hash_of(key), key) {
            Place::Hit(at) => Some(at),
            Place::Free(..) => None,
        }
    }

    /// The slot `key` is in, or the slot it would go in.
    ///
    /// The table must not be empty.
    fn place<Q>(&self, hash: u64, key: &Q) -> Place
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        let tag = tag_of(hash);
        let mask = self.mask();
        let mut at = (hash as usize) & mask;
        let mut tomb = usize::MAX;
        loop {
            let t = self.tags[at];
            if t == EMPTY {
                // The chain ends here, so the key is not in the table. A
                // tombstone earlier on the chain takes the entry instead:
                // the chain must not be broken in the middle.
                return if tomb == usize::MAX { Place::Free(at, false) } else { Place::Free(tomb, true) };
            }
            if t == TOMB {
                if tomb == usize::MAX {
                    tomb = at;
                }
            } else if t == tag
                && let Some((k, _)) = &self.slots[at]
                && <K as Borrow<Q>>::borrow(k) == key
            {
                return Place::Hit(at);
            }
            at = (at + 1) & mask;
        }
    }

    /// Make room for `n` more entries.
    fn reserve(&mut self, n: usize) {
        if self.tags.is_empty() {
            self.alloc(cap_for(self.len + n));
            return;
        }
        if self.len + self.dead + n <= room(self.tags.len()) {
            return;
        }
        // Too full. A table that is mostly tombstones is rebuilt at the same
        // size; one that is full of entries grows.
        let cap = if self.dead > self.len { self.tags.len() } else { self.tags.len() * 2 };
        self.rehash(cap);
        if self.len + n > room(cap) {
            self.rehash(cap * 2);
        }
    }

    /// A table of `cap` free slots.
    fn alloc(&mut self, cap: usize) {
        self.tags.reserve(cap);
        self.slots.reserve(cap);
        while self.tags.len() < cap {
            self.tags.push(EMPTY);
            self.slots.push(None);
        }
    }

    /// Rebuild the table at `cap`, dropping the tombstones.
    fn rehash(&mut self, cap: usize) {
        let old_tags = core::mem::take(&mut self.tags);
        let old_slots = core::mem::take(&mut self.slots);
        self.len = 0;
        self.dead = 0;
        self.alloc(cap);
        for (t, slot) in old_tags.into_iter().zip(old_slots) {
            if t == EMPTY || t == TOMB {
                continue;
            }
            let Some((k, v)) = slot else { continue };
            let hash = hash_of(&k);
            let mask = self.mask();
            let mut at = (hash as usize) & mask;
            while self.tags[at] != EMPTY {
                at = (at + 1) & mask;
            }
            self.tags[at] = tag_of(hash);
            self.slots[at] = Some((k, v));
            self.len += 1;
        }
    }

    /// The mask of the bucket index: the table is a power of two.
    #[inline]
    fn mask(&self) -> usize {
        self.tags.len() - 1
    }
}

/// The hash of a key, put through a finalizer.
///
/// `Fx` is cheap and good enough for the index's own maps, but its multiply
/// leaves the low bits of the hash weakly mixed, and the bucket index is
/// taken from them. The finalizer is the standard one: the high bits into
/// the low ones, twice over, for the price of a couple of multiplies.
#[inline]
fn hash_of<Q: Hash + ?Sized>(key: &Q) -> u64 {
    mix(Fx.hash_one(key))
}

/// The finalizer: the high bits into the low ones, twice over.
#[inline]
fn mix(mut z: u64) -> u64 {
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// The tag of a hash: its top seven bits, never zero and never the tombstone.
#[inline]
fn tag_of(hash: u64) -> u8 {
    ((hash >> 57) as u8) + 1
}

/// The entries a table of `cap` slots holds without growing.
#[inline]
fn room(cap: usize) -> usize {
    cap * 7 / 8
}

/// The table for `n` entries: a power of two with room to spare.
fn cap_for(n: usize) -> usize {
    let mut cap = 8;
    while room(cap) < n {
        cap *= 2;
    }
    cap
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec::Vec;
    use hashbrown::HashMap;

    /// A deterministic generator: the corpus has to be the same on every
    /// run, and the tree links no random number crate.
    struct Rng(u64);

    impl Rng {
        fn next(&mut self) -> u64 {
            let mut x = self.0;
            x ^= x >> 12;
            x ^= x << 25;
            x ^= x >> 27;
            self.0 = x;
            x.wrapping_mul(0x2545_F491_4F6C_DD1D)
        }
    }

    /// The corpus: byte strings out of a small alphabet, so the keys
    /// collide with each other and the table's probing is exercised.
    fn corpus(rng: &mut Rng, n: usize) -> Vec<Vec<u8>> {
        let mut keys = Vec::with_capacity(n);
        for _ in 0..n {
            let len = (rng.next() % 24) as usize;
            let mut k = Vec::with_capacity(len);
            for _ in 0..len {
                k.push((rng.next() % 5) as u8);
            }
            keys.push(k);
        }
        keys
    }

    /// A corpus with no key twice: the table has to hold every one of them
    /// at once, and the lookups have to tell them apart.
    fn distinct_corpus(rng: &mut Rng, n: usize) -> Vec<Vec<u8>> {
        let mut keys = Vec::with_capacity(n);
        for i in 0..n {
            let mut k = Vec::with_capacity(12);
            for _ in 0..8 {
                k.push((rng.next() % 5) as u8);
            }
            k.extend_from_slice(&(i as u32).to_be_bytes());
            keys.push(k);
        }
        keys
    }

    /// The two maps must hold the same entries.
    ///
    /// The oracle is `hashbrown`: the same operations, in the same order, on
    /// the same inputs, with the same answers.
    fn agree<K, V>(mine: &Map<K, V>, theirs: &HashMap<K, V, Fx>)
    where
        K: Hash + Eq + Ord + Clone,
        V: Ord + Clone,
    {
        // The counters must agree with the table they count: the oracle
        // cannot see them, so they are checked here.
        let entries = mine.slots.iter().filter(|s| s.is_some()).count();
        let tombs = mine.tags.iter().filter(|&&t| t == TOMB).count();
        assert_eq!(mine.len, entries, "the length does not match the table");
        assert_eq!(mine.dead, tombs, "the tombstone count does not match the table");
        assert!(agree_silent(mine, theirs), "the maps hold different entries");
    }

    /// The same check, without the panic.
    fn agree_silent<K, V>(mine: &Map<K, V>, theirs: &HashMap<K, V, Fx>) -> bool
    where
        K: Hash + Eq + Ord + Clone,
        V: Ord + Clone,
    {
        if mine.len() != theirs.len() {
            return false;
        }
        let mut a: Vec<(K, V)> = mine.slots.iter().filter_map(|s| s.as_ref().map(|(k, v)| (k.clone(), v.clone()))).collect();
        let mut b: Vec<(K, V)> = theirs.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
        a.sort();
        b.sort();
        a == b
    }

    /// The map is held to the oracle, over a corpus that collides.
    #[test]
    fn the_map_agrees_with_the_oracle() {
        let mut rng = Rng(0x1234_5678_9abc_def0);
        let keys = corpus(&mut rng, 256);
        let mut mine: Map<Vec<u8>, u32> = Map::new();
        let mut theirs: HashMap<Vec<u8>, u32, Fx> = HashMap::with_hasher(Fx);
        for _ in 0..4000 {
            let key = &keys[(rng.next() as usize) % keys.len()];
            match rng.next() % 10 {
                0..=4 => {
                    let v = rng.next() as u32;
                    assert_eq!(mine.insert(key.clone(), v), theirs.insert(key.clone(), v), "insert");
                }
                5..=7 => {
                    assert_eq!(mine.get(key.as_slice()), theirs.get(key.as_slice()), "get");
                }
                8 => {
                    assert_eq!(mine.remove(key.as_slice()), theirs.remove(key.as_slice()), "remove");
                }
                _ => {
                    let a = mine.get_or_insert_with(key.clone(), || 7);
                    let b = theirs.entry(key.clone()).or_insert_with(|| 7);
                    assert_eq!(a, b, "get_or_insert_with");
                }
            }
            agree(&mine, &theirs);
        }
    }

    /// The same, over a corpus big enough to make the table grow and be
    /// rebuilt — the rehash is where a table loses entries.
    #[test]
    fn the_map_agrees_with_the_oracle_while_it_grows() {
        let mut rng = Rng(0x0fed_cba9_8765_4321);
        let keys = distinct_corpus(&mut rng, 20_000);
        let mut mine: Map<Vec<u8>, u32> = Map::new();
        let mut theirs: HashMap<Vec<u8>, u32, Fx> = HashMap::with_hasher(Fx);
        for (i, key) in keys.iter().enumerate() {
            mine.insert(key.clone(), i as u32);
            theirs.insert(key.clone(), i as u32);
            if i % 512 == 0 {
                agree(&mine, &theirs);
            }
        }
        agree(&mine, &theirs);
        // And every key is still findable after the table has been rebuilt
        // several times.
        for (i, key) in keys.iter().enumerate() {
            assert_eq!(mine.get(key.as_slice()), Some(&(i as u32)));
            assert_eq!(theirs.get(key.as_slice()), Some(&(i as u32)));
        }
    }

    /// The same, with the entries taken out again: the tombstones are what
    /// a removal leaves behind, and the chain past one must not end.
    #[test]
    fn the_map_agrees_with_the_oracle_after_removals() {
        let mut rng = Rng(0xdead_beef_cafe_f00d);
        let keys = corpus(&mut rng, 512);
        let mut mine: Map<Vec<u8>, u32> = Map::new();
        let mut theirs: HashMap<Vec<u8>, u32, Fx> = HashMap::with_hasher(Fx);
        for (i, key) in keys.iter().enumerate() {
            mine.insert(key.clone(), i as u32);
            theirs.insert(key.clone(), i as u32);
        }
        for key in keys.iter().step_by(3) {
            assert_eq!(mine.remove(key.as_slice()), theirs.remove(key.as_slice()));
            agree(&mine, &theirs);
        }
        // The keys that are still there are still there, and the ones that
        // are gone are gone.
        for (i, key) in keys.iter().enumerate() {
            assert_eq!(mine.get(key.as_slice()), theirs.get(key.as_slice()), "key {i}");
        }
        // And the table still works after the holes.
        for (i, key) in keys.iter().enumerate() {
            mine.insert(key.clone(), i as u32);
            theirs.insert(key.clone(), i as u32);
        }
        agree(&mine, &theirs);
    }

    /// A key that is not there is not there.
    #[test]
    fn a_key_that_is_not_there_is_not_there() {
        let mut mine: Map<Vec<u8>, u32> = Map::new();
        assert!(mine.get(b"nothing".as_slice()).is_none());
        assert!(mine.remove(b"nothing".as_slice()).is_none());
        assert_eq!(mine.len(), 0);
        mine.insert(b"something".to_vec(), 1);
        assert!(mine.get(b"nothing".as_slice()).is_none());
        assert_eq!(mine.get(b"something".as_slice()), Some(&1));
        assert_eq!(mine.len(), 1);
    }

    /// The comparison has to be able to fail.
    ///
    /// A differential test that cannot tell two maps apart is not a test: it
    /// would pass with a map that answers nothing, and the oracle would be
    /// decoration. So the comparison is run against two maps that differ, and
    /// it has to say so.
    #[test]
    fn the_comparison_notices_a_difference() {
        let mut mine: Map<u32, u32> = Map::new();
        let mut theirs: HashMap<u32, u32, Fx> = HashMap::with_hasher(Fx);
        mine.insert(1, 1);
        theirs.insert(2, 2);
        assert!(!agree_silent(&mine, &theirs), "the comparison must fail on maps that differ");

        let mut mine: Map<u32, u32> = Map::new();
        let mut theirs: HashMap<u32, u32, Fx> = HashMap::with_hasher(Fx);
        mine.insert(1, 1);
        theirs.insert(1, 1);
        assert!(agree_silent(&mine, &theirs), "the comparison must pass on maps that agree");
    }
}
