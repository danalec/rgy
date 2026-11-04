// SPDX-License-Identifier: MIT
//
// Transported from fsearch (https://github.com/noahdunnagan/fsearch), MIT
// licensed, Copyright (c) 2026 Noah Dunnagan. Its notice is in
// LICENSE-MIT-FSEARCH and in the repository's NOTICE.
//! The name index: every entry on disk in one flat blob, mmap-able as is.
//!
//! Layout trick: entries are emitted one directory *block* at a time, blocks
//! in depth-first order. So every directory's children are contiguous (and
//! sorted, for path lookup), and every directory's whole subtree is the single
//! range `dir_start..dir_end`. Scoping a search to a folder is a range bound,
//! not a filter.
//!
//! Names are interned: 7.5M entries share ~2M distinct names, so each name
//! is stored once, and queries score unique names, not entries. Which names
//! can match a query is answered 64 names at a time from per-class bitmaps
//! (see `name_bits`) before any name is read.

use crate::fold;
use crate::hmap::Map;
use alloc::borrow::Cow;
use alloc::boxed::Box;
use alloc::vec;
use alloc::vec::Vec;
use rgy_discovery::walk::{KIND_DIR, Listing, NONE};

#[cfg(feature = "std")]
use rgx_core::memmap2::{Mmap, MmapMut};
#[cfg(feature = "std")]
use std::io::Write;
#[cfg(feature = "std")]
use std::path::Path;

/// Index storage: a file mapping or an owned buffer.
///
/// Lessons from rgx:
/// - `Mmap::map` can FAIL (Windows sharing/commit limits, empty files,
///   network paths) — a load that only maps turns that into "no index".
///   `Blob::from_file` falls back to reading the bytes.
/// - A mapping is a promise about the file: truncation after load is a
///   SIGBUS/access violation, not an error. The header's `total` is
///   checked against the file length at load (below) and Windows refuses
///   `SetEndOfFile` on a mapped section (`ERROR_USER_MAPPED_FILE` 1224),
///   so the residual race is a replace/shrink on unix — stated here, not
///   claimed away (rgx `src/mmap.rs`'s `live_len` rule).
/// - On Windows a mapping costs two kernel objects and measured 1.51x
///   SLOWER than `read()` for scans; this index is random-access (bitmaps,
///   per-entry tables), where on-demand paging is the point, so mapping
///   stays first and the owned buffer is the fallback — never the other
///   way around, and never a threshold constant nothing calls (rgx's
///   `AUTO_MMAP_THRESHOLD` trap).
pub enum Blob {
    /// The file, mapped. The `std` path: a mapping is cheaper than a read
    /// for the random access this index does.
    #[cfg(feature = "std")]
    Mapped(Mmap),
    Owned(Box<[u8]>),
}

impl core::ops::Deref for Blob {
    type Target = [u8];
    fn deref(&self) -> &[u8] {
        match self {
            #[cfg(feature = "std")]
            Blob::Mapped(m) => m,
            Blob::Owned(v) => v,
        }
    }
}

impl Blob {
    /// Map the file; if mapping fails, read it into memory instead.
    #[cfg(feature = "std")]
    pub fn from_file(f: &std::fs::File) -> std::io::Result<Blob> {
        match unsafe { Mmap::map(f) } {
            Ok(m) => Ok(Blob::Mapped(m)),
            Err(_) => {
                use std::io::Read;
                let mut out = Vec::new();
                let mut f = f.try_clone()?;
                f.read_to_end(&mut out)?;
                Ok(Blob::Owned(out.into_boxed_slice()))
            }
        }
    }
}

/// The write side of [`Blob`]'s build path: an anonymous map where the OS
/// grants one (the big-block rule — returned to the OS on drop), a plain
/// Vec where it does not (Windows under commit pressure must not panic a
/// 5M-name build).
pub enum BlobMut {
    /// The anonymous map. The `std` path.
    #[cfg(feature = "std")]
    Mapped(MmapMut),
    Owned(Vec<u8>),
}

impl core::ops::Deref for BlobMut {
    type Target = [u8];
    fn deref(&self) -> &[u8] {
        match self {
            #[cfg(feature = "std")]
            BlobMut::Mapped(m) => m,
            BlobMut::Owned(v) => v,
        }
    }
}

impl core::ops::DerefMut for BlobMut {
    fn deref_mut(&mut self) -> &mut [u8] {
        match self {
            #[cfg(feature = "std")]
            BlobMut::Mapped(m) => m,
            BlobMut::Owned(v) => v,
        }
    }
}

impl BlobMut {
    #[cfg(feature = "std")]
    pub fn anon(len: usize) -> BlobMut {
        match MmapMut::map_anon(len) {
            Ok(m) => BlobMut::Mapped(m),
            Err(_) => BlobMut::Owned(vec![0u8; len]),
        }
    }

    /// The same buffer, read-only.
    #[cfg(not(feature = "std"))]
    pub fn anon(len: usize) -> BlobMut {
        BlobMut::Owned(vec![0u8; len])
    }

    pub fn freeze(self) -> Blob {
        match self {
            #[cfg(feature = "std")]
            BlobMut::Mapped(m) => Blob::Mapped(m.make_read_only().expect("freeze map")),
            BlobMut::Owned(v) => Blob::Owned(v.into_boxed_slice()),
        }
    }
}

/// The blob's magic.
///
/// `FSIDX014` is `FSIDX013` with the priority tier folded into the entry's
/// kind byte: the tier the entry inherits from its directory rides in the
/// bits the kind byte did not use, so the search reads it with the kind it
/// reads anyway. An index built before that says something else, so it is
/// not loaded: a stale index would rank every entry by the tier its kind
/// byte used to hold, and the answer would depend on whether the index had
/// been rebuilt.
///
/// `FSIDX013` is `FSIDX012` with the ranking adjustment folded into the
/// record: an entry carries the location prior it inherits from its
/// directory, so a search reads it per entry instead of walking to the
/// parent to find it. An index built before that says something else, so
/// it is not loaded: a stale index would rank an entry by a prior the
/// search no longer reads, and the answer would depend on whether the
/// index had been rebuilt.
///
/// `FSIDX012` is `FSIDX011` with the name masks folded: a mask says which
/// character classes a name holds, and the classes are now the ones of the
/// *folded* name (see `crate::fold`), because that is the name a query is
/// compared against. An index built before the fold says something else,
/// so it is not loaded: a stale index would reject a name whose folded
/// form holds a class its raw form does not, and the answer would depend
/// on whether the index had been rebuilt.
/// `FSIDX015` is `FSIDX014` with the root's digest in the header: the blob
/// says which tree it was built for, and not only the sidecar beside it
/// does. The sidecar and the blob are two files, and a crash between the
/// two writes leaves them disagreeing — the sidecar naming the tree the
/// blob was built for before, the blob holding the tree built after. A
/// loader that trusted the sidecar would serve the wrong tree's paths for
/// the right tree. An index built before this says nothing about its
/// root, so it is not loaded: a stale index cannot be checked against the
/// tree it is asked about, and the answer would depend on whether the
/// index had been rebuilt.
const MAGIC: &[u8; 8] = b"FSIDX015";

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

pub struct Index {
    map: Blob,
    pub n: usize,
    pub d: usize,
    /// Distinct names.
    pub u: usize,
    names_len: usize,
    u1: usize,
    /// Words per name bitmap (64 names each), and all bitmaps' words.
    pub words: usize,
    bits_len: usize,
    zones_len: usize,
    ext_slots: usize,
    /// FSEvents id the index is current as of; replay starts here.
    pub event_id: u64,
    /// Wall-clock second the index is known complete as of (0: unknown).
    /// If FSEvents history from `event_id` is gone, folders changed since
    /// then are what needs relisting.
    pub synced_at: u32,
    /// The digest of the root's bytes, or 0 when the index does not say.
    ///
    /// The index is built for a tree, and the tree it was built for is what
    /// makes its paths mean anything. The digest is the tree's, and it is
    /// in the header so that the bytes on disk carry it: a loader can then
    /// check the blob against the tree it is asked about, and not only
    /// against a sidecar that a crash may have left behind.
    ///
    /// Zero means the index does not say, and an index that does not say is
    /// not served for a tree.
    pub root_digest: u64,
    off: [usize; NSEC],
    plan: crate::sync::OnceCell<MemoPlan>,
    counts: crate::sync::OnceCell<[[u32; CLASSES]; 2]>,
    top_prior: crate::sync::OnceCell<i8>,
    by_key: crate::sync::OnceCell<ByKey>,
}

/// Per distinct name, what bounds its score for one fuzzy token without
/// reading the name (see `query::Searcher::ranked`).
#[derive(Clone, Copy, Default)]
#[repr(C)]
pub struct NameInfo {
    /// The best location prior among the name's folders, plus the best rank
    /// tweak its flags allow, less the length penalty.
    pub key: i8,
    /// Length past a leading dot (at most 255).
    pub len: u8,
    /// Stem length (up to the last dot) past a leading dot (at most 255).
    pub stem: u8,
    /// First byte past a leading dot, ASCII-folded.
    pub head: u8,
}

/// The names with the highest `NameInfo::key`, best first: every name whose
/// key is at least `cover`, and how many have each key from `cover` up.
pub struct ByKey {
    pub cover: i32,
    pub list: Vec<(i8, u32)>,
    pub counts: Vec<u32>,
    /// Per `NameInfo::head`: its names in `list`, in order.
    pub heads: Vec<Vec<u32>>,
    /// Per `NameInfo::head`: the names whose length or stem is at most
    /// SHORT, the ones a token that short can match whole.
    pub short: Vec<Vec<u32>>,
}

/// See `ByKey::short`.
pub const SHORT: usize = 3;

/// How to fold per-dir data down the tree in parallel (see `memo_plan`).
pub struct MemoPlan {
    /// Dirs to do one by one, parents first.
    pub upper: Vec<u32>,
    /// (dir, its descendants' id range): the range's parents are the dir or
    /// inside the range, so each runs on its own once `upper` is done.
    pub chunks: Vec<(u32, core::ops::Range<u32>)>,
}

