// SPDX-License-Identifier: Apache-2.0

//! Terminal search UI (zen-rs `ui.rs`, transported).
//!
//! Per the rules this becomes `search-ui`, backed by the
//! new name searcher + content grep. The sparkline stays, and the
//! percentiles are there too: the meter is the launcher's, and the
//! terminal reads it from the model rather than keeping its own.

mod preview;

pub mod runner;

pub mod term;

mod ui;

pub use runner::{NoRunner, Runner};
pub use ui::*;
