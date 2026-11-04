// SPDX-License-Identifier: Apache-2.0

//! DFS directory-block vocabulary (the `rgy_discovery::Source` block type).
//!
//! Transported from fsearch `walk.rs`: the kinds/flags every backend lists
//! and the [`Listing`] block the name index lays out in DFS order. The
//! platform walks themselves stay in their crates (macOS `getattrlistbulk`,
//! Windows MFT, Linux `getdents64`).
//!
//! The portable walk reads the filesystem through [`crate::dir`], which
//! asks the operating system directly: the paths here are bytes, the
//! separator is `/`, and the walk needs no `std` to do its work. What the
//! `std` feature still buys is the threads the fan-out runs on -- without
//! it the same fan-out runs the same work in order on the calling thread.

use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, Ordering};

use crate::dir;
use crate::path;

/// Sentinel for "no child listing" (an entry with no directory table).
pub const NONE: u32 = u32::MAX;

pub const KIND_FILE: u8 = 0;
pub const KIND_DIR: u8 = 1;
pub const KIND_LINK: u8 = 2;
pub const KIND_OTHER: u8 = 3;

/// Entry flag bit: hidden (UF_HIDDEN on macOS, FILE_ATTRIBUTE_HIDDEN on
/// Windows; the backends agree on the bit).
pub const FLAG_HIDDEN: u8 = 1 << 2;
/// Directory that is a mount point the scan did not descend into.
pub const FLAG_MOUNT: u8 = 1 << 3;

/// One directory's entries as listed by a backend, names packed in one
/// buffer. Listing id 0 is the scan root.
#[derive(Clone, Copy)]
pub struct RawEnt {
    pub name_off: u32,
    pub name_len: u16,
    /// kind in the low 2 bits, FLAG_* above.
    pub kind: u8,
    pub size: u64,
    pub mtime: u32,
    /// Temp id of the listing for this directory, or [`NONE`].
    pub child: u32,
}

/// A DFS directory block: one directory's [`RawEnt`]s and packed names.
pub struct Listing {
    pub id: u32,
    pub names: Vec<u8>,
    pub ents: Vec<RawEnt>,
}

/// Folders a scan must not open (macOS Full-Disk-Access gated folders set
/// this; the portable walk honours it the same way).
///
/// The cell is `rgy-par`'s, so it exists in a `no_std` build too: the
/// platform backends read it from their own walks, and a backend that
/// could not ask would have to keep its own copy.
pub static SKIP: rgy_par::sync::OnceCell<Vec<Vec<u8>>> = rgy_par::sync::OnceCell::new();

/// Whether `path` is inside a folder a scan must not open.
pub fn blocked(path: &[u8]) -> bool {
    SKIP.get().is_some_and(|v| {
        v.iter()
            .any(|s| path.starts_with(s) && (path.len() == s.len() || path[s.len()] == b'/'))
    })
}

/// The entries of one folder, as the block stream carries them.
///
/// The reading is [`crate::dir`]'s, and its contract is the walk's: the
/// names are the platform's own bytes, `.` and `..` are not in the list,
/// an entry whose metadata cannot be read is left out, a link is a link
/// and not the thing it points at, and the order is the platform's.
///
/// The kind is the kind in the low two bits and the flags above them,
/// which is what [`RawEnt`] says.
fn list_dir(dir_path: &[u8]) -> Option<Listing> {
    let list = dir::list_dir(dir_path)?;
    let mut names = Vec::with_capacity(list.names.len() * 16);
    let mut ents = Vec::with_capacity(list.stats.len());
    for (name, stat) in list.names.iter().zip(list.stats.iter()) {
        let name_off = names.len() as u32;
        names.extend_from_slice(name);
        ents.push(RawEnt {
            name_off,
            name_len: name.len().min(u16::MAX as usize) as u16,
            kind: stat.kind | stat.flags,
            size: stat.size,
            mtime: stat.mtime,
            child: NONE,
        });
    }
    Some(Listing { id: 0, names, ents })
}

