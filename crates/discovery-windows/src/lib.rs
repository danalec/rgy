//! Windows discovery backend: NTFS MFT scan + USN journal tail.
//!
//! Transported from zen-rs (`ntfs.rs`, `volume.rs`, `indexer.rs`) with
//! the RE-audit fixes (M1 cluster floor, M2 resident ValueOffset, D1 USN
//! offsets, D2 resume advance, D3 all-ones ReasonMask, D9 record-size rule,
//! M3 extension records, M4 name length, M5 name filter, M6 fixup strictness).
//!
//! `ntfs` re-exports the shared change vocabulary from `discovery` so the
//! old `ntfs::UsnRecord` paths keep working — one type, one home.

pub mod indexer;
pub mod ntfs;
pub mod seam;
pub mod volume;

pub use seam::{MftSource, UsnWatch};
