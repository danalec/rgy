// SPDX-License-Identifier: Apache-2.0

//! MFT walk and USN tailing: the two ways this indexer learns about files.

use alloc::vec;
use alloc::vec::Vec;

use crate::ntfs;
use crate::ntfs::ROOT_FRN;
use crate::volume::{Volume, VolumeError};

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

/// One indexed directory entry: the `rgy_discovery::Source` row a [`NameRow`]s
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

/// Group [`NameRow`]s into DFS [`rgy_discovery::walk::Listing`]s for
/// [`rgy_name_core::name_index::Index::build`]: one block per directory,
/// listing 0 = the children of `root_frn`, each directory entry's `child`
/// naming the listing of that directory's children.
///
/// Rows of a directory's aliases (same `frn`, different name) point at the
/// same child listing — directories are not hard-linked on NTFS, so this is
/// only a hard-link-style edge case in the data.
pub fn listings_from_rows(rows: &[NameRow], root_frn: u64) -> Vec<rgy_discovery::walk::Listing> {
    use rgy_discovery::walk::{KIND_DIR, KIND_FILE, KIND_LINK, Listing, NONE, RawEnt};

    // The rows by their parent: a sort and a scan, not a table. The rows of
    // one parent are one run of the vector, and each run is read once.
    let mut order: Vec<u32> = (0..rows.len() as u32).collect();
    order.sort_by_key(|&i| rows[i as usize].parent_frn);

    // (parent frn, slot in `out` that takes this listing's id)
    let mut out: Vec<Listing> = Vec::new();
    let mut next_id = 1u32;
    // (parent_frn, Option<(listing index, ent index)> whose child field
    // takes the id of the listing we are about to build)
    let mut stack: Vec<(u64, Option<(usize, usize)>)> = vec![(root_frn, None)];
    while let Some((parent, fix)) = stack.pop() {
        let indices = rows_of(&order, rows, parent);
        if indices.is_empty() {
            continue;
        }
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
            let r = &rows[ri as usize];
            let name_off = names.len() as u32;
            names.extend_from_slice(&r.name);
            // The tree's walk model: a symlink/reparse point is its own kind,
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

/// The rows of one parent, out of the rows sorted by their parent.
///
/// The rows of a parent are one run of the sorted vector, and the sort is
/// stable, so the rows of a folder keep the order the scan gave them.
fn rows_of<'a>(order: &'a [u32], rows: &[NameRow], parent: u64) -> &'a [u32] {
    let start = order.partition_point(|&i| rows[i as usize].parent_frn < parent);
    let end = order.partition_point(|&i| rows[i as usize].parent_frn <= parent);
    &order[start..end]
}

#[derive(Debug, Default, Clone, Copy)]
pub struct IndexStats {
    pub records_read: u64,
    pub records_in_use: u64,
    pub names_seen: u64,
    pub files: u64,
    pub directories: u64,
    /// Records (or sibling overflow records) the volume refused to return.
    /// Counted rather than fatal: the volume is live and records move.
    pub read_errors: u64,
    /// Entries flagged `FILE_ATTRIBUTE_REPARSE_POINT` — junctions and symlinks.
    pub reparse_points: u64,
}

pub struct Indexer<'a> {
    volume: &'a Volume,
    /// Whether the scan may use the fast extent-streaming path.
    ///
    /// The choice used to be an environment variable the backend read for
    /// itself. A `no_std` crate cannot read the environment, and a backend
    /// should not: the product reads it and passes the decision down.
    streamed: bool,
}

impl<'a> Indexer<'a> {
    /// The indexer for one volume.
    ///
    /// There is no volume ordinal to carry. A multi-volume scan gives each
    /// volume its own name index (`zen-rs`'s `index-all` builds one per
    /// volume), so two volumes never share an FRN space and no key has to
    /// disambiguate them.
    ///
    /// The scan uses the fast extent-streaming path ([`Self::scan_via_runs`])
    /// and falls back to the record-by-record path when the volume refuses
    /// it. [`Self::with_records`] chooses the path explicitly.
    pub fn new(volume: &'a Volume) -> Self {
        Indexer {
            volume,
            streamed: true,
        }
    }