/// A cooperative stop for a scan in flight, shared with whatever asked for
/// it (`interrupt::cancel_on_interrupt` hands out the one Ctrl-C trips).
///
/// The walk polls the token once per directory, and once per level in the
/// parallel path, so how long a cancel takes to land is bounded by one
/// directory read — long enough to be free, short enough to feel immediate.
///
/// Two tokens are equal when they share a flag, which is what makes "the
/// handler tripped the token the walk is holding" a testable statement
/// rather than a comment.
#[derive(Clone)]
pub struct Cancel(Flag);

#[derive(Clone)]
enum Flag {
    /// The caller's own flag, for a caller that trips it itself.
    Own(Arc<AtomicBool>),
    /// The process-wide flag a signal handler writes to. Read directly, with
    /// no lock and no lazy init: a handler runs on whatever stack the
    /// interrupt landed on, and re-entering an initializer there is how you
    /// deadlock a signal handler.
    Global(&'static AtomicBool),
}

impl Default for Cancel {
    fn default() -> Self {
        Self(Flag::Own(Arc::new(AtomicBool::new(false))))
    }
}

impl Cancel {
    pub fn new() -> Self {
        Self::default()
    }

    /// Ask the scan to stop. Only a relaxed store on an atomic, which is
    /// what a signal handler is allowed to do.
    pub fn trip(&self) {
        match &self.0 {
            Flag::Own(f) => f.store(true, Ordering::Relaxed),
            Flag::Global(f) => f.store(true, Ordering::Relaxed),
        }
    }

    pub fn is_cancelled(&self) -> bool {
        match &self.0 {
            Flag::Own(f) => f.load(Ordering::Relaxed),
            Flag::Global(f) => f.load(Ordering::Relaxed),
        }
    }

    /// A token tripped by this process's Ctrl-C handler.
    pub(crate) fn on_global(flag: &'static AtomicBool) -> Self {
        Self(Flag::Global(flag))
    }
}

impl core::fmt::Debug for Cancel {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Cancel")
            .field("cancelled", &self.is_cancelled())
            .finish()
    }
}

impl PartialEq for Cancel {
    fn eq(&self, other: &Self) -> bool {
        match (&self.0, &other.0) {
            (Flag::Own(a), Flag::Own(b)) => Arc::ptr_eq(a, b),
            (Flag::Global(a), Flag::Global(b)) => core::ptr::eq(*a, *b),
            // Only the two kinds can meet, and a token of one kind never
            // equals one of the other.
            _ => false,
        }
    }
}

/// The scan stopped before it finished.
///
/// A distinct error rather than a shorter `Vec`: a scan cut short is a tree
/// missing whole subtrees, and a caller that cannot tell that apart from a
/// small tree would write an index that answers every query wrongly. Nothing
/// partial is ever returned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cancelled;

impl core::fmt::Display for Cancelled {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("scan cancelled")
    }
}

impl core::error::Error for Cancelled {}

/// How far a scan has got. Handed to the progress callback once per
/// directory (per level, in the parallel path), which is often enough that
/// the callback should throttle its own output rather than rely on us to.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Progress {
    /// Directories listed so far.
    pub dirs: u64,
    /// Entries seen in them.
    pub entries: u64,
}

/// The walk's stack entry: the folder's bytes, and the parent slot
/// (listing index, entry index) whose `child` takes this listing's id.
type Frame = (Vec<u8>, Option<(usize, usize)>);

