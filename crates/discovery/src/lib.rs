//! The platform discovery seam (`IMPLEMENTATION_PLAN.md` §4.2).
//!
//! `Source` enumerates the filesystem as DFS directory blocks; `Watch`
//! delivers change batches, or [`ChangeBatch::Rescan`] when the feed loses
//! history. Backends: `discovery-windows` (MFT + USN), later macOS and Linux.
//!
//! The USN change vocabulary lives here rather than in a platform backend so
//! `name-core` can apply live updates without depending on any of them —
//! the same reason the reason bits and `FILE_ATTRIBUTE_REPARSE_POINT` are
//! not in `discovery-windows::ntfs` (that module re-exports them for the
//! old paths).

/// A USN journal change record (`USN_RECORD_V2` tail fields at the SDK
/// offsets: `FileAttributes`@52, `FileNameLength`@56, `FileNameOffset`@58).
///
/// Moved out of `ntfs.rs` at the workspace merge: this is what a `Watch`
/// feed produces and what `name_core::index::Index::apply_usn` consumes, so
/// it cannot live in the Windows backend crate without a dependency cycle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsnRecord {
    pub file_reference: u64,
    pub parent_reference: u64,
    /// The record's own USN. Retained because it is part of the on-disk record
    /// and is what a resume point refers to; the reader takes its next start
    /// from the output buffer's leading USN instead of deriving it from these,
    /// so nothing in the read path consumes it.
    #[allow(dead_code)]
    pub usn: u64,
    pub reason: u32,
    pub file_attributes: u32,
    pub name: String,
}

// USN journal reason flags.
pub const USN_REASON_FILE_CREATE: u32 = 0x0000_0100;
pub const USN_REASON_FILE_DELETE: u32 = 0x0000_0200;
pub const USN_REASON_RENAME_OLD_NAME: u32 = 0x0000_1000;
pub const USN_REASON_RENAME_NEW_NAME: u32 = 0x0000_2000;
pub const USN_REASON_RENAME_DUPLICATE_SOURCE: u32 = 0x0000_4000;
pub const USN_REASON_RENAME_DUPLICATE_TARGET: u32 = 0x0000_8000;
/// Set on records written after the journal was deleted or wrapped past the
/// point a reader last saw. A record carrying this bit means the gap between
/// the previous resume point and this one is not a contiguous history.
pub const USN_REASON_JOURNAL_DELETE: u32 = 0x0001_0000;

/// Reasons that change which paths exist. Anything else (size/metadata writes)
/// does not affect the path index. Deliberately excluded from the journal
/// *request*: the reader asks for every reason (all-ones mask, like
/// Zen) and `Index::apply_usn` filters per record against the bits
/// above. Kept as the documented grouping and for tests.
#[allow(dead_code)]
pub const USN_REASON_PATH_RELEVANT: u32 = USN_REASON_FILE_CREATE
    | USN_REASON_FILE_DELETE
    | USN_REASON_RENAME_OLD_NAME
    | USN_REASON_RENAME_NEW_NAME
    | USN_REASON_RENAME_DUPLICATE_SOURCE
    | USN_REASON_RENAME_DUPLICATE_TARGET;

/// NTFS reparse bit (`FILE_ATTRIBUTE_REPARSE_POINT`), in the same vocabulary
/// as the journal's `file_attributes`.
pub const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;

/// One batch of live changes from a [`Watch`] feed.
pub enum ChangeBatch {
    /// Journal/OS events to apply in order.
    Events(Vec<UsnRecord>),
    /// The feed lost history; the caller must rescan from scratch.
    Rescan,
}

/// Full enumeration source: walks the filesystem as DFS directory blocks.
pub trait Source {
    /// Enumerate everything under the source root as [`walk::Listing`]s —
    /// listing 0 is the root's children, `RawEnt::child` links into the
    /// listing of each subdirectory. This is the block stream
    /// `name_core::name_index::Index::build` consumes.
    fn enumerate(&self) -> Result<Vec<walk::Listing>, String>;
}

/// Live change feed: produces [`ChangeBatch`]es.
pub trait Watch {
    /// The next batch of live changes. [`ChangeBatch::Rescan`] means the
    /// feed lost history and the caller must rebuild from a [`Source`].
    fn next_batch(&mut self) -> Result<ChangeBatch, String>;
}

pub mod interrupt;
pub mod walk;

/// The portable [`Source`]: a std-based DFS walk (see [`walk::scan`]).
/// Correct on every platform; the macOS getattrlistbulk fast path and the
/// Windows MFT walk return the same blocks from their own crates.
pub struct PortableSource {
    pub root: Vec<u8>,
    /// Directory-read fan-out handed to [`walk::scan`]. 0 or 1 walks inline;
    /// higher values use a pool. Ignored by backends with their own walk.
    pub threads: usize,
}

impl PortableSource {
    /// Default fan-out for callers that just want a portable walk.
    pub const DEFAULT_THREADS: usize = 4;

    pub fn new(root: Vec<u8>, threads: usize) -> Self {
        Self { root, threads }
    }

    /// [`Source::enumerate`], with a cancel token and a progress callback.
    ///
    /// `on_progress` fires once per level (parallel) or per directory
    /// (sequential), which is often enough that the caller should throttle
    /// its own output. It is called after the cancel check, so the last thing
    /// a scan says is never a state it did not finish in.
    pub fn enumerate_cancellable(
        &self,
        cancel: &walk::Cancel,
        on_progress: &(dyn Fn(walk::Progress) + Sync),
    ) -> Result<Vec<walk::Listing>, walk::Cancelled> {
        walk::scan_with(&self.root, self.threads, cancel, on_progress)
    }
}

impl Source for PortableSource {
    fn enumerate(&self) -> Result<Vec<walk::Listing>, String> {
        Ok(walk::scan(&self.root, self.threads))
    }
}
