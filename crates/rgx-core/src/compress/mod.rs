//! In-house compression codecs, vendored from upstream.
//!
//! **Provenance.** Copied by hand from `upstream/src/b02/compress/` (deflate,
//! zlib, bzip2, bwt, huffman, lz77, crc32, error) and
//! `upstream/src/b01/io/binary/gzip.rs`, at the state of upstream commit `1a839b8` /
//! `a6d36d7`. The `alloc::` paths rewritten to `std::`/`vec!`, fixture paths
//! re-anchored to this crate's `testdata/`, gzip's cross-module paths point at
//! the sibling modules here, and the `isa-huffman` fast paths stay `#[cfg]`'d
//! out because this crate does not define the feature.
//!
//! `cap.rs` is rgx's own: the ceiling on how many bytes one decode may emit.
//! lz4 is deliberately absent — its frame wrapper is not interop-proven,
//! only its block format is, so .lz4 files still go through `lz4_flex`.

pub mod bwt;
pub mod bzip2;
pub mod cap;
pub mod crc32;
pub mod deflate;
pub mod error;
pub mod gzip;
pub mod huffman;
pub mod lz77;
pub mod lzma;
pub mod xz;
pub mod zlib;
pub mod zstd;

pub use bzip2::Bzip2Decoder;
pub use cap::{Bounded, OutputCap, UNLIMITED};
pub use deflate::DeflateDecoder;
pub use gzip::{GzipError, GzipReader, GzipResult, GzipWriter};
pub use xz::XzReader;
pub use zlib::{ZlibDecoder, ZlibEncoder};
pub use zstd::ZstdDecoder;
