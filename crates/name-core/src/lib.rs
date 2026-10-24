// SPDX-License-Identifier: Apache-2.0

//! Name index, query filters, and fuzzy ranking.
//!
//! fsearch's index/scoring engine with the zen-rs filter vocabulary merged
//! (`ext/dir/type/size`, globs, `|`, `re:`), plus the persistence glue that
//! saves a built index across processes.
//!
//! The crate is Apache-2.0, like the rest of the product -- see COPYING.
//! Three of its files (`name_index`, `name_query`, `content`) are transported
//! from fsearch and keep fsearch's MIT terms; their headers say so, and the
//! text is in `LICENSE-MIT-FSEARCH`. The other modules were written here,
//! on the same design, and are Apache-2.0.

pub mod fuzzy;
/// fsearch's live overlay (dead bits + overrides).
pub mod live;
/// fsearch's name index v2 (interned names, DFS layout, class bitmaps).
pub mod name_index;
/// fsearch's query parser/scorer with the zen-rs filter vocabulary merged.
pub mod name_query;
/// The ranking key: the priority ladder, the fixed-point score, and the one
/// `u64` the final order compares.
pub mod rank;
pub mod regex;
/// The user's priority rules: what a name is worth, and who it is for.
pub mod rules;

/// The comparison fold: ASCII case-fold + NFC (parity row 1.12). The
/// display and the I/O keep the bytes the filesystem gave; this is what a
/// comparison sees.
pub mod fold;
/// Unicode NFC normalization (UAX #15), the normalization half of the fold.
pub mod nfc;
/// The UCD's tables (generated): decompositions, combining classes,
/// composites and quick-check sets.
mod nfc_table;
/// The UCD's conformance vectors (generated): test data for [`nfc`].
#[cfg(test)]
mod nfc_vectors;

/// Readings: a name transliterated into the scripts a query can be in.
pub mod readings;
/// The UCD's Unihan readings (generated): what a Han character is called
/// in kana, and what it is called in pinyin.
mod readings_table;

/// fsearch's content index (trigram postings + per-file 5/7-gram bloom).
/// Kept inside name-core beside `live` for the same reason: it reads the
/// name index's blob internals (`HDR`/`sec!`) and shares `Query`; a
/// `content-core` crate splits out with the Phase 4 glue to rgx's engine.
pub mod content;

/// Persisting a built name index so a repeat query pays an mmap instead of
/// a full tree walk, plus the root check that makes loading one safe.
pub mod persist;

/// Cloud placeholders: the files whose bytes are not on this disk, and the
/// attribute that says so. The content path asks before it opens, so a
/// search never downloads a file the user did not ask for.
pub mod placeholder;

/// The incremental search session: what a keystroke re-uses (parity row
/// S.10). A query that narrows the previous one is answered by re-ranking
/// the entries the previous search matched, instead of walking the index
/// again.
pub mod session;

/// The lookup surface: five indexes over one record store (parity row
/// I.1). `file_name`, `folder_name`, `folder_backend`, `file_path` and
/// `folder_path`, all reading the same bytes the search reads.
pub mod lookup;

/// The scan workers' interactive priority: the platform's answer to "this
/// work is interactive, do not let it wait behind the machine's other work".
/// Windows has MMCSS, macOS has a QoS class, Linux has the approximation in
/// `scan_priority` -- which says so in its own comments.
mod scan_priority;

/// fsearch's `rgy_engine::no_materialize`: on macOS it marks files so a scan
/// does not materialize cloud placeholders. Portable no-op — the platform
/// backends can set their own policy.
pub fn no_materialize() {}
