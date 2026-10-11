//! rar-codec: pure-Rust RAR container codec.
//!
//! Vendored from libarchive (`archive_read_support_format_rar5.c`,
//! `archive_read_support_format_rar.c`, BSD-2-Clause) and RARLAB's published
//! RAR technote (`https://www.rarlab.com/technote.htm`). Per-file provenance
//! is in `scripts/vendored-sources.tsv`; this header is the one place the
//! tree declares where it came from. Zero dependencies, `#![no_std]` +
//! `alloc`, `#![forbid(unsafe_code)]`.
//!
//! ## Decode
//!
//! Walks the full header structure of RAR4 and RAR5 archives. Extraction
//! supports stored entries, the RAR5 LZ77/Huffman method (version 0,
//! including solid archives and the delta, E8/E8E9 and ARM filters), and
//! the RAR4 LZ77/Huffman method (0x31..=0x35, including solid archives,
//! the standard E8/E8E9, delta, RGB and audio VM filters, and PPMd var-H
//! blocks inside the packed stream), all with CRC-32 verification. RAR5
//! multivolume archives reassemble across volumes via
//! [`Archive::open_multi`]. Encrypted payloads, PPMd filters, arbitrary
//! RAR VM program filters and the RAR 7.x compression algorithm
//! (version 1) are reported as [`Error::Unsupported`].
//!
//! ## Encode
//!
//! Writes valid RAR5 archives using the store method. The RAR5 container
//! layout is publicly documented by RARLAB; writing store-only archives
//! needs no proprietary algorithm.
//!
//! ```
//! use rar_codec::ArchiveBuilder;
//!
//! let mut w = ArchiveBuilder::new();
//! w.add_stored("hello.txt", b"hi there").unwrap();
//! let bytes = w.finish();
//!
//! let arc = rar_codec::Archive::open(&bytes).unwrap();
//! assert_eq!(arc.entries().len(), 1);
//! assert_eq!(arc.entries()[0].name, "hello.txt");
//! assert_eq!(arc.extract(&arc.entries()[0]).unwrap(), b"hi there");
//! ```

#![no_std]
#![forbid(unsafe_code)]

#[cfg(any(test, feature = "std"))]
extern crate std;

extern crate alloc;

mod bitreader;
mod crc32;
mod error;
mod huffman;
mod ppmd7;
mod read;
mod unpack;
mod unpack3;
mod vint;
mod write;

pub use error::{Error, Result};
pub use read::{Archive, Entry, EntryKind, Format, METHOD_STORE};
pub use write::ArchiveBuilder;
