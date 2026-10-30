// SPDX-License-Identifier: Apache-2.0
//
// Without std the overlay's write half has no filesystem to write from,
// so it is unreachable there: the code is here, the callers are not.
#![cfg_attr(not(feature = "std"), allow(dead_code))]

//! The overlay: what the disk holds now, over the snapshot the base holds.
//!
//! The base is a walk, frozen. The overlay is the difference between that
//! walk and the disk as it stands:
//!
//!   * the **dead set** — base entries that are gone, a bitset over the base
//!     entry numbers, read through [`Live::is_dead`];
//!   * the **overlay map** — [`Live::over`], the paths the base does not
//!     have, with the metadata they have now.
//!
//! One operation maintains both: list a folder again and diff it against
//! what is here. A create, a delete, a rename and an edit are the same
//! operation seen from the folder that holds the entry, and a diff is
//! idempotent — replaying a feed, a duplicated event and an event that
//! races a compaction all land on the same state.
//!
//! The two halves are split on purpose. [`Live::fetch`] reads the disk and
//! needs only `&self`, so a daemon can re-list a folder under a read lock
//! while searches keep answering; [`Live::apply`] writes the difference and
//! needs `&mut self`.
//!
//! The two path spaces matter here: the base holds root-relative paths and
//! the overlay holds openable ones. The Live knows the tree the base was
//! built from, so it can cross between them: an overlay entry finds its
//! twin in the base, and the base's entry for a path the overlay holds
//! dies with it. Without the crossing, the same file answers twice — once
//! from the base, once from the overlay — and a file that went away keeps
//! answering from the base's snapshot.

use crate::name_index::enc_size;
use crate::name_index::{Index, name_mask};
use alloc::collections::BTreeMap;
use alloc::vec;
use alloc::vec::Vec;
use rgy_discovery::walk;
use rgy_discovery::walk::{FLAG_MOUNT, KIND_DIR, Listing, NONE, RawEnt};
// `String` is the watch feed's error type.
use crate::hmap::Map;
use alloc::string::String;

/// One entry of the overlay.
#[derive(Clone, Copy)]
pub struct OEnt {
    /// `rgy_discovery::walk::KIND_*`, with the flag bits of `walk::FLAG_*`.
    pub kind: u8,
    pub size: u64,
    pub mtime: u32,
    /// `name_index::name_mask` of the entry's name, so a search can reject
    /// most of the overlay with one AND.
    pub mask: u64,
    /// The location prior of the nearest folder the base knows. Set when the
    /// entry goes in, because that is when the base is consulted.
    pub prior: i8,
    /// The slot this entry keeps in [`Flat`]. Set when it goes in.
    slot: u32,
}

impl OEnt {
    /// `path` may be the whole path or just the name; the mask is taken from
    /// the last component either way.
    pub fn new(path: &[u8], kind: u8, size: u64, mtime: u32) -> OEnt {
        let name = &path[path.iter().rposition(|&b| b == b'/').map_or(0, |p| p + 1)..];
        OEnt { kind, size, mtime, mask: name_mask(name), prior: 0, slot: 0 }
    }
}

/// The overlay laid out for searching: every entry's name mask in one array
/// to scan, its path and value in the same slot. A removed entry's slot is
/// reused; until then its mask is zero, which no name has.
#[derive(Default)]
pub struct Flat {
    pub masks: Vec<u64>,
    pub items: Vec<(Vec<u8>, OEnt)>,
    free: Vec<u32>,
}

impl Flat {
    /// Put an entry in its slot, growing the arrays if it needs one.
    fn set(&mut self, path: &[u8], e: OEnt) {
        let s = e.slot as usize;
        if s == self.masks.len() {
            self.masks.push(0);
            self.items.push((Vec::new(), e));
        }
        self.masks[s] = e.mask;
        if self.items[s].0 != path {
            self.items[s].0 = path.to_vec();
        }
        self.items[s].1 = e;
    }

    /// Empty a slot and offer it again.
    fn clear(&mut self, e: OEnt) {
        let s = e.slot as usize;
        self.masks[s] = 0;
        self.items[s].0 = Vec::new();
        self.free.push(e.slot);
    }
}

