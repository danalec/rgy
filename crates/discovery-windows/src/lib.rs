// SPDX-License-Identifier: Apache-2.0

//! Windows discovery backend: NTFS MFT scan + USN journal tail.
//!
//! Transported from zen-rs (`ntfs.rs`, `volume.rs`, `indexer.rs`) with
//! the RE-audit fixes (M1 cluster floor, M2 resident ValueOffset, D1 USN
//! offsets, D2 resume advance, D3 all-ones ReasonMask, D9 record-size rule,
//! M3 extension records, M4 name length, M5 name filter, M6 fixup strictness).
//!
//! `ntfs` re-exports the shared change vocabulary from `discovery` so the
//! old `ntfs::UsnRecord` paths keep working — one type, one home.

// Windows only, and this line is what says so.
//
// Everything here is Win32 -- `DeviceIoControl` with `FSCTL_ENUM_USN_DATA`,
// `CreateFileW` on `\\.\C:`, `ReadFile` on a volume handle -- so it could not
// work elsewhere. The calls are declared by hand, in `src/win.rs`, and a
// hand-written declaration of a platform call is still a platform call: it
// links against `kernel32`, and there is no `kernel32` on a Linux box.
//
// An empty crate off-Windows is what keeps `cargo check --workspace` honest
// on the platforms the rest of the stack supports. `ntfs.rs` is pure byte
// parsing and needs none of this, but it is only reachable through here.
#![no_std]
#![cfg(windows)]

// The crate is `no_std` + `alloc`: the Win32 calls, the byte parsing and the
// maps are all that is here, and none of them needs a standard library. The
// allocator is the one thing `no_std` still has to be handed.
extern crate alloc;

pub mod indexer;
pub mod ntfs;
pub mod paths;
pub mod rdcw;
pub mod seam;
pub mod volume;
pub mod win;

/// The tree's own map for the cache of rebuilt paths, and the crate it
/// replaced is the oracle it is held to.
mod hmap;

pub use rdcw::RdcwWatch;
pub use seam::{MftSource, UsnWatch};