/// A typed view of one section of `self.map`, `self.$len` long. Shared with
/// content segments, which use the same file layout (see `layout`).
macro_rules! sec {
    ($name:ident, $s:expr, $t:ty, $len:ident) => {
        pub fn $name(&self) -> &[$t] {
            unsafe { core::slice::from_raw_parts(self.map.as_ptr().add(self.off[$s as usize]) as *const $t, self.$len) }
        }
    };
}
// Re-exported so the content index can name the same section macro (see
// `content.rs`), which reads the same blob internals.
#[cfg(feature = "std")]
pub(crate) use sec;

impl Index {
    // Per distinct name: the name bitmaps (see `bitmap`), offset into
    // `names` (u + 1 entries).
    sec!(name_bits, Sec::NameBits, u64, bits_len);
    sec!(name_off, Sec::NameOff, u32, u1);
    sec!(names, Sec::Names, u8, names_len);
    // Per entry: its name id, kind (walk::KIND_* | walk::FLAG_*), dir id of
    // its parent, size (see size_of), mtime.
    sec!(ent_name, Sec::EntName, u32, n);
    sec!(kind, Sec::Kind, u8, n);
    // Per entry: the location prior it inherits from its directory. The
    // search reads this per entry, so it lives beside the entry rather
    // than one hop away in the directory table.
    sec!(rank_adj, Sec::RankAdj, i8, n);
    sec!(parent, Sec::Parent, u32, n);
    sec!(size_raw, Sec::Size, u32, n);
    sec!(mtime, Sec::Mtime, u32, n);
    // Per dir: entry index (ascending, so parents precede children), block
    // of children, end of the whole subtree range, location prior.
    sec!(dir_entry, Sec::DirEntry, u32, d);
    sec!(dir_start, Sec::DirStart, u32, d);
    sec!(dir_len, Sec::DirLen, u32, d);
    sec!(dir_end, Sec::DirEnd, u32, d);
    sec!(dir_prior, Sec::DirPrior, i8, d);
    sec!(dir_parent, Sec::DirParent, u32, d);
    // Per distinct name, the entries carrying it (best location prior
    // first): a selective query visits only these instead of every entry.
    sec!(name_ents_off, Sec::NameEntsOff, u32, u1);
    sec!(name_ents, Sec::NameEnts, u32, n);
    // Per distinct name, the highest location prior among the folders
    // holding it: bounds what any entry with the name can score.
    sec!(name_prior, Sec::NamePrior, i8, u);
    // Per distinct name, what bounds its score without reading it.
    sec!(name_info, Sec::NameInfo, NameInfo, u);
    // Per distinct name, its extension as an `ext_table` slot: 0 none, 255
    // one not in the table.
    sec!(name_ext, Sec::NameExt, u8, u);
    // The EXT_SLOTS most common extensions among the names, lowercased: slot
    // i is `[len, bytes..]` (slot 0 and 255 unused).
    sec!(ext_table, Sec::Exts, [u8; 16], ext_slots);

    /// The `name_ext` slot of extension `e` (lowercase): 255 if it is not
    /// in the table (names with such an extension have to be read).
    pub fn ext_slot(&self, e: &[u8]) -> u8 {
        let t = self.ext_table();
        (1..255).find(|&i| t[i][0] as usize == e.len() && &t[i][1..1 + e.len()] == e).unwrap_or(255) as u8
    }
    // Per ZONE entries: what a filter on size, mtime or kind can skip.
    sec!(zones, Sec::Zones, Zone, zones_len);

    /// Name bitmap `b` (see `BM_FIRST` and on): bit `k % 64` of word `k / 64`
    /// is set when name `k` has the property.
    pub fn bitmap(&self, b: usize) -> &[u64] {
        &self.name_bits()[b * self.words..][..self.words]
    }

    /// How many names have each char class: a query checks rare ones first.
    pub fn class_counts(&self) -> &[u32; CLASSES] {
        &self.counts()[0]
    }

    /// How many names have each char class at least twice.
    pub fn double_counts(&self) -> &[u32; CLASSES] {
        &self.counts()[1]
    }

    fn counts(&self) -> &[[u32; CLASSES]; 2] {
        self.counts.get_or_init(|| {
            let count = |b: &[u64]| b.iter().map(|x| x.count_ones()).sum();
            [
                core::array::from_fn(|c| self.bitmap(BM_FIRST + c).iter().zip(self.bitmap(BM_SECOND + c)).map(|(a, b)| (a | b).count_ones()).sum()),
                core::array::from_fn(|c| count(self.bitmap(BM_DOUBLE + c))),
            ]
        })
    }

    /// The highest `name_prior`.
    pub fn top_prior(&self) -> i8 {
        *self.top_prior.get_or_init(|| self.name_prior().iter().copied().max().unwrap_or(0))
    }

    /// The names with the highest keys (see `ByKey`).
    pub fn by_key(&self) -> &ByKey {
        self.by_key.get_or_init(|| {
            const WANT: usize = 1 << 14;
            let info = self.name_info();
            let mut count = [0usize; 256];
            for x in info {
                count[(x.key as i32 + 128) as usize] += 1;
            }
            // The lowest key such that all names at or above it fit.
            let (mut cover, mut n) = (i8::MAX as i32 + 1, 0);
            while cover > i8::MIN as i32 && n + count[(cover - 1 + 128) as usize] <= WANT {
                cover -= 1;
                n += count[(cover + 128) as usize];
            }
            let (mut list, mut short) = (Vec::new(), vec![Vec::new(); 256]);
            for (k, x) in info.iter().enumerate() {
                if x.key as i32 >= cover {
                    list.push((x.key, k as u32));
                }
                if x.len as usize <= SHORT || x.stem as usize <= SHORT {
                    short[x.head as usize].push(k as u32);
                }
            }
            list.sort_unstable_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
            let mut heads = vec![Vec::new(); 256];
            for &(_, k) in &list {
                heads[info[k as usize].head as usize].push(k);
            }
            let counts = (cover..=i8::MAX as i32).map(|k| count[(k + 128) as usize] as u32).collect();
            ByKey { cover, list, counts, heads, short }
        })
    }

    pub fn uname(&self, id: u32) -> &[u8] {
        let o = self.name_off();
        &self.names()[o[id as usize] as usize..o[id as usize + 1] as usize]
    }

    /// The 64 bytes of the map from name `id` on, if the name is no longer
    /// and the map goes that far: SIMD reads them whole and masks off what
    /// follows the name.
    #[inline]
    pub fn uname_wide(&self, id: u32) -> Option<&[u8; 64]> {
        let o = self.name_off();
        let (a, b) = (o[id as usize] as usize, o[id as usize + 1] as usize);
        let at = self.off[Sec::Names as usize] + a;
        (b - a <= 64).then(|| self.map.get(at..at + 64)?.try_into().ok()).flatten()
    }

    /// The name of distinct name `id`, folded for comparison, and the
    /// 64-byte window the wide scorer reads.
    ///
    /// The fold is what makes a name typed in NFC match one the filesystem
    /// stored in NFD. The index keeps the bytes the
    /// filesystem gave: the name that is displayed and the path that is
    /// opened are the original ones, and the fold is a view for comparison
    /// only.
    ///
    /// The window is the index's own bytes when the fold left the name
    /// alone. A name the fold changed has no window in the index, so the
    /// wide scorer reads it the other way (`fuzzy_score_capped` copies the
    /// name into a buffer of its own).
    pub fn uname_folded(&self, id: u32) -> (Cow<'_, [u8]>, Option<&[u8; 64]>) {
        let raw = self.uname(id);
        match fold::fold(raw) {
            Cow::Borrowed(_) => (Cow::Borrowed(raw), self.uname_wide(id)),
            Cow::Owned(folded) => (Cow::Owned(folded), None),
        }
    }

    pub fn name(&self, i: usize) -> &[u8] {
        self.uname(self.ent_name()[i])
    }

    /// The entries that carry the distinct name `id`.
    ///
    /// The name table is the index's own: a name is stored once and the
    /// entries that carry it point at it. This is what the `file_name` and
    /// `folder_name` lookups of the lookup surface read.
    pub fn name_ents_of(&self, id: u32) -> Vec<u32> {
        let (s, e) = (self.name_ents_off()[id as usize] as usize, self.name_ents_off()[id as usize + 1] as usize);
        self.name_ents()[s..e].to_vec()
    }

    pub fn size_of(&self, i: usize) -> u64 {
        dec_size(self.size_raw()[i])
    }

    pub fn dir_of(&self, entry: u32) -> Option<u32> {
        self.dir_entry().binary_search(&entry).ok().map(|d| d as u32)
    }

    pub fn children(&self, d: u32) -> core::ops::Range<usize> {
        let s = self.dir_start()[d as usize] as usize;
        s..s + self.dir_len()[d as usize] as usize
    }

    /// Dir ids of `k`'s strict descendants: one contiguous range, since dir
    /// ids follow entry order and a subtree's entries are contiguous.
    pub fn descendants(&self, k: u32) -> core::ops::Range<u32> {
        let de = self.dir_entry();
        let (s, e) = (self.dir_start()[k as usize], self.dir_end()[k as usize]);
        de.partition_point(|&x| x < s) as u32..de.partition_point(|&x| x < e) as u32
    }

    /// Split the dir tree for parallel top-down folding: subtrees of at most
    /// ~4k dirs become chunks; the children of bigger dirs go in `upper`.
    pub fn memo_plan(&self) -> &MemoPlan {
        self.plan.get_or_init(|| {
            const CHUNK: u32 = 4096;
            let de = self.dir_entry();
            let mut p = MemoPlan { upper: Vec::new(), chunks: Vec::new() };
            let mut big = vec![0u32];
            while let Some(k) = big.pop() {
                // k's child dirs: the dirs whose entry is in k's children block.
                let (s, l) = (self.dir_start()[k as usize], self.dir_len()[k as usize]);
                let kids = de.partition_point(|&x| x < s) as u32..de.partition_point(|&x| x < s + l) as u32;
                for c in kids {
                    p.upper.push(c);
                    let r = self.descendants(c);
                    if r.end - r.start > CHUNK {
                        big.push(c)
                    } else if r.start < r.end {
                        p.chunks.push((c, r))
                    }
                }
            }
            p.chunks.sort_by_key(|c| c.1.start);
            p
        })
    }

