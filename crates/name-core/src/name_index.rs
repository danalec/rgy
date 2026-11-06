// SPDX-License-Identifier: Apache-2.0

//! The v2 name index.
//!
//! A name index is one flat byte blob: a 4 KiB header, then twenty-two
//! sections laid out one after another on 64-byte boundaries. The header says
//! how many entries, directories and distinct names there are; every section
//! length is derived from those four numbers, so a reader never trusts a
//! stored offset and a writer never has to store one.
//!
//! The layout below is the on-disk contract (`FSIDX015`). It is written here
//! as data-format facts, not as anybody's code: the magic, the header fields,
//! the section order, the alignment rule and the field packing are what a
//! reader of a blob needs to know. Everything that *computes* the sections —
//! the DFS walk, the name interning, the bitmaps, the priority ladder — is a
//! fresh implementation that produces the same bytes.
//!
//! The crate is `no_std`: the index and every accessor work over bytes the
//! caller owns. `std` buys only the mmap-backed `Blob` variants and the
//! `save`/`load` file plumbing, all behind `#[cfg(feature = "std")]`.

// The section walks are index-based by nature: the indices are the ids the
// format stores. Keep the default clippy lints off this file's loops.
#![allow(clippy::needless_range_loop, clippy::unnecessary_sort_by, clippy::type_complexity, clippy::manual_map)]

use alloc::borrow::Cow;
use alloc::boxed::Box;
use alloc::vec;
use alloc::vec::Vec;
use core::cmp::Reverse;
use core::hash::{BuildHasher, Hasher};
use core::ops::{Deref, DerefMut, Range};

use crate::fold;
use crate::hmap::Map;
use crate::rank::Tier;
use crate::rules::Rules;
use crate::sync::OnceCell;
use rgy_discovery::walk::{KIND_DIR, Listing, NONE, RawEnt};

#[cfg(feature = "std")]
use rgx_core::memmap2::{Mmap, MmapMut};

/// The on-disk magic. Only `FSIDX015` is accepted: the version lineage is
/// `011` (name masks) -> `012` (rank adjustment) -> `013` (priority tier in
/// the kind byte) -> `014` (the root's digest in the header) -> `015`.
const MAGIC: &[u8; 8] = b"FSIDX015";

/// The header is 4 KiB. Section 0 starts exactly here.
pub(crate) const HDR: usize = 4096;

/// A name this short can be matched whole without a pass over every word.
pub const SHORT: usize = 3;
/// Entries per `Zone` aggregate.
pub const ZONE: usize = 1024;
/// Character classes (bits 0..40 of a name mask).
pub const CLASSES: usize = 41;
/// The class for bytes at or above 0x80.
pub const CLASS_NON_ASCII: usize = 39;
/// First-half class bitmaps: `BM_FIRST + c`.
pub const BM_FIRST: usize = 0;
/// Second-half class bitmaps: `BM_SECOND + c`.
pub const BM_SECOND: usize = CLASSES; // 41
/// Word-start bitmaps: `BM_START + h`, `h` in `0..23`.
pub const BM_START: usize = 2 * CLASSES; // 82
/// The name starts with a dot.
pub const BM_DOT: usize = BM_START + 23; // 105
/// The name ends with `.app`.
pub const BM_APP: usize = BM_DOT + 1; // 106
/// Doubled-class bitmaps: `BM_DOUBLE + c`.
pub const BM_DOUBLE: usize = BM_APP + 1; // 107
/// Bitmaps per index.
pub const NBITMAPS: usize = BM_DOUBLE + CLASSES; // 148

/// The physical section order. This order — not the order sections are
/// computed in — is where each section lives in the blob.
#[derive(Clone, Copy)]
enum Sec {
    NameBits,
    NameOff,
    Names,
    EntName,
    Kind,
    RankAdj,
    Parent,
    Size,
    Mtime,
    DirEntry,
    DirStart,
    DirLen,
    DirEnd,
    DirPrior,
    DirParent,
    NameEntsOff,
    NameEnts,
    NamePrior,
    NameInfo,
    NameExt,
    Exts,
    Zones,
}
const NSEC: usize = 22;

/// A typed view of one section of `self.map`, `self.$len` long.
///
/// The cast is the read side of `as_bytes`: sections are little-endian and
/// start on 64-byte boundaries, so every element type's alignment holds.
macro_rules! sec {
    ($name:ident, $s:expr, $t:ty, $len:ident) => {
        pub fn $name(&self) -> &[$t] {
            unsafe { core::slice::from_raw_parts(self.map.as_ptr().add(self.off[$s as usize]) as *const $t, self.$len) }
        }
    };
}

/// Re-exported so another module that reads the same section layout can
/// reuse the macro.
#[cfg(feature = "std")]
#[allow(unused_imports)]
pub(crate) use sec;

/// Index storage: a file mapping or an owned buffer.
pub enum Blob {
    #[cfg(feature = "std")]
    Mapped(Mmap),
    Owned(Box<[u8]>),
}

/// The write side of Blob's build path: anonymous map where the OS grants
/// one, a plain Vec where it does not.
pub enum BlobMut {
    #[cfg(feature = "std")]
    Mapped(MmapMut),
    Owned(Vec<u8>),
}

/// What is wrong with an index's parent chain.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ForestFault {
    BrokenChain { folder: u32, parent: u32 },
    WrongKind { folder: u32, parent: u32 },
    Cycle { folder: u32 },
    DeadAncestor { entry: u32, folder: u32 },
}

/// The names with the highest `NameInfo::key`, best first.
pub struct ByKey {
    pub cover: i32,
    pub list: Vec<(i8, u32)>,
    pub counts: Vec<u32>,
    /// Per head byte: its names in `list`.
    pub heads: Vec<Vec<u32>>,
    /// Per head byte: names whose len/stem is at most `SHORT`.
    pub short: Vec<Vec<u32>>,
}

/// How to fold per-dir data down the tree in parallel.
pub struct MemoPlan {
    /// Dirs parents-first.
    pub upper: Vec<u32>,
    /// `(dir, descendants' dir-id range)`.
    pub chunks: Vec<(u32, Range<u32>)>,
}

/// A run of `ZONE` entries at a glance.
#[derive(Clone, Copy, Default)]
#[repr(C)]
pub struct Zone {
    pub max_size: u32,
    pub min_mtime: u32,
    pub max_mtime: u32,
    pub kinds: u32,
}

/// Per distinct name, what bounds its score for one fuzzy token.
#[derive(Clone, Copy, Default)]
#[repr(C)]
pub struct NameInfo {
    pub key: i8,
    pub len: u8,
    pub stem: u8,
    pub head: u8,
}

/// FxHash: interning millions of names wants a hasher cheaper than SipHash.
#[derive(Clone, Copy, Default)]
pub struct Fx;
/// The hasher `Fx` builds. The field is private.
pub struct FxH(u64);

