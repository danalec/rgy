//! Linux discovery backend (`IMPLEMENTATION_PLAN.md` §4.2): a `getdents64`
//! bulk walk producing the same DFS [`Listing`]s as every other `Source`,
//! and a recursive inotify [`Watch`] whose overflow becomes `Rescan`.
//!
//! `statx` is used for metadata where it is available (mtime is what the
//! name index's `mtime:` filter reads); `fanotify` FID hardening is later
//! work and not in this module.
//!
//! The whole backend is `cfg(target_os = "linux")` — on other hosts this
//! crate compiles empty and the portable `discovery::PortableSource` is
//! the Source.

#[cfg(target_os = "linux")]
mod walk;
#[cfg(target_os = "linux")]
mod watch;

#[cfg(target_os = "linux")]
pub use walk::LinuxSource;
#[cfg(target_os = "linux")]
pub use watch::InotifyWatch;

// On a non-Linux host this crate compiles empty: the portable
// `discovery::PortableSource` is the Source and the Windows/macOS
// backends own their platforms.