/// The index as it is right now.
pub struct Live {
    /// The snapshot the overlay sits on.
    pub base: Index,
    /// The base entries that are gone, one bit per entry.
    dead: Vec<u64>,
    pub dead_count: usize,
    /// The paths the base does not have, with their current metadata.
    pub over: BTreeMap<Vec<u8>, OEnt>,
    /// `over` laid out for searches.
    pub flat: Flat,
    /// The last change-feed id fully applied.
    pub event_id: u64,
    /// How many times the overlay changed.
    ///
    /// The index's own event id is fixed when the index is built, so it
    /// cannot say that the overlay moved: a session answered before a
    /// change would still be answered from after it. This one moves with
    /// the overlay, and it is what a session holds.
    pub revision: u64,
    /// The folders added or removed since the list was last drained; the
    /// content index re-syncs these whole subtrees.
    pub trees: Vec<Vec<u8>>,
    /// The last name search's scored names, reused while the query narrows.
    pub names_cache: crate::name_query::NameCache,
    /// The wall-clock second up to which every change is known applied.
    pub synced_at: u32,
    /// Overlay folder to the prior of its nearest base folder.
    priors: Map<Vec<u8>, i8>,
    /// The tree the base was built from, as the disk spells it.
    ///
    /// The base holds root-relative paths ("/a/b") and the overlay holds
    /// openable ones ("<root>/a/b"). The root is the one thing that relates
    /// them, so the Live has to know it: without it the two talk about
    /// different files, and the same file is answered twice — once from the
    /// base, once from the overlay.
    root: Vec<u8>,
}

/// What [`Live::fetch`] read from the disk for one folder update.
pub struct Fetched {
    path: Vec<u8>,
    recursive: bool,
    blocked: bool,
    attrs: Option<OEnt>,
    listing: Option<Listing>,
    scans: Map<Vec<u8>, Vec<Listing>>,
}

/// What an update did.
pub enum Applied {
    Done,
    /// The whole disk needs rescanning: the history was lost at the root.
    Rebuild,
}

impl Live {
    /// A live index over a snapshot rooted at `/`.
    ///
    /// The base's paths and the overlay's are then the same paths, and the
    /// root-relative crossing is the identity. A tree that is not at `/` has
    /// to say so: [`Live::with_root`].
    pub fn new(base: Index) -> Live {
        Live::with_root(base, b"/")
    }

    /// A live index over a snapshot of the tree at `root` on disk.
    ///
    /// The base is built from a walk of `root`, so it holds paths relative
    /// to it ("/a/b"), while the overlay holds paths that can be opened
    /// ("<root>/a/b"). The two are the same files, and this is what says so.
    pub fn with_root(base: Index, root: &[u8]) -> Live {
        base.prefault();
        let words = base.n.div_ceil(64);
        let event_id = base.event_id;
        let synced_at = base.synced_at;
        Live {
            base,
            dead: vec![0; words],
            dead_count: 0,
            over: BTreeMap::new(),
            flat: Flat::default(),
            event_id,
            revision: 0,
            trees: Vec::new(),
            names_cache: Default::default(),
            synced_at,
            priors: Map::new(),
            root: normalize(root),
        }
    }

    /// The base's path for an on-disk path.
    ///
    /// The one crossing between the two spaces. `None` when the path is not
    /// under the root at all: a path outside the tree is not this index's
    /// business.
    fn base_path(&self, on_disk: &[u8]) -> Option<Vec<u8>> {
        if self.root == b"/" {
            return Some(on_disk.to_vec());
        }
        let rest = on_disk.strip_prefix(self.root.as_slice())?;
        if rest.is_empty() {
            // The root itself: the tree's own path is "/".
            return Some(b"/".to_vec());
        }
        let rel = match rest.split_first() {
            Some((b'/', tail)) => tail,
            // A sibling spelling ("/a/b2" under "/a"): not under the root.
            _ => return None,
        };
        let mut p = Vec::with_capacity(rel.len() + 1);
        p.push(b'/');
        p.extend_from_slice(rel);
        Some(p)
    }

    /// The on-disk path of a base path: [`Live::base_path`] backwards.
    fn openable(&self, base_path: &[u8]) -> Vec<u8> {
        if self.root == b"/" {
            return base_path.to_vec();
        }
        let rel = base_path.strip_prefix(b"/").unwrap_or(base_path);
        let mut p = self.root.clone();
        if !p.is_empty() && !p.ends_with(b"/") && !rel.is_empty() {
            p.push(b'/');
        }
        p.extend_from_slice(rel);
        p
    }

    /// Is this base entry gone?
    #[inline]
    pub fn is_dead(&self, i: u32) -> bool {
        self.dead[i as usize >> 6] & (1 << (i & 63)) != 0
    }