    pub fn path(&self, i: usize, out: &mut Vec<u8>) {
        out.clear();
        if i == 0 {
            out.push(b'/');
            return;
        }
        // The entries from i up: the walk goes folder to parent folder (one
        // dependent load a level); their entries and names load alongside.
        let (de, dp) = (self.dir_entry(), self.dir_parent());
        // Deep enough for any real tree without allocating. This used to be
        // the only buffer, and the walk stopped when it filled: a path nested
        // deeper than 256 came back missing its outermost components, with
        // nothing saying it had been cut. Plausible, and wrong.
        const SHALLOW: usize = 256;
        let mut inline = [0u32; SHALLOW];
        inline[0] = i as u32;
        let (mut k, mut d) = (1, self.parent()[i]);
        while d != 0 && k < SHALLOW {
            inline[k] = de[d as usize];
            k += 1;
            d = dp[d as usize];
        }
        if d == 0 {
            self.emit_chain(&inline[..k], out);
            return;
        }
        // Past the inline buffer: finish in a Vec. A tree this deep is rare
        // (generated build trees, nested package installs) and allocating for
        // it costs far less than the truncation it replaces.
        let mut chain = inline[..k].to_vec();
        while d != 0 {
            chain.push(de[d as usize]);
            d = dp[d as usize];
        }
        self.emit_chain(&chain, out);
    }

    /// Write a root-to-leaf chain of entry indices as an absolute path.
    fn emit_chain(&self, chain: &[u32], out: &mut Vec<u8>) {
        for &e in chain.iter().rev() {
            out.push(b'/');
            out.extend_from_slice(self.name(e as usize));
        }
    }

    /// Resolve an absolute path to its entry.
    pub fn lookup(&self, path: &[u8]) -> Option<u32> {
        let mut e = 0u32;
        for comp in path.split(|&b| b == b'/').filter(|c| !c.is_empty()) {
            let r = self.children(self.dir_of(e)?);
            let (mut lo, mut hi) = (r.start, r.end);
            while lo < hi {
                let mid = (lo + hi) / 2;
                if self.name(mid) < comp { lo = mid + 1 } else { hi = mid }
            }
            e = if lo < r.end && self.name(lo) == comp {
                lo as u32
            } else {
                // APFS is case-insensitive by default; callers may not be.
                r.clone().find(|&c| self.name(c).eq_ignore_ascii_case(comp))? as u32
            };
        }
        Some(e)
    }

    /// Lay listings out as blocks in DFS order and compute everything derived.
    /// Listing id 0 is the root ("/").
    ///
    /// An empty `ls` builds the one-entry index that describes just the root:
    /// an empty directory, or a root that could not be listed at all — which
    /// is what pointing a CLI at a path that does not exist produces, since
    /// the walk yields nothing rather than failing. Without this the DFS
    /// starts from `by_id[0] == NONE` and indexes past the end of an empty
    /// vector.
    pub fn build(ls: Vec<Listing>, event_id: u64, synced_at: u32, home: &[u8]) -> Index {
        Index::build_with_rules(ls, event_id, synced_at, home, &crate::rules::Rules::defaults())
    }

    /// The index with the user's priority rules.
    ///
    /// The rules are read here and baked into the index: a record's tier is
    /// what its folder is worth, and the search reads that per candidate
    /// instead of walking the rules again.
    pub fn build_with_rules(mut ls: Vec<Listing>, event_id: u64, synced_at: u32, home: &[u8], rules: &crate::rules::Rules) -> Index {
        rgy_par::for_each_mut(&mut ls, |_, l| {
            let names = &l.names;
            l.ents.sort_unstable_by(|a, b| {
                names[a.name_off as usize..][..a.name_len as usize].cmp(&names[b.name_off as usize..][..b.name_len as usize])
            })
        });
        // Listing 0 is the root: the DFS below starts from `by_id[0]`, so a
        // stream with no listing claiming that id would index past the end.
        // `scan` always stamps it, but `build` takes a bare `Vec` and is
        // called from tests and live-update paths too, so it is made true here
        // rather than assumed.
        if ls.is_empty() {
            // A scan of a root that listed nothing — a path that does not
            // exist, a file where a directory was expected — is still a tree
            // with a root in it, just one with no entries.
            ls.push(Listing { id: 0, names: Vec::new(), ents: Vec::new() });
        }
        let max_id = ls.iter().map(|l| l.id).max().unwrap_or(0) as usize;
        let mut by_id = vec![NONE; max_id + 1];
        for (i, l) in ls.iter().enumerate() {
            by_id[l.id as usize] = i as u32;
        }
        debug_assert_ne!(by_id[0], NONE, "the DFS below starts from `by_id[0]`");
        let n_max = 1 + ls.iter().map(|l| l.ents.len()).sum::<usize>();
        let names_max: usize = ls.iter().map(|l| l.names.len()).sum();

        // Pass 1: DFS layout into plain vectors (sizes are only upper bounds
        // until unreachable listings are known).
        let mut name_off = vec![0u32; n_max];
        let mut name_len = vec![0u16; n_max];
        let mut kind = vec![0u8; n_max];
        let mut parent = vec![0u32; n_max];
        let mut size = vec![0u64; n_max];
        let mut mtime = vec![0u32; n_max];
        let mut names = Vec::with_capacity(names_max);
        kind[0] = KIND_DIR;
        let mut pos = 1usize;
        let mut blocks: Vec<(u32, u32, u32)> = Vec::with_capacity(ls.len()); // (dir entry, start, len)
        let mut stack: Vec<(u32, u32)> = vec![(0, by_id[0])];
        let mut kids: Vec<(u32, u32)> = Vec::new();
        while let Some((e, li)) = stack.pop() {
            let l = &ls[li as usize];
            let start = pos;
            kids.clear();
            for r in &l.ents {
                name_off[pos] = names.len() as u32;
                name_len[pos] = r.name_len;
                names.extend_from_slice(&l.names[r.name_off as usize..][..r.name_len as usize]);
                kind[pos] = r.kind;
                size[pos] = r.size;
                mtime[pos] = r.mtime;
                parent[pos] = e; // entry index for now, converted below
                if let Some(&li) = by_id.get(r.child as usize).filter(|&&li| li != NONE) {
                    kids.push((pos as u32, li));
                }
                pos += 1;
            }
            blocks.push((e, start as u32, (pos - start) as u32));
            stack.extend(kids.iter().rev());
        }
        drop(ls);
        let n = pos;
        blocks.sort_unstable();
        let d = blocks.len();
        let mut dir_of = vec![NONE; n];
        for (k, b) in blocks.iter().enumerate() {
            dir_of[b.0 as usize] = k as u32;
        }
        rgy_par::for_each_mut(&mut parent[..n], |_, p| *p = dir_of[*p as usize]);
        parent[0] = 0;
        drop(dir_of);

        // Intern names in entry order.
        let mut ids: Map<&[u8], u32> = Map::with_capacity(n / 3);
        let mut ent_name = vec![0u32; n];
        let mut uoff = vec![0u32];
        let mut unames: Vec<u8> = Vec::with_capacity(names.len() / 2);
        for i in 0..n {
            let nm = &names[name_off[i] as usize..][..name_len[i] as usize];
            let next = ids.len() as u32;
            let id = *ids.get_or_insert_with(nm, || {
                unames.extend_from_slice(nm);
                uoff.push(unames.len() as u32);
                next
            });
            ent_name[i] = id;
        }
        let u = ids.len();
        drop(ids);
        drop((names, name_off, name_len));
        let words = u.div_ceil(64);
        // Each 64-name block's word of every bitmap, then transposed so each
        // bitmap is contiguous.
        let mut by_block = vec![0u64; words * NBITMAPS];
        rgy_par::for_each_owned(rgy_par::split_mut(&mut by_block, NBITMAPS), |w, out| {
            for k in w * 64..(w * 64 + 64).min(u) {
                // The bitmaps describe the folded name: they are what a
                // query's classes are tested against, and a query is
                // compared against the folded name.
                let folded = fold::fold(&unames[uoff[k] as usize..uoff[k + 1] as usize]);
                for b in name_bitmaps(&folded) {
                    out[b] |= 1 << (k % 64);
                }
            }
        });
        let mut ubits = vec![0u64; words * NBITMAPS];
        rgy_par::for_each_owned(rgy_par::split_mut(&mut ubits, words), |b, out| {
            for (w, o) in out.iter_mut().enumerate() {
                *o = by_block[w * NBITMAPS + b];
            }
        });
        drop(by_block);
        let enc: Vec<u32> = size[..n].iter().map(|&s| enc_size(s)).collect();
        drop(size);
        // Entries grouped by name (counting sort keeps them ascending).
        let mut ne_off = vec![0u32; u + 1];
        for &id in &ent_name {
            ne_off[id as usize + 1] += 1;
        }
        for k in 0..u {
            ne_off[k + 1] += ne_off[k];
        }
        let mut fill = ne_off[..u].to_vec();
        let mut ne = vec![0u32; n];
        for (i, &id) in ent_name.iter().enumerate() {
            ne[fill[id as usize] as usize] = i as u32;
            fill[id as usize] += 1;
        }
        drop(fill);

        // Subtree end: own block end, folded upward (children have larger ids).
        let dir_entry: Vec<u32> = blocks.iter().map(|b| b.0).collect();
        let mut end: Vec<u32> = blocks.iter().map(|b| b.1 + b.2).collect();
        for k in (1..d).rev() {
            let p = parent[dir_entry[k] as usize] as usize;
            end[p] = end[p].max(end[k]);
        }
        let comps: Vec<&[u8]> = home.split(|&b| b == b'/').filter(|c| !c.is_empty()).collect();
        let mut depth = vec![0u8; d];
        // How many leading components of `home` this dir's path matches;
        // u8::MAX once it diverges.
        let mut hm = vec![0u8; d];
        // The level each dir is worth, and the level the files under it are
        // worth: a rule says who it is for, so there are two. A dir takes
        // the level of the closest rule in its chain that is for folders,
        // and the files under it the closest that is for files.
        let mut folder_level = vec![crate::rank::Tier::Normal; d];
        let mut file_level = vec![crate::rank::Tier::Normal; d];
        let uname_levels: Vec<(Option<crate::rank::Tier>, Option<crate::rank::Tier>)> =
            rgy_par::map_collect(u, |k| rules.levels_of(&unames[uoff[k] as usize..uoff[k + 1] as usize]));
        // The level each entry is in, and the offset it is worth in the
        // score. The level is the tier the ranking compares; the offset is
        // what the score says about two entries in the same tier.
        let mut level = vec![crate::rank::Tier::Normal; n];
        let mut prior = vec![0i8; n];
        // The home bonus each dir carries, inherited by the dirs under it.
        //
        // The bonus says "this is under the user's home", and everything
        // under the home is under it: the old prior inherited it down the
        // tree, and the rules replaced that inheritance with their own
        // (the level, which is inherited too) -- and the bonus with it.
        let mut home_bonus = vec![0i8; d];
        for k in 1..d {
            let e = dir_entry[k] as usize;
            let p = parent[e] as usize;
            let uid = ent_name[e] as usize;
            let nm = &unames[uoff[uid] as usize..uoff[uid + 1] as usize];
            depth[k] = depth[p].saturating_add(1);
            hm[k] = match hm[p] {
                u8::MAX => u8::MAX,
                h if h as usize >= comps.len() => h,
                h if comps[h as usize] == nm => h + 1,
                _ => u8::MAX,
            };
            let entered_home = hm[k] as usize == comps.len() && (hm[p] as usize) < comps.len();
            let (own_folder, own_file) = uname_levels[uid];
            folder_level[k] = own_folder.unwrap_or(folder_level[p]);
            file_level[k] = own_file.unwrap_or(file_level[p]);
            home_bonus[k] = if entered_home { 15 } else { home_bonus[p] };
            // The dir itself, and the files under it.
            level[e] = folder_level[k];
            prior[e] = (crate::rules::offset(folder_level[k]) + home_bonus[k] as i32).clamp(-100, 60) as i8;
        }
        // The files: their own name's rule, or the level of the folder they
        // are in.
        for i in 0..n {
            if kind[i] & 3 == KIND_DIR || i == 0 {
                continue;
            }
            let p = parent[i] as usize;
            let lvl = uname_levels[ent_name[i] as usize].1.unwrap_or(file_level[p]);
            level[i] = lvl;
            prior[i] = (crate::rules::offset(lvl) + home_bonus[p] as i32).clamp(-100, 60) as i8;
        }
        drop(uname_levels);

        // The location prior each entry inherits from its directory, folded
        // into the record. The search reads this per entry, and reading it
        // from the directory table means a dependent load -- the entry's
        // parent, then that parent's prior -- in the hottest loop there
        // is. The reference's record store keeps the same thing in the record
        // (its `record_store.rs` is the largest module in its engine), and
        // the reason is the same: a ranking property that is read per
        // candidate belongs to the candidate.
        let rank_adj = prior;

        // The priority tier each entry is in, in the kind byte's free bits
        // (5 to 7).
        //
        // The tier is the level the rules gave the entry, and the search
        // reads it once per candidate. Computing it there cost more than
        // the whole rest of the ranking key -- 13% of the name bench,
        // measured -- because the prior it comes from is a load and the
        // tier is a chain of compares on top of it, right on the path to
        // the key's comparison. The kind byte is loaded anyway, so the tier
        // rides along in it, and it sits in the bits the key wants it in,
        // so the search does not unpack it and repack it.
        //
        // The overlay's entries have no index to carry it: the search
        // derives their tier from the prior they carry themselves.
        for i in 0..n {
            kind[i] |= (level[i].bits() as u8) << 5;
        }
        drop(level);

        // Write the blob, freeing each array once its last reader is done:
        // the build's peak is here, the blob beside the arrays it copies.
        let lens = section_lens(n, d, u, unames.len()).expect("the sizes fit in a usize");
        let (off, total) = layout_checked(&lens).expect("the built index's offsets fit in a usize");
        let mut m = BlobMut::anon(total);
        let mut put = |s: Sec, v: &[u8]| m[off[s as usize]..][..v.len()].copy_from_slice(v);
        put(Sec::NameBits, as_bytes(&ubits));
        drop(ubits);
        put(Sec::NameOff, as_bytes(&uoff));
        put(Sec::Names, &unames);
        put(Sec::EntName, as_bytes(&ent_name));
        put(Sec::Kind, &kind[..n]);
        put(Sec::RankAdj, as_bytes(&rank_adj));
        put(Sec::Parent, as_bytes(&parent[..n]));
        put(Sec::Size, as_bytes(&enc));
        put(Sec::Mtime, as_bytes(&mtime[..n]));
        put(Sec::Zones, as_bytes(&zones(&kind[..n], &enc, &mtime[..n])));
        drop((kind, enc, mtime));
        put(Sec::DirEntry, as_bytes(&dir_entry));
        put(Sec::DirStart, as_bytes(&blocks.iter().map(|b| b.1).collect::<Vec<_>>()));
        put(Sec::DirLen, as_bytes(&blocks.iter().map(|b| b.2).collect::<Vec<_>>()));
        drop(blocks);
        put(Sec::DirEnd, as_bytes(&end));
        drop(end);
        // The prior of each DIR, not of each entry: the section is read per
        // directory (an entry the overlay added takes the prior of the
        // directory it is in), and the entries and the directories are
        // numbered apart. The per-entry prior is in `Sec::RankAdj`.
        put(Sec::DirPrior, as_bytes(&dir_entry.iter().map(|&e| rank_adj[e as usize]).collect::<Vec<_>>()));
        put(Sec::DirParent, as_bytes(&dir_entry.iter().map(|&e| parent[e as usize]).collect::<Vec<_>>()));
        drop(dir_entry);
        // Each name's entries best location prior first (then ascending): a
        // search visiting them can stop at the first that cannot make it.
        rgy_par::sort_unstable_by_key(&mut ne, |&e| (ent_name[e as usize], core::cmp::Reverse(rank_adj[e as usize]), e));
        put(Sec::NameEntsOff, as_bytes(&ne_off));
        put(Sec::NameEnts, as_bytes(&ne));
        drop((ne_off, ne));
        let mut name_prior = vec![i8::MIN; u];
        for (i, &k) in ent_name.iter().enumerate().skip(1) {
            name_prior[k as usize] = name_prior[k as usize].max(rank_adj[i]);
        }
        put(Sec::NamePrior, as_bytes(&name_prior));
        let info: Vec<NameInfo> = rgy_par::map_collect(u, |k| NameInfo::of(&unames[uoff[k] as usize..uoff[k + 1] as usize], name_prior[k]));
        put(Sec::NameInfo, as_bytes(&info));
        let (table, slots) = ext_slots(&unames, &uoff);
        put(Sec::NameExt, &slots);
        put(Sec::Exts, as_bytes(&table));
        // The root's digest is zero here: the build does not know which
        // tree it is building for -- that is the caller's word, and the
        // caller says it when the index is saved (see `root_digest`).
        m[..HDR].copy_from_slice(&header(MAGIC, &[n as u64, d as u64, u as u64, unames.len() as u64, event_id, synced_at as u64, 0]));
        Index::from_blob(m.freeze()).unwrap()
    }