/// The index itself. All the numbers a reader needs come from the header;
/// the four memo cells are filled on first use.
pub struct Index {
    map: Blob,
    pub n: usize,
    pub d: usize,
    pub u: usize,
    names_len: usize,
    u1: usize,
    pub words: usize,
    bits_len: usize,
    zones_len: usize,
    ext_slots: usize,
    pub event_id: u64,
    pub synced_at: u32,
    pub root_digest: u64,
    off: [usize; NSEC],
    plan: OnceCell<MemoPlan>,
    counts: OnceCell<[[u32; CLASSES]; 2]>,
    top_prior: OnceCell<i8>,
    by_key: OnceCell<ByKey>,
}

// ---------------------------------------------------------------------------
// Blob / BlobMut
// ---------------------------------------------------------------------------

impl Deref for Blob {
    type Target = [u8];
    fn deref(&self) -> &[u8] {
        match self {
            #[cfg(feature = "std")]
            Blob::Mapped(m) => m,
            Blob::Owned(b) => b,
        }
    }
}

impl Deref for BlobMut {
    type Target = [u8];
    fn deref(&self) -> &[u8] {
        match self {
            #[cfg(feature = "std")]
            BlobMut::Mapped(m) => m,
            BlobMut::Owned(v) => v,
        }
    }
}

impl DerefMut for BlobMut {
    fn deref_mut(&mut self) -> &mut [u8] {
        match self {
            #[cfg(feature = "std")]
            BlobMut::Mapped(m) => m,
            BlobMut::Owned(v) => v,
        }
    }
}

#[cfg(feature = "std")]
impl Blob {
    /// Map a file, falling back to a full read when a mapping is refused.
    pub fn from_file(f: &std::fs::File) -> std::io::Result<Blob> {
        if let Ok(m) = unsafe { Mmap::map(f) } {
            return Ok(Blob::Mapped(m));
        }
        use std::io::Read;
        let mut copy = f.try_clone()?;
        let mut buf = Vec::new();
        copy.read_to_end(&mut buf)?;
        Ok(Blob::Owned(buf.into_boxed_slice()))
    }
}

impl BlobMut {
    /// A zeroed writable buffer of `len` bytes.
    #[cfg(feature = "std")]
    pub fn anon(len: usize) -> BlobMut {
        match MmapMut::map_anon(len) {
            Ok(m) => BlobMut::Mapped(m),
            Err(_) => BlobMut::Owned(vec![0u8; len]),
        }
    }

    /// A zeroed writable buffer of `len` bytes.
    #[cfg(not(feature = "std"))]
    pub fn anon(len: usize) -> BlobMut {
        BlobMut::Owned(vec![0u8; len])
    }

    /// Turn the writable buffer into a readable blob.
    pub fn freeze(self) -> Blob {
        match self {
            #[cfg(feature = "std")]
            BlobMut::Mapped(m) => Blob::Mapped(m.make_read_only().expect("freeze map")),
            BlobMut::Owned(v) => Blob::Owned(v.into_boxed_slice()),
        }
    }
}

impl core::fmt::Display for ForestFault {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match *self {
            ForestFault::BrokenChain { folder, parent } => write!(f, "broken parent chain: {folder} names the folder {parent}, which is not there"),
            ForestFault::WrongKind { folder, parent } => write!(f, "wrong record kind: the parent {parent} of {folder} is not a folder"),
            ForestFault::Cycle { folder } => write!(f, "cycle detected: the chain of {folder} does not reach the root"),
            ForestFault::DeadAncestor { entry, folder } => write!(f, "dead ancestor: the entry {entry} survives the folder {folder}, which is gone"),
        }
    }
}

impl BuildHasher for Fx {
    type Hasher = FxH;
    fn build_hasher(&self) -> FxH {
        FxH(0)
    }
}

impl Hasher for FxH {
    fn write(&mut self, bytes: &[u8]) {
        for chunk in bytes.chunks(8) {
            let mut word = 0u64;
            for (i, &b) in chunk.iter().enumerate() {
                word |= (b as u64) << (8 * i);
            }
            self.0 = (self.0.rotate_left(5) ^ word).wrapping_mul(0x517c_c1b7_2722_0a95);
        }
    }
    fn finish(&self) -> u64 {
        self.0
    }
}

// ---------------------------------------------------------------------------
// The index
// ---------------------------------------------------------------------------

impl Index {
    sec!(name_bits, Sec::NameBits, u64, bits_len);
    sec!(name_off, Sec::NameOff, u32, u1);
    sec!(names, Sec::Names, u8, names_len);
    sec!(ent_name, Sec::EntName, u32, n);
    sec!(kind, Sec::Kind, u8, n);
    sec!(rank_adj, Sec::RankAdj, i8, n);
    sec!(parent, Sec::Parent, u32, n);
    sec!(size_raw, Sec::Size, u32, n);
    sec!(mtime, Sec::Mtime, u32, n);
    sec!(dir_entry, Sec::DirEntry, u32, d);
    sec!(dir_start, Sec::DirStart, u32, d);
    sec!(dir_len, Sec::DirLen, u32, d);
    sec!(dir_end, Sec::DirEnd, u32, d);
    sec!(dir_prior, Sec::DirPrior, i8, d);
    sec!(dir_parent, Sec::DirParent, u32, d);
    sec!(name_ents_off, Sec::NameEntsOff, u32, u1);
    sec!(name_ents, Sec::NameEnts, u32, n);
    sec!(name_prior, Sec::NamePrior, i8, u);
    sec!(name_info, Sec::NameInfo, NameInfo, u);
    sec!(name_ext, Sec::NameExt, u8, u);
    sec!(ext_table, Sec::Exts, [u8; 16], ext_slots);
    sec!(zones, Sec::Zones, Zone, zones_len);

    /// The extension-table slot of a lowercased extension, or 255.
    pub fn ext_slot(&self, e: &[u8]) -> u8 {
        if e.len() > 15 {
            return 255;
        }
        let t = self.ext_table();
        (1..255).find(|&i| t[i][0] as usize == e.len() && &t[i][1..1 + e.len()] == e).unwrap_or(255) as u8
    }

    /// One bitmap: `words` u64 words, bit `k % 64` of word `k / 64` set for
    /// name `k`.
    pub fn bitmap(&self, b: usize) -> &[u64] {
        &self.name_bits()[b * self.words..][..self.words]
    }

    /// Names carrying class `c` in either half.
    pub fn class_counts(&self) -> &[u32; CLASSES] {
        &self.counts()[0]
    }

    /// Names carrying class `c` at least twice.
    pub fn double_counts(&self) -> &[u32; CLASSES] {
        &self.counts()[1]
    }

    /// The two class histograms, computed once.
    fn counts(&self) -> &[[u32; CLASSES]; 2] {
        self.counts.get_or_init(|| {
            let mut out = [[0u32; CLASSES]; 2];
            for c in 0..CLASSES {
                let (mut a, mut b) = (0u32, 0u32);
                for w in 0..self.words {
                    a += (self.bitmap(BM_FIRST + c)[w] | self.bitmap(BM_SECOND + c)[w]).count_ones();
                    b += self.bitmap(BM_DOUBLE + c)[w].count_ones();
                }
                out[0][c] = a;
                out[1][c] = b;
            }
            out
        })
    }

