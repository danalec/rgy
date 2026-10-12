//! In-house compression codecs.
//!
//! **Provenance.** The codec modules here are independent reimplementations
//! of the published formats, written from those specifications rather than
//! translated from any implementation's source:
//!
//! - `crc32` / `xz::crc64` — the IEEE 802.3 / FIPS CRC-32 and the XZ
//!   CRC-64, both specified by their polynomial and bit-at-a-time
//!   recurrence.
//! - `lz77`, `huffman`, `deflate` — RFC 1951 (DEFLATE), with RFC 1950
//!   (zlib) and RFC 1952 (gzip) framing in `zlib` and `gzip`.
//! - `bwt`, `bzip2` — the bzip2 algorithm: RLE, Burrows-Wheeler,
//!   move-to-front, Huffman, and the block framing the format defines.
//! - `lzma` — the LZMA algorithm as published in the LZMA specification
//!   (range coding, Markov probability models, the literal/match state
//!   machine), plus the LZMA2 chunk framing.
//! - `xz` — the XZ container format specification (stream header, blocks,
//!   index, footer, check types) over the LZMA2 decoder.
//! - `zstd` — RFC 8878, decoding only.
//!
//! `error` and `cap` are rgx's own: the error vocabulary the decoders fail
//! with, and the ceiling on how many bytes one decode may emit. The cap is
//! a bomb guard, counted as bytes are emitted — decoding first and
//! measuring afterwards costs exactly what a compression bomb was built to
//! cost.
//!
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
