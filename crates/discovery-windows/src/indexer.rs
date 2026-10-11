// SPDX-License-Identifier: Apache-2.0

//! MFT walk and USN tailing: the two ways this indexer learns about files.

use crate::ntfs;
use crate::volume::{Volume, VolumeError};
use name_core::index::{Index, NodeKind, PathResolver, ROOT_FRN, volume_key};

/// Maximum number of $MFT records to read. Real volumes sit far below this;
/// the bound exists only so a corrupt volume cannot spin forever.
const MAX_RECORDS: u64 = 16_000_000;

/// Where record assembly resumes after a hole or a gap that ends at `gap_end`.
///
/// A record overlapping `gap_end` is torn — its missing half is zeros — so its
/// remaining bytes are discarded and assembly resumes at the next record
/// boundary. Returns `(bytes_to_skip, stream_offset_of_next_record)`.
fn resume_after_gap(gap_end: u64, rec_size: u64) -> (u64, u64) {
    let skip = (rec_size - gap_end % rec_size) % rec_size;
    (skip, gap_end + skip)
}

/// One indexed directory entry: the `discovery::Source` row a [`NameRow`]s
/// scan produces, before the rows are grouped into DFS [`Listing`]s.
#[derive(Clone, Debug)]
pub struct NameRow {
    /// Raw FRN (no volume ordinal) of the record owning this name — the
    /// directory linkage when `is_dir`.
    pub frn: u64,
    /// Raw parent FRN the name sits under.
    pub parent_frn: u64,
    pub name: Vec<u8>,
    pub is_dir: bool,
    /// Reparse point (junction / symlink / mount): a `link` kind in the
    /// name index, whatever the record's dir bit says.
    pub link: bool,
    pub size: u64,
    /// Last-write time from `$STANDARD_INFORMATION`, unix seconds.
    pub mtime: u32,
}

/// Is this `$FILE_NAME` entry one a user search should see? Zen drops
/// DOS-only 8.3 names (namespace 2) and names pointing inside the internal
/// NTFS folders (parent 0..15 except the volume root at 5) — "ignore name attr
/// for internal ntfs folder". Filtering at index time keeps them out of
/// `best_name` and the alias lists without changing the parser.
fn name_is_indexable(e: &ntfs::FileNameEntry) -> bool {
    if e.namespace == ntfs::NAMESPACE_DOS {
        return false;
    }
    let parent = ntfs::frn_number(e.parent_frn);
    !(parent < 0x10 && parent != ROOT_FRN)
}

/// Group [`NameRow`]s into DFS [`discovery::walk::Listing`]s for
/// [`name_core::name_index::Index::build`]: one block per directory,
/// listing 0 = the children of `root_frn`, each directory entry's `child`
/// naming the listing of that directory's children.
///
/// Rows of a directory's aliases (same `frn`, different name) point at the
/// same child listing — directories are not hard-linked on NTFS, so this is
/// only a hard-link-style edge case in the data.
pub fn listings_from_rows(rows: &[NameRow], root_frn: u64) -> Vec<discovery::walk::Listing> {
    use discovery::walk::{KIND_DIR, KIND_FILE, KIND_LINK, Listing, NONE, RawEnt};
    use std::collections::HashMap;

    let mut by_parent: HashMap<u64, Vec<usize>> = HashMap::new();
    for (i, r) in rows.iter().enumerate() {
        by_parent.entry(r.parent_frn).or_default().push(i);
    }

    // (parent frn, slot in `out` that takes this listing's id)
    let mut out: Vec<Listing> = Vec::new();
    let mut next_id = 1u32;
    // (parent_frn, Option<(listing index, ent index)> whose child field
    // takes the id of the listing we are about to build)
    let mut stack: Vec<(u64, Option<(usize, usize)>)> = vec![(root_frn, None)];
    while let Some((parent, fix)) = stack.pop() {
        let Some(indices) = by_parent.get(&parent) else {
            continue;
        };
        let id = if fix.is_none() { 0 } else { next_id };
        if fix.is_some() {
            next_id += 1;
        }
        if let Some((li, ei)) = fix {
            out[li].ents[ei].child = id;
        }
        let mut names = Vec::new();
        let mut ents = Vec::new();
        let mut kids = Vec::new();
        for &ri in indices {
            let r = &rows[ri];
            let name_off = names.len() as u32;
            names.extend_from_slice(&r.name);
            // fsearch's walk model: a symlink/reparse point is its own kind,
            // not a file or dir. Zen's `type:link` rides on it.
            let kind = if r.link {
                KIND_LINK
            } else if r.is_dir {
                KIND_DIR
            } else {
                KIND_FILE
            };
            let ei = ents.len();
            ents.push(RawEnt {
                name_off,
                name_len: r.name.len() as u16,
                kind,
                size: r.size,
                mtime: r.mtime,
                child: NONE,
            });
            if r.is_dir && !r.link {
                kids.push((r.frn, ei));
            }
        }
        let li = out.len();
        out.push(Listing { id, names, ents });
        // Reverse push: pop order gives the first dirent first (stack).
        for (child_frn, ei) in kids.into_iter().rev() {
            stack.push((child_frn, Some((li, ei))));
        }
    }
    out
}