    /// The largest name prior in the index.
    pub fn top_prior(&self) -> i8 {
        *self.top_prior.get_or_init(|| self.name_prior().iter().copied().max().unwrap_or(0))
    }

    /// The best-scoring names, best first, plus the head/short buckets.
    pub fn by_key(&self) -> &ByKey {
        self.by_key.get_or_init(|| {
            const WANT: usize = 1 << 14;
            let info = self.name_info();
            let mut hist = [0u32; 256];
            for &x in info {
                hist[(x.key as i32 + 128) as usize] += 1;
            }
            // Lower `cover` until the names at or above it would exceed the
            // working set; `cover` is then the lowest key on the list.
            let mut cover = i8::MAX as i32 + 1;
            let mut taken = 0usize;
            while cover > i8::MIN as i32 && taken + hist[(cover - 1 + 128) as usize] as usize <= WANT {
                cover -= 1;
                taken += hist[(cover + 128) as usize] as usize;
            }
            let mut list: Vec<(i8, u32)> = (0..self.u).filter(|&k| info[k].key as i32 >= cover).map(|k| (info[k].key, k as u32)).collect();
            list.sort_unstable_by_key(|&x| Reverse(x.0));
            let counts: Vec<u32> = (cover..=i8::MAX as i32).map(|k| hist[(k + 128) as usize]).collect();
            let mut heads: Vec<Vec<u32>> = vec![Vec::new(); 256];
            let mut short: Vec<Vec<u32>> = vec![Vec::new(); 256];
            for k in 0..self.u {
                let x = info[k];
                let h = x.head as usize;
                if x.key as i32 >= cover {
                    heads[h].push(k as u32);
                } else if x.len as usize <= SHORT || x.stem as usize <= SHORT {
                    short[h].push(k as u32);
                }
            }
            ByKey { cover, list, counts, heads, short }
        })
    }

    /// The bytes of distinct name `id`.
    pub fn uname(&self, id: u32) -> &[u8] {
        let s = self.name_off()[id as usize] as usize;
        let e = self.name_off()[id as usize + 1] as usize;
        &self.names()[s..e]
    }

    /// A 64-byte window of the `Names` section starting at this name, when
    /// the name is no longer than 64 bytes and the window is still in bounds.
    #[inline]
    pub fn uname_wide(&self, id: u32) -> Option<&[u8; 64]> {
        let s = self.name_off()[id as usize] as usize;
        let e = self.name_off()[id as usize + 1] as usize;
        if e - s > 64 {
            return None;
        }
        let names = self.names();
        if s + 64 > names.len() {
            return None;
        }
        let p = unsafe { names.as_ptr().add(s) as *const [u8; 64] };
        Some(unsafe { &*p })
    }

    /// The comparison view of a name (NFC) and its optional raw wide window.
    /// A name that needed normalising has no raw window to reuse.
    pub fn uname_folded(&self, id: u32) -> (Cow<'_, [u8]>, Option<&[u8; 64]>) {
        let raw = self.uname(id);
        let folded = fold::fold(raw);
        let wide = match &folded {
            Cow::Borrowed(_) => self.uname_wide(id),
            Cow::Owned(_) => None,
        };
        (folded, wide)
    }

    /// The raw name of entry `i`.
    pub fn name(&self, i: usize) -> &[u8] {
        self.uname(self.ent_name()[i])
    }

    /// The entries carrying distinct name `id`.
    pub fn name_ents_of(&self, id: u32) -> Vec<u32> {
        let s = self.name_ents_off()[id as usize] as usize;
        let e = self.name_ents_off()[id as usize + 1] as usize;
        self.name_ents()[s..e].to_vec()
    }

    /// The decoded size of entry `i`.
    pub fn size_of(&self, i: usize) -> u64 {
        dec_size(self.size_raw()[i])
    }

    /// The directory id whose entry is `entry`, if it is a directory.
    pub fn dir_of(&self, entry: u32) -> Option<u32> {
        self.dir_entry().binary_search(&entry).ok().map(|i| i as u32)
    }

    /// The entry range of a directory's immediate children.
    pub fn children(&self, d: u32) -> Range<usize> {
        let s = self.dir_start()[d as usize] as usize;
        s..s + self.dir_len()[d as usize] as usize
    }

    /// The dir-id range of a directory's descendants (excluding itself).
    pub fn descendants(&self, k: u32) -> Range<u32> {
        let a = self.dir_start()[k as usize];
        let b = self.dir_end()[k as usize];
        let lo = self.dir_entry().partition_point(|&e| e < a) as u32;
        let hi = self.dir_entry().partition_point(|&e| e < b) as u32;
        lo..hi
    }

    /// A parents-first fold order, with big subtrees split into parallel
    /// chunks. `upper` holds every dir a chunk does not cover; `chunks`
    /// holds `(root, descendants)` for each large subtree.
    pub fn memo_plan(&self) -> &MemoPlan {
        self.plan.get_or_init(|| {
            const CHUNK: usize = 4096;
            let desc: Vec<Range<u32>> = (0..self.d).map(|k| self.descendants(k as u32)).collect();
            let mut covered = vec![false; self.d];
            let mut chunks: Vec<(u32, Range<u32>)> = Vec::new();
            for k in 0..self.d {
                if covered[k] {
                    continue;
                }
                let r = &desc[k];
                if (r.end - r.start) as usize >= CHUNK {
                    chunks.push((k as u32, r.clone()));
                    for c in r.start as usize..r.end as usize {
                        covered[c] = true;
                    }
                }
            }
            chunks.sort_by_key(|c| c.1.start);
            let upper: Vec<u32> = (0..self.d as u32).filter(|&k| !covered[k as usize]).collect();
            MemoPlan { upper, chunks }
        })
    }

    /// Write the absolute `/`-path of entry `i` into `out` (cleared first).
    pub fn path(&self, i: usize, out: &mut Vec<u8>) {
        out.clear();
        let mut chain: Vec<u32> = Vec::new();
        let mut e = i as u32;
        while e != 0 {
            chain.push(e);
            let d = self.parent()[e as usize] as usize;
            e = self.dir_entry()[d];
        }
        chain.reverse();
        self.emit_chain(&chain, out);
    }

    fn emit_chain(&self, chain: &[u32], out: &mut Vec<u8>) {
        out.push(b'/');
        for (k, &e) in chain.iter().enumerate() {
            if k > 0 {
                out.push(b'/');
            }
            out.extend_from_slice(self.name(e as usize));
        }
    }