    /// Check the parent chain of the live set.
    ///
    /// The index's own chain is checked by [`Index::validate_forest`]; what
    /// only the live set can see is the **dead ancestor**: an entry that
    /// survives a folder the change feed killed. Such an entry has no path
    /// -- its parent is gone -- and the search would answer with a path
    /// that cannot be opened.
    ///
    /// The walk is O(d): a folder's ancestors are the same for every entry
    /// under it, and the tree is written in depth-first order, so a
    /// parent's id is smaller than its child's.
    pub fn validate_forest(&self) -> Result<(), crate::name_index::ForestFault> {
        self.base.validate_forest()?;
        let de = self.base.dir_entry();
        let dp = self.base.dir_parent();
        // For each folder: is it dead, and does it have a dead ancestor?
        // A folder whose parent is dead is itself orphaned, so the flag
        // folds down the chain.
        let mut orphan = vec![false; self.base.d];
        for k in 1..self.base.d {
            let p = dp[k] as usize;
            let e = de[k] as usize;
            let dead = e < self.base.n && self.is_dead(e as u32);
            if dead || orphan[p] {
                orphan[k] = true;
                // A folder that is alive under a folder that is gone: the
                // entries under it have no path either.
                if !dead {
                    for child in self.base.children(k as u32) {
                        if child < self.base.n && !self.is_dead(child as u32) {
                            return Err(crate::name_index::ForestFault::DeadAncestor { entry: child as u32, folder: k as u32 });
                        }
                    }
                }
            }
        }
        Ok(())
    }

    /// Mark a base entry gone.
    fn kill(&mut self, i: u32) {
        let w = &mut self.dead[i as usize >> 6];
        if *w & (1 << (i & 63)) == 0 {
            *w |= 1 << (i & 63);
            self.dead_count += 1;
        }
    }

    /// Mark a base entry and everything under it gone.
    fn kill_subtree(&mut self, e: u32) {
        self.kill(e);
        if let Some(d) = self.base.dir_of(e) {
            let (a, b) = (self.base.dir_start()[d as usize], self.base.dir_end()[d as usize]);
            for i in a..b {
                self.kill(i);
            }
        }
    }

    /// Put an entry in the overlay, stamping the prior it ranks with.
    ///
    /// The prior is the one of the nearest folder the base knows, so an
    /// entry added under a deep folder ranks with that folder and not with
    /// the root.
    fn put(&mut self, path: Vec<u8>, mut e: OEnt) {
        let cut = path.iter().rposition(|&b| b == b'/').unwrap_or(0);
        e.prior = match self.priors.get(&path[..cut]) {
            Some(&p) => p,
            None => {
                let mut up = &path[..cut];
                let mut prior = 0;
                while !up.is_empty() {
                    let bp = self.base_path(up);
                    if let Some(d) = bp.and_then(|bp| self.base.lookup(&bp)).and_then(|e| self.base.dir_of(e)) {
                        prior = self.base.dir_prior()[d as usize];
                        break;
                    }
                    up = &up[..up.iter().rposition(|&b| b == b'/').unwrap_or(0)];
                }
                self.priors.insert(path[..cut].to_vec(), prior);
                prior
            }
        };
        e.slot = match self.over.get(&path) {
            Some(old) => old.slot,
            None => self.flat.free.pop().unwrap_or(self.flat.masks.len() as u32),
        };
        self.flat.set(&path, e);
        self.over.insert(path, e);
    }

    /// Forget a path and everything under it in the overlay.
    fn drop_over_subtree(&mut self, path: &[u8]) {
        if let Some(o) = self.over.remove(path) {
            self.flat.clear(o);
        }
        let (lo, hi) = subtree_bounds(path);
        let keys: Vec<Vec<u8>> = self.over.range(lo..hi).map(|(k, _)| k.clone()).collect();
        for k in keys {
            if let Some(o) = self.over.remove(&k) {
                self.flat.clear(o);
            }
        }
    }

    /// The alive base entry for a path, if the base has one.
    ///
    /// The path is an on-disk one; the base holds root-relative paths, so
    /// this is where the two spaces meet.
    fn base_alive(&self, path: &[u8]) -> Option<u32> {
        let bp = self.base_path(path)?;
        self.base.lookup(&bp).filter(|&e| !self.is_dead(e))
    }

    /// A path is gone: kill what the base holds of it, drop what the overlay
    /// holds of it, and remember the folder for the content index.
    fn remove_path(&mut self, path: &[u8]) {
        self.trees.push(path.to_vec());
        if let Some(e) = self.base_alive(path) {
            self.kill_subtree(e);
        }
        self.drop_over_subtree(path);
    }

    /// Bring one folder, or with `recursive` its whole subtree, in line with
    /// the disk.
    pub fn apply_dir(&mut self, path: &[u8], recursive: bool) -> Applied {
        let f = self.fetch(path, recursive);
        self.apply(f)
    }

