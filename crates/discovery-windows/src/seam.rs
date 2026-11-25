// SPDX-License-Identifier: Apache-2.0

//! The Windows backends behind the `rgy_discovery::Source`/`Watch` seam.
//!
//! `MftSource` enumerates the volume as DFS directory blocks via the MFT
//! walk (the trap suite in `indexer`); `UsnWatch` tails the USN journal as
//! [`ChangeBatch`]es. Together they are the Windows half of
//! the discovery seam

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use rgy_discovery::walk::Listing;
use rgy_discovery::{ChangeBatch, Source, Watch};

use crate::indexer::{Indexer, listings_from_rows};
use crate::paths::Paths;

/// `Source`: the NTFS MFT walk as DFS [`Listing`]s for
/// `rgy_name_core::name_index::Index::build`.
pub struct MftSource<'a> {
    pub indexer: Indexer<'a>,
    /// Volume root FRN — 5 (`ROOT_FRN`) on every real volume.
    pub root_frn: u64,
}

impl MftSource<'_> {
    pub fn new<'a>(indexer: Indexer<'a>) -> MftSource<'a> {
        MftSource {
            indexer,
            root_frn: crate::ntfs::ROOT_FRN,
        }
    }
}

impl Source for MftSource<'_> {
    fn enumerate(&self) -> Result<Vec<Listing>, String> {
        let mut progress = |_a: u64, _b: u64, _c: u64| {};
        let (rows, _stats) = self
            .indexer
            .scan_rows(&mut progress)
            .map_err(|e| format!("MFT scan failed: {e:?}"))?;
        Ok(listings_from_rows(&rows, self.root_frn))
    }
}

/// `Watch`: the USN journal tail. A journal wrap or recreate is reported as
/// [`ChangeBatch::Rescan`] — the feed lost history and the index must be
/// rebuilt from a [`MftSource`].
///
/// The journal names a file and its parent's file reference, and nothing
/// else. The path is rebuilt from the MFT ([`crate::paths`]), so a change
/// arrives at the folder it happened in.
pub struct UsnWatch<'a> {
    pub indexer: Indexer<'a>,
    pub start_usn: i64,
    pub journal_id: u64,
    pub max_records: usize,
    /// The paths the journal's records are placed at.
    paths: Paths<'a>,
}

impl<'a> UsnWatch<'a> {
    pub fn new(indexer: Indexer<'a>) -> UsnWatch<'a> {
        let paths = Paths::new(indexer.volume());
        UsnWatch {
            indexer,
            // The documented "first available record" sentinel.
            start_usn: crate::ntfs::FIRST_AVAILABLE_USN,
            journal_id: 0,
            max_records: 4096,
            paths,
        }
    }
}

impl Watch for UsnWatch<'_> {
    fn next_batch(&mut self) -> Result<ChangeBatch, String> {
        let (records, wrapped) = self
            .indexer
            .read_usn_batch(&mut self.start_usn, &mut self.journal_id, self.max_records)
            .map_err(|e| format!("USN read failed: {e:?}"))?;
        if wrapped {
            return Ok(ChangeBatch::Rescan);
        }
        // Place each record: the journal names the file, and the path comes
        // from the MFT. A record whose path cannot be rebuilt keeps the bare
        // name it came with, which names no folder — the change is then not
        // applied, rather than applied to the wrong folder.
        let records = records
            .into_iter()
            .map(|mut r| {
                if let Some(path) = self.paths.of_record(&r) {
                    r.name = path;
                }
                r
            })
            .collect();
        Ok(ChangeBatch::Events(records))
    }

    /// The USN position the records read so far run up to: the watermark an
    /// index carries, and the point a later run resumes the journal from.
    fn position(&self) -> Option<u64> {
        Some(self.start_usn.max(0) as u64)
    }
}