#[derive(Debug, Default, Clone, Copy)]
pub struct IndexStats {
    pub records_read: u64,
    pub records_in_use: u64,
    pub names_seen: u64,
    pub files: u64,
    pub directories: u64,
    pub resolved_paths: usize,
    pub orphan_edges: usize,
    pub cycle_edges: usize,
    /// Records (or sibling overflow records) the volume refused to return.
    /// Counted rather than fatal: the volume is live and records move.
    pub read_errors: u64,
    /// Entries flagged `FILE_ATTRIBUTE_REPARSE_POINT` — junctions and symlinks.
    pub reparse_points: u64,
}

pub struct Indexer<'a> {
    volume: &'a Volume,
    /// Position of this volume in a multi-volume scan. Prepended to every FRN
    /// so two volumes cannot collide on the same record number.
    ordinal: u16,
}

/// What one `tail_usn` pass learned.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct TailOutcome {
    pub applied: usize,
    pub next_start_usn: i64,
    /// Identity and window of the journal the read went against, as reported
    /// by `FSCTL_QUERY_USN_JOURNAL`. Surfaced so a resume point that refuses to
    /// advance can be told apart from a query that returned nothing.
    pub journal_id: u64,
    pub journal_lowest_valid_usn: i64,
    pub journal_next_usn: i64,
    /// The journal's identity changed since the caller last looked: it was
    /// deleted and recreated, so the old USN describes a different journal.
    pub journal_id_changed: bool,
    /// A record carried `USN_REASON_JOURNAL_DELETE`, meaning the journal
    /// wrapped past the reader's position and history in between is gone.
    pub journal_wrapped: bool,
    /// The requested USN sat outside the journal's live window and was moved
    /// to `lowest_valid_usn` rather than read from a recycled hole.
    pub start_usn_was_stale: bool,
}

impl<'a> Indexer<'a> {
    pub fn new(volume: &'a Volume, ordinal: u16) -> Self {
        Indexer { volume, ordinal }
    }

    /// Walk the MFT and build a complete path index.
    ///
    /// Attempts fast extent streaming by reading MFT cluster runs directly.
    /// If direct streaming is unavailable or yields no entries, falls back to
    /// sequential record-by-record `FSCTL_GET_NTFS_FILE_RECORD` calls.
    ///
    /// Writes into the caller's `index` rather than building its own, so a
    /// multi-volume scan can accumulate several volumes into one index. Each
    /// volume seeds its own root with its ordinal, which is what keeps the
    /// second volume's records from resolving against the first volume's prefix.
    pub fn scan_all(
        &self,
        index: &mut Index,
        progress: &mut dyn FnMut(u64, u64, u64),
    ) -> Result<IndexStats, VolumeError> {
        let mut stats = IndexStats::default();
        let mut resolver = PathResolver::new();
        let mut rows = Vec::new();

        index.add_root(self.ordinal, &self.volume.root);
        let limit = self.volume.record_count.clamp(1, MAX_RECORDS);

        // The extent-streaming path is the default: it derives each record's
        // FRN from its offset in the `$MFT` data stream (`stream_offset /
        // record_size`) and reads whole extents instead of one FSCTL per
        // record. On E:\ the two paths were verified to agree exactly —
        // 1,008,640 records read, 326,162 in use, 322,421 resolved, 0 orphans,
        // 0 read errors on both — so the faster one carries the default. The
        // per-record path remains the reference and fallback: it asks for
        // record N by number, so its FRN cannot drift even on a volume whose
        // run list cannot be decoded. `ERS_USE_RECORDS=1` forces it.
        let streamed = std::env::var_os("ERS_USE_RECORDS").is_none()
            && match self.volume.read_mft_data_runs() {
                Ok(runs) => self
                    .scan_via_runs(&runs, limit, &mut resolver, &mut stats, progress, &mut rows)
                    .is_ok(),
                Err(_) => false,
            };

        // Fallback to sequential record-by-record scan if streaming didn't succeed
        if !streamed {
            stats = IndexStats::default();
            resolver = PathResolver::new();
            rows.clear();
            self.scan_via_records(limit, &mut resolver, &mut stats, progress, &mut rows)?;
        }

        let resolve_stats = resolver.resolve_into(index);
        stats.resolved_paths = resolve_stats.resolved;
        stats.orphan_edges = resolve_stats.orphans;
        stats.cycle_edges = resolve_stats.cycles;

        Ok(stats)
    }