    /// The disk half of [`Live::apply_dir`]: list the folder (or stat it) and
    /// read ahead the subtree of any folder that will be new to the index.
    ///
    /// Needs only `&self`, so the caller can do this under a read lock and
    /// searches keep answering while the disk is read.
    pub fn fetch(&self, path: &[u8], recursive: bool) -> Fetched {
        let p = normalize(path);
        let mut f = Fetched { path: p, recursive, blocked: false, attrs: None, listing: None, scans: Map::new() };
        // Not even an lstat inside folders we may not touch.
        if walk::blocked(&f.path) {
            f.blocked = true;
            return f;
        }
        if recursive {
            if f.path != b"/" {
                f.attrs = lstat(&f.path);
                if f.attrs.is_some_and(|a| a.kind & 3 == KIND_DIR && a.kind & FLAG_MOUNT == 0) {
                    f.scans.insert(f.path.clone(), walk::scan(&f.path, 4));
                }
            }
            return f;
        }
        let Some(listing) = walk::list_one(&f.path) else {
            f.attrs = lstat(&f.path);
            return f;
        };
        // A child that will be a folder gets its subtree read ahead now.
        let cur = self.current_children(&f.path);
        for r in &listing.ents {
            if r.kind & 3 != KIND_DIR || r.kind & FLAG_MOUNT != 0 {
                continue;
            }
            let name = &listing.names[r.name_off as usize..][..r.name_len as usize];
            let was_dir = match cur.get(name) {
                Some(Some(c)) => self.base.kind()[*c as usize] & 3 == KIND_DIR,
                Some(None) => self.over.get(&join(&f.path, name)).is_some_and(|o| o.kind & 3 == KIND_DIR),
                None => false,
            };
            if !was_dir {
                let child = join(&f.path, name);
                let ls = walk::scan(&child, 4);
                f.scans.insert(child, ls);
            }
        }
        f.listing = Some(listing);
        f
    }

    /// The children this index holds for a folder: the base's children by
    /// name, and the overlay's children that the base does not have.
    fn current_children(&self, p: &[u8]) -> Map<Vec<u8>, Option<u32>> {
        let mut cur: Map<Vec<u8>, Option<u32>> = Map::new();
        if let Some(d) = self.base_alive(p).and_then(|e| self.base.dir_of(e)) {
            for c in self.base.children(d) {
                if !self.is_dead(c as u32) {
                    cur.insert(self.base.name(c).to_vec(), Some(c as u32));
                }
            }
        }
        let (lo, hi) = subtree_bounds(p);
        let plen = lo.len();
        for k in self.over.range(lo..hi).map(|(k, _)| k) {
            if !k[plen..].contains(&b'/') {
                cur.insert(k[plen..].to_vec(), None);
            }
        }
        cur
    }

    /// The memory half of [`Live::apply_dir`]: diff what was read into what
    /// is here.
    pub fn apply(&mut self, mut f: Fetched) -> Applied {
        self.revision += 1;
        if f.blocked {
            return Applied::Done;
        }
        let p = core::mem::take(&mut f.path);
        if f.recursive {
            if p == b"/" {
                return Applied::Rebuild;
            }
            self.remove_path(&p);
            if let Some(attrs) = f.attrs {
                let scan = f.scans.remove(&p);
                self.add_new(p, attrs, scan);
            }
            return Applied::Done;
        }
        let Some(listing) = f.listing else {
            if f.attrs.is_none() {
                self.remove_path(&p);
            }
            return Applied::Done;
        };
        let mut cur = self.current_children(&p);
        for r in &listing.ents {
            let name = &listing.names[r.name_off as usize..][..r.name_len as usize];
            let child = join(&p, name);
            let now = OEnt::new(name, r.kind, r.size, r.mtime);
            match cur.remove(name) {
                // New to the index.
                None => {
                    let scan = f.scans.remove(&child);
                    self.add_new(child, now, scan)
                }
                // The base holds it.
                Some(Some(c)) => {
                    let c = c as usize;
                    let was_kind = self.base.kind()[c];
                    if was_kind & 3 != now.kind & 3 {
                        self.kill_subtree(c as u32);
                        let scan = f.scans.remove(&child);
                        self.add_new(child, now, scan);
                    } else if now.kind & 3 != KIND_DIR && (self.base.size_raw()[c] != enc_size(now.size) || self.base.mtime()[c] != now.mtime) {
                        self.kill(c as u32);
                        self.put(child, now);
                    }
                }
                // The overlay holds it.
                Some(None) => {
                    let old = self.over[&child];
                    if old.kind & 3 != now.kind & 3 {
                        self.drop_over_subtree(&child);
                        let scan = f.scans.remove(&child);
                        self.add_new(child, now, scan);
                    } else if (old.kind, old.size, old.mtime) != (now.kind, now.size, now.mtime) {
                        self.put(child, now);
                    }
                }
            }
        }
        // What is here and no longer on the disk.
        for (name, c) in cur.into_entries() {
            let child = join(&p, &name);
            match c {
                Some(c) => {
                    if self.base.kind()[c as usize] & 3 == KIND_DIR {
                        self.trees.push(child);
                    }
                    self.kill_subtree(c)
                }
                None => {
                    if self.over.get(&child).is_some_and(|o| o.kind & 3 == KIND_DIR) {
                        self.trees.push(child.clone());
                    }
                    self.drop_over_subtree(&child)
                }
            }
        }
        Applied::Done
    }

