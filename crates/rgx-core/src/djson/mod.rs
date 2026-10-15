//! rgx's JSON value, parser, serializer and streaming layer.
//!
//! The suite is organised in two stages:
//!
//! * **Stage 1** ([`json_scan`]) produces two bitsets over the document:
//!   which bytes are structural (`{ } [ ] : ,` and whitespace outside
//!   strings) and which are string-delimiter quotes. Stage 1 is the one
//!   piece of upstream lineage the suite still carries -- the
//!   structural-index scanner whose vendoring pin is in
//!   `scripts/vendored-sources.tsv`.
//! * **Stage 2** ([`index_parser`]) walks the two bitsets and populates
//!   an [`Arena`] with the parsed document. The walker holds two sorted
//!   `Vec<usize>` snapshots of the bitset iterations (one for
//!   structural, one for quotes) and a current byte offset; every
//!   token is a question about a known structural position rather
//!   than a fresh scan.
//!
//! The arena is the load-bearing data model. A parsed JSON value is a
//! `Copy` handle into an `Arena { strings, arrays, objects, root }`.
//! Object keys are `usize` slots into the arena's string table
//! (deduped), so an escaped key whose source bytes needed decoding is
//! owned by the arena just like a non-escaped one. The `BTreeMap` key
//! type is `usize` and the arena's allocator owns every string in the
//! document. `parse` returns `(Value, Arena)`; the caller drops the
//! arena when it is done with the value.
//!
//! `no_std` + `alloc`. `serde` is opt-in. The streaming layer
//! ([`stream`]) is `std`-only and feeds one document at a time into
//! the parser; the rest of the suite is `no_std`-clean.

/// Byte-walking helpers shared by [`stream`] and [`raw_value`]. The
/// two functions here are the only piece of the streaming layer that
/// both modules need; extracting them avoids the `raw_value` ->
/// `stream` import chain the alternative would require.
pub(super) mod byte_scan;
pub mod error;
/// Stage 2 of the djson two-stage parser. The walker reads the
/// structural-index bitsets produced by [`json_scan`] and populates
/// an [`Arena`]; it is the only public entry point
/// [`crate::djson::parse`] uses.
pub mod index_parser;
/// Stage 1: structural-index scanner. The vendoring pin in
/// `scripts/vendored-sources.tsv` points at the upstream
/// `simdjson/simdjson` `json_scan` module -- this is the one piece of
/// upstream lineage the djson suite still carries. The copy-time
/// changes are listed in `scripts/vendored-sources.tsv`.
pub mod json_scan;
/// Lazy (on-demand) views over the same structural index: values are
/// validated like [`index_parser`] validates them but materialised
/// only where the consumer touches them. See the module header for the
/// three contracts that keep it a companion of the Arena parser.
pub mod ondemand;
pub mod parser;
/// Borrowed byte-slice pointer to a JSON fragment: `RawValue` and its
/// `from_str` entry point. The type and the byte walker are always
/// compiled.
pub mod raw_value;
/// Streaming and NDJSON layer. Carries a `Vec<u8>` between feeds,
/// hands out one [`Arena`] per complete document. The `from_reader`
/// convenience inside is `std`-only; the parser, the NDJSON
/// iterator and the `ndjson` constructor are `no_std` + `alloc`.
pub mod stream;
pub mod value;

pub use error::{ErrorCategory, JsonError, JsonResult};
pub use parser::{parse, to_string, to_string_pretty};
pub use raw_value::RawValue;
#[cfg(feature = "std")]
pub use stream::from_reader;
pub use stream::{ndjson, NdJsonIter, StreamParser};
pub use value::{escape_json_string, write_escaped, Arena, Number, Value};