/// Portable whole-tree scan: one [`Listing`] per directory, DFS pre-order,
/// listing 0 = `root`. This is the `rgy_discovery::Source` block stream — the
/// macOS getattrlistbulk fast path lives in `discovery-macos` and returns
/// the same blocks.
///
/// `threads` > 1 fans the directory reads out across a pool of that width;
/// the resulting block stream is identical to [`scan_sequential`] either way
/// (a differential test pins that). `threads` of 0 or 1 runs inline.
///
/// Within one listing, entry order is whatever the platform's directory
/// read returned, so it
/// is not stable across machines — that is fine, because `Index::build`
/// sorts every listing's entries by name before laying out the index (and
/// `Index::lookup` binary-searches that sorted order). What the index does
/// depend on is the *listing* order, which is DFS pre-order in both paths.
pub fn scan(root: &[u8], threads: usize) -> Vec<Listing> {
    // A token nobody can trip cannot cancel anything.
    scan_with(root, threads, &Cancel::new(), &|_| {})
        .expect("a fresh cancel token is never tripped")
}

/// [`scan`], with a cancel token and a progress callback.
///
/// `on_progress` is called after each level (parallel path) or each
/// directory (sequential path), with the running totals.
pub fn scan_with(
    root: &[u8],
    threads: usize,
    cancel: &Cancel,
    // `Sync` because the parallel path hands it to the pool.
    on_progress: &(dyn Fn(Progress) + Sync),
) -> Result<Vec<Listing>, Cancelled> {
    if threads > 1 {
        return walk_parallel(root, threads, cancel, on_progress);
    }
    scan_sequential_with(root, cancel, on_progress)
}

/// The reference walk: strictly sequential, one folder at a time.
///
/// Kept public and unoptimised on purpose — it is the correctness oracle
/// that [`walk_parallel`] is tested against.
pub fn scan_sequential(root: &[u8]) -> Vec<Listing> {
    scan_sequential_with(root, &Cancel::new(), &|_| {})
        .expect("a fresh cancel token is never tripped")
}

/// [`scan_sequential`], with a cancel token and a progress callback.
pub fn scan_sequential_with(
    root: &[u8],
    cancel: &Cancel,
    on_progress: &(dyn Fn(Progress) + Sync),
) -> Result<Vec<Listing>, Cancelled> {
    let mut out: Vec<Listing> = Vec::new();
    let mut next_id = 1u32;
    let mut seen = 0u64;
    // (path, parent_slot in out where the child id goes) — filled after the
    // child listing is built. The path is bytes: the tree's separator is
    // `/`, and the bytes are the bytes the filesystem gave.
    let mut stack: Vec<Frame> = alloc::vec![(root.to_vec(), None)];
    while let Some((dir, fix)) = stack.pop() {
        if cancel.is_cancelled() {
            return Err(Cancelled);
        }
        if blocked(&dir) {
            continue;
        }
        let Some(mut listing) = list_dir(&dir) else {
            continue;
        };
        let id = if fix.is_none() { 0 } else { next_id };
        if fix.is_some() {
            next_id += 1;
        }
        listing.id = id;
        if let Some((li, ei)) = fix {
            out[li].ents[ei].child = id;
        }
        // Collect subdirectories before pushing so the walk stays DFS.
        let mut kids = Vec::new();
        for (ei, ent) in listing.ents.iter().enumerate() {
            if ent.kind & 3 == KIND_DIR {
                let name = &listing.names
                    [ent.name_off as usize..(ent.name_off as usize + ent.name_len as usize)];
                kids.push((path::join(&dir, name), ei));
            }
        }
        let li = out.len();
        seen += listing.ents.len() as u64;
        out.push(listing);
        for (p, ei) in kids.into_iter().rev() {
            stack.push((p, Some((li, ei))));
        }
        on_progress(Progress {
            dirs: out.len() as u64,
            entries: seen,
        });
    }
    Ok(out)
}