    /// Apply one batch of change events, as a [`rgy_discovery::Watch`] feed
    /// produces them.
    ///
    /// Each record names the changed path in `name`, and the granularity of
    /// the feed says what to re-list:
    ///
    ///   * a **file**-granular feed (the USN journal) names the file that
    ///     changed, so its folder is re-listed;
    ///   * a **directory**-granular feed (macOS fsevents) sets
    ///     `FILE_ATTRIBUTE_DIRECTORY` and names the folder whose *contents*
    ///     changed, so that folder is re-listed — re-listing its parent
    ///     would see only the folder's own mtime and miss the edit inside.
    ///
    /// That one bit is the whole difference between the two granularities.
    ///
    /// `name` is a platform path join (`\` between parts on Windows) while
    /// this index speaks `/`, so the separators are normalized first and the
    /// folder is taken at the last separator of either kind.
    pub fn apply_events(&mut self, events: &[rgy_discovery::UsnRecord]) {
        self.revision += 1;
        let mut dirs: Vec<Vec<u8>> = Vec::new();
        for e in events {
            let p: Vec<u8> = e.name.as_bytes().iter().map(|&b| if b == b'\\' { b'/' } else { b }).collect();
            if !self.in_tree(&p) {
                // Another tree's change: the feed can be wider than the
                // index (the Windows journal is the volume's, and the
                // daemon indexes a folder of it), and a change in another
                // tree is not this index's business.
                continue;
            }
            if e.file_attributes & rgy_discovery::FILE_ATTRIBUTE_DIRECTORY != 0 {
                dirs.push(p);
            } else {
                let cut = p.iter().rposition(|&b| b == b'/').unwrap_or(0);
                dirs.push(p[..cut.max(1)].to_vec());
            }
        }
        dirs.sort();
        dirs.dedup();
        for d in dirs {
            self.apply_dir(&d, false);
        }
    }

    /// Is this path inside the tree this index covers?
    ///
    /// The path and the root are both in the index's internal form (`/`),
    /// so the test is a prefix test on those.
    fn in_tree(&self, path: &[u8]) -> bool {
        if self.root == b"/" {
            return true;
        }
        match path.strip_prefix(self.root.as_slice()) {
            Some(rest) => rest.is_empty() || rest.starts_with(b"/"),
            None => false,
        }
    }

    /// Drive a change feed to its end: consume the feed's batches and keep
    /// this index in step with the disk. Events go through
    /// [`Live::apply_events`]; a rescan re-lists the root wholly. The feed's
    /// error ends the loop and is returned.
    ///
    /// This holds `&mut self` for the whole feed, which suits a caller that
    /// owns the `Live` outright. A daemon that shares it behind an `RwLock`
    /// takes the write lock per batch instead — lock, apply one batch,
    /// unlock — so queries interleave; holding the write across this loop
    /// would block every query for the feed's whole life.
    pub fn run_watch_feed(&mut self, watch: &mut dyn rgy_discovery::Watch, root: &[u8]) -> Result<(), String> {
        loop {
            match watch.next_batch()? {
                rgy_discovery::ChangeBatch::Events(events) => self.apply_events(&events),
                rgy_discovery::ChangeBatch::Rescan => {
                    self.apply_dir(root, true);
                }
            }
        }
    }

    /// Add an entry and, if it is a folder, its subtree. The subtree was
    /// read ahead by [`Live::fetch`]; it is scanned here if that did not
    /// happen.
    fn add_new(&mut self, path: Vec<u8>, e: OEnt, scan: Option<Vec<Listing>>) {
        let is_dir = e.kind & 3 == KIND_DIR && e.kind & FLAG_MOUNT == 0;
        self.put(path.clone(), e);
        if !is_dir {
            return;
        }
        self.trees.push(path.clone());
        let ls = scan.unwrap_or_else(|| walk::scan(&path, 4));
        for_each_path(&ls, &path, |p, r| {
            let e = OEnt::new(&p, r.kind, r.size, r.mtime);
            self.put(p, e);
        });
    }

