//! Name index, query filters, and fuzzy ranking.
//!
//! fsearch's index/scoring engine with the zen-rs filter vocabulary merged
//! (`ext/dir/type/size`, globs, `|`, `re:`), plus the persistence glue that
//! saves a built index across processes. Dual-licensed (MIT OR Apache-2.0)
//! throughout -- see COPYING; the Apache-2.0-only zen code lives in
//! `zen-rs`, `search-ui` and `discovery-windows`.

pub mod fuzzy;
/// fsearch's live overlay (dead bits + overrides).
pub mod live;
/// fsearch's name index v2 (interned names, DFS layout, class bitmaps).
pub mod name_index;
/// fsearch's query parser/scorer with the zen-rs filter vocabulary merged.
pub mod name_query;
pub mod regex;

/// fsearch's content index (trigram postings + per-file 5/7-gram bloom).
/// Kept inside name-core beside `live` for the same reason: it reads the
/// name index's blob internals (`HDR`/`sec!`) and shares `Query`; a
/// `content-core` crate splits out with the Phase 4 glue to rgx's engine.
pub mod content;

/// Persisting a built name index so a repeat query pays an mmap instead of
/// a full tree walk, plus the root check that makes loading one safe.
pub mod persist;

/// fsearch's `engine::no_materialize`: on macOS it marks files so a scan
/// does not materialize cloud placeholders. Portable no-op — the platform
/// backends can set their own policy.
pub fn no_materialize() {}