    /// Resolve an absolute path to an entry. Children of each directory are
    /// sorted by name, so each component is a binary search; an exact miss
    /// falls back to an ASCII-case-insensitive scan.
    pub fn lookup(&self, path: &[u8]) -> Option<u32> {
        if path.is_empty() {
            return None;
        }
        let parts: Vec<&[u8]> = path.split(|&b| b == b'/').filter(|p| !p.is_empty()).collect();
        if parts.is_empty() {
            return Some(0);
        }
        let mut d = 0u32;
        for (i, &comp) in parts.iter().enumerate() {
            let r = self.children(d);
            let (mut lo, mut hi) = (r.start, r.end);
            while lo < hi {
                let mid = lo + (hi - lo) / 2;
                if self.name(mid) < comp {
                    lo = mid + 1;
                } else {
                    hi = mid;
                }
            }
            let found =
                if lo < r.end && self.name(lo) == comp { Some(lo) } else { (r.start..r.end).find(|&c| self.name(c).eq_ignore_ascii_case(comp)) };
            let e = found? as u32;
            if i + 1 == parts.len() {
                return Some(e);
            }
            d = self.dir_of(e)?;
        }
        None
    }

    /// Build an index with the default priority rules.
    pub fn build(ls: Vec<Listing>, event_id: u64, synced_at: u32, home: &[u8]) -> Index {
        Self::build_with_rules(ls, event_id, synced_at, home, &Rules::defaults())
    }

    /// Build an index from a DFS block stream.
    pub fn build_with_rules(mut ls: Vec<Listing>, event_id: u64, synced_at: u32, home: &[u8], rules: &Rules) -> Index {
        // 1. A root listing always exists, and every listing's entries are
        //    sorted by their raw name bytes before the layout is fixed.
        if ls.is_empty() {
            ls.push(Listing { id: 0, names: Vec::new(), ents: Vec::new() });
        }
        rgy_par::for_each_mut(&mut ls, |_, l| {
            let names = &l.names;
            l.ents.sort_unstable_by(|a, b| {
                let span = |e: &RawEnt| {
                    let end = (e.name_off as usize + e.name_len as usize).min(names.len());
                    let start = (e.name_off as usize).min(end);
                    &names[start..end]
                };
                span(a).cmp(span(b))
            });
        });

        // Map a listing id to its position; ids are small and dense in a real
        // walk, but a hash map keeps a corrupt id from asking for a huge Vec.
        let mut by_id: Map<u32, usize> = Map::with_capacity(ls.len());
        for (i, l) in ls.iter().enumerate() {
            by_id.insert(l.id, i);
        }
        let root = by_id.get(&0).copied().unwrap_or(0);
        let mut visited = vec![false; ls.len()];
        if root < visited.len() {
            visited[root] = true;
        }

        // 2. Lay the entries out directory-block at a time, depth first. A
        //    directory's children are one contiguous block; its whole subtree
        //    is the block followed immediately by its children dirs' subtrees.
        let mut ename: Vec<Vec<u8>> = Vec::new();
        let mut ekind: Vec<u8> = Vec::new();
        let mut esize: Vec<u64> = Vec::new();
        let mut emtime: Vec<u32> = Vec::new();
        let mut eparent: Vec<u32> = Vec::new();
        ename.push(Vec::new());
        ekind.push(KIND_DIR);
        esize.push(0);
        emtime.push(0);
        eparent.push(0);

        let mut de: Vec<u32> = vec![0]; // dir_entry
        let mut dp: Vec<u32> = vec![0]; // dir_parent
        let mut ds: Vec<u32> = vec![0]; // dir_start
        let mut dl: Vec<u32> = vec![0]; // dir_len
        let mut stack: Vec<(usize, u32, u32)> = vec![(root, 0, 0)];

        while let Some((li, d, _)) = stack.pop() {
            let start = ename.len() as u32;
            let listing = &ls[li];
            let mut kids: Vec<(usize, u32, u32)> = Vec::new();
            for ent in &listing.ents {
                let end = (ent.name_off as usize + ent.name_len as usize).min(listing.names.len());
                let begin = (ent.name_off as usize).min(end);
                let name = listing.names[begin..end].to_vec();
                let e = ename.len() as u32;
                ename.push(name);
                ekind.push(ent.kind);
                esize.push(ent.size);
                emtime.push(ent.mtime);
                eparent.push(d);
                let child = if ent.child == NONE { None } else { by_id.get(&ent.child).copied().filter(|&ci| ci < ls.len() && !visited[ci]) };
                if let Some(ci) = child {
                    visited[ci] = true;
                    let c = de.len() as u32;
                    de.push(e);
                    dp.push(d);
                    ds.push(0);
                    dl.push(0);
                    kids.push((ci, c, e));
                }
            }
            let end = ename.len() as u32;
            ds[d as usize] = start;
            dl[d as usize] = end - start;
            // Reverse push: the first child's whole subtree is emitted first.
            for (ci, c, e) in kids.into_iter().rev() {
                stack.push((ci, c, e));
            }
        }
        let n = ename.len();
        let d = de.len();

        // End of each subtree: children block end, widened by each child's
        // end. Parent dir ids are always smaller, so one reverse pass folds
        // every descendant's end up to its root.
        let mut dend: Vec<u32> = (0..d).map(|k| ds[k] + dl[k]).collect();
        for k in (1..d).rev() {
            let p = dp[k] as usize;
            if dend[k] > dend[p] {
                dend[p] = dend[k];
            }
        }

        // 3. Intern the names in entry order.
        let mut intern: Map<&[u8], u32> = Map::with_capacity(n);
        let mut ent_name: Vec<u32> = Vec::with_capacity(n);
        let mut uoff: Vec<u32> = vec![0];
        let mut unames: Vec<u8> = Vec::new();
        for i in 0..n {
            let name: &[u8] = &ename[i];
            let id = match intern.get(name) {
                Some(&id) => id,
                None => {
                    let id = (uoff.len() - 1) as u32;
                    intern.insert(name, id);
                    unames.extend_from_slice(name);
                    uoff.push(unames.len() as u32);
                    id
                }
            };
            ent_name.push(id);
        }
        drop(intern);
        let u = uoff.len() - 1;
        let words = u.div_ceil(64);
        let names_len = unames.len();

        // 4. The bitmaps, one contiguous run of `words` words each.
        let mut ubits: Vec<u64> = vec![0u64; words * NBITMAPS];
        for k in 0..u {
            let name = &unames[uoff[k] as usize..uoff[k + 1] as usize];
            let folded = fold::fold(name);
            let fb: &[u8] = &folded;
            let w = k / 64;
            let bit = 1u64 << (k % 64);
            for b in name_bitmaps(fb) {
                ubits[b * words + w] |= bit;
            }
        }

        // 5. Encoded sizes and the priority ladder.
        let enc: Vec<u32> = esize.iter().map(|&s| enc_size(s)).collect();
        let home_parts: Vec<&[u8]> = home.split(|&b| b == b'/').filter(|p| !p.is_empty()).collect();
        // `hm[e]` counts how many leading home components e's chain matches.
        let mut hm = vec![0usize; n];
        for e in 1..n {
            let pe = de[eparent[e] as usize] as usize;
            let mut h = hm[pe];
            if h < home_parts.len() && ename[e].eq_ignore_ascii_case(home_parts[h]) {
                h += 1;
            }
            hm[e] = h;
        }
        // Two chains: the level a folder record sees, and the level a file
        // record sees. A rule close to the record wins; otherwise the level
        // is inherited from the parent directory.
        let mut folder_level = vec![Tier::Normal; n];
        let mut file_level = vec![Tier::Normal; n];
        for e in 0..n {
            let (a, b) = rules.levels_of(&ename[e]);
            let pe = if e == 0 { 0 } else { de[eparent[e] as usize] as usize };
            folder_level[e] = a.unwrap_or(if e == 0 { Tier::Normal } else { folder_level[pe] });
            file_level[e] = b.unwrap_or(if e == 0 { Tier::Normal } else { file_level[pe] });
        }
        let mut rank_adj: Vec<i8> = Vec::with_capacity(n);
        let mut level: Vec<Tier> = Vec::with_capacity(n);
        for e in 0..n {
            let t = if ekind[e] & 3 == KIND_DIR { folder_level[e] } else { file_level[e] };
            let bonus = if !home_parts.is_empty() && hm[e] >= home_parts.len() { 15 } else { 0 };
            rank_adj.push((crate::rules::offset(t) + bonus).clamp(-100, 60) as i8);
            level.push(t);
        }
        for e in 0..n {
            ekind[e] |= (level[e].bits() as u8) << 5;
        }

        // 6. Entries grouped by name, best prior first within each name.
        let mut ne: Vec<u32> = (0..n as u32).collect();
        rgy_par::sort_unstable_by_key(&mut ne, |&e| (ent_name[e as usize], Reverse(rank_adj[e as usize]), e));
        let mut per_name = vec![0u32; u + 1];
        for &e in &ne {
            per_name[ent_name[e as usize] as usize] += 1;
        }
        let mut ne_off = vec![0u32; u + 1];
        for k in 0..u {
            ne_off[k + 1] = ne_off[k] + per_name[k];
        }

        // 7. Per-name prior, NameInfo, extension table, zone aggregates.
        let mut name_prior = vec![i8::MIN; u];
        for i in 1..n {
            let k = ent_name[i] as usize;
            if rank_adj[i] > name_prior[k] {
                name_prior[k] = rank_adj[i];
            }
        }
        let info: Vec<NameInfo> = (0..u).map(|k| NameInfo::of(&unames[uoff[k] as usize..uoff[k + 1] as usize], name_prior[k])).collect();
        let (table, slots) = ext_slots(&unames, &uoff);
        let dir_prior: Vec<i8> = (0..d).map(|k| rank_adj[de[k] as usize]).collect();
        let dir_parent: Vec<u32> = dp.clone();
        let zvec = zones(&ekind, &enc, &emtime);

        // 8. Lay the sections out and write them in `Sec` order.
        let lens = section_lens(n, d, u, names_len).expect("the built index's section sizes fit in a usize");
        let (off, total) = layout_checked(&lens).expect("the built index's offsets fit in a usize");
        let mut m = BlobMut::anon(total);
        {
            let h = header(MAGIC, &[n as u64, d as u64, u as u64, names_len as u64, event_id, synced_at as u64, 0]);
            let buf: &mut [u8] = &mut m;
            buf[..HDR].copy_from_slice(&h);
        }
        put(&mut m, &off, Sec::NameBits, as_bytes(&ubits));
        put(&mut m, &off, Sec::NameOff, as_bytes(&uoff));
        put(&mut m, &off, Sec::Names, as_bytes(&unames));
        put(&mut m, &off, Sec::EntName, as_bytes(&ent_name));
        put(&mut m, &off, Sec::Kind, as_bytes(&ekind));
        put(&mut m, &off, Sec::RankAdj, as_bytes(&rank_adj));
        put(&mut m, &off, Sec::Parent, as_bytes(&eparent));
        put(&mut m, &off, Sec::Size, as_bytes(&enc));
        put(&mut m, &off, Sec::Mtime, as_bytes(&emtime));
        put(&mut m, &off, Sec::DirEntry, as_bytes(&de));
        put(&mut m, &off, Sec::DirStart, as_bytes(&ds));
        put(&mut m, &off, Sec::DirLen, as_bytes(&dl));
        put(&mut m, &off, Sec::DirEnd, as_bytes(&dend));
        put(&mut m, &off, Sec::DirPrior, as_bytes(&dir_prior));
        put(&mut m, &off, Sec::DirParent, as_bytes(&dir_parent));
        put(&mut m, &off, Sec::NameEntsOff, as_bytes(&ne_off));
        put(&mut m, &off, Sec::NameEnts, as_bytes(&ne));
        put(&mut m, &off, Sec::NamePrior, as_bytes(&name_prior));
        put(&mut m, &off, Sec::NameInfo, as_bytes(&info));
        put(&mut m, &off, Sec::NameExt, as_bytes(&slots));
        put(&mut m, &off, Sec::Exts, as_bytes(&table));
        put(&mut m, &off, Sec::Zones, as_bytes(&zvec));

        Self::from_blob(m.freeze()).expect("the built index reads back")
    }