    /// The folders whose listing may have changed since a wall-clock second:
    /// adding, removing or renaming an entry bumps its folder's mtime. One
    /// lstat per folder, in parallel, under a read lock; re-listing the few
    /// that changed is [`Live::apply_dir`]'s job. This is how a lost fsevents
    /// history is recovered without crawling the whole disk.
    pub fn changed_dirs(&self, since: u32) -> Vec<Vec<u8>> {
        let idx = &self.base;
        let de = idx.dir_entry();
        let check = |p: &[u8]| !walk::blocked(p) && lstat(p).is_some_and(|o| o.mtime >= since);
        let mut out: Vec<Vec<u8>> = rgy_par::map_collect_with(idx.d.saturating_sub(1), 1024, |ki| {
            let k = ki + 1;
            if self.is_dead(de[k]) {
                return None;
            }
            let mut buf = Vec::new();
            idx.path(de[k] as usize, &mut buf);
            // The base holds root-relative paths; the disk wants
            // openable ones.
            let on_disk = self.openable(&buf);
            check(&on_disk).then_some(on_disk)
        })
        .into_iter()
        .flatten()
        .collect();
        out.extend(self.over.iter().filter(|(p, o)| o.kind & 3 == KIND_DIR && o.kind & FLAG_MOUNT == 0 && check(p)).map(|(p, _)| p.clone()));
        let root = self.openable(b"/");
        if lstat(&root).is_some_and(|o| o.mtime >= since) {
            out.push(root);
        }
        out.sort();
        out
    }

    /// Everything alive, as listings for [`Index::build`].
    pub fn to_listings(&self) -> Vec<Listing> {
        // The overlay's keys are openable paths and the tree below is in
        // the base's root-relative space, so every key is translated before
        // it is grouped. Without that, an overlay whose root is not "/"
        // never joins the tree it belongs to, and the rebuild loses it.
        // The overlay's entries, grouped by the folder they are in. The
        // grouping is a sort and a scan, not a table: the overlay is
        // already sorted by its own keys, and the groups are read once
        // each.
        let mut by_parent: Vec<Overlay> = Vec::new();
        for (k, v) in &self.over {
            let Some(bp) = self.base_path(k) else { continue };
            let cut = bp.iter().rposition(|&b| b == b'/').unwrap_or(0);
            let parent: Vec<u8> = if cut == 0 { b"/".to_vec() } else { bp[..cut].to_vec() };
            let name = bp[cut + 1..].to_vec();
            by_parent.push((parent, name, *v));
        }
        // The sort is stable, so the children of a folder keep the order the
        // overlay's own keys have.
        by_parent.sort_by(|a, b| a.0.cmp(&b.0));
        let mut out = Vec::new();
        // (the base folder id if the base still has it, the path, the listing id)
        let mut stack: Vec<(Option<u32>, Vec<u8>, u32)> = vec![(Some(0), b"/".to_vec(), 0)];
        let mut next = 1u32;
        while let Some((bd, path, id)) = stack.pop() {
            let mut l = Listing { id, names: Vec::new(), ents: Vec::new() };
            let mut push = |l: &mut Listing, name: &[u8], kind: u8, size: u64, mtime: u32, child_path: Option<Vec<u8>>, bdir: Option<u32>| {
                let child = match child_path {
                    Some(cp) => {
                        let c = next;
                        next += 1;
                        stack.push((bdir, cp, c));
                        c
                    }
                    None => NONE,
                };
                l.ents.push(RawEnt { name_off: l.names.len() as u32, name_len: name.len() as u16, kind, size, mtime, child });
                l.names.extend_from_slice(name);
            };
            if let Some(d) = bd {
                for c in self.base.children(d) {
                    if self.is_dead(c as u32) {
                        continue;
                    }
                    let name = self.base.name(c);
                    let k = self.base.kind()[c];
                    // The base's dir id, and with it the path of the child,
                    // only when the child is a folder.
                    let sub = self.base.dir_of(c as u32);
                    let cp = sub.map(|_| {
                        let mut p = Vec::new();
                        self.base.path(c, &mut p);
                        p
                    });
                    push(&mut l, name, k, self.base.size_of(c), self.base.mtime()[c], cp, sub);
                }
            }
            if let Some(kids) = overlay_children(&by_parent, &path) {
                for &(_, ref name, o) in kids {
                    let descend = o.kind & 3 == KIND_DIR && o.kind & FLAG_MOUNT == 0;
                    push(&mut l, name, o.kind, o.size, o.mtime, descend.then(|| join(&path, name)), None);
                }
            }
            out.push(l);
        }
        out
    }
}

