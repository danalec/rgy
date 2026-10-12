//! Independent reimplementation of `memmap2-rs` upstream commit f16835d (v0.9.0).
//! Written from the upstream source to understand the public API; no verbatim copies.

#![allow(clippy::all)]
#![allow(clippy::missing_safety_doc)]
#![allow(clippy::len_without_is_empty)]
#![allow(dead_code)]
#![allow(unexpected_cfgs)]

mod advice;
mod error;
mod mmap;
mod mmap_mut;
mod options;
mod platform;

#[cfg(unix)]
pub use self::advice::{Advice, UncheckedAdvice};
pub use self::mmap::Mmap;
pub use self::mmap_mut::MmapMut;
pub use self::options::MmapOptions;
pub use self::platform::{MmapAsRawDesc, MmapRawDescriptor};