    /// The reader/validator. Offsets are recomputed from the header counts;
    /// every arithmetic step is checked so a corrupt header is refused.
    fn from_blob(map: Blob) -> Option<Index> {
        let [n, d, u, names_len, event_id, synced_at, root_digest] = {
            let head: &[u8] = &map;
            fields::<7>(head, MAGIC)?
        };
        let n = usize::try_from(n).ok()?;
        let d = usize::try_from(d).ok()?;
        let u = usize::try_from(u).ok()?;
        let names_len = usize::try_from(names_len).ok()?;
        let synced_at = u32::try_from(synced_at).ok()?;
        let u1 = u.checked_add(1)?;
        let words = u.div_ceil(64);
        let bits_len = words.checked_mul(NBITMAPS)?;
        let (off, total) = layout_checked(&section_lens(n, d, u, names_len)?)?;
        let map_len = {
            let bytes: &[u8] = &map;
            bytes.len()
        };
        if map_len < total {
            return None;
        }
        Some(Index {
            map,
            n,
            d,
            u,
            names_len,
            u1,
            words,
            bits_len,
            zones_len: n.div_ceil(ZONE),
            ext_slots: 256,
            event_id,
            synced_at,
            root_digest,
            off,
            plan: OnceCell::new(),
            counts: OnceCell::new(),
            top_prior: OnceCell::new(),
            by_key: OnceCell::new(),
        })
    }

