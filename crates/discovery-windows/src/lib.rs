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
// work elsewhere. More to the point, it could not *build* elsewhere: the
// `windows` crate family does not cross-compile. `windows-future 0.3.2`
// references `windows_core::imp::IMarshal` and `windows_core::imp::marshaler`,
// which `windows-core 0.62.2` only defines for the Windows target, so every
// non-Windows build died in a dependency before reaching a line of ours. It
// breaks on `x86_64-unknown-linux-gnu` too, which is how we know it is not
// ours. Pinning does not help: `windows 0.62.2` is the newest, and it is the
// one that pulls the broken `windows-future`.
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
pub mod seam;
pub mod volume;

pub use seam::{MftSource, UsnWatch};
