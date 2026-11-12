// SPDX-License-Identifier: Apache-2.0

//! The launcher's model.
//!
//! Everything here is a decision, and not one of them touches the machine:
//! what the user typed, what the engine answered, which result is
//! selected, what the window's geometry is, and whether two Control
//! presses in a row were the hotkey. The window, the font and the daemon
//! client are the machine — they live in the binary and in the platform
//! crates — and this crate is the part that can be tested without one.
//!
//! The crate is `no_std` + `alloc` for that reason, and it goes into the
//! bare-metal gate with the rest of the library tree.

#![no_std]

extern crate alloc;

pub mod actions;
pub mod app;
pub mod commands;
pub mod hotkey;
pub mod latency;
pub mod layer;
pub mod layout;
pub mod query;
pub mod results;
pub mod settings;
pub mod source;
pub mod text;
pub mod theme;
pub mod tools;
pub mod usage;

pub use actions::Action;
pub use app::{App, Mode, Preview};
pub use commands::{COMMANDS, Command};
pub use hotkey::DoubleCtrl;
pub use latency::{Latency, LatencyMeter};
pub use layer::{Layer, Split};
pub use layout::Layout;
pub use query::{Query, ShortFilter};
pub use results::{Results, Row};
pub use settings::{Fzf, Hotkey, Overlay, Settings, TapKey};
pub use source::{Answer, Source};
pub use text::{Lang, Text};
pub use theme::{Palette, Rgb, Theme};
pub use tools::{ContentBackend, content_backend};