    /// Walk the MFT and collect every indexed name as a [`NameRow`] — the
    /// `discovery::Source` feed for the name index v2 (grouped into DFS
    /// [`discovery::walk::Listing`]s by [`listings_from_rows`]). Same traps
    /// and both scan paths as [`Self::scan_all`]; the rows come out beside
    /// the PathResolver writes the old index still consumes.
    pub fn scan_rows(
        &self,
        progress: &mut dyn FnMut(u64, u64, u64),
    ) -> Result<(Vec<NameRow>, IndexStats), VolumeError> {
        let mut stats = IndexStats::default();
        let mut resolver = PathResolver::new();
        let mut rows = Vec::new();
        let limit = self.volume.record_count.clamp(1, MAX_RECORDS);

        let streamed = std::env::var_os("ERS_USE_RECORDS").is_none()
            && match self.volume.read_mft_data_runs() {
                Ok(runs) => self
                    .scan_via_runs(&runs, limit, &mut resolver, &mut stats, progress, &mut rows)
                    .is_ok(),
                Err(_) => false,
            };
        if !streamed {
            stats = IndexStats::default();
            rows.clear();
            self.scan_via_records(limit, &mut resolver, &mut stats, progress, &mut rows)?;
        }
        Ok((rows, stats))
    }

    /// Fast path: stream the MFT's logical record space through its cluster
    /// extents.
    ///
    /// A record's file reference number is its position in the `$MFT` data
    /// stream — `stream_offset / record_size` — not its position in the list of
    /// physical extents. Extents can be sparse, and record 0 only ever holds
    /// part of the run list, so counting records by walking the physical runs
    /// assigns a wrong FRN to almost every record: `volume_key(ordinal, counter)`
    /// then fails to match `volume_key(ordinal, parent_frn)` and every
    /// non-top-level path resolves as an orphan. Sparse extents are skipped as
    /// unallocated stream space (records there are free), and a record that
    /// straddles a chunk or extent boundary is reassembled before its FRN is
    /// computed.
    fn scan_via_runs(
        &self,
        runs: &[ntfs::MftRun],
        limit: u64,
        resolver: &mut PathResolver,
        stats: &mut IndexStats,
        progress: &mut dyn FnMut(u64, u64, u64),
        rows: &mut Vec<NameRow>,
    ) -> Result<(), VolumeError> {
        let rec_size = self.volume.record_size as u64;
        // No floor here: a 512- or 1 KiB-cluster volume must stream at its own
        // cluster size. The old `max(4096)` turned the chunk unit into 4 KiB on
        // those volumes and, once a chunk stopped holding a whole number of
        // records (combined with the rounding below), desynchronised the FRN
        // stride. The unit is rounded up to whole records anyway.
        let cluster_size = self.volume.bytes_per_cluster as u64;

        // Every chunk must hold a whole number of MFT records. If it does not, a record
        // straddles the chunk boundary and the `rec_size` stride desynchronises,
        // so every subsequent record is read at the wrong offset.
        let unit = cluster_size.max(rec_size);
        let unit = unit.div_ceil(rec_size) * rec_size;
        let mut chunk_size = unit;
        while chunk_size < (1 << 20) {
            chunk_size += unit;
        }

        let mut buf = vec![0u8; chunk_size as usize];
        let mut fixed = vec![0u8; rec_size as usize];
        // A record being reassembled across a chunk or extent boundary.
        let mut carry: Vec<u8> = Vec::with_capacity(rec_size as usize);
        // Stream offset of the record `carry` is filling — its FRN once full.
        let mut record_stream_start = 0u64;
        // Tail bytes of a record torn by a hole or gap: discarded before
        // reassembly resumes at the next record boundary.
        let mut skip_bytes = 0u64;

        let stream_end = limit * rec_size;
        let mut seen_file = false;

        for run in runs {
            if record_stream_start >= stream_end && carry.is_empty() && skip_bytes == 0 {
                break;
            }
            let run_bytes = run.clusters * cluster_size;
            let logical = run.start_vcn * cluster_size;

            if run.start_lcn.is_none() {
                // Sparse extent: unallocated stream space that reads as zeros.
                // The records it covers cannot be live, and a record cut by the
                // hole is torn — but the space still counts in the numbering.
                carry.clear();
                let (skip, next) = resume_after_gap(logical + run_bytes, rec_size);
                skip_bytes = skip;
                record_stream_start = next;
                continue;
            }

            // Where the bytes of this extent must land in the stream. Anything
            // else means an unlisted gap: the record being assembled (or being
            // skipped) is torn and the numbering must resume at the boundary.
            let expected = if !carry.is_empty() {
                record_stream_start + carry.len() as u64
            } else if skip_bytes > 0 {
                record_stream_start - skip_bytes
            } else {
                record_stream_start
            };
            if logical != expected {
                carry.clear();
                let (skip, next) = resume_after_gap(logical, rec_size);
                skip_bytes = skip;
                record_stream_start = next;
            } else if carry.is_empty() && skip_bytes == 0 {
                let (skip, next) = resume_after_gap(logical, rec_size);
                skip_bytes = skip;
                record_stream_start = next;
            }

            let mut offset = run.start_lcn.unwrap_or(0) as u64 * cluster_size;
            let mut remaining = run_bytes;

            while remaining > 0 {
                let to_read = chunk_size.min(remaining) as usize;
                let n = self.volume.read_at(offset, &mut buf[..to_read])?;
                if n == 0 {
                    break;
                }

                let mut off = 0usize;
                while off < n {
                    if skip_bytes > 0 {
                        let take = (skip_bytes as usize).min(n - off);
                        skip_bytes -= take as u64;
                        off += take;
                        continue;
                    }
                    let need = rec_size as usize - carry.len();
                    let take = need.min(n - off);
                    carry.extend_from_slice(&buf[off..off + take]);
                    off += take;

                    if carry.len() < rec_size as usize {
                        break;
                    }

                    let frn = record_stream_start / rec_size;
                    record_stream_start += rec_size;
                    if frn >= limit {
                        carry.clear();
                        return Ok(());
                    }
                    if frn.is_multiple_of(10_000) {
                        progress(frn, resolver.edge_count() as u64, limit);
                    }
                    stats.records_read += 1;

                    if carry.len() >= 4 && &carry[0..4] == b"FILE" {
                        seen_file = true;
                        fixed.copy_from_slice(&carry);
                        // Fixups are 512-byte for MFT records regardless of
                        // volume sector size (see ntfs::MFT_SECTOR_SIZE). A
                        // record whose USN tails do not match is abandoned
                        // here — same as the Zen bulk walk ("bad US
                        // %x != %x", FUN_1400d0820).
                        if ntfs::apply_fixups(&mut fixed) {
                            self.ingest_record(&fixed, frn, resolver, stats, rows);
                        }
                    }
                    carry.clear();
                }

                offset += n as u64;
                remaining -= n as u64;
            }
        }

        // Streaming that produced no live record at all has not demonstrated it
        // is reading the MFT; let the caller fall back to the reference path.
        if !seen_file {
            return Err(VolumeError::IoControl {
                op: "scan_via_runs",
                code: 0,
            });
        }

        Ok(())
    }