/// The overlay's entries, grouped by the folder they are in: the folder, the
/// name in it, and the entry itself.
type Overlay = (Vec<u8>, Vec<u8>, OEnt);

/// The overlay's entries of one folder, out of the entries grouped by
/// folder.
///
/// The groups are sorted by their folder, so the ones of a folder are one
/// run of the vector and a pair of binary searches finds them.
fn overlay_children<'a>(by_parent: &'a [Overlay], path: &[u8]) -> Option<&'a [Overlay]> {
    let start = by_parent.partition_point(|e| e.0.as_slice() < path);
    let end = by_parent.partition_point(|e| e.0.as_slice() <= path);
    (start < end).then(|| &by_parent[start..end])
}

/// Visit every entry of a scan with its full path.
pub fn for_each_path(ls: &[Listing], root: &[u8], mut f: impl FnMut(Vec<u8>, &RawEnt)) {
    // The listings by their id: the walk numbers them from zero, one per
    // listing, so the id is the place in the vector.
    let n = ls.iter().map(|l| l.id as usize + 1).max().unwrap_or(0);
    let mut by_id: Vec<Option<&Listing>> = vec![None; n];
    for l in ls {
        by_id[l.id as usize] = Some(l);
    }
    let mut stack = vec![(0u32, root.to_vec())];
    while let Some((id, path)) = stack.pop() {
        let Some(l) = by_id.get(id as usize).copied().flatten() else { continue };
        for r in &l.ents {
            let p = join(&path, &l.names[r.name_off as usize..][..r.name_len as usize]);
            if r.child != NONE {
                stack.push((r.child, p.clone()));
            }
            f(p, r);
        }
    }
}

/// Normalize a path: `/` separators, and no trailing separator, so a
/// folder and its listing name one thing.
///
/// The separators are normalized because the disk spells a path with `\` on
/// Windows and this index speaks `/` everywhere: a folder re-listed as
/// "C:\a\b" and the same folder named "C:/a/b" by a feed have to be the
/// same folder, or it is listed twice.
pub(crate) fn normalize(path: &[u8]) -> Vec<u8> {
    let mut p: Vec<u8> = path.iter().map(|&b| if b == b'\\' { b'/' } else { b }).collect();
    while p.len() > 1 && p.last() == Some(&b'/') {
        p.pop();
    }
    p
}

/// Join a folder and a name.
///
/// The seam owns the joining: the platform backends join the same way, so
/// a path that came from a feed and a path that came from a walk name the
/// same folder.
pub fn join(dir: &[u8], name: &[u8]) -> Vec<u8> {
    rgy_discovery::path::join(dir, name)
}

/// The key range holding everything strictly under a path.
fn subtree_bounds(path: &[u8]) -> (Vec<u8>, Vec<u8>) {
    let lo = join(path, b"");
    let mut hi = lo.clone();
    *hi.last_mut().unwrap() += 1; // '/' + 1 == '0'
    (lo, hi)
}

/// Stat a path without following links, and report what it holds now.
///
/// The seam's stat: a link is a link and not the thing it points at, and
/// the disk is the only place the answer comes from. The overlay needs it
/// to tell a file that changed from a file that is gone, and that answer
/// is worth having wherever there is a disk to ask.
pub fn lstat(path: &[u8]) -> Option<OEnt> {
    let s = rgy_discovery::dir::stat(path)?;
    Some(OEnt::new(path, s.kind, s.size, s.mtime))
}

#[cfg(all(test, feature = "std"))]
mod tests {
    use super::*;
    use crate::name_index::Index;
    use crate::name_query::{Hit, Query, Searcher};
    use alloc::format;
    use alloc::vec;
    use rgy_discovery::{PortableSource, Source};
    extern crate std;