    /// The blob as portable bytes: a fresh header plus the section payload.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = header(
            MAGIC,
            &[self.n as u64, self.d as u64, self.u as u64, self.names_len as u64, self.event_id, self.synced_at as u64, self.root_digest],
        );
        let map: &[u8] = &self.map;
        out.extend_from_slice(&map[HDR..]);
        out
    }

    /// An index from portable bytes.
    pub fn from_bytes(bytes: &[u8]) -> Option<Index> {
        Self::from_blob(Blob::Owned(bytes.to_vec().into_boxed_slice()))
    }

    /// The event id from a blob's header alone.
    pub fn header_event_id(bytes: &[u8]) -> Option<u64> {
        fields::<7>(bytes, MAGIC).map(|f| f[4])
    }

    /// The root digest from a blob's header alone.
    pub fn header_root_digest(bytes: &[u8]) -> Option<u64> {
        fields::<7>(bytes, MAGIC).map(|f| f[6])
    }

    /// Stamp the digest of the tree this index was built for.
    pub fn set_root_digest(&mut self, digest: u64) {
        self.root_digest = digest;
    }

    /// Write the index to `<path>` through a `.tmp` file and a rename.
    #[cfg(feature = "std")]
    pub fn save(&self, path: &std::path::Path) -> std::io::Result<()> {
        use std::io::Write;
        let mut tmp = path.as_os_str().to_os_string();
        tmp.push(".tmp");
        let tmp = std::path::PathBuf::from(tmp);
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(&self.to_bytes())?;
        f.sync_data()?;
        std::fs::rename(&tmp, path)
    }

    /// The event id of a saved index, from the first 64 bytes alone.
    #[cfg(feature = "std")]
    pub fn saved_event_id(path: &std::path::Path) -> Option<u64> {
        use std::io::Read;
        let mut f = std::fs::File::open(path).ok()?;
        let mut h = [0u8; 64];
        f.read_exact(&mut h).ok()?;
        Self::header_event_id(&h)
    }

    /// Load an index, mmap-first.
    #[cfg(feature = "std")]
    pub fn load(path: &std::path::Path) -> Option<Index> {
        let f = std::fs::File::open(path).ok()?;
        Self::from_blob(Blob::from_file(&f).ok()?)
    }

    /// The size of the blob in bytes.
    pub fn bytes(&self) -> usize {
        let map: &[u8] = &self.map;
        map.len()
    }

    /// Fault in the hot sections and warm the four memo cells.
    pub fn prefault(&self) {
        let map: &[u8] = &self.map;
        for s in [Sec::NameBits, Sec::NameOff, Sec::EntName, Sec::Kind, Sec::Parent, Sec::NameInfo] {
            let at = self.off[s as usize];
            if at < map.len() {
                unsafe {
                    let _ = core::ptr::read_volatile(map.as_ptr().add(at));
                }
            }
        }
        let _ = self.counts();
        let _ = self.top_prior();
        let _ = self.by_key();
        let _ = self.memo_plan();
    }

    /// Check the parent chains of directories and of entries.
    pub fn validate_forest(&self) -> Result<(), ForestFault> {
        check_forest(self.dir_parent(), self.dir_entry(), self.kind(), self.parent(), self.n, self.d)
    }
}

impl NameInfo {
    /// The cheap bounds of a name for one fuzzy token.
    pub fn of(name: &[u8], prior: i8) -> NameInfo {
        let folded = fold::fold(name);
        let s: &[u8] = &folded;
        let off = (s.len() > 1 && s[0] == b'.') as usize;
        let stem = s[off..].iter().rposition(|&b| b == b'.').map_or(s.len(), |r| off + r);
        let is_app = s.ends_with(b".app");
        let tweak = 10 + if is_app { 25 } else { 0 } - if off == 1 { 8 } else { 0 };
        let key = ((prior as i32) + tweak - (s.len().min(80) / 3) as i32).max(i8::MIN as i32) as i8;
        NameInfo {
            key,
            len: (s.len() - off).min(255) as u8,
            stem: (stem - off).min(255) as u8,
            head: s.get(off).map_or(0, |&b| crate::name_query::fold(b)),
        }
    }
}

// ---------------------------------------------------------------------------
// Format helpers
// ---------------------------------------------------------------------------

/// The 4 KiB header: magic, then `fields.len()` little-endian u64s at 8+8k.
pub(crate) fn header(magic: &[u8; 8], fields: &[u64]) -> Vec<u8> {
    let mut h = vec![0u8; HDR];
    h[..8].copy_from_slice(magic);
    for (k, v) in fields.iter().enumerate() {
        h[8 + k * 8..16 + k * 8].copy_from_slice(&v.to_le_bytes());
    }
    h
}

/// The header's fields, if `b` starts with `magic`.
pub(crate) fn fields<const N: usize>(b: &[u8], magic: &[u8; 8]) -> Option<[u64; N]> {
    (b.len() >= 8 + N * 8 && &b[..8] == magic).then(|| core::array::from_fn(|k| u64::from_le_bytes(b[8 + k * 8..16 + k * 8].try_into().unwrap())))
}

/// Section offsets and total size, with unchecked arithmetic (the builder
/// side of a format whose sizes are known good).
#[cfg(feature = "std")]
#[allow(dead_code)]
pub(crate) fn layout<const N: usize>(lens: &[usize; N]) -> ([usize; N], usize) {
    let mut off = [0usize; N];
    let mut at = HDR;
    for (k, &l) in lens.iter().enumerate() {
        off[k] = at;
        at = (at + l + 63) & !63;
    }
    (off, at)
}

/// Section offsets and total size; every section starts on a 64-byte
/// boundary. Overflow is refused.
pub(crate) fn layout_checked<const N: usize>(lens: &[usize; N]) -> Option<([usize; N], usize)> {
    let mut off = [0usize; N];
    let mut at = HDR;
    for (k, &l) in lens.iter().enumerate() {
        off[k] = at;
        at = (at.checked_add(l)?.checked_add(63)?) & !63;
    }
    Some((off, at))
}

/// Reinterpret a typed slice as its raw little-endian bytes.
pub(crate) fn as_bytes<T: Copy>(v: &[T]) -> &[u8] {
    unsafe { core::slice::from_raw_parts(v.as_ptr() as *const u8, core::mem::size_of_val(v)) }
}

/// The exact byte length of every section, in `Sec` order.
fn section_lens(n: usize, d: usize, u: usize, names_len: usize) -> Option<[usize; NSEC]> {
    let n4 = n.checked_mul(4)?;
    let d4 = d.checked_mul(4)?;
    let u4 = u.checked_add(1)?.checked_mul(4)?;
    let u4b = u.checked_mul(4)?;
    let z = n.div_ceil(ZONE).checked_mul(core::mem::size_of::<Zone>())?;
    let bits = u.div_ceil(64).checked_mul(NBITMAPS)?.checked_mul(8)?;
    Some([bits, u4, names_len, n4, n, n, n4, n4, n4, d4, d4, d4, d4, d, d4, u4, n4, u, u4b, u, 256 * 16, z])
}

/// Copy one section's bytes into the writable blob at its computed offset.
fn put(m: &mut BlobMut, off: &[usize; NSEC], s: Sec, bytes: &[u8]) {
    let at = off[s as usize];
    let buf: &mut [u8] = m;
    buf[at..at + bytes.len()].copy_from_slice(bytes);
}

/// Size encoding: below 2 GiB exact, above it 2 MiB granularity with the
/// high bit as a flag. The encoding keeps order.
pub fn enc_size(s: u64) -> u32 {
    const HI: u32 = 1u32 << 31;
    if s < (1u64 << 31) { s as u32 } else { HI | ((s >> 21).min((1u64 << 31) - 1) as u32) }
}

/// The inverse of `enc_size`.
pub fn dec_size(v: u32) -> u64 {
    const HI: u32 = 1u32 << 31;
    if v & HI == 0 { v as u64 } else { ((v & !HI) as u64) << 21 }
}

