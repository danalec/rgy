//! Name index, query filters, and fuzzy ranking.
//!
//! Everything-rs's layer, transported intact (RE-audit fixes included).
//! Per `IMPLEMENTATION_PLAN.md` §4.1 fsearch's index/scoring engine merges
//! in here later and the filter vocabulary below (`ext/dir/type/size`,
//! globs, `|`, `re:`) is the one that stays.

pub mod fuzzy;
pub mod index;
pub mod regex;
