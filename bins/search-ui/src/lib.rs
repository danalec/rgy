//! Terminal search UI (zen-rs `ui.rs`, transported).
//!
//! Per `IMPLEMENTATION_PLAN.md` §3 this becomes `search-ui`, backed by the
//! new name searcher + content grep. The sparkline stays; p50/p90 and
//! candidates-skipped come later.

mod ui;

pub use ui::*;