/// The class bit of one byte.
#[inline]
pub fn char_bit(b: u8) -> u64 {
    match b {
        b'a'..=b'z' => 1u64 << (b - b'a'),
        b'A'..=b'Z' => 1u64 << (b - b'A'),
        b'0'..=b'9' => 1u64 << (26 + b - b'0'),
        b'.' => 1u64 << 36,
        b'-' | b'_' => 1u64 << 37,
        b' ' => 1u64 << 38,
        0x80..=0xff => 1u64 << CLASS_NON_ASCII,
        _ => 1u64 << 40,
    }
}

/// The classes present in `s`.
pub fn char_mask(s: &[u8]) -> u64 {
    s.iter().fold(0u64, |m, &b| m | char_bit(b))
}

/// Classes present at least twice in `s`.
pub fn doubled(s: &[u8]) -> u64 {
    let (mut once, mut twice) = (0u64, 0u64);
    for &b in s {
        let c = char_bit(b);
        if once & c != 0 {
            twice |= c;
        } else {
            once |= c;
        }
    }
    twice
}

/// The word-start hash of one byte.
#[inline]
pub fn start_hash(b: u8) -> usize {
    (b.to_ascii_lowercase() % 23) as usize
}

/// The start bit of one byte: `1 << (CLASSES + start_hash(b))`.
#[inline]
pub fn start_bit(b: u8) -> u64 {
    1u64 << (CLASSES + start_hash(b))
}

/// A name mask from already-folded bytes: its classes plus the start bit of
/// the first byte past a leading dot and of each space-starting word.
pub(crate) fn mask_of(s: &[u8]) -> u64 {
    let mut m = char_mask(s);
    let off = (s.len() > 1 && s[0] == b'.') as usize;
    if let Some(&b) = s.get(off) {
        m |= start_bit(b);
    }
    for i in 1..s.len() {
        if s[i - 1] == b' ' {
            m |= start_bit(s[i]);
        }
    }
    m
}

/// The mask of a name, folded for comparison.
pub fn name_mask(s: &[u8]) -> u64 {
    let f = fold::fold(s);
    mask_of(&f)
}

/// Which bitmaps a folded name sets. `s` is already folded; `mask_of` is
/// reused so the start bits and the BM_START bitmaps cannot disagree.
fn name_bitmaps(s: &[u8]) -> impl Iterator<Item = usize> {
    let mut out = Vec::new();
    let half = s.len() / 2;
    let first = char_mask(&s[..half]);
    let second = char_mask(&s[half..]);
    for c in 0..CLASSES {
        if first & (1u64 << c) != 0 {
            out.push(BM_FIRST + c);
        }
        if second & (1u64 << c) != 0 {
            out.push(BM_SECOND + c);
        }
    }
    let starts = mask_of(s) >> CLASSES;
    for h in 0..(64 - CLASSES) {
        if starts & (1u64 << h) != 0 {
            out.push(BM_START + h);
        }
    }
    if s.first() == Some(&b'.') {
        out.push(BM_DOT);
    }
    if s.ends_with(b".app") {
        out.push(BM_APP);
    }
    let twice = doubled(s);
    for c in 0..CLASSES {
        if twice & (1u64 << c) != 0 {
            out.push(BM_DOUBLE + c);
        }
    }
    out.into_iter()
}

/// The lowercased extension of a name, as a `[len, bytes..]` row, or `None`
/// when there is no dot or the extension is longer than 15 bytes.
fn ext_key(name: &[u8]) -> Option<[u8; 16]> {
    let f = fold::fold(name);
    let s: &[u8] = &f;
    let at = s.iter().rposition(|&b| b == b'.')? + 1;
    let tail = &s[at..];
    if tail.len() > 15 {
        return None;
    }
    let mut row = [0u8; 16];
    row[0] = tail.len() as u8;
    for (i, &b) in tail.iter().enumerate() {
        row[1 + i] = crate::name_query::fold(b);
    }
    Some(row)
}

/// The 256-slot extension table and the slot of every distinct name. Slots
/// 0 and 255 are reserved; the 254 most common extensions take 1..=254,
/// ties broken by their bytes.
pub(crate) fn ext_slots(names: &[u8], off: &[u32]) -> (Vec<[u8; 16]>, Vec<u8>) {
    let u = off.len().saturating_sub(1);
    let mut freq: Map<[u8; 16], u32> = Map::with_capacity(u);
    let mut keys: Vec<Option<[u8; 16]>> = Vec::with_capacity(u);
    for k in 0..u {
        let name = &names[off[k] as usize..off[k + 1] as usize];
        let key = ext_key(name);
        if let Some(row) = key {
            *freq.get_or_insert_with(row, || 0) += 1;
        }
        keys.push(key);
    }
    let mut ranked: Vec<([u8; 16], u32)> = freq.into_entries().collect();
    ranked.sort_unstable_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    let mut table = vec![[0u8; 16]; 256];
    for (i, (row, _)) in ranked.into_iter().take(254).enumerate() {
        table[i + 1] = row;
    }
    let slots: Vec<u8> = keys
        .iter()
        .map(|key| match key {
            None => 0,
            Some(row) => (1..255)
                .find(|&i| {
                    let len = row[0] as usize;
                    table[i][0] == row[0] && table[i][1..1 + len] == row[1..1 + len]
                })
                .unwrap_or(255) as u8,
        })
        .collect();
    (table, slots)
}

/// One `Zone` per `ZONE` entries: largest encoded size, mtime bounds, and
/// the bit for every kind present.
pub(crate) fn zones(kind: &[u8], size: &[u32], mtime: &[u32]) -> Vec<Zone> {
    let mut out = Vec::with_capacity(kind.len().div_ceil(ZONE));
    let mut at = 0;
    while at < kind.len() {
        let end = (at + ZONE).min(kind.len());
        let mut z = Zone { max_size: 0, min_mtime: u32::MAX, max_mtime: 0, kinds: 0 };
        for i in at..end {
            z.max_size = z.max_size.max(size.get(i).copied().unwrap_or(0));
            let t = mtime.get(i).copied().unwrap_or(0);
            z.min_mtime = z.min_mtime.min(t);
            z.max_mtime = z.max_mtime.max(t);
            z.kinds |= 1u32 << (kind[i] & 3);
        }
        out.push(z);
        at = end;
    }
    out
}

