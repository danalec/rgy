// SPDX-License-Identifier: Apache-2.0

//! macOS discovery backend: fsearch's
//! `getattrlistbulk` walk and FSEvents watch, transported verbatim.
//!
//! The whole backend is `cfg(target_os = "macos")`; on other hosts this
//! crate compiles empty and `rgy_discovery::PortableSource` is the Source.
//!
//! **Type-checked, not run.** The code is fsearch's, which compiles on
//! macOS; the only edits are import paths. `cargo check --target
//! x86_64-apple-darwin` compiles it here — that target is installed — but
//! a Mac is still what it takes to run it: the walk, the FSEvents stream
//! and the flags they carry have never been exercised on a real kernel.
//!
//! The FSEvents watch is a callback feed and the daemon speaks a pull, so
//! [`feed::FseventsWatch`] is the adapter between the two: it owns the
//! stream (dropping the stream stops the feed) and turns each pushed batch
//! into one [`rgy_discovery::ChangeBatch`]. Its decisions — which folder an
//! event means, and which events belong to the indexed tree — are plain
//! functions over [`event::Event`], so they are tested on every platform
//! rather than only on a Mac.
//!
//! # `no_std`
//!
//! The crate is `no_std` + `alloc`: the walk, the feed and the record
//! reader run on bytes the caller owns, and nothing in the crate names
//! `std`. The three things a `std` crate would have reached for are the
//! tree's own: the paths (`rgy_discovery::path`), the channel
//! (`rgy_par::mpsc`) and the locks (`rgy_par::sync`).
//!
//! The `std` feature is what the parallel walk's fan-out needs: with it
//! the workers are threads (`rgy-par`), without it there is one worker, on
//! the thread that called. Nothing else changes between the two builds.
//!
//! The one thing the FSEvents callback needs that `std` does not have is
//! a way to be called from a thread the Rust runtime does not own: the
//! callback's state is `Send + Sync` and holds no thread of its own.

#![no_std]

extern crate alloc;
/// The test macros (`vec!`, `format!`) and the `std` the tests use.
///
/// The crate is `no_std`, so the core prelude has none of them. This is
/// the `std` declaration that brings them in, and it stands down outside
/// the tests: a crate cannot be declared twice.
#[cfg(test)]
#[macro_use]
extern crate std;

#[cfg(target_os = "macos")]
mod fsevents;
#[cfg(target_os = "macos")]
mod walk;

pub mod event;
pub mod feed;
pub mod record;

pub use event::{
    Event, HISTORY_DONE, ITEM_IS_DIR, KERNEL_DROPPED, MUST_SCAN_SUBDIRS, USER_DROPPED,
};
pub use feed::{FseventsWatch, Grain, KeepAlive};

#[cfg(target_os = "macos")]
pub use fsevents::{current_id, watch};
#[cfg(target_os = "macos")]
pub use walk::{list_one, scan};
