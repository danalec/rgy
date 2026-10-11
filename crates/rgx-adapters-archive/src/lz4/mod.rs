//! # LZ4 decompression
//! Vendored from the LZ4 project (`https://lz4.org`,
//! `lib/lz4frame.c`, `lib/lz4.c`, `lib/lz4hc.c`). rgx only decodes, so
//! the encoder half (`Lz4Encoder`, `encode_block_graph` and the
//! graph-theoretic match finder it pulls in) is carried along unused.
//!
//! ## Mechanical adjustments for `std`
//!
//! `crate::b01::libm::{sqrt,log2}` become `f64::{sqrt,log2}` in
//! `graph_optimize.rs`; the upstream sources are `no_std`-friendly and
//! have no `f64` methods. The `include_bytes!` test vectors point at
//! this crate's `testdata/`.
//!
//! ## Block-size read
//!
//! The frame walk as vendored read `(header >> 1) & 0x7FFF_FFFF`.
//! `lz4frame.c` actually emits `size | (uncompressed << 31) | (last <<
//! 0)`: size is bits 0..30 and the last-block flag is bit 0 of the same
//! field, so they overlap and the size is never shifted. The shifted
//! read returned roughly half the real length, so every non-empty frame
//! from `lz4` failed to walk -- the defect survived because the same
//! shifted layout was the only thing that decoded. `testdata/lz4_*.lz4`
//! are reference-CLI output and pin the corrected read.
//!
//! With the corrected read, bit 0 is ambiguous (any odd block size
//! sets it), so termination follows the end mark (`block_size == 0`),
//! not the flag.
//!
//! ## Dependent blocks (`-BD`)
//!
//! Each block must decode straight into the running buffer: a match in
//! block N resolves against bytes block N-1 produced. Decoding into a
//! fresh `Vec` per block, as the vendored copy did, breaks that.
//!
//! ## Legacy container
//!
//! `lz4 -l` emits the deprecated legacy container, magic `0x184C2102`,
//! which is a different format from the modern frame. `decode_legacy`
//! in `lz4.rs` reads it. The legacy format is a bare sequence of
//! length-prefixed blocks with no frame header, no block checksums and
//! no end-mark semantics, so it shares only the block compressor with
//! the frame reader. Dictionary-compressed frames (`lz4 -D`) that
//! match across the dictionary are still rejected.
//!
//! The blanket allow below matches `sevenz`/`tar`: rgx never encodes,
//! so the encoder half and its match finder are carried along unused,
//! and silencing that per-item would be something a re-vendor had to
//! replay. `unreachable_patterns` is deliberately NOT allowed: a
//! shadowed match arm in a parser is a real defect, not vendoring
//! noise.
#![allow(
    dead_code,
    unused_imports,
    unused_mut,
    unused_variables,
    unused_assignments,
    // `lz4::lz4` mirrors the vendored upstream layout (scripts/vendored-sources.tsv).
    clippy::module_inception
)]

pub mod error;
pub mod graph_optimize;
pub mod lz4;

pub use lz4::{Lz4Decoder, Lz4Encoder};
