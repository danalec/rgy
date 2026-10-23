// SPDX-License-Identifier: Apache-2.0

//! macOS discovery backend (`docs/design.md` §2.2): fsearch's
//! `getattrlistbulk` walk and FSEvents watch, transported verbatim.
//!
//! The whole backend is `cfg(target_os = "macos")`; on other hosts this
//! crate compiles empty and `discovery::PortableSource` is the Source.
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
//! into one [`discovery::ChangeBatch`]. Its decisions — which folder an
//! event means, and which events belong to the indexed tree — are plain
//! functions over [`event::Event`], so they are tested on every platform
//! rather than only on a Mac.

#[cfg(target_os = "macos")]
mod fsevents;
#[cfg(target_os = "macos")]
mod walk;

pub mod event;
pub mod feed;

pub use event::{
    Event, HISTORY_DONE, ITEM_IS_DIR, KERNEL_DROPPED, MUST_SCAN_SUBDIRS, USER_DROPPED,
};
pub use feed::{FseventsWatch, Grain, KeepAlive};

#[cfg(target_os = "macos")]
pub use fsevents::{current_id, watch};
#[cfg(target_os = "macos")]
pub use walk::{list_one, scan};
