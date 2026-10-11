// SPDX-License-Identifier: Apache-2.0

//! Which files a query searches, which packs they live in, and how those packs
//! are cut into claimable pieces.
//!
//! This is the whole planner of a pack query, and none of it reads a byte: it
//! turns a decoded manifest and a [`SearchConfig`] into a table of units that
//! workers claim. Holding it here rather than in `rgx-tv` is not tidiness —
//! the boundary invariant the paper states is enforced by [`cut_units`], and
//! the one predicate that decides membership ([`kept_flags`]) is the single
//! place a file can be dropped, so a decoded record and a locator cannot
//! disagree about it.

use alloc::sync::Arc;
use alloc::vec::Vec;

use crate::tv::format::{FileLoc, FileRecord, Manifest, ManifestHead, RawManifest};
use crate::tv::query::{SearchConfig, SearchMatch};

/// Concatenate the buckets in key order. The key says *where*, so the generic
/// bound is the whole contract: a path that keys by pack keeps pack order, and
/// the buffered path keys by pack and offset because its units are finer.
///
/// Every mode keys its result buckets by the claim itself and concatenates
/// them in that order, so two runs of the same query on the same store
/// print the same bytes in the same order.
pub fn merge_buckets<K: Ord>(
    mut buckets: Vec<(K, Vec<SearchMatch>)>,
    total_lines: usize,
) -> Vec<SearchMatch> {
    buckets.sort_by(|a, b| a.0.cmp(&b.0));
    let mut out: Vec<SearchMatch> = Vec::with_capacity(total_lines);
    for (_, mut b) in buckets {
        out.append(&mut b);
    }
    out
}

/// Splits a manifest's records into the ones that reach the worker loop and the
/// 0-byte ones. Takes the records by value: the cold path moves them.
pub fn split_owned(
    files: Vec<FileRecord>,
    config: &SearchConfig,
) -> (Vec<FileRecord>, Vec<FileRecord>) {
    let mut active = Vec::new();
    let mut empty = Vec::new();
    for f in files {
        if !kept(&f, config) {
            continue;
        }
        if f.is_empty {
            empty.push(f);
        } else {
            active.push(f);
        }
    }
    (active, empty)
}

/// Whether a record is searched at all: not deleted, not binary unless asked,
/// and not over `-M`. One predicate, because the two splitters below are the
/// only two places a file can be dropped and they must drop the same ones.
#[inline]
pub fn kept(f: &FileRecord, config: &SearchConfig) -> bool {
    kept_flags(f.is_deleted, f.is_binary, f.length, config)
}

#[inline]
pub fn kept_loc(f: &FileLoc, config: &SearchConfig) -> bool {
    kept_flags(f.is_deleted(), f.is_binary(), f.length, config)
}

/// One predicate over the three properties that decide membership, so a
/// decoded record and a locator cannot disagree about it.
#[inline]
pub fn kept_flags(is_deleted: bool, is_binary: bool, length: u64, config: &SearchConfig) -> bool {
    if is_deleted {
        return false;
    }
    if !config.search_binary && is_binary {
        return false;
    }
    match config.max_file_size {
        Some(max) if length > max => return false,
        _ => {}
    }
    true
}

/// Applies [`kept`] to a cached index of file records, for a pool whose index
/// was built under different limits than this query carries.
pub fn narrow(idx: &[u32], manifest: &Manifest, config: &SearchConfig) -> Vec<u32> {
    if config.search_binary && config.max_file_size.is_none() {
        return idx.to_vec();
    }
    idx.iter()
        .copied()
        .filter(|&i| kept(&manifest.files[i as usize], config))
        .collect()
}

/// The same split as [`split_owned`], over locators rather than decoded
/// records — the query path, where no path has been decoded. Returns indices
/// into `locs`: the ones that reach the worker loop, and the 0-byte ones.
pub fn split_locs(locs: &[FileLoc], config: &SearchConfig) -> (Vec<u32>, Vec<u32>) {
    let mut active = Vec::with_capacity(locs.len());
    let mut empty = Vec::new();
    for (i, f) in locs.iter().enumerate() {
        if !kept_loc(f, config) {
            continue;
        }
        if f.is_empty() {
            empty.push(i as u32);
        } else {
            active.push(i as u32);
        }
    }
    (active, empty)
}

#[derive(Clone, Copy)]
pub struct Unit {
    /// Position in `table.order` — the pack, for its path and length.
    pub pack: u32,
    /// `files[first..last]` in the plan, ascending, and the first file's offset
    /// is where this unit starts.
    pub first: u32,
    pub last: u32,
    /// Byte range within the pack payload: `start..start + len`.
    pub start: u32,
    pub len: u32,
}

