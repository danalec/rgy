//! macOS discovery backend (`docs/IMPLEMENTATION_PLAN.md` §4.2): fsearch's
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
//! The FSEvents watch is not wired into the daemon yet, and `searchd`'s
//! `open_watch` says why (the stream is callback-based and directory-
//! granular, where the `Watch` trait speaks in file-granular records). The
//! stream and its flag vocabulary are exported here so the adapter that
//! closes that gap has a surface to build on, and so a caller can name the
//! `Event` type the stream's channel carries.

#[cfg(target_os = "macos")]
mod fsevents;
#[cfg(target_os = "macos")]
mod walk;

#[cfg(target_os = "macos")]
pub use fsevents::{
    Event, HISTORY_DONE, KERNEL_DROPPED, MUST_SCAN_SUBDIRS, USER_DROPPED, current_id, watch,
};
#[cfg(target_os = "macos")]
pub use walk::{list_one, scan};