    /// The indexer for one volume, with the scan path chosen explicitly.
    ///
    /// `streamed` picks the fast extent-streaming path
    /// ([`Self::scan_via_runs`]); `false` forces the record-by-record
    /// `FSCTL_GET_NTFS_FILE_RECORD` path ([`Self::scan_via_records`]) and
    /// skips the streaming attempt altogether.
    ///
    /// This is where the product's `ERS_USE_RECORDS` decision lands: the
    /// daemon and the CLI read the variable and pass the answer down,
    /// because the backend itself is `no_std` and cannot.
    pub fn with_records(volume: &'a Volume, streamed: bool) -> Self {
        Indexer { volume, streamed }
    }

    /// The volume this indexer reads.
    pub fn volume(&self) -> &'a Volume {
        self.volume
    }

    /// Walk the MFT and collect every indexed name as a [`NameRow`] — the
    /// `rgy_discovery::Source` feed for the name index (grouped into DFS
    /// [`rgy_discovery::walk::Listing`]s by [`listings_from_rows`]).
    ///
    /// Attempts fast extent streaming by reading MFT cluster runs directly
    /// and falls back to sequential record-by-record
    /// `FSCTL_GET_NTFS_FILE_RECORD` calls. Which of the two is taken is the
    /// indexer's own choice (see [`Self::new`] and [`Self::with_records`]).
    /// Both paths and the name rules are documented on
    /// [`Self::scan_via_runs`] and [`Self::ingest_record`].
    pub fn scan_rows(
        &self,
        progress: &mut dyn FnMut(u64, u64, u64),
    ) -> Result<(Vec<NameRow>, IndexStats), VolumeError> {
        let mut stats = IndexStats::default();
        let mut rows = Vec::new();
        let limit = self.volume.record_count.clamp(1, MAX_RECORDS);

        let streamed = self.streamed
            && match self.volume.read_mft_data_runs() {
                Ok(runs) => self
                    .scan_via_runs(&runs, limit, &mut stats, progress, &mut rows)
                    .is_ok(),
                Err(_) => false,
            };
        if !streamed {
            stats = IndexStats::default();
            rows.clear();
            self.scan_via_records(limit, &mut stats, progress, &mut rows)?;
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
    /// assigns a wrong FRN to almost every record: the record's number then
    /// fails to match its parent's and every
    /// non-top-level path resolves as an orphan. Sparse extents are skipped as
    /// unallocated stream space (records there are free), and a record that
    /// straddles a chunk or extent boundary is reassembled before its FRN is
    /// computed.
    fn scan_via_runs(
        &self,
        runs: &[ntfs::MftRun],
        limit: u64,
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
                        progress(frn, rows.len() as u64, limit);
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
                            self.ingest_record(&fixed, frn, stats, rows);
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
                progress(frn, rows.len() as u64, limit);
            }
            stats.records_read += 1;

            // One unreadable record must not abort a 5.5 M-record scan: count it
            // and move on. The volume is live and a record can be freed between
            // the MFT size query and this read.
            match self.volume.read_file_record(frn) {
                Ok(Some(record)) => self.ingest_record(&record, frn, stats, rows),
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
            for n in ntfs::parse_file_names(record, frn) {
                if !name_is_indexable(&n) {
                    continue;
                }
                stats.names_seen += 1;
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
        // The targets already read: the attribute list names a handful of
        // siblings, so a vector and a scan are cheaper than a table.
        let mut seen_targets: Vec<u64> = Vec::new();
        for entry in ntfs::parse_attr_list(list) {
            // Only $FILE_NAME rows that point somewhere else; a nonzero VCN
            // marks a data run, never a name.
            if entry.attr_type != ntfs::ATTR_FILE_NAME || entry.start_vcn != 0 {
                continue;
            }
            let target = ntfs::frn_number(entry.mft_reference);
            if target == frn || seen_targets.contains(&target) {
                continue;
            }
            seen_targets.push(target);

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
    /// `rgy_discovery::Watch` feed (`UsnWatch`). All-ones ReasonMask, V4 max
    /// version. The returned `next_usn` is the resume point even when no
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
            if *start_usn < journal.lowest_valid_usn && *start_usn != ntfs::FIRST_AVAILABLE_USN {
                // The journal recycled the records between the resume point
                // and the oldest one it still holds: that history is gone,
                // and the feed has to say so — the index has to be rebuilt,
                // because what happened in the gap is not known, and an
                // index that claims to be whole is not.
                //
                // The sentinel is the exception: it is not a position, it
                // is the kernel's way of saying "the first record you can
                // read", and nothing was lost.
                wrapped = true;
            }
            // Either way the read starts at the oldest record the journal
            // still holds: a resume point above the head is ahead of it,
            // and one below the window is in a hole the journal recycled.
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

    use crate::ntfs::testutil::build_record;

    fn ingest_rows(records: &[(u64, Vec<u8>)]) -> (Vec<NameRow>, IndexStats) {
        let volume = crate::volume::Volume::dummy_for_tests();
        let indexer = Indexer::new(&volume);
        let mut stats = IndexStats::default();
        let mut rows = Vec::new();
        for (frn, rec) in records {
            indexer.ingest_record(rec, *frn, &mut stats, &mut rows);
        }
        (rows, stats)
    }

    #[test]
    fn ingest_indexes_the_base_record() {
        let rec = build_record(0, ROOT_FRN, ntfs::NAMESPACE_WIN32, "hello.txt");
        let (rows, stats) = ingest_rows(&[(42, rec)]);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].frn, 42);
        assert_eq!(rows[0].parent_frn, ROOT_FRN);
        assert_eq!(rows[0].name, b"hello.txt");
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
        let (rows, stats) = ingest_rows(&[(42, base), (77, ext)]);
        assert!(
            rows.iter().all(|r| r.frn == 42),
            "phantom extension entry: {rows:?}"
        );
        assert_eq!(
            rows.len(),
            2,
            "the extension name is a second name of the base"
        );
        let mut names: Vec<_> = rows.iter().map(|r| r.name.clone()).collect();
        names.sort();
        assert_eq!(names[0], b"long-name.dat".to_vec());
        assert_eq!(names[1], b"other-link.dat".to_vec());
        assert_eq!(stats.files, 1, "the extension is not a second file");
    }

    #[test]
    fn dos_and_internal_names_are_dropped() {
        // M5: Zen drops DOS-only 8.3 names (namespace 2) and names whose
        // parent is an internal NTFS folder (0..15 except root 5) — "ignore
        // name attr for internal ntfs folder".
        let rec = build_record(0, ROOT_FRN, ntfs::NAMESPACE_DOS, "HELLO~1.TXT");
        let (rows, stats) = ingest_rows(&[(42, rec)]);
        assert!(rows.is_empty());
        assert_eq!(stats.files, 0);
        // The record itself was still read and its name counted.
        assert_eq!(stats.records_in_use, 1);
        assert_eq!(stats.names_seen, 1);

        // Parent FRN 8 ($Extend) is an internal NTFS folder.
        let rec = build_record(0, 8, ntfs::NAMESPACE_WIN32, "ObjId");
        let (rows, _) = ingest_rows(&[(42, rec)]);
        assert!(rows.is_empty());
    }

    #[test]
    fn win32_and_dos_name_is_kept() {
        // Namespace 3 is the combined Win32+DOS name (e.g. "hello.txt" that is
        // also its own 8.3 form) — that one is a real user-visible name.
        let rec = build_record(0, ROOT_FRN, ntfs::NAMESPACE_WIN32_AND_DOS, "readme.txt");
        let (rows, _) = ingest_rows(&[(42, rec)]);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].name, b"readme.txt");
    }
}

#[cfg(test)]
mod listings_tests {
    use super::*;
    use rgy_discovery::walk::{KIND_DIR, KIND_FILE, NONE};

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
        // A junction: dir bit + reparse. The walk model calls it a link,
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
        assert_eq!(ls[0].ents[0].kind, rgy_discovery::walk::KIND_LINK);
        assert_eq!(ls[0].ents[0].child, NONE);
    }
}