/// The same walk with the directory reads fanned out across the pool.
///
/// Listing a directory is I/O-bound and the next frontier depends on the
/// current one's result, so this expands breadth-first (one parallel batch
/// per directory depth) and only then renumbers into DFS pre-order. The
/// output is byte-identical to [`scan_sequential`].
///
/// The pool is `rgy-par`'s, at the width the caller asked for: the walk
/// has its own width, and it is not the query's. Without the `std` feature
/// there are no threads to run it on, and the same fan-out runs the same
/// work in order on the calling thread.
fn walk_parallel(
    root: &[u8],
    threads: usize,
    cancel: &Cancel,
    on_progress: &(dyn Fn(Progress) + Sync),
) -> Result<Vec<Listing>, Cancelled> {
    /// A discovered directory before DFS numbering.
    struct Node {
        listing: Listing,
        /// `(parent node index, entry index inside the parent's listing)`.
        parent: Option<(usize, usize)>,
        /// Subdirectory node indices, in listing order.
        children: Vec<usize>,
    }

    let mut nodes: Vec<Node> = Vec::new();
    let mut seen = 0u64;
    // (path, parent node, entry index in the parent's listing)
    let mut pending: Vec<Frame> = alloc::vec![(root.to_vec(), None)];

    while !pending.is_empty() {
        // Once per level rather than per directory: the level below is a
        // batch of directory reads already in flight, and stopping between
        // them buys nothing a whole read does not already cost.
        if cancel.is_cancelled() {
            return Err(Cancelled);
        }
        // The workers take the indices off a shared counter, one at a time:
        // a directory with a hundred thousand entries does not hold the
        // others up the way a fixed split would.
        let listings: Vec<Option<Listing>> = rgy_par::each_index_at(
            threads,
            pending.len(),
            || (),
            |_, i| {
                let (dir, _) = &pending[i];
                if blocked(dir) { None } else { list_dir(dir) }
            },
        );

        let mut next = Vec::new();
        for ((dir, parent), mut listing) in pending.iter().zip(listings) {
            // A blocked or unreadable directory yields no block at all, so the
            // parent's `child` link stays NONE — same as the sequential walk.
            let Some(listing) = listing.take() else {
                continue;
            };
            let node = nodes.len();
            if let Some((p, _)) = *parent {
                nodes[p].children.push(node);
            }
            // Subdirectories in listing order, so the DFS below reaches them
            // in the same order the sequential walk pops them off its stack.
            for (ei, ent) in listing.ents.iter().enumerate() {
                if ent.kind & 3 == KIND_DIR {
                    let name = &listing.names
                        [ent.name_off as usize..(ent.name_off as usize + ent.name_len as usize)];
                    next.push((path::join(dir, name), Some((node, ei))));
                }
            }
            seen += listing.ents.len() as u64;
            nodes.push(Node {
                listing,
                parent: *parent,
                children: Vec::new(),
            });
        }
        pending = next;
        on_progress(Progress {
            dirs: nodes.len() as u64,
            entries: seen,
        });
    }

    if nodes.is_empty() {
        return Ok(Vec::new());
    }

    // DFS pre-order over the discovered tree. Iterative: a deep tree must
    // not turn into deep Rust recursion.
    let mut order: Vec<usize> = alloc::vec![0];
    order.reserve(nodes.len());
    let mut cursor: Vec<(usize, usize)> = alloc::vec![(0, 0)];
    while let Some(&(node, seen)) = cursor.last() {
        if seen < nodes[node].children.len() {
            let kid = nodes[node].children[seen];
            cursor.last_mut().expect("cursor non-empty").1 += 1;
            order.push(kid);
            cursor.push((kid, 0));
        } else {
            cursor.pop();
        }
    }

    // Stamp ids and resolve every `child` link to its DFS id.
    for (id, &node) in order.iter().enumerate() {
        let id = id as u32;
        nodes[node].listing.id = id;
        if let Some((parent, ei)) = nodes[node].parent {
            nodes[parent].listing.ents[ei].child = id;
        }
    }

    let mut out: Vec<Listing> = Vec::with_capacity(order.len());
    for &node in &order {
        out.push(core::mem::replace(
            &mut nodes[node].listing,
            Listing {
                id: 0,
                names: Vec::new(),
                ents: Vec::new(),
            },
        ));
    }
    Ok(out)
}

/// List a single directory (no recursion). Subdirectories come back with
/// `child == NONE`. Used by the live updater.
pub fn list_one(path: &[u8]) -> Option<Listing> {
    list_dir(path)
}