    fn from_blob(map: Blob) -> Option<Index> {
        let [n, d, u, names_len, event_id, synced_at, root_digest] = fields(&map, MAGIC)?;
        // The header is bytes that may be corrupt — a truncated file, a
        // file another version wrote, a file on a failing disk — and every
        // number in it becomes a size. The arithmetic is checked here, so
        // a count that does not make sense is refused instead of wrapping
        // into a small one: a wrapped count is a section read out of the
        // map, and past its end.
        let n = usize::try_from(n).ok()?;
        let d = usize::try_from(d).ok()?;
        let u = usize::try_from(u).ok()?;
        let names_len = usize::try_from(names_len).ok()?;
        let synced_at = u32::try_from(synced_at).ok()?;
        let u1 = u.checked_add(1)?;
        let words = u.div_ceil(64);
        let bits_len = words.checked_mul(NBITMAPS)?;
        let (off, total) = layout_checked(&section_lens(n, d, u, names_len)?)?;
        if map.len() < total {
            return None;
        }
        Some(Index {
            n,
            d,
            u,
            u1,
            names_len,
            words,
            bits_len,
            zones_len: n.div_ceil(ZONE),
            ext_slots: 256,
            event_id,
            synced_at,
            root_digest,
            off,
            map,
            plan: crate::sync::OnceCell::new(),
            counts: crate::sync::OnceCell::new(),
            top_prior: crate::sync::OnceCell::new(),
            by_key: crate::sync::OnceCell::new(),
        })
    }

