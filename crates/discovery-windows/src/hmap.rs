// SPDX-License-Identifier: Apache-2.0

//! The tree's own map for the file-reference cache.
//!
//! The backend's cache of rebuilt paths was `hashbrown`'s, and `hashbrown`
//! was the only crate the library tree linked. This is the map that
//! replaced it: an open-addressing table over the file reference itself.
//!
//! It is smaller than the general map in `rgy-name-core` on purpose, because
//! the cache is smaller: the keys are file references, so they need no hash
//! trait and no borrowing — and nothing is ever taken out of the cache, so
//! there are no tombstones either.
//!
//! `hashbrown` stays in the tree as the differential oracle this is measured
//! against (see the tests at the bottom). A dev-dependency does not enter a
//! binary, and the crate that replaces one is held to it until the oracle is
//! retired.

use alloc::vec::Vec;

/// A free slot: the probe chain ends here.
const EMPTY: u8 = 0;

/// The cache of the paths of a volume, by file reference.
///
/// The file references are spread over the volume's record space and are not
/// dense, so the table is open addressing over the reference. Nothing is
/// ever taken out of it: a path, once rebuilt, is the path.
pub struct Cache {
    /// The tag of every slot: free, or the key's own.
    tags: Vec<u8>,
    /// The entries, parallel to `tags`.
    slots: Vec<Option<(u64, Vec<u8>)>>,
    /// The entries in the cache.
    len: usize,
}

impl Default for Cache {
    fn default() -> Self {
        Cache::new()
    }
}

impl Cache {
    /// A cache with nothing in it.
    pub fn new() -> Self {
        Cache {
            tags: Vec::new(),
            slots: Vec::new(),
            len: 0,
        }
    }

    /// The path of the entry with this file reference, if the cache has it.
    pub fn get(&self, frn: u64) -> Option<&[u8]> {
        if self.tags.is_empty() {
            return None;
        }
        let hash = hash_of(frn);
        let tag = tag_of(hash);
        let mask = self.mask();
        let mut at = (hash as usize) & mask;
        loop {
            let t = self.tags[at];
            if t == EMPTY {
                return None;
            }
            if t == tag
                && let Some((k, p)) = &self.slots[at]
                && *k == frn
            {
                return Some(p.as_slice());
            }
            at = (at + 1) & mask;
        }
    }

    /// Remember the path of the entry with this file reference.
    pub fn insert(&mut self, frn: u64, path: Vec<u8>) {
        if self.tags.is_empty() {
            self.alloc(cap_for(1));
        }
        let hash = hash_of(frn);
        let tag = tag_of(hash);
        let mask = self.mask();
        let mut at = (hash as usize) & mask;
        loop {
            let t = self.tags[at];
            if t == EMPTY {
                self.tags[at] = tag;
                self.slots[at] = Some((frn, path));
                self.len += 1;
                // The table is at most seven parts in eight full: it grows
                // before the probe has to walk a long chain.
                if self.len * 8 >= self.tags.len() * 7 {
                    self.rehash(self.tags.len() * 2);
                }
                return;
            }
            if t == tag
                && let Some((k, p)) = &mut self.slots[at]
                && *k == frn
            {
                *p = path;
                return;
            }
            at = (at + 1) & mask;
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

    /// Rebuild the table at `cap`.
    fn rehash(&mut self, cap: usize) {
        let old_tags = core::mem::take(&mut self.tags);
        let old_slots = core::mem::take(&mut self.slots);
        self.len = 0;
        self.alloc(cap);
        for (t, slot) in old_tags.into_iter().zip(old_slots) {
            if t == EMPTY {
                continue;
            }
            let Some((k, v)) = slot else { continue };
            let hash = hash_of(k);
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

/// The hash of a file reference.
///
/// The reference is a number, not a name, so there is nothing to hash: the
/// finalizer spreads it, because the multiply of a multiplicative hash
/// leaves the low bits weakly mixed and the bucket index is taken from them.
#[inline]
fn hash_of(frn: u64) -> u64 {
    let mut z = frn;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// The tag of a hash: its top seven bits, never zero.
#[inline]
fn tag_of(hash: u64) -> u8 {
    ((hash >> 57) as u8) + 1
}

/// The table for `n` entries: a power of two with room to spare.
fn cap_for(n: usize) -> usize {
    let mut cap = 8;
    while cap * 7 / 8 < n {
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

    /// The two caches must hold the same entries.
    fn agree(mine: &Cache, theirs: &HashMap<u64, Vec<u8>>) {
        assert_eq!(
            mine.len,
            theirs.len(),
            "the caches hold a different number of entries"
        );
        let mut a: Vec<(u64, Vec<u8>)> = mine.slots.iter().filter_map(|s| s.clone()).collect();
        let mut b: Vec<(u64, Vec<u8>)> = theirs.iter().map(|(k, v)| (*k, v.clone())).collect();
        a.sort();
        b.sort();
        assert_eq!(a, b, "the caches hold different entries");
    }

    /// The cache is held to the oracle, over a corpus that collides.
    #[test]
    fn the_cache_agrees_with_the_oracle() {
        let mut rng = Rng(0x1234_5678_9abc_def0);
        let mut mine = Cache::new();
        let mut theirs: HashMap<u64, Vec<u8>> = HashMap::new();
        for _ in 0..4000 {
            // The references are the volume's own: small numbers, with
            // gaps, and the same one asked for again and again.
            let frn = rng.next() % 512;
            match rng.next() % 4 {
                0 => {
                    let path = alloc::vec![(rng.next() % 256) as u8; (rng.next() % 32) as usize];
                    mine.insert(frn, path.clone());
                    theirs.insert(frn, path);
                }
                1 => {
                    assert_eq!(mine.get(frn), theirs.get(&frn).map(|p| p.as_slice()), "get");
                }
                2 => {
                    // A reference that was never cached is not there.
                    assert_eq!(mine.get(frn + 4096), None);
                }
                _ => {
                    assert_eq!(mine.len, theirs.len());
                }
            }
            agree(&mine, &theirs);
        }
    }

    /// The same, over a corpus big enough to make the table grow and be
    /// rebuilt — the rehash is where a table loses entries.
    #[test]
    fn the_cache_agrees_with_the_oracle_while_it_grows() {
        let mut mine = Cache::new();
        let mut theirs: HashMap<u64, Vec<u8>> = HashMap::new();
        for i in 0..50_000u64 {
            let frn = i * 7 + 3;
            let path = alloc::vec![(i % 251) as u8; 12];
            mine.insert(frn, path.clone());
            theirs.insert(frn, path);
            if i % 1024 == 0 {
                agree(&mine, &theirs);
            }
        }
        agree(&mine, &theirs);
        // And every reference is still findable after the table has been
        // rebuilt several times.
        for i in 0..50_000u64 {
            let frn = i * 7 + 3;
            assert_eq!(mine.get(frn), theirs.get(&frn).map(|p| p.as_slice()));
        }
    }

    /// A reference that is not there is not there.
    #[test]
    fn a_reference_that_is_not_there_is_not_there() {
        let mut mine = Cache::new();
        assert!(mine.get(5).is_none());
        assert_eq!(mine.len, 0);
        mine.insert(5, alloc::vec![b'a', b'b']);
        assert_eq!(mine.get(5), Some(&b"ab"[..]));
        assert!(mine.get(6).is_none());
        assert_eq!(mine.len, 1);
    }
}
