//! Name index, query filters, and fuzzy ranking.
//!
//! Everything-rs's layer, transported intact (RE-audit fixes included).
//! Per `IMPLEMENTATION_PLAN.md` §4.1 fsearch's index/scoring engine merges
//! in here later and the filter vocabulary below (`ext/dir/type/size`,
//! globs, `|`, `re:`) is the one that stays.

pub mod fuzzy;
/// fsearch's name index v2 (interned names, DFS layout, class bitmaps).
pub mod name_index;
/// fsearch's query parser/scorer with the everything-rs filter vocabulary merged.
pub mod name_query;

pub mod index;
/// fsearch's live overlay (dead bits + overrides).
pub mod live;
pub mod regex;

/// fsearch's `engine::no_materialize`: on macOS it marks files so a scan
/// does not materialize cloud placeholders. Portable no-op — the platform
/// backends can set their own policy.
pub fn no_materialize() {}