    /// Fallback path: walk records one by one via FSCTL_GET_NTFS_FILE_RECORD.
    fn scan_via_records(
        &self,
        limit: u64,
        resolver: &mut PathResolver,
        stats: &mut IndexStats,
        progress: &mut dyn FnMut(u64, u64, u64),
        rows: &mut Vec<NameRow>,
    ) -> Result<(), VolumeError> {
        let mut frn: u64 = 0;

        // The walk ends at `limit` (record_count, capped by MAX_RECORDS) and
        // nowhere else. A free slot reads as `Ok(None)` — the FSCTL echoes a
        // neighbour's file reference, which `read_file_record` rejects — and
        // a run of free slots is normal MFT layout, not the end of the MFT:
        // stopping after MFT_SLACK holes truncated E:\ at record 101,947 of
        // 1,008,640 and left 3,582 orphan edges whose parents sat in the
        // unscanned tail.
        while frn < limit {
            if frn.is_multiple_of(10_000) {
                progress(frn, resolver.edge_count() as u64, limit);
            }
            stats.records_read += 1;

            // One unreadable record must not abort a 5.5 M-record scan: count it
            // and move on. The volume is live and a record can be freed between
            // the MFT size query and this read.
            match self.volume.read_file_record(frn) {
                Ok(Some(record)) => self.ingest_record(&record, frn, resolver, stats, rows),
                Ok(None) => {}
                Err(_) => stats.read_errors += 1,
            }
            frn += 1;
        }

        Ok(())
    }