    /// A tree in a temporary directory, with one file in it.
    fn tree(tag: &str) -> (std::path::PathBuf, Vec<u8>) {
        let dir = std::env::temp_dir().join(format!("rgy_live_{tag}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("only.txt"), b"hello\n").unwrap();
        let root = dir.to_string_lossy().into_owned().into_bytes();
        (dir, root)
    }

    /// The snapshot a daemon builds of a tree.
    fn base(root: &[u8]) -> Index {
        let src = PortableSource::new(root.to_vec(), 2);
        Index::build(src.enumerate().unwrap(), 0, 1, b"/")
    }

    /// The live index over a tree, knowing where the tree is.
    fn live(root: &[u8]) -> Live {
        Live::with_root(base(root), root)
    }

    /// The hits a query finds.
    fn hits(live: &Live, query: &str) -> Vec<Hit> {
        let s = Searcher { live };
        s.search_uncapped(&Query::parse(query, "").expect("parses"))
    }

    /// What a directory-granular feed says when a folder's contents change.
    fn folder_changed(root: &[u8]) -> Vec<rgy_discovery::UsnRecord> {
        vec![rgy_discovery::UsnRecord {
            file_reference: 0,
            parent_reference: 0,
            usn: 1,
            reason: rgy_discovery::USN_REASON_FILE_CREATE,
            file_attributes: rgy_discovery::FILE_ATTRIBUTE_DIRECTORY,
            name: String::from_utf8(root.to_vec()).unwrap(),
        }]
    }

    /// A folder that is re-listed must not answer the same file twice.
    ///
    /// The base holds root-relative paths ("/only.txt") and the overlay
    /// holds openable ones ("<root>/only.txt"). Without the crossing
    /// between the two, a re-listed folder's files land in the overlay
    /// while the base still holds them, and the search answers both: one
    /// file, two hits.
    #[test]
    fn a_re_listed_folder_answers_each_file_once() {
        let (dir, root) = tree("dup");
        let mut live = live(&root);
        assert_eq!(hits(&live, "only").len(), 1, "the snapshot answers the file once");

        live.apply_events(&folder_changed(&root));

        assert_eq!(hits(&live, "only").len(), 1, "the re-listed file is answered once, not twice");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A file that went away stops being answered.
    ///
    /// The base's entry for it has to die: the overlay is what knows the
    /// file is gone, and a base that still holds it is a file that answers
    /// to a name it no longer has.
    #[test]
    fn a_deleted_file_stops_being_answered() {
        let (dir, root) = tree("gone");
        let mut live = live(&root);
        assert_eq!(hits(&live, "only").len(), 1);

        std::fs::remove_file(dir.join("only.txt")).unwrap();
        live.apply_events(&folder_changed(&root));

        assert_eq!(hits(&live, "only").len(), 0, "the deleted file is not answered");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A change in another tree is not this index's business.
    ///
    /// The feed can be wider than the tree: the Windows journal is the
    /// volume's, and the daemon indexes a folder of it. Applying another
    /// tree's change would put entries in the index that the tree never
    /// had.
    #[test]
    fn a_change_in_another_tree_is_not_applied() {
        let (dir, root) = tree("other");
        let mut live = live(&root);
        std::fs::write(dir.join("only.txt"), b"hello\n").unwrap();

        // A folder outside the tree.
        let outside = std::env::temp_dir().join(format!("rgy_elsewhere_{}", std::process::id()));
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("other.txt"), b"other\n").unwrap();
        let name = outside.to_string_lossy().into_owned();

        live.apply_events(&[rgy_discovery::UsnRecord {
            file_reference: 0,
            parent_reference: 0,
            usn: 1,
            reason: rgy_discovery::USN_REASON_FILE_CREATE,
            file_attributes: rgy_discovery::FILE_ATTRIBUTE_DIRECTORY,
            name: name.clone(),
        }]);

        assert!(live.over.is_empty(), "another tree's change was applied: {:?}", live.over.keys().collect::<Vec<_>>());
        let _ = std::fs::remove_dir_all(&outside);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A rebuild keeps what the overlay knows.
    ///
    /// `to_listings` is how the live index is written back out; the
    /// overlay's paths have to be translated into the base's space for
    /// the tree they hang off, or the rebuild loses them.
    #[test]
    fn the_overlay_survives_the_rebuild() {
        let (dir, root) = tree("rebuild");
        let mut live = live(&root);
        live.apply_events(&folder_changed(&root));

        let rebuilt = Index::build(live.to_listings(), 0, 1, b"/");
        let live2 = Live::with_root(rebuilt, &root);
        assert_eq!(hits(&live2, "only").len(), 1, "the file is still there after the rebuild");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A live set whose parent chain is a forest says so, and the dead
    /// ancestor is the fault only the live set can see.
    ///
    /// The index's own chain is checked by `Index::validate_forest`; what
    /// only the live set knows is that a folder the change feed killed
    /// leaves its children without a path.
    #[test]
    fn the_live_forest_is_checked() {
        let dir = std::env::temp_dir().join(format!("rgy_forest_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(dir.join("sub/inner.txt"), b"hi\n").unwrap();
        let root = dir.to_string_lossy().into_owned().into_bytes();
        let live = live(&root);
        assert_eq!(live.validate_forest(), Ok(()), "a live set over a real tree is a forest: {:?}", live.validate_forest());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