    /// The index in bytes: the header and the payload, exactly as [`save`]
    /// writes them.
    ///
    /// Portable: this is the *format*, without the filesystem. A caller
    /// that has the bytes — over a pipe, in a message, in an image built
    /// elsewhere — can build the index with [`Index::from_bytes`], and the
    /// bytes are the same ones the daemon writes to disk.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.map.len());
        out.extend_from_slice(&header(
            MAGIC,
            &[self.n as u64, self.d as u64, self.u as u64, self.names_len as u64, self.event_id, self.synced_at as u64, self.root_digest],
        ));
        out.extend_from_slice(&self.map[HDR..]);
        out
    }

    /// The index from the bytes [`Index::to_bytes`] writes.
    ///
    /// The bytes are copied: the index owns its map, and a mapped file is
    /// the cheaper way to own one (see [`Blob`]). `None` when the bytes are
    /// not an index — a wrong magic, or a truncated payload.
    pub fn from_bytes(bytes: &[u8]) -> Option<Index> {
        Index::from_blob(Blob::Owned(bytes.to_vec().into_boxed_slice()))
    }

    /// The event id an index is current as of, from the header alone.
    ///
    /// The header is 64 bytes of the file's first 4 KiB, so this answers
    /// "is the saved index current?" without reading the payload.
    pub fn header_event_id(bytes: &[u8]) -> Option<u64> {
        fields::<7>(bytes, MAGIC).map(|f| f[4])
    }

    /// The digest of the root this index was built for, from the header
    /// alone.
    ///
    /// The header is 64 bytes of the file's first 4 KiB, so this answers
    /// "is this blob the tree I am asking about?" without reading the
    /// payload -- and the payload is the part that costs.
    pub fn header_root_digest(bytes: &[u8]) -> Option<u64> {
        fields::<7>(bytes, MAGIC).map(|f| f[6])
    }

    /// Say which tree this index is for.
    ///
    /// The digest is the tree's, and the tree's bytes are the caller's:
    /// the caller is the one that knows the root, and the index is built
    /// for a tree, not for a file. A digest of zero means the index does
    /// not say, and an index that does not say is not served for a tree.
    ///
    /// The stamp happens when the index is saved, because the root is
    /// known then and not before: the build walks the tree and knows
    /// nothing about which one it walked.
    pub fn set_root_digest(&mut self, digest: u64) {
        self.root_digest = digest;
    }

    /// Write atomically (tmp + rename), stamping the current event id.
    ///
    /// The std path: writing the disk is what this is for.
    #[cfg(feature = "std")]
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        let tmp = path.with_extension("tmp");
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(&self.to_bytes())?;
        f.sync_data()?;
        std::fs::rename(tmp, path)
    }

    /// The event id a saved index is current as of, from its header alone.
    ///
    /// The std path: reading the disk is what this is for.
    #[cfg(feature = "std")]
    pub fn saved_event_id(path: &Path) -> Option<u64> {
        use std::io::Read;
        let mut h = [0u8; 64];
        std::fs::File::open(path).ok()?.read_exact(&mut h).ok()?;
        Index::header_event_id(&h)
    }

    /// The index in a file, mapped.
    ///
    /// The std path: reading the disk is what this is for.
    #[cfg(feature = "std")]
    pub fn load(path: &Path) -> Option<Index> {
        let f = std::fs::File::open(path).ok()?;
        // mmap first (random-access bitmaps page in on demand), owned
        // bytes when mapping fails (Windows sharing/commit limits, empty
        // files) — rgx lesson: a map failure is not "no index".
        Index::from_blob(Blob::from_file(&f).ok()?)
    }

    pub fn bytes(&self) -> usize {
        self.map.len()
    }

    /// Fault in the arrays every query scans so the first one is fast; the
    /// rest (sizes, mtimes, dir tables) page in on demand.
    pub fn prefault(&self) {
        let mut sum = 0u8;
        for s in [Sec::NameBits, Sec::NameOff, Sec::Names, Sec::EntName, Sec::Kind, Sec::Parent] {
            let start = self.off[s as usize];
            let end = self.off.get(s as usize + 1).copied().unwrap_or(self.map.len());
            for i in (start..end).step_by(16 * 1024) {
                sum = sum.wrapping_add(unsafe { core::ptr::read_volatile(self.map.as_ptr().add(i)) });
            }
        }
        core::hint::black_box(sum);
        // Built here rather than by the first search that needs them (~5
        // ms each), which after a compaction would be the next keystroke.
        self.class_counts();
        self.top_prior();
        self.by_key();
        self.memo_plan();
    }

    /// Check the parent chain: the index is a forest, and a forest that is
    /// not a forest is an index that would answer with paths that do not
    /// exist.
    ///
    /// The four faults are the ones The reference's path index names (`REVERSE_
    /// ENGINEERING_REPORT.md` §4.5): a broken chain, a dead ancestor, a
    /// wrong record kind and a cycle. rgy's chain is the directory table
    /// (`DirParent`), one pointer per folder, and the entry table
    /// (`Parent`), one pointer per entry:
    ///
    /// * **broken chain** -- a parent pointer that names a folder that is
    ///   not there;
    /// * **dead ancestor** -- a parent that the live set has killed: the
    ///   entry survives its own folder;
    /// * **wrong record kind** -- a parent that is not a folder;
    /// * **cycle** -- a chain that never reaches the root.
    ///
    /// The walk is bounded by the number of folders, so a cycle is caught
    /// rather than followed forever. The check is O(d): each folder's
    /// parent is visited once, because a parent always has a smaller id
    /// than its child (the tree is written in depth-first order).
    pub fn validate_forest(&self) -> Result<(), ForestFault> {
        check_forest(self.dir_parent(), self.dir_entry(), self.kind(), self.parent(), self.n, self.d)
    }
}

/// Check a parent chain: `dp` is each folder's parent folder, `de` each
/// folder's entry, `kind` each entry's kind, `parent` each entry's parent
/// folder.
///
/// The check is a single pass because the tree is written in depth-first
/// order: a parent always has a smaller id than its child, so the parent's
/// side is known when the child is reached. A chain that breaks that rule
/// cannot reach the root -- it is a cycle, or it points forward, which is
/// the same defect seen from the other side.
fn check_forest(dp: &[u32], de: &[u32], kind: &[u8], parent: &[u32], n: usize, d: usize) -> Result<(), ForestFault> {
    for (k, &p_raw) in dp.iter().enumerate().take(d).skip(1) {
        let p = p_raw as usize;
        if p >= d {
            return Err(ForestFault::BrokenChain { folder: k as u32, parent: p_raw });
        }
        if p >= k {
            // The parent is not before the child: the chain is a cycle (or
            // points forward, which is the same defect seen from the other
            // side).
            return Err(ForestFault::Cycle { folder: k as u32 });
        }
        // The parent's own entry must be a folder.
        let pe = de[p] as usize;
        if pe >= n || kind[pe] & 3 != KIND_DIR {
            return Err(ForestFault::WrongKind { folder: k as u32, parent: p as u32 });
        }
    }
    // The entries' own parents must name a folder that exists.
    for (i, &p_raw) in parent.iter().enumerate().take(n) {
        let p = p_raw as usize;
        if p >= d {
            return Err(ForestFault::BrokenChain { folder: i as u32, parent: p_raw });
        }
        let pe = de[p] as usize;
        if pe >= n || kind[pe] & 3 != KIND_DIR {
            return Err(ForestFault::WrongKind { folder: i as u32, parent: p as u32 });
        }
    }
    Ok(())
}

/// What is wrong with an index's parent chain.
///
/// The four faults are the reference's (`core_db/path_index.rs`): a broken chain,
/// a dead ancestor, a wrong record kind and a cycle. rgy's chain is the
/// directory table, so the dead-ancestor fault is the live set's to raise
/// (see [`crate::live::Live::validate_forest`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ForestFault {
    /// A parent pointer names a folder that is not there.
    BrokenChain { folder: u32, parent: u32 },
    /// A parent is not a folder.
    WrongKind { folder: u32, parent: u32 },
    /// The chain never reaches the root.
    Cycle { folder: u32 },
    /// An entry survives a folder the live set killed.
    DeadAncestor { entry: u32, folder: u32 },
}

impl core::fmt::Display for ForestFault {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            ForestFault::BrokenChain { folder, parent } => write!(f, "broken parent chain: {folder} names the folder {parent}, which is not there"),
            ForestFault::WrongKind { folder, parent } => write!(f, "wrong record kind: the parent {parent} of {folder} is not a folder"),
            ForestFault::Cycle { folder } => write!(f, "cycle detected: the chain of {folder} does not reach the root"),
            ForestFault::DeadAncestor { entry, folder } => write!(f, "dead ancestor: the entry {entry} survives the folder {folder}, which is gone"),
        }
    }
}

/// Section files (this index, content segments): a 4 KiB header (magic,
/// then u64 fields), then the sections, each 64-byte aligned.
pub(crate) const HDR: usize = 4096;

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

/// Section offsets for these lengths, and the file size.
///
/// The content segments read the same layout, and they are the `std`
/// half of it: the name index's own layout is the checked one (see
/// `layout_checked`), because the name index is the half that reads
/// bytes that may be corrupt.
#[cfg(feature = "std")]
pub(crate) fn layout<const N: usize>(lens: &[usize; N]) -> ([usize; N], usize) {
    let mut off = [0usize; N];
    let mut at = HDR;
    for (k, &l) in lens.iter().enumerate() {
        off[k] = at;
        at = (at + l + 63) & !63;
    }
    (off, at)
}

/// `layout`, with the arithmetic checked.
///
/// The offsets are what every section is read at, and a header that does
/// not make sense must not wrap into an offset that points somewhere
/// else: `None` is the header refusing to make sense.
pub(crate) fn layout_checked<const N: usize>(lens: &[usize; N]) -> Option<([usize; N], usize)> {
    let mut off = [0usize; N];
    let mut at = HDR;
    for (k, &l) in lens.iter().enumerate() {
        off[k] = at;
        at = (at.checked_add(l)?.checked_add(63)?) & !63;
    }
    Some((off, at))
}

pub(crate) fn as_bytes<T: Copy>(v: &[T]) -> &[u8] {
    unsafe { core::slice::from_raw_parts(v.as_ptr() as *const u8, core::mem::size_of_val(v)) }
}

/// A name's extension (the bytes after its last dot) as `[len, bytes..]`,
/// lowercased; None if it has no dot or a longer extension.
fn ext_key(name: &[u8]) -> Option<[u8; 16]> {
    let dot = name.iter().rposition(|&b| b == b'.')?;
    let e = &name[dot + 1..];
    if e.len() > 15 {
        return None;
    }
    let mut key = [0u8; 16];
    key[0] = e.len() as u8;
    for (k, &b) in key[1..].iter_mut().zip(e) {
        *k = crate::name_query::fold(b);
    }
    Some(key)
}

/// The table of the 254 most common extensions among `names` (each at
/// most 15 bytes) and each name's slot in it (see `Index::name_ext`).
///
/// The extension is the one of the *folded* name: the table prefilters a
/// comparison, and a comparison sees the folded name.
pub(crate) fn ext_slots(names: &[u8], off: &[u32]) -> (Vec<[u8; 16]>, Vec<u8>) {
    let name = |k: usize| &names[off[k] as usize..off[k + 1] as usize];
    let ext = |k: usize| ext_key(&fold::fold(name(k)));
    // The counts, one map per piece of the names, merged at the end: the
    // merge is a walk over the maps, and the pieces are the workers.
    let parts = rgy_par::map_chunks(off.len() - 1, 1 << 16, |r| {
        r.fold(Map::<[u8; 16], u32>::default(), |mut m, k| {
            if let Some(e) = ext(k) {
                *m.get_or_insert_with(e, || 0) += 1;
            }
            m
        })
    });
    let mut count = Map::<[u8; 16], u32>::default();
    for part in parts {
        for (e, n) in part.into_entries() {
            *count.get_or_insert_with(e, || 0) += n;
        }
    }
    let mut common: Vec<([u8; 16], u32)> = count.into_entries().collect();
    common.sort_by(|(a, m), (b, n)| n.cmp(m).then_with(|| a[1..=a[0] as usize].cmp(&b[1..=b[0] as usize])));
    let mut table = vec![[0u8; 16]; 256];
    // The slot of an extension is a sorted vector with a binary search, not
    // a table: it is built once, read once per name, and holds at most 254
    // entries.
    let mut by_ext: Vec<([u8; 16], u8)> = Vec::with_capacity(254);
    for (i, (e, _)) in common.into_iter().take(254).enumerate() {
        table[i + 1] = e;
        by_ext.push((e, i as u8 + 1));
    }
    by_ext.sort_unstable_by_key(|(a, _)| *a);
    let slots = rgy_par::map_collect(off.len() - 1, |k| {
        let n = name(k);
        match ext(k) {
            Some(e) => by_ext.binary_search_by(|(probe, _)| probe.cmp(&e)).map_or(255, |at| by_ext[at].1),
            None => n.contains(&b'.') as u8 * 255,
        }
    });
    (table, slots)
}