/// The size a claim aims for, in bytes. A file larger than this is a unit of
/// its own: it cannot be cut without breaking the boundary invariant, and
/// pretending otherwise would make a unit that holds a piece of a file.
pub const UNIT_BYTES: u32 = 1 << 20;

/// Which packs to read, in what order, and which files each one answers for.
pub struct PackPlan {
    /// The manifest as it was read, so a result can decode the name of the
    /// file it belongs to. Held rather than dropped: this is the buffer the
    /// paths are slices of.
    pub raw: Arc<RawManifest>,
    /// Every file's fixed fields, in manifest order.
    pub locs: Arc<Vec<FileLoc>>,
    /// Indices into `locs`, ascending within each pack's run.
    pub files: Vec<u32>,
    /// Pack `p`'s files are `files[bounds[p] .. bounds[p + 1]]`. An empty range
    /// means every file in that pack was skipped: deleted, 0-byte or binary.
    pub bounds: Vec<u32>,
    /// The claimable pieces of every pack, longest first, which is the greedy
    /// order for a fixed set of workers and the same one the pack queue used.
    pub units: Vec<Unit>,
}

impl PackPlan {
    pub fn build(
        raw: Arc<RawManifest>,
        head: &ManifestHead,
        locs: Arc<Vec<FileLoc>>,
        active: &[u32],
    ) -> PackPlan {
        let np = head.packs.len();

        // `pack_id` to position, by direct index rather than by hash. The
        // ids are a dense range the manifest itself defines, so the hash
        // was pure overhead. The table is one allocation of `max_id + 1`
        // and a miss is a sentinel rather than a `None`.
        let max_id = head.packs.iter().map(|p| p.pack_id).max().unwrap_or(0) as usize;
        let mut slot_of = vec![u32::MAX; max_id + 1];
        for (pos, p) in head.packs.iter().enumerate() {
            slot_of[p.pack_id as usize] = pos as u32;
        }

        let mut counts = vec![0u32; np];
        for &fi in active {
            let id = locs[fi as usize].pack_id as usize;
            if let Some(&pos) = slot_of.get(id) {
                if pos != u32::MAX {
                    counts[pos as usize] += 1;
                }
            }
        }

        let mut bounds = vec![0u32; np + 1];
        for i in 0..np {
            bounds[i + 1] = bounds[i] + counts[i];
        }

        // `active` is ascending, so every pack's run comes out ascending too.
        let mut files = vec![0u32; bounds[np] as usize];
        let mut fill: Vec<u32> = bounds[..np].to_vec();
        for &fi in active {
            let id = locs[fi as usize].pack_id as usize;
            if let Some(&pos) = slot_of.get(id) {
                if pos != u32::MAX {
                    let w = &mut fill[pos as usize];
                    files[*w as usize] = fi;
                    *w += 1;
                }
            }
        }

        let units = cut_units(&files, &bounds, &locs);
        PackPlan {
            raw,
            locs,
            files,
            bounds,
            units,
        }
    }
}

/// Cut every pack's file run into units of at most [`UNIT_BYTES`], at file
/// boundaries, longest unit first.
///
/// The cut is a single pass over the file run: a unit grows until the next
/// file would take it past the target, and then the file starts the next one.
/// A file bigger than the target is a unit by itself rather than a cut one
/// — the loop below cannot split it, and a unit holding part of a file
/// would report that file twice.
pub fn cut_units(files: &[u32], bounds: &[u32], locs: &[FileLoc]) -> Vec<Unit> {
    let mut units: Vec<Unit> = Vec::with_capacity(bounds.len());
    for p in 0..bounds.len().saturating_sub(1) {
        let (from, to) = (bounds[p] as usize, bounds[p + 1] as usize);
        let mut k = from;
        while k < to {
            let first = k;
            let start = locs[files[k] as usize].offset as u32;
            let mut end = start;
            while k < to {
                let f = &locs[files[k] as usize];
                let f_end = (f.offset + f.length) as u32;
                // A file that does not fit joins the next unit; a file that is
                // bigger than a whole unit is one by itself, because there is
                // no legal cut inside it.
                if k > first && f_end - start > UNIT_BYTES {
                    break;
                }
                end = f_end;
                k += 1;
            }
            units.push(Unit {
                pack: p as u32,
                first: first as u32,
                last: k as u32,
                start,
                len: end - start,
            });
        }
    }
    units.sort_by_key(|u| core::cmp::Reverse(u.len));
    units
}