    /// Ingest one MFT record: every name it owns, canonical plus aliases.
    ///
    /// The canonical name is the best of the set; the rest (hard links and the
    /// DOS 8.3 short name) are recorded as aliases so a search finds the file
    /// under either name. When the record's attributes spill into sibling
    /// records, the `$ATTRIBUTE_LIST` names those siblings and they are read
    /// here too — without this, a file whose name set is too large to fit shows
    /// up as an orphan edge.
    fn ingest_record(
        &self,
        record: &[u8],
        frn: u64,
        resolver: &mut PathResolver,
        stats: &mut IndexStats,
        rows: &mut Vec<NameRow>,
    ) {
        let Some(hdr) = ntfs::parse_file_record(record) else {
            return;
        };
        if hdr.flags & ntfs::MFT_RECORD_IN_USE == 0 {
            return;
        }
        stats.records_in_use += 1;
        let mtime = ntfs::standard_info_mtime(record).unwrap_or(0);
        let is_dir = hdr.flags & ntfs::MFT_RECORD_IS_DIRECTORY != 0;

        // Extension record: `$ATTRIBUTE_LIST` spill targets carry their extra
        // `$FILE_NAME` values here, and the record's base reference (offset
        // 0x20) points at the base FRN that owns them. Zen queues these
        // ("found extended MFT file record %016I64x base %016I64x") and merges
        // the names into the base record's name set (FUN_1400d0820 /
        // FUN_1400d13a0). Indexing the extension FRN standalone is a phantom
        // entry: its names become aliases of the BASE key so the file resolves
        // to one path. Duplicates with `overflow_names` (which chases the same
        // siblings from the base's resident `$ATTRIBUTE_LIST`) are harmless —
        // same (base, parent, name) alias, same resolved path.
        let base_raw = ntfs::frn_number(hdr.base_frn);
        if base_raw != 0 && base_raw != ntfs::frn_number(frn) {
            let base_key = volume_key(self.ordinal, base_raw);
            for n in ntfs::parse_file_names(record, frn) {
                if !name_is_indexable(&n) {
                    continue;
                }
                stats.names_seen += 1;
                resolver.add_alias(base_key, volume_key(self.ordinal, n.parent_frn), &n.name);
                let link = n.is_reparse_point();
                rows.push(NameRow {
                    frn: base_raw,
                    parent_frn: ntfs::frn_number(n.parent_frn),
                    name: n.name.into_bytes(),
                    is_dir,
                    link,
                    size: n.file_size,
                    mtime,
                });
            }
            return;
        }

        let mut names = ntfs::parse_file_names(record, frn);

        if ntfs::attrs_split(record) {
            names.extend(self.overflow_names(record, frn, stats));
        }
        stats.names_seen += names.len() as u64;

        // Drop DOS-only and internal-folder names before ranking so they
        // cannot win `best_name` or leak into the alias lists. `names_seen`
        // counts everything the record owned, indexed or not.
        names.retain(name_is_indexable);
        if names.is_empty() {
            return;
        }

        let kind = if is_dir {
            NodeKind::Directory
        } else {
            NodeKind::File
        };
        if is_dir {
            stats.directories += 1;
        } else {
            stats.files += 1;
        }

        let Some(best) = ntfs::best_name(&names) else {
            return;
        };
        if best.is_reparse_point() {
            stats.reparse_points += 1;
        }

        // FRNs are only unique within a volume, so fold this volume's ordinal
        // into the key before the index ever sees it.
        let key = volume_key(self.ordinal, frn);
        let parent_key = volume_key(self.ordinal, best.parent_frn);
        resolver.add(
            key,
            parent_key,
            &best.name,
            kind,
            best.file_size,
            best.file_attributes,
        );
        rows.push(NameRow {
            frn,
            parent_frn: ntfs::frn_number(best.parent_frn),
            name: best.name.clone().into_bytes(),
            is_dir,
            link: best.is_reparse_point(),
            size: best.file_size,
            mtime,
        });

        for n in &names {
            if n.name == best.name && n.parent_frn == best.parent_frn {
                continue;
            }
            resolver.add_alias(key, volume_key(self.ordinal, n.parent_frn), &n.name);
            rows.push(NameRow {
                frn,
                parent_frn: ntfs::frn_number(n.parent_frn),
                name: n.name.clone().into_bytes(),
                is_dir,
                link: n.is_reparse_point(),
                size: n.file_size,
                mtime,
            });
        }
    }

    /// Names held in sibling records, named by the record's `$ATTRIBUTE_LIST`.
    ///
    /// The list names which record holds each attribute but not *where* in that
    /// record it sits — `AttrId` is an instance id, not an offset. The sibling
    /// belongs to the same file, so its names are the same file's names.
    fn overflow_names(
        &self,
        record: &[u8],
        frn: u64,
        stats: &mut IndexStats,
    ) -> Vec<ntfs::FileNameEntry> {
        let Some(list) = ntfs::attr_list_value(record) else {
            return Vec::new();
        };

        let mut out = Vec::new();
        let mut seen_targets = std::collections::HashSet::new();
        for entry in ntfs::parse_attr_list(list) {
            // Only $FILE_NAME rows that point somewhere else; a nonzero VCN
            // marks a data run, never a name.
            if entry.attr_type != ntfs::ATTR_FILE_NAME || entry.start_vcn != 0 {
                continue;
            }
            let target = ntfs::frn_number(entry.mft_reference);
            if target == frn || !seen_targets.insert(target) {
                continue;
            }

            let sibling = match self.volume.read_file_record(target) {
                Ok(Some(s)) => s,
                Ok(None) => {
                    stats.read_errors += 1;
                    continue;
                }
                Err(_) => {
                    stats.read_errors += 1;
                    continue;
                }
            };
            out.extend(ntfs::parse_file_names(&sibling, target));
        }
        out
    }

