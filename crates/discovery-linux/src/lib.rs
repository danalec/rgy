// SPDX-License-Identifier: Apache-2.0

//! Linux discovery backend: a `getdents64`
//! bulk walk producing the same DFS [`Listing`]s as every other `Source`,
//! and a recursive inotify [`Watch`] whose overflow becomes `Rescan`.
//!
//! `statx` is used for metadata where it is available (mtime is what the
//! name index's `mtime:` filter reads); `fanotify` FID hardening is later
//! work and not in this module.
//!
//! The whole backend is `cfg(target_os = "linux")` — on other hosts this
//! crate compiles empty and the portable `rgy_discovery::PortableSource` is
//! the Source.
//!
//! The crate is `no_std`: it asks the kernel through the calls it declares
//! itself -- the house pattern, the one `crates/discovery/src/file.rs` and
//! `crates/name-core/src/clock.rs` follow -- and keeps its paths as bytes,
//! the shape the rest of the tree speaks. Nothing here needs the standard
//! library, and nothing here brings it in.

#![no_std]

extern crate alloc;

#[cfg(target_os = "linux")]
mod walk;
#[cfg(target_os = "linux")]
mod watch;

#[cfg(target_os = "linux")]
pub use walk::LinuxSource;
#[cfg(target_os = "linux")]
pub use watch::InotifyWatch;

// On a non-Linux host this crate compiles empty: the portable
// `rgy_discovery::PortableSource` is the Source and the Windows/macOS
// backends own their platforms.
