// SPDX-License-Identifier: Apache-2.0

//! The Windows backends behind the `discovery::Source`/`Watch` seam.
//!
//! `MftSource` enumerates the volume as DFS directory blocks via the MFT
//! walk (the trap suite in `indexer`); `UsnWatch` tails the USN journal as
//! [`ChangeBatch`]es. Together they are the Windows half of
//! `docs/IMPLEMENTATION_PLAN.md` §4.2.

use discovery::walk::Listing;
use discovery::{ChangeBatch, Source, Watch};

use crate::indexer::{Indexer, listings_from_rows};

/// `Source`: the NTFS MFT walk as DFS [`Listing`]s for
/// `name_core::name_index::Index::build`.
pub struct MftSource<'a> {
    pub indexer: Indexer<'a>,
    /// Volume root FRN — 5 (`ROOT_FRN`) on every real volume.
    pub root_frn: u64,
}

impl MftSource<'_> {
    pub fn new<'a>(indexer: Indexer<'a>) -> MftSource<'a> {
        MftSource {
            indexer,
            root_frn: name_core::index::ROOT_FRN,
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
pub struct UsnWatch<'a> {
    pub indexer: Indexer<'a>,
    pub start_usn: i64,
    pub journal_id: u64,
    pub max_records: usize,
}

impl UsnWatch<'_> {
    pub fn new<'a>(indexer: Indexer<'a>) -> UsnWatch<'a> {
        UsnWatch {
            indexer,
            // The documented "first available record" sentinel.
            start_usn: 0x7FFF_FFFF,
            journal_id: 0,
            max_records: 4096,
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
        Ok(ChangeBatch::Events(records))
    }
}