    /// Read whatever USN records are pending on the volume and apply them.
    ///
    /// `start_usn` is the resume checkpoint and is updated to the next unread
    /// position. `known_journal_id` is the journal identity from the previous
    /// pass, or 0 to accept whatever the volume currently has.
    ///
    /// The request carries the real 64-bit `UsnJournalID`, so the kernel rejects
    /// a read against a recreated journal rather than silently serving the wrong
    /// one. `start_usn` is passed through as-is; the caller clamps it to the
    /// journal window first.
    /// Read whatever USN records are pending, without applying them — the
    /// `discovery::Watch` feed (`UsnWatch`). Same request as
    /// [`Self::tail_usn`] (all-ones ReasonMask, V4 max version) and the same
    /// resume rule: the returned `next_usn` is the resume point even when no
    /// records matched.
    pub fn read_usn_batch(
        &self,
        start_usn: &mut i64,
        known_journal_id: &mut u64,
        max_records: usize,
    ) -> Result<(Vec<ntfs::UsnRecord>, bool), VolumeError> {
        let journal = self.volume.query_usn_journal()?;
        let mut journal_id_changed = false;
        let mut wrapped = false;

        if *known_journal_id != 0 && *known_journal_id != journal.usn_journal_id {
            journal_id_changed = true;
            *start_usn = journal.first_usn;
        }
        *known_journal_id = journal.usn_journal_id;
        if !journal.covers(*start_usn) {
            *start_usn = journal.lowest_valid_usn;
        }
        if journal_id_changed {
            // The feed lost history with the journal; the caller rescans
            wrapped = true;
        }

        let request = ntfs::ReadUsnJournalData {
            start_usn: *start_usn,
            reason_mask: u32::MAX,
            return_only_on_close: 0,
            timeout: 0,
            bytes_to_wait_for: 0,
            usn_journal_id: journal.usn_journal_id,
            min_major_version: 2,
            max_major_version: 4,
        };
        let (mut records, next_start) = self.volume.read_usn(&request)?;
        if records.len() > max_records {
            records.truncate(max_records);
        }
        if records
            .iter()
            .any(|r| r.reason & ntfs::USN_REASON_JOURNAL_DELETE != 0)
        {
            wrapped = true;
        }
        *start_usn = next_start;
        Ok((records, wrapped))
    }