impl NameInfo {
    /// The info of a name whose folders' best location prior is `prior`.
    ///
    /// The name is folded first: the length, the stem and the head are
    /// what a comparison sees, and a query typed in NFC
    /// has to line up with a name the filesystem stored in NFD.
    pub fn of(name: &[u8], prior: i8) -> NameInfo {
        let folded = fold::fold(name);
        let name = &folded[..];
        let off = (name.len() > 1 && name[0] == b'.') as usize;
        let stem = name.iter().rposition(|&b| b == b'.').filter(|&p| p > off).unwrap_or(name.len());
        let tweak = 10 + if name.ends_with(b".app") { 25 } else { 0 } - if name.first() == Some(&b'.') { 8 } else { 0 };
        NameInfo {
            key: (prior as i32 + tweak - (name.len() as i32).min(80) / 3).max(i8::MIN as i32) as i8,
            len: (name.len() - off).min(255) as u8,
            stem: (stem - off).min(255) as u8,
            head: name.get(off).map_or(0, |&b| crate::name_query::fold(b)),
        }
    }
}

/// The sizes of the sections, checked.
///
/// The header is read from bytes that may be corrupt, and every number in
/// it becomes a size: the arithmetic has to be checked, or a count of
/// `u64::MAX` wraps into a small one and the reader goes on to read a
/// slice of the wrong size — out of the map, and past its end. `None` is
/// the header refusing to make sense.
fn section_lens(n: usize, d: usize, u: usize, names_len: usize) -> Option<[usize; NSEC]> {
    let n4 = n.checked_mul(4)?;
    let d4 = d.checked_mul(4)?;
    let u4 = u.checked_add(1)?.checked_mul(4)?;
    let u4b = u.checked_mul(4)?;
    let z = n.div_ceil(ZONE).checked_mul(core::mem::size_of::<Zone>())?;
    let bits = u.div_ceil(64).checked_mul(NBITMAPS)?.checked_mul(8)?;
    Some([bits, u4, names_len, n4, n, n, n4, n4, n4, d4, d4, d4, d4, d, d4, u4, n4, u, u4b, u, 256 * 16, z])
}

/// Entries per `Zone`.
pub const ZONE: usize = 1024;

/// A run of ZONE entries at a glance: entries matching a size, mtime or
/// kind filter cluster (a folder's big files, the recently modified), so
/// a pass over every entry skips most runs whole.
#[derive(Clone, Copy, Default)]
#[repr(C)]
pub struct Zone {
    /// Largest `size_raw` (its encoding keeps order).
    pub max_size: u32,
    pub min_mtime: u32,
    pub max_mtime: u32,
    /// Bit `kind & 3` for every kind present.
    pub kinds: u32,
}

pub(crate) fn zones(kind: &[u8], size: &[u32], mtime: &[u32]) -> Vec<Zone> {
    (0..kind.len().div_ceil(ZONE))
        .map(|z| {
            let r = z * ZONE..((z + 1) * ZONE).min(kind.len());
            Zone {
                max_size: size[r.clone()].iter().copied().max().unwrap_or(0),
                min_mtime: mtime[r.clone()].iter().copied().min().unwrap_or(0),
                max_mtime: mtime[r.clone()].iter().copied().max().unwrap_or(0),
                kinds: kind[r].iter().fold(0, |m, &k| m | 1 << (k & 3)),
            }
        })
        .collect()
}

/// Sizes in 4 bytes: exact below 2 GiB, 2 MiB granularity above.
pub fn enc_size(s: u64) -> u32 {
    if s < 1 << 31 { s as u32 } else { (1 << 31) | (s >> 21).min((1 << 31) - 1) as u32 }
}

pub fn dec_size(v: u32) -> u64 {
    if v & (1 << 31) == 0 { v as u64 } else { ((v & !(1 << 31)) as u64) << 21 }
}

/// FxHash: interning 7.5M names wants a hasher cheaper than SipHash.
#[derive(Clone, Copy, Default)]
pub struct Fx;
pub struct FxH(u64);
impl core::hash::BuildHasher for Fx {
    type Hasher = FxH;
    fn build_hasher(&self) -> FxH {
        FxH(0)
    }
}
impl core::hash::Hasher for FxH {
    fn write(&mut self, bytes: &[u8]) {
        for c in bytes.chunks(8) {
            let mut w = [0u8; 8];
            w[..c.len()].copy_from_slice(c);
            self.0 = (self.0.rotate_left(5) ^ u64::from_le_bytes(w)).wrapping_mul(0x51_7c_c1_b7_27_22_0a_95);
        }
    }
    fn finish(&self) -> u64 {
        self.0
    }
}

/// Which character classes a name contains. A query token can only match a
/// name whose mask is a superset of the token's mask, which rejects most of
/// the disk with one AND per entry.
///
/// The bytes are taken as they are: the caller folds the name first (see
/// `name_mask`, which does), because the classes a comparison sees are the
/// classes of the folded name.
pub fn char_mask(s: &[u8]) -> u64 {
    s.iter().fold(0, |m, &b| m | char_bit(b))
}

/// A name's mask: `char_mask`, plus in the spare high bits a hash of the
/// first byte of the name (past a leading dot) and of each space-separated
/// word: where a typo match may start (`query::typo_score`), so a search
/// rejects every other name without reading it.
///
/// The name is folded first: the mask says which classes a *comparison*
/// sees, and a comparison sees the folded name.
pub fn name_mask(s: &[u8]) -> u64 {
    mask_of(&fold::fold(s))
}

/// `name_mask` for a name that is already folded: the mask without
/// folding it again.
///
/// The hot path folds the name once and needs its mask from the same
/// bytes, and folding twice would walk the name twice for nothing.
pub(crate) fn mask_of(s: &[u8]) -> u64 {
    let off = (s.len() > 1 && s[0] == b'.') as usize;
    let starts = s.get(off).into_iter().chain(s.windows(2).filter(|w| w[0] == b' ').map(|w| &w[1]));
    starts.fold(char_mask(s), |m, &b| m | start_bit(b))
}

/// The `name_mask` bit for a word starting with `b` (bits 41..64).
#[inline]
pub fn start_bit(b: u8) -> u64 {
    1 << (CLASSES + start_hash(b))
}

#[inline]
pub fn start_hash(b: u8) -> usize {
    (b.to_ascii_lowercase() % 23) as usize
}

/// Char classes (`char_bit`).
pub const CLASSES: usize = 41;
/// The class of the bytes at U+0080 and above: the UTF-8 of a kana or a
/// Han character. A name with one of these may be read in another script
/// (see `readings`), so the class filter cannot reject it on its bytes
/// alone.
pub const CLASS_NON_ASCII: usize = 39;
/// Name bitmaps, per char class: names with that class in their first half
/// (`BM_FIRST + class`), in their second half (`BM_SECOND + class`). A
/// query's chars must appear in order, so a name can only match if some
/// prefix of the query fits its first half and the rest its second half.
pub const BM_FIRST: usize = 0;
pub const BM_SECOND: usize = CLASSES;
/// Names with a word starting with a letter of this `start_bit` hash.
pub const BM_START: usize = 2 * CLASSES;
/// Names starting with a dot; names ending in ".app".
pub const BM_DOT: usize = BM_START + 23;
pub const BM_APP: usize = BM_DOT + 1;
/// Names with this char class at least twice: a token with a class twice
/// can only match those (with a typo, it may lose one).
pub const BM_DOUBLE: usize = BM_APP + 1;
pub const NBITMAPS: usize = BM_DOUBLE + CLASSES;

/// The bitmaps (see `BM_FIRST` and on) a name is in.
///
/// The name is the folded one (see `crate::fold`): the bitmaps are what a
/// query's classes are tested against, so they have to describe the name
/// a comparison sees.
fn name_bitmaps(s: &[u8]) -> impl Iterator<Item = usize> {
    let (a, b) = s.split_at(s.len() / 2);
    let class = |m: u64, base: usize| (0..CLASSES).filter(move |c| m & (1 << c) != 0).map(move |c| base + c);
    let starts = mask_of(s) >> CLASSES;
    class(char_mask(a), BM_FIRST)
        .chain(class(char_mask(b), BM_SECOND))
        .chain((0..23).filter(move |h| starts & (1 << h) != 0).map(|h| BM_START + h))
        .chain(s.first().is_some_and(|&c| c == b'.').then_some(BM_DOT))
        .chain(s.ends_with(b".app").then_some(BM_APP))
        .chain(class(doubled(s), BM_DOUBLE))
}

/// The char classes `s` has at least twice.
pub fn doubled(s: &[u8]) -> u64 {
    let (mut once, mut twice) = (0, 0);
    for &b in s {
        twice |= once & char_bit(b);
        once |= char_bit(b);
    }
    twice
}

#[inline]
pub fn char_bit(b: u8) -> u64 {
    match b {
        b'a'..=b'z' => 1 << (b - b'a'),
        b'A'..=b'Z' => 1 << (b - b'A'),
        b'0'..=b'9' => 1 << (26 + b - b'0'),
        b'.' => 1 << 36,
        b'-' | b'_' => 1 << 37,
        b' ' => 1 << 38,
        0x80.. => 1 << 39,
        _ => 1 << 40,
    }
}

#[cfg(all(test, feature = "std"))]
mod blob_tests {
    use super::*;
    use alloc::format;

