//! macOS discovery backend (`docs/IMPLEMENTATION_PLAN.md` §4.2): fsearch's
//! `getattrlistbulk` walk and FSEvents watch, transported verbatim.
//!
//! The whole backend is `cfg(target_os = "macos")`; on other hosts this
//! crate compiles empty and `discovery::PortableSource` is the Source.
//! **Untyped on this workspace host** (no macOS target installed) — the
//! code is fsearch's, which compiles on macOS; the only edits are import
//! paths. Verify with `cargo check --target aarch64-apple-darwin` on a
//! Mac before relying on it.

#[cfg(target_os = "macos")]
mod fsevents;
#[cfg(target_os = "macos")]
mod walk;

#[cfg(target_os = "macos")]
pub use fsevents::watch;
#[cfg(target_os = "macos")]
pub use walk::{list_one, scan};