    pub fn tail_usn(
        &self,
        index: &mut Index,
        start_usn: &mut i64,
        known_journal_id: &mut u64,
        max_records: usize,
    ) -> Result<TailOutcome, VolumeError> {
        let journal = self.volume.query_usn_journal()?;
        let mut outcome = TailOutcome {
            next_start_usn: *start_usn,
            journal_id: journal.usn_journal_id,
            journal_lowest_valid_usn: journal.lowest_valid_usn,
            journal_next_usn: journal.next_usn,
            ..TailOutcome::default()
        };

        if *known_journal_id != 0 && *known_journal_id != journal.usn_journal_id {
            outcome.journal_id_changed = true;
            // The old checkpoint names a journal that no longer exists. Resume
            // from the oldest record this one still has, not from a dead USN.
            *start_usn = journal.first_usn;
        }
        *known_journal_id = journal.usn_journal_id;

        if !journal.covers(*start_usn) {
            outcome.start_usn_was_stale = true;
            *start_usn = journal.lowest_valid_usn;
        }

        let request = ntfs::ReadUsnJournalData {
            start_usn: *start_usn,
            // Zen's journal reader (FUN_1400cfe80) asks for every
            // reason with an all-ones mask. Filtering happens per record in
            // `Index::apply_usn` (create/delete/rename bits) — narrowing the
            // *request* to PATH_RELEVANT | JOURNAL_DELETE hid non-path activity
            // that still advanced the journal and made zero-record reads look
            // like "nothing pending".
            reason_mask: u32::MAX,
            return_only_on_close: 0,
            timeout: 0,
            bytes_to_wait_for: 0,
            usn_journal_id: journal.usn_journal_id,
            min_major_version: 2,
            // 4, not 3: modern Windows writes USN_RECORD_V4 and a request that
            // caps at 3 gets no records back on a journal that only has V4s.
            max_major_version: 4,
        };

        let (records, next_start) = self.volume.read_usn(&request)?;

        for rec in records.iter().take(max_records) {
            if rec.reason & ntfs::USN_REASON_JOURNAL_DELETE != 0 {
                outcome.journal_wrapped = true;
            }
            if index.apply_usn(rec, self.ordinal) {
                outcome.applied += 1;
            }
        }

        // Unconditional: the output buffer's leading USN is the resume point
        // even when no records matched — the journal still advanced. Deriving
        // "stay put unless records arrived" re-reads the same window forever
        // once the journal has moved past the filter.
        *start_usn = next_start;
        outcome.next_start_usn = *start_usn;
        Ok(outcome)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn index_stats_default_is_zeroed() {
        let s = IndexStats::default();
        assert_eq!(s.records_read, 0);
        assert_eq!(s.files, 0);
    }

    #[test]
    #[allow(clippy::assertions_on_constants)]
    fn max_records_bound_is_sane() {
        // Deliberate tripwire, not a behavioural test: this is a compile-time
        // constant, so all this can catch is someone zeroing or inverting it.
        // (MFT_SLACK used to sit here too; it was an early-exit on hole runs
        // that truncated healthy sparse MFTs — see `scan_via_records`.)
        assert!(MAX_RECORDS > 1_000_000);
    }

    #[test]
    fn resume_after_gap_on_a_record_boundary_skips_nothing() {
        // The gap ended exactly where a record ends: nothing was torn.
        assert_eq!(resume_after_gap(2048, 1024), (0, 2048));
    }

    #[test]
    fn resume_after_gap_discards_the_torn_record_tail() {
        // The gap ended 100 bytes into a record: that record is torn, so its
        // remaining 924 bytes are discarded and the next record starts at the
        // following boundary. Without this the first record after any hole is
        // read from the wrong offset and every FRN after it drifts.
        assert_eq!(resume_after_gap(2148, 1024), (924, 3072));
    }

    // --- hand-built records for ingest_record -----------------------------

    /// A FILE record with one nameless $FILE_NAME at attrs_offset 0x38, and
    /// `base_frn` at 0x20 (0 for a base record, the owner's FRN for an
    /// extension record).
    fn build_record(base_frn: u64, parent_frn: u64, ns: u8, name: &str) -> Vec<u8> {
        let name_bytes = name.encode_utf16().count() * 2;
        let value_len = 66 + name_bytes;
        let attr_len = 0x18 + value_len;
        let need = 0x38 + attr_len;
        let mut rec = vec![0u8; need];
        rec[0..4].copy_from_slice(b"FILE");
        rec[0x14..0x16].copy_from_slice(&0x38u16.to_le_bytes()); // attrs_offset
        rec[0x16..0x18].copy_from_slice(&1u16.to_le_bytes()); // in use
        rec[0x18..0x1c].copy_from_slice(&(need as u32).to_le_bytes()); // bytes_in_use
        rec[0x20..0x28].copy_from_slice(&base_frn.to_le_bytes()); // base file reference

        let attr = &mut rec[0x38..];
        attr[0x00..0x04].copy_from_slice(&ntfs::ATTR_FILE_NAME.to_le_bytes());
        attr[0x04..0x08].copy_from_slice(&(attr_len as u32).to_le_bytes());
        // NameLength/NameOffset stay 0: the canonical nameless layout.
        attr[0x10..0x14].copy_from_slice(&(value_len as u32).to_le_bytes());
        attr[0x18..0x20].copy_from_slice(&parent_frn.to_le_bytes());
        attr[0x18 + 64] = (name_bytes / 2) as u8; // FileNameLength, chars
        attr[0x18 + 65] = ns;
        for (i, c) in name.encode_utf16().enumerate() {
            attr[0x18 + 66 + i * 2..0x18 + 68 + i * 2].copy_from_slice(&c.to_le_bytes());
        }
        rec
    }

    fn ingest_and_resolve(records: &[(u64, Vec<u8>)]) -> (Index, IndexStats) {
        let volume = crate::volume::Volume::dummy_for_tests();
        let indexer = Indexer::new(&volume, 0);
        let mut resolver = PathResolver::new();
        let mut stats = IndexStats::default();
        for (frn, rec) in records {
            indexer.ingest_record(rec, *frn, &mut resolver, &mut stats, &mut Vec::new());
        }
        let mut index = Index::new("C:\\");
        resolver.resolve_into(&mut index);
        (index, stats)
    }

    #[test]
    fn ingest_indexes_the_base_record() {
        let rec = build_record(0, ROOT_FRN, ntfs::NAMESPACE_WIN32, "hello.txt");
        let (index, stats) = ingest_and_resolve(&[(42, rec)]);
        assert!(index.contains(volume_key(0, 42)));
        assert_eq!(index.path_of(volume_key(0, 42)), Some("C:\\hello.txt"));
        assert_eq!(stats.files, 1);
        assert_eq!(stats.records_in_use, 1);
    }

    #[test]
    fn extension_record_names_attach_to_the_base_frn() {
        // M3: an extension record (base_frn != 0 and != own FRN) owns names for
        // its base file. Zen queues these ("found extended MFT file
        // record") and merges them into the base — the extension FRN itself
        // must never become an index entry, or the file shows up twice.
        let base = build_record(0, ROOT_FRN, ntfs::NAMESPACE_WIN32, "long-name.dat");
        // Extension of FRN 42, holding an extra hard-link name.
        let ext = build_record(42, ROOT_FRN, ntfs::NAMESPACE_WIN32, "other-link.dat");
        let (index, stats) = ingest_and_resolve(&[(42, base), (77, ext)]);
        assert!(index.contains(volume_key(0, 42)));
        assert!(
            !index.contains(volume_key(0, 77)),
            "phantom extension entry"
        );
        assert_eq!(index.path_of(volume_key(0, 42)), Some("C:\\long-name.dat"));
        assert_eq!(index.alias_count(), 1);
        assert_eq!(stats.files, 1, "the extension is not a second file");
    }

    #[test]
    fn dos_and_internal_names_are_dropped() {
        // M5: Zen drops DOS-only 8.3 names (namespace 2) and names whose
        // parent is an internal NTFS folder (0..15 except root 5) — "ignore
        // name attr for internal ntfs folder".
        let rec = build_record(0, ROOT_FRN, ntfs::NAMESPACE_DOS, "HELLO~1.TXT");
        let (index, stats) = ingest_and_resolve(&[(42, rec)]);
        assert!(!index.contains(volume_key(0, 42)));
        assert_eq!(stats.files, 0);
        // The record itself was still read and its name counted.
        assert_eq!(stats.records_in_use, 1);
        assert_eq!(stats.names_seen, 1);

        // Parent FRN 8 ($Extend) is an internal NTFS folder.
        let rec = build_record(0, 8, ntfs::NAMESPACE_WIN32, "ObjId");
        let (index, _) = ingest_and_resolve(&[(42, rec)]);
        assert!(!index.contains(volume_key(0, 42)));
    }

    #[test]
    fn win32_and_dos_name_is_kept() {
        // Namespace 3 is the combined Win32+DOS name (e.g. "hello.txt" that is
        // also its own 8.3 form) — that one is a real user-visible name.
        let rec = build_record(0, ROOT_FRN, ntfs::NAMESPACE_WIN32_AND_DOS, "readme.txt");
        let (index, _) = ingest_and_resolve(&[(42, rec)]);
        assert_eq!(index.path_of(volume_key(0, 42)), Some("C:\\readme.txt"));
    }
}

#[cfg(test)]
mod listings_tests {
    use super::*;
    use discovery::walk::{KIND_DIR, KIND_FILE, NONE};