    #[test]
    fn blob_from_file_reads_the_bytes() {
        let dir = std::env::temp_dir().join(format!("rgy_blob_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let p = dir.join("b.bin");
        std::fs::write(&p, b"hello blob").unwrap();
        let f = std::fs::File::open(&p).unwrap();
        let b = Blob::from_file(&f).unwrap();
        assert_eq!(&b[..], b"hello blob");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The acceptance rule: a non-empty file must
    /// arrive as `Blob::Mapped`, so a regression to the owned fallback is
    /// visible here instead of silently costing the whole blob in RSS. On
    /// FreeBSD this exercises `memmap2/platform/freebsd.rs`; on Linux and
    /// macOS it holds the same promise over their own arms.
    #[test]
    #[cfg(unix)]
    fn blob_from_file_maps_a_non_empty_file() {
        let dir = std::env::temp_dir().join(format!("rgy_blobmap_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let p = dir.join("m.bin");
        std::fs::write(&p, vec![7u8; 4096]).unwrap();
        let f = std::fs::File::open(&p).unwrap();
        let b = Blob::from_file(&f).unwrap();
        assert!(matches!(b, Blob::Mapped(_)), "a non-empty file must map on unix; got Owned — the fallback won");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn index_save_load_round_trips_through_blob() {
        // Build -> freeze -> save -> load exercises both Blob arms: the
        // anonymous map on build and the file map (or owned fallback) on
        // load, with the header size check in between.
        use rgy_discovery::walk::{KIND_FILE, Listing, NONE, RawEnt};
        let mut names = b"hi.txt".to_vec();
        let ents = vec![RawEnt { name_off: 0, name_len: 6, kind: KIND_FILE, size: 3, mtime: 7, child: NONE }];
        let ls = vec![Listing { id: 0, names: core::mem::take(&mut names), ents }];
        let idx = Index::build(ls, 42, 1, b"/");
        let dir = std::env::temp_dir().join(format!("rgy_idx_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let p = dir.join("i.fsi");
        idx.save(&p).unwrap();
        let loaded = Index::load(&p).expect("load after save");
        assert_eq!(loaded.n, idx.n);
        assert_eq!(loaded.u, idx.u);
        assert_eq!(Index::saved_event_id(&p), Some(42));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The format is portable: the bytes an index writes are the bytes an
    /// index reads, with no filesystem in between.
    ///
    /// This is what makes the index shippable — an index built on one
    /// machine can be handed to another as bytes, and the daemon that
    /// reads it does not need to have built it.
    #[test]
    fn the_index_format_round_trips_through_bytes() {
        use rgy_discovery::walk::{KIND_FILE, Listing, NONE, RawEnt};
        let mut names = b"hi.txt".to_vec();
        let ents = vec![RawEnt { name_off: 0, name_len: 6, kind: KIND_FILE, size: 3, mtime: 7, child: NONE }];
        let ls = vec![Listing { id: 0, names: core::mem::take(&mut names), ents }];
        let idx = Index::build(ls, 42, 1, b"/");
        let bytes = idx.to_bytes();
        let loaded = Index::from_bytes(&bytes).expect("the bytes are an index");
        assert_eq!(loaded.n, idx.n);
        assert_eq!(loaded.u, idx.u);
        assert_eq!(loaded.d, idx.d);
        assert_eq!(Index::header_event_id(&bytes), Some(42));
        // The bytes are the file's bytes: the same header, the same
        // payload, so a file and a message carry the same index.
        let dir = std::env::temp_dir().join(format!("rgy_idxbytes_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let p = dir.join("i.fsi");
        std::fs::write(&p, &bytes).unwrap();
        assert_eq!(Index::saved_event_id(&p), Some(42));
        let from_file = Index::load(&p).expect("load after write");
        assert_eq!(from_file.u, idx.u);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Bytes that are not an index are refused, not half-read.
    #[test]
    fn bytes_that_are_not_an_index_are_refused() {
        assert!(Index::from_bytes(b"not an index at all").is_none());
        assert!(Index::from_bytes(&[]).is_none());
        assert_eq!(Index::header_event_id(b"nope"), None);
    }

    /// The parent chain of a well-formed index is a forest, and the check
    /// says so.
    ///
    /// The check is the one the index needs: a forest
    /// that is not a forest is an index that answers with paths that do not
    /// exist, and a corrupt index must be detected rather than read.
    #[test]
    fn a_built_index_is_a_forest() {
        use rgy_discovery::walk::{KIND_DIR, KIND_FILE, Listing, NONE, RawEnt};
        // /a/b/c.txt and /a/d.txt: two folders, a file under each.
        let mut names = b"a".to_vec();
        let a = RawEnt { name_off: 0, name_len: 1, kind: KIND_DIR, size: 0, mtime: 1, child: 1 };
        let d = RawEnt { name_off: 1, name_len: 1, kind: KIND_FILE, size: 2, mtime: 3, child: NONE };
        let mut inner = b"b".to_vec();
        let b = RawEnt { name_off: 0, name_len: 1, kind: KIND_DIR, size: 0, mtime: 4, child: 2 };
        let c = RawEnt { name_off: 1, name_len: 5, kind: KIND_FILE, size: 5, mtime: 6, child: NONE };
        names.extend_from_slice(b"d");
        inner.extend_from_slice(b"c.txt");
        let ls = vec![Listing { id: 0, names, ents: vec![a, d] }, Listing { id: 1, names: inner, ents: vec![b, c] }];
        let idx = Index::build(ls, 0, 1, b"/");
        assert_eq!(idx.validate_forest(), Ok(()), "a built index is a forest: {:?}", idx.validate_forest());
    }

    /// A header that does not make sense is refused, not read.
    ///
    /// The index is read back from bytes that may be corrupt: a truncated
    /// file, a file another version wrote, a file on a failing disk. The
    /// counts in the header become sizes, and the arithmetic has to be
    /// checked — a count of `u64::MAX` wraps into a small one in a
    /// release build, and the reader goes on to read a slice of the wrong
    /// size: out of the map, and past its end.
    #[test]
    fn a_header_that_does_not_make_sense_is_refused() {
        use rgy_discovery::walk::{KIND_FILE, Listing, NONE, RawEnt};
        let mut names = b"hi.txt".to_vec();
        let ents = vec![RawEnt { name_off: 0, name_len: 6, kind: KIND_FILE, size: 3, mtime: 7, child: NONE }];
        let ls = vec![Listing { id: 0, names: core::mem::take(&mut names), ents }];
        let bytes = Index::build(ls, 42, 1, b"/").to_bytes();
        assert!(Index::from_bytes(&bytes).is_some(), "the good bytes are an index");

        // The header's counts: n, d, u, names_len, event_id, synced_at.
        let forge = |n: u64, d: u64, u: u64| {
            let mut b = bytes.clone();
            b[8..16].copy_from_slice(&n.to_le_bytes());
            b[16..24].copy_from_slice(&d.to_le_bytes());
            b[24..32].copy_from_slice(&u.to_le_bytes());
            b
        };
        // The counts that do not make sense: they ask for more than the
        // arithmetic, or more than the file, holds.
        assert!(Index::from_bytes(&forge(u64::MAX, 0, 0)).is_none(), "a count of u64::MAX is refused");
        assert!(Index::from_bytes(&forge(0, u64::MAX, 0)).is_none(), "so is a count of folders");
        assert!(Index::from_bytes(&forge(0, 0, u64::MAX)).is_none(), "so is a count of names");
        let huge = forge(1_000_000_000, 0, 0);
        assert!(Index::from_bytes(&huge).is_none(), "a count past the file is refused");
        let folders = forge(0, 1_000_000_000, 0);
        assert!(Index::from_bytes(&folders).is_none(), "a count of folders past the file");
        // And the header's own width: a second that does not fit the u32
        // the index keeps is refused too.
        let mut synced = bytes.clone();
        synced[48..56].copy_from_slice(&u64::MAX.to_le_bytes());
        assert!(Index::from_bytes(&synced).is_none(), "a second past the u32 is refused");
        // The refusal is the reader's own: nothing is read out of bounds.
        let half = bytes[..bytes.len() / 2].to_vec();
        assert!(Index::from_bytes(&half).is_none(), "a truncated payload is refused");
    }

    /// Each of the four faults is caught.
    ///
    /// The chain is forged rather than built, because a built index is
    /// sound by construction: what the check is for is an index whose bytes
    /// were corrupted, and a check that has never seen a broken chain has
    /// not been shown to catch one.
    #[test]
    fn the_four_faults_are_caught() {
        use rgy_discovery::walk::KIND_FILE;
        // A sound chain: three folders, the root first, each parent before
        // its child, and every parent's entry a folder.
        let sound_dp = vec![0u32, 0, 1];
        let sound_de = vec![0u32, 1, 2];
        let sound_kind = vec![KIND_DIR, KIND_DIR, KIND_DIR];
        let sound_parent = vec![0u32, 0, 1, 1];
        assert_eq!(check_forest(&sound_dp, &sound_de, &sound_kind, &sound_parent, 4, 3), Ok(()), "the sound chain passes");

        // A cycle: the second folder is its own parent.
        let mut dp = sound_dp.clone();
        dp[2] = 2;
        assert!(
            matches!(check_forest(&dp, &sound_de, &sound_kind, &sound_parent, 4, 3), Err(ForestFault::Cycle { folder: 2 })),
            "a cycle is caught: {:?}",
            check_forest(&dp, &sound_de, &sound_kind, &sound_parent, 4, 3)
        );

        // A broken chain: the parent names a folder that is not there.
        let mut dp = sound_dp.clone();
        dp[2] = 99;
        assert!(
            matches!(check_forest(&dp, &sound_de, &sound_kind, &sound_parent, 4, 3), Err(ForestFault::BrokenChain { folder: 2, parent: 99 })),
            "a broken chain is caught: {:?}",
            check_forest(&dp, &sound_de, &sound_kind, &sound_parent, 4, 3)
        );

        // A wrong record kind: the parent is a file.
        let mut kind = sound_kind.clone();
        kind[1] = KIND_FILE;
        assert!(
            matches!(check_forest(&sound_dp, &sound_de, &kind, &sound_parent, 4, 3), Err(ForestFault::WrongKind { folder: 2, parent: 1 })),
            "a file as a parent is caught: {:?}",
            check_forest(&sound_dp, &sound_de, &kind, &sound_parent, 4, 3)
        );

        // A broken chain on the entry side: an entry names a folder that is
        // not there.
        let mut parent = sound_parent.clone();
        parent[3] = 42;
        assert!(
            matches!(check_forest(&sound_dp, &sound_de, &sound_kind, &parent, 4, 3), Err(ForestFault::BrokenChain { folder: 3, parent: 42 })),
            "an entry with no parent folder is caught: {:?}",
            check_forest(&sound_dp, &sound_de, &sound_kind, &parent, 4, 3)
        );
    }

    /// A parent that is not a folder is a wrong record kind.
    #[test]
    fn a_parent_that_is_not_a_folder_is_caught() {
        use rgy_discovery::walk::{KIND_FILE, Listing, NONE, RawEnt};
        // A listing whose child listing is claimed by a file: the file's
        // entry is not a folder, so the chain through it is wrong.
        let names = b"a.txt".to_vec();
        let a = RawEnt { name_off: 0, name_len: 5, kind: KIND_FILE, size: 1, mtime: 1, child: 1 };
        let ls = vec![
            Listing { id: 0, names: names.clone(), ents: vec![a] },
            Listing { id: 1, names, ents: vec![RawEnt { name_off: 0, name_len: 1, kind: KIND_FILE, size: 1, mtime: 2, child: NONE }] },
        ];
        let idx = Index::build(ls, 0, 1, b"/");
        // The build puts the child under the file, and the check says so.
        assert!(matches!(idx.validate_forest(), Err(ForestFault::WrongKind { .. })), "a file cannot be a parent: {:?}", idx.validate_forest());
    }
}

#[cfg(all(test, feature = "std"))]
mod path_tests {
    use super::*;
    use alloc::format;
    use alloc::string::{String, ToString};

    /// A path nested deeper than the fixed 256-entry chain used to come back
    /// missing its outermost components. Nothing said it had been cut, so the
    /// result was a plausible-looking wrong path -- the worst shape a
    /// reconstruction can fail in.
    ///
    /// Round-tripped through `lookup` rather than by hand-picking an entry
    /// index: `lookup` walks the children by name and so survives either
    /// implementation, which makes it the honest way to ask for the entry
    /// whose path is in question.
    #[test]
    fn a_path_deeper_than_the_inline_chain_is_not_truncated() {
        use rgy_discovery::walk::{KIND_DIR, KIND_FILE, Listing, NONE, RawEnt};

        // More than the 256 the inline chain holds, so the walk has to spill.
        const DEPTH: usize = 300;
        let mut listings = Vec::with_capacity(DEPTH + 1);
        let mut expected = String::new();
        for level in 0..=DEPTH {
            // The last listing holds the leaf; every one before it a folder
            // named after its level, so the expected path is unambiguous.
            let (name, kind, child) =
                if level == DEPTH { ("leaf.txt".to_string(), KIND_FILE, NONE) } else { (format!("d{level}"), KIND_DIR, level as u32 + 1) };
            if level < DEPTH {
                expected.push_str(&format!("/{name}"));
            } else {
                expected.push_str("/leaf.txt");
            }
            listings.push(Listing {
                id: level as u32,
                names: name.as_bytes().to_vec(),
                ents: vec![RawEnt { name_off: 0, name_len: name.len() as u16, kind, size: 3, mtime: 7, child }],
            });
        }
        let idx = Index::build(listings, 0, 1, b"/");

        let entry = idx.lookup(expected.as_bytes()).expect("the deep path resolves");
        let mut out = Vec::new();
        idx.path(entry as usize, &mut out);
        assert_eq!(String::from_utf8_lossy(&out), expected, "a path deeper than the inline chain must come back whole");
    }

    /// The shallow case must stay on the allocation-free path and still be
    /// right: the spill exists for the deep tail, not to change what a normal
    /// tree produces.
    #[test]
    fn a_shallow_path_is_reconstructed_exactly() {
        use rgy_discovery::walk::{KIND_DIR, KIND_FILE, Listing, NONE, RawEnt};

        let listings = vec![
            Listing { id: 0, names: b"docs".to_vec(), ents: vec![RawEnt { name_off: 0, name_len: 4, kind: KIND_DIR, size: 0, mtime: 7, child: 1 }] },
            Listing {
                id: 1,
                names: b"notes.txt".to_vec(),
                ents: vec![RawEnt { name_off: 0, name_len: 9, kind: KIND_FILE, size: 3, mtime: 7, child: NONE }],
            },
        ];
        let idx = Index::build(listings, 0, 1, b"/");
        let entry = idx.lookup(b"/docs/notes.txt").expect("resolves");
        let mut out = Vec::new();
        idx.path(entry as usize, &mut out);
        assert_eq!(String::from_utf8_lossy(&out), "/docs/notes.txt");
    }
}

#[cfg(all(test, feature = "std"))]
mod prior_tests {
    use super::*;
    use rgy_discovery::walk::{KIND_DIR, KIND_FILE, Listing, NONE, RawEnt};

    /// A tree where the folders and the entries are numbered apart, and
    /// where the home holds a folder that holds a file.
    ///
    /// The file "alpha.txt" sorts before the folder "home", so the folder
    /// is the second entry and the second directory: a directory's number
    /// and its entry's number are two different things, and a section that
    /// mixes them reads the wrong prior.
    ///
    /// The tree is:
    ///
    /// ```text
    /// /alpha.txt
    /// /home/invoice.txt
    /// /home/docs/invoice.txt
    /// ```
    fn tree() -> Index {
        let listings = vec![
            Listing {
                id: 0,
                names: b"alpha.txthome".to_vec(),
                ents: vec![
                    RawEnt { name_off: 0, name_len: 9, kind: KIND_FILE, size: 3, mtime: 7, child: NONE },
                    RawEnt { name_off: 9, name_len: 4, kind: KIND_DIR, size: 0, mtime: 7, child: 1 },
                ],
            },
            Listing {
                id: 1,
                names: b"docsinvoice.txt".to_vec(),
                ents: vec![
                    RawEnt { name_off: 0, name_len: 4, kind: KIND_DIR, size: 0, mtime: 7, child: 2 },
                    RawEnt { name_off: 4, name_len: 11, kind: KIND_FILE, size: 3, mtime: 7, child: NONE },
                ],
            },
            Listing {
                id: 2,
                names: b"invoice.txt".to_vec(),
                ents: vec![RawEnt { name_off: 0, name_len: 11, kind: KIND_FILE, size: 3, mtime: 7, child: NONE }],
            },
        ];
        Index::build(listings, 0, 1, b"/home")
    }

    /// Everything under the home is under it, and the bonus says so.
    ///
    /// The bonus used to be inherited down the tree. The rules replaced
    /// that inheritance with their own -- the level, which is inherited
    /// too -- and the bonus with it: the files under a folder inside the
    /// home lost the fifteen points the home folder itself still had.
    #[test]
    fn the_home_bonus_reaches_the_files_under_the_home() {
        let idx = tree();
        let under = idx.lookup(b"/home/invoice.txt").expect("the file under the home resolves") as usize;
        let deeper = idx.lookup(b"/home/docs/invoice.txt").expect("the file under the folder in the home resolves") as usize;
        let outside = idx.lookup(b"/alpha.txt").expect("the file outside the home resolves") as usize;
        assert_eq!(idx.rank_adj()[under], 15, "the file under the home carries the bonus");
        assert_eq!(idx.rank_adj()[deeper], 15, "the file under a folder in the home carries it too");
        assert_eq!(idx.rank_adj()[outside], 0, "the file outside it does not");
    }

    /// The directory's prior is the prior of that directory.
    ///
    /// The section is read per directory -- an entry the overlay added
    /// takes the prior of the directory it is in -- and it was written with
    /// the per-entry array, so a directory read the prior of whatever entry
    /// happened to carry its number. Here the two are not the same: the
    /// home folder is the second directory and the third entry.
    #[test]
    fn the_directory_prior_is_the_prior_of_that_directory() {
        let idx = tree();
        let home = idx.lookup(b"/home").expect("the home folder resolves") as usize;
        let d = idx.dir_of(home as u32).expect("the home folder is a directory") as usize;
        assert_eq!(idx.dir_prior()[d], 15, "the directory carries the prior of its entry");
        let root = idx.dir_of(0).expect("the root is a directory") as usize;
        assert_eq!(idx.dir_prior()[root], 0, "the root carries nothing");
    }
}

#[cfg(all(test, feature = "std"))]
mod ext_tests {
    use super::*;
    use alloc::string::String;
    use rgy_discovery::walk::{KIND_FILE, Listing, NONE, RawEnt};

    /// The extension slot of a name is the slot of its extension.
    ///
    /// The table of the common extensions is built once over the corpus and
    /// read once per name, and the two readers have to agree: a name whose
    /// slot is wrong is a name whose extension filter lets through a file
    /// it should have skipped. The readers are independent -- the slot
    /// stored with the name, and the table's own scan.
    #[test]
    fn a_name_s_extension_slot_is_the_slot_of_its_extension() {
        let names: [&[u8]; 6] = [b"alpha.txt", b"beta.txt", b"gamma.rs", b"delta.RS", b"epsilon.md", b"zeta"];
        let mut blob = Vec::new();
        let mut ents = Vec::new();
        for n in names {
            ents.push(RawEnt { name_off: blob.len() as u32, name_len: n.len() as u16, kind: KIND_FILE, size: 1, mtime: 1, child: NONE });
            blob.extend_from_slice(n);
        }
        let idx = Index::build(vec![Listing { id: 0, names: blob, ents }], 0, 1, b"/");
        let slots = idx.name_ext();
        for (k, &slot) in slots.iter().enumerate().take(idx.u) {
            let name = idx.uname(k as u32);
            let want = match ext_key(&crate::fold::fold(name)) {
                Some(key) => idx.ext_slot(&key[1..1 + key[0] as usize]),
                None => u8::from(name.contains(&b'.')) * 255,
            };
            assert_eq!(slot, want, "the slot of {:?}", String::from_utf8_lossy(name));
        }
        // The common extensions are in the table, and a name with no
        // extension has none.
        assert_ne!(idx.ext_slot(b"txt"), 255, "txt is in the table");
        assert_ne!(idx.ext_slot(b"rs"), 255, "rs is in the table");
        let plain = (0..idx.u).find(|&k| !idx.uname(k as u32).contains(&b'.')).expect("a name with no dot");
        assert_eq!(slots[plain as usize], 0, "a name with no dot has no extension");
    }
}