#[cfg(all(test, feature = "std"))]
mod tests {
    extern crate std;

    use super::*;
    use alloc::format;
    use alloc::string::{String, ToString};
    use std::collections::BTreeMap;

    /// A tree with the shapes that break naive parallel walks: nesting,
    /// empty directories, a deep chain, and a non-ASCII name.
    fn fixture(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("rgy-walk-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("a/b/c")).expect("fixture dirs");
        std::fs::create_dir_all(dir.join("a/empty")).expect("fixture dirs");
        std::fs::create_dir_all(dir.join("zeta")).expect("fixture dirs");
        std::fs::create_dir_all(dir.join("a/b/c/deep/deeper")).expect("fixture dirs");
        for f in [
            "top.txt",
            "a/one.txt",
            "a/b/two.rs",
            "a/b/c/three.md",
            "zeta/four.json",
        ] {
            std::fs::write(dir.join(f), b"x").expect("fixture file");
        }
        std::fs::write(dir.join("a/acentua\u{e7}ao.txt"), b"y").expect("fixture file");
        dir
    }

    fn name_of(l: &Listing, e: &RawEnt) -> String {
        let start = e.name_off as usize;
        let end = start + e.name_len as usize;
        String::from_utf8_lossy(&l.names[start..end]).into_owned()
    }

    /// A directory's rows: `(name, kind, size, mtime, child directory path)`.
    type Rows = BTreeMap<String, Vec<(String, u8, u64, u32, Option<String>)>>;

    /// Reshape a block stream into `directory path -> rows`, so the
    /// comparison does not depend on the order the platform's directory
    /// read happened to
    /// return entries in (which is not stable, and `Index::build` sorts it
    /// away anyway).
    fn normalize(ls: &[Listing]) -> Rows {
        let mut paths: Vec<Option<String>> = alloc::vec![None; ls.len()];
        if ls.is_empty() {
            return BTreeMap::new();
        }
        paths[0] = Some(String::new());
        // DFS pre-order gives every listing a lower id than its children, so
        // one forward pass resolves every path.
        for id in 0..ls.len() {
            let Some(base) = paths[id].clone() else {
                continue;
            };
            for ent in &ls[id].ents {
                let child = ent.child as usize;
                if ent.child == NONE || child >= ls.len() {
                    continue;
                }
                paths[child] = Some(format!("{base}/{}", name_of(&ls[id], ent)));
            }
        }
        ls.iter()
            .enumerate()
            .map(|(id, l)| {
                let base = paths[id].clone().unwrap_or_default();
                let rows = l
                    .ents
                    .iter()
                    .map(|e| {
                        (
                            name_of(l, e),
                            e.kind,
                            e.size,
                            e.mtime,
                            if e.child == NONE {
                                None
                            } else {
                                paths.get(e.child as usize).cloned().flatten()
                            },
                        )
                    })
                    .collect();
                (base, rows)
            })
            .collect()
    }

    /// The tree, read until two reads in a row agree.
    ///
    /// A directory's timestamp lives in two records on NTFS: the one the
    /// directory keeps about itself, and the one its parent keeps about
    /// it. The parent's copy is refreshed a moment later -- a file's is
    /// not, because a file's timestamp changes when the file is written
    /// and the parent's entry changes with it -- and the tree truncates
    /// both to seconds. A read that lands on the wrong side of a second
    /// boundary therefore sees a different answer than the read before
    /// it, for a tree that did not change at all.
    ///
    /// Two reads of a tree that has stopped changing must agree. This is
    /// the tree that has stopped changing: the comparison that follows is
    /// about the walk, not about the platform's bookkeeping.
    fn settled(root: &[u8]) -> Rows {
        let mut last = normalize(&scan_sequential(root));
        for _ in 0..50 {
            let now = normalize(&scan_sequential(root));
            if now == last {
                return last;
            }
            last = now;
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        last
    }

    #[test]
    fn parallel_walk_matches_sequential_oracle() {
        let dir = fixture("diff");
        let root = dir.to_string_lossy().as_bytes().to_vec();
        let want = settled(&root);
        assert!(!want.is_empty(), "fixture produced no blocks");
        for threads in [2usize, 4, 8] {
            let got = normalize(&scan(&root, threads));
            assert_eq!(
                want, got,
                "parallel walk with threads={threads} diverged from the sequential oracle"
            );
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn zero_and_one_thread_take_the_inline_path() {
        let dir = fixture("inline");
        let root = dir.to_string_lossy().as_bytes().to_vec();
        let want = settled(&root);
        for threads in [0usize, 1] {
            assert_eq!(want, normalize(&scan(&root, threads)));
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn unreadable_root_yields_no_blocks() {
        let root = std::env::temp_dir().join("rgy-walk-absent-dir-zzz");
        let root = root.to_string_lossy().into_owned();
        let root = root.as_bytes();
        assert!(scan_sequential(root).is_empty());
        assert!(scan(root, 4).is_empty());
    }

    /// A cancelled scan must yield `Err(Cancelled)` and no blocks, in both
    /// paths. The reason this is a distinct error rather than a shorter
    /// `Vec`: a cut-short scan is a tree missing whole subtrees, and a
    /// caller that cannot tell that from a small tree writes an index that
    /// answers every query wrongly. Returning a partial listing would be
    /// exactly that mistake.
    #[test]
    fn a_cancelled_scan_returns_the_error_and_no_blocks() {
        let dir = fixture("cancel");
        let root = dir.to_string_lossy().as_bytes().to_vec();
        for threads in [1usize, 4] {
            let cancel = Cancel::new();
            cancel.trip();
            let blocked = |r: Result<Vec<Listing>, Cancelled>| match r {
                Err(Cancelled) => "cancelled".to_string(),
                Ok(ls) => format!("{} blocks for a scan cancelled before it started", ls.len()),
            };
            assert_eq!(
                "cancelled",
                blocked(scan_with(&root, threads, &cancel, &|_| {})),
                "threads={threads} did not refuse a pre-cancelled scan"
            );
            assert_eq!(
                "cancelled",
                blocked(scan_sequential_with(&root, &cancel, &|_| {})),
                "the sequential oracle must refuse a cancelled scan too"
            );
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The scan a caller can cancel is the same scan it would have got, and
    /// the progress it reports is a real count of the tree — not a guess, and
    /// not a constant. Progress counts directory blocks listed so far and
    /// the entries in them, so the final call must account for the whole
    /// fixture and the counts must never go backwards.
    #[test]
    fn progress_counts_the_tree_and_grows() {
        let dir = fixture("progress");
        let root = dir.to_string_lossy().as_bytes().to_vec();
        let cancel = Cancel::new();
        for threads in [1usize, 4] {
            // The callback is `Fn` + `Sync` (the parallel path hands it to the
            // pool), so the tally behind it needs interior mutability.
            let seen = std::sync::Mutex::new(Vec::new());
            let blocks = scan_with(&root, threads, &cancel, &|p| {
                seen.lock().expect("progress tally").push(p)
            })
            .expect("not cancelled");
            let seen = seen.into_inner().expect("progress tally");
            let last = *seen
                .last()
                .expect("a scan that listed nothing reported no progress");
            assert_eq!(
                (last.dirs as usize, last.entries as usize),
                (
                    blocks.len(),
                    blocks.iter().map(|b| b.ents.len()).sum::<usize>()
                ),
                "threads={threads}: the final progress line does not match the blocks returned"
            );
            assert!(
                seen.windows(2)
                    .all(|w| w[1].dirs >= w[0].dirs && w[1].entries >= w[0].entries),
                "threads={threads}: progress went backwards: {seen:?}"
            );
            // The untripped token must be the one thing that made a
            // difference: the same walk, uncancelled, is complete.
            assert!(!cancel.is_cancelled());
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
