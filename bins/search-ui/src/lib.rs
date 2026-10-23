// SPDX-License-Identifier: Apache-2.0

//! Terminal search UI (zen-rs `ui.rs`, transported).
//!
//! Per `docs/design.md` §3 this becomes `search-ui`, backed by the
//! new name searcher + content grep. The sparkline stays; p50/p90 and
//! candidates-skipped come later.

pub mod latency;

mod ui;

pub use ui::*;
