//! The Tesla Valve pack store, as a format and a plan.
//!
//! `rgx-tv` is the `std` crate that owns the filesystem, the threads and the
//! wall clock; this module is the part of it that owns the *decisions* — the
//! on-disk codec, the query vocabulary, and the planner that turns a manifest
//! into claimable units. Everything here is `no_std` + `alloc`, behind the
//! `tv` feature, because a decision does not need a filesystem and a plan
//! does not need a thread.
//!
//! The split is not cosmetic. `rgx` copies the parts of this crate it needs
//! rather than linking the whole of `rgx-tv`, so the algorithmic half of the
//! store being here is what lets the main binary take it without taking the
//! thread pool with it. And the format being here is what makes the on-disk
//! layout a property of the store rather than a property of whichever process
//! happens to be reading it this time.

pub mod format;
pub mod plan;
pub mod query;

pub use format::{
    FileLoc, FileRecord, Manifest, ManifestHead, PackHeader, PackRecord, RawManifest,
    DEFAULT_PACK_SIZE, FORMAT_VERSION, MANIFEST_MAGIC, MANIFEST_VERSION, PACK_MAGIC,
};
pub use plan::{
    cut_units, kept, kept_flags, kept_loc, merge_buckets, narrow, split_locs, split_owned,
    PackPlan, Unit, UNIT_BYTES,
};
pub use query::{EngineMode, ReadMode, SearchConfig, SearchMatch, Unmap};