    #[test]
    fn rows_become_dfs_listings_with_child_links() {
        // root(5) has "a" (dir, frn 10) and "b.txt" (file, frn 11);
        // dir 10 has "c.txt" (frn 12).
        let rows = vec![
            NameRow {
                frn: 10,
                parent_frn: 5,
                name: b"a".to_vec(),
                is_dir: true,
                link: false,
                size: 0,
                mtime: 1,
            },
            NameRow {
                frn: 11,
                parent_frn: 5,
                name: b"b.txt".to_vec(),
                is_dir: false,
                link: false,
                size: 7,
                mtime: 2,
            },
            NameRow {
                frn: 12,
                parent_frn: 10,
                name: b"c.txt".to_vec(),
                is_dir: false,
                link: false,
                size: 9,
                mtime: 3,
            },
        ];
        let ls = listings_from_rows(&rows, 5);
        assert_eq!(ls.len(), 2);
        assert_eq!(ls[0].id, 0); // root's children first
        assert_eq!(ls[0].ents.len(), 2);
        // The dir dirent points at listing 1.
        let dir_ent = ls[0].ents.iter().find(|e| e.kind & 3 == KIND_DIR).unwrap();
        assert_eq!(dir_ent.child, 1);
        let file_ent = ls[0].ents.iter().find(|e| e.kind & 3 == KIND_FILE).unwrap();
        assert_eq!(file_ent.child, NONE);
        assert_eq!(file_ent.size, 7);
        assert_eq!(file_ent.mtime, 2);
        // Listing 1 holds the dir's children.
        assert_eq!(ls[1].id, 1);
        assert_eq!(ls[1].ents.len(), 1);
        assert_eq!(
            &ls[1].names[ls[1].ents[0].name_off as usize..][..5],
            b"c.txt"
        );
        assert_eq!(ls[1].ents[0].child, NONE);
    }

    #[test]
    fn reparse_rows_are_link_kind_and_do_not_get_child_listings() {
        // A junction: dir bit + reparse. fsearch's model calls it a link,
        // and it does not own a children listing.
        let rows = vec![
            NameRow {
                frn: 10,
                parent_frn: 5,
                name: b"j".to_vec(),
                is_dir: true,
                link: true,
                size: 0,
                mtime: 0,
            },
            NameRow {
                frn: 12,
                parent_frn: 10,
                name: b"inside".to_vec(),
                is_dir: false,
                link: false,
                size: 1,
                mtime: 0,
            },
        ];
        let ls = listings_from_rows(&rows, 5);
        assert_eq!(ls.len(), 1, "the junction gets no child listing");
        assert_eq!(ls[0].ents[0].kind, discovery::walk::KIND_LINK);
        assert_eq!(ls[0].ents[0].child, NONE);
    }
}