/// Validate the two parent chains. Directory `k`'s parent must be a folder
/// that precedes it; every entry's parent dir must exist and be a folder.
fn check_forest(dp: &[u32], de: &[u32], kind: &[u8], parent: &[u32], n: usize, d: usize) -> Result<(), ForestFault> {
    for k in 1..d {
        let p = dp[k] as usize;
        if p >= d {
            return Err(ForestFault::BrokenChain { folder: k as u32, parent: p as u32 });
        }
        if p >= k {
            return Err(ForestFault::Cycle { folder: k as u32 });
        }
        let pe = de[p] as usize;
        if pe >= n || kind[pe] & 3 != KIND_DIR {
            return Err(ForestFault::WrongKind { folder: k as u32, parent: p as u32 });
        }
    }
    for i in 0..n.min(parent.len()) {
        let p = parent[i] as usize;
        if p >= d {
            return Err(ForestFault::BrokenChain { folder: i as u32, parent: p as u32 });
        }
        let pe = de[p] as usize;
        if pe >= n || kind[pe] & 3 != KIND_DIR {
            return Err(ForestFault::WrongKind { folder: i as u32, parent: p as u32 });
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Tests — freshly written, no_std-safe (no std paths, alloc types explicit).
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::{ForestFault, Index, Sec, layout_checked, section_lens};
    use alloc::vec;
    use alloc::vec::Vec;
    use rgy_discovery::walk::{KIND_DIR, KIND_FILE, Listing, NONE, RawEnt};

    /// A two-folder tree: `/docs/readme.md`, `/docs/notes.txt`, `/notes.txt`.
    fn tree() -> Index {
        let listings = vec![
            Listing {
                id: 0,
                names: b"docsnotes.txt".to_vec(),
                ents: vec![
                    RawEnt { name_off: 0, name_len: 4, kind: KIND_DIR, size: 0, mtime: 1, child: 1 },
                    RawEnt { name_off: 4, name_len: 9, kind: KIND_FILE, size: 10, mtime: 2, child: NONE },
                ],
            },
            Listing {
                id: 1,
                names: b"readme.md".to_vec(),
                ents: vec![RawEnt { name_off: 0, name_len: 9, kind: KIND_FILE, size: 20, mtime: 3, child: NONE }],
            },
        ];
        Index::build(listings, 42, 7, b"/")
    }

    #[test]
    fn a_built_index_round_trips_through_bytes() {
        let idx = tree();
        let bytes = idx.to_bytes();
        assert!(bytes.starts_with(b"FSIDX015"), "the v2 magic is at offset 0");
        assert!(bytes.len() >= super::HDR + 64, "the header and a section follow");
        let back = Index::from_bytes(&bytes).expect("the bytes load");
        assert_eq!(back.n, idx.n);
        assert_eq!(back.d, idx.d);
        assert_eq!(back.u, idx.u);
        assert_eq!(back.event_id, 42);
        assert_eq!(back.synced_at, 7);
        assert_eq!(back.root_digest, 0);
        assert_eq!(back.bytes(), bytes.len());
        assert_eq!(back.to_bytes(), bytes, "re-serialising is byte-identical");
        assert!(Index::from_bytes(b"not an index at all").is_none());
        assert!(Index::from_bytes(&[]).is_none());
    }

    #[test]
    fn the_header_accessors_answer_from_bytes_alone() {
        let mut idx = tree();
        idx.set_root_digest(0xDEAD_BEEF);
        let bytes = idx.to_bytes();
        assert_eq!(Index::header_event_id(&bytes), Some(42));
        assert_eq!(Index::header_root_digest(&bytes), Some(0xDEAD_BEEF));
        let back = Index::from_bytes(&bytes).expect("loads");
        assert_eq!(back.root_digest, 0xDEAD_BEEF);
        assert_eq!(Index::header_event_id(&back.to_bytes()), Some(42));
        let mut wrong_magic = [0u8; 64];
        wrong_magic[..8].copy_from_slice(b"FSIDX014");
        assert_eq!(Index::header_event_id(&wrong_magic), None);
        assert_eq!(Index::header_root_digest(&wrong_magic), None);
        assert_eq!(Index::header_event_id(b"FSIDX015"), None, "a short header is refused");
    }

    #[test]
    fn a_path_is_reconstructed_from_the_parent_chain() {
        let idx = tree();
        let file = idx.lookup(b"/docs/readme.md").expect("the file resolves");
        let mut out = Vec::new();
        idx.path(file as usize, &mut out);
        assert_eq!(out, b"/docs/readme.md".to_vec());
        let folder = idx.lookup(b"/docs").expect("the folder resolves");
        idx.path(folder as usize, &mut out);
        assert_eq!(out, b"/docs".to_vec(), "path clears the caller's buffer");
        idx.path(0, &mut out);
        assert_eq!(out, b"/".to_vec());
    }

    #[test]
    fn a_deep_path_is_not_truncated() {
        let depth = 300usize;
        let mut listings: Vec<Listing> = Vec::with_capacity(depth);
        let mut expected: Vec<u8> = Vec::new();
        for k in 0..depth {
            let last = k + 1 == depth;
            let (name, len, kind, child) =
                if last { (b"leaf.txt".to_vec(), 8u16, KIND_FILE, NONE) } else { (b"dir".to_vec(), 3u16, KIND_DIR, (k + 1) as u32) };
            expected.push(b'/');
            expected.extend_from_slice(&name);
            listings.push(Listing { id: k as u32, names: name, ents: vec![RawEnt { name_off: 0, name_len: len, kind, size: 0, mtime: 0, child }] });
        }
        let idx = Index::build(listings, 0, 0, b"/");
        let e = idx.lookup(&expected).expect("the deep path resolves");
        let mut out = Vec::new();
        idx.path(e as usize, &mut out);
        assert_eq!(out, expected, "no 256-entry inline-chain cut");
    }

    #[test]
    fn validate_forest_catches_a_broken_chain_and_a_cycle() {
        let idx = tree();
        assert_eq!(idx.validate_forest(), Ok(()), "a built index is a forest");
        let lens = section_lens(idx.n, idx.d, idx.u, idx.names_len).unwrap();
        let (off, _) = layout_checked(&lens).unwrap();

        // An entry whose parent directory id is out of range.
        let mut broken = idx.to_bytes();
        let at = off[Sec::Parent as usize] + 4;
        broken[at..at + 4].copy_from_slice(&999u32.to_le_bytes());
        let bad = Index::from_bytes(&broken).expect("the forged counts still load");
        assert!(matches!(bad.validate_forest(), Err(ForestFault::BrokenChain { .. })));

        // A directory that is its own parent.
        let mut cyclic = idx.to_bytes();
        let at = off[Sec::DirParent as usize] + 4;
        cyclic[at..at + 4].copy_from_slice(&1u32.to_le_bytes());
        let bad = Index::from_bytes(&cyclic).expect("the forged counts still load");
        assert!(matches!(bad.validate_forest(), Err(ForestFault::Cycle { .. })));
    }

    #[test]
    fn a_file_that_claims_children_is_a_wrong_kind() {
        let listings = vec![
            Listing {
                id: 0,
                names: b"fakefile.txt".to_vec(),
                ents: vec![RawEnt { name_off: 0, name_len: 8, kind: KIND_FILE, size: 0, mtime: 0, child: 1 }],
            },
            Listing {
                id: 1,
                names: b"child".to_vec(),
                ents: vec![RawEnt { name_off: 0, name_len: 5, kind: KIND_FILE, size: 0, mtime: 0, child: NONE }],
            },
        ];
        let idx = Index::build(listings, 0, 0, b"/");
        assert!(matches!(idx.validate_forest(), Err(ForestFault::WrongKind { .. })));
    }
}
