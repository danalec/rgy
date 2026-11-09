# crates/launcher/src

## Overview
The sixteen library sources of `rgy-launcher`: the launcher's model — the
query, the results, the actions, the hotkey, the geometry, the usage history
and the text/theme vocabulary. The crate root declares `#![no_std]` with
`extern crate alloc`; there is no `std` half on purpose, because every piece
here is a decision that does not touch the machine. The window, the font and
the daemon client live in the binary and the platform crates. The only `std`
references in the directory are inside `#[cfg(test)]` test code in `app.rs`.

## Interface
- `[lib.rs:no_std]` — crate root `#![no_std]` and `extern crate alloc`.
- `[lib.rs:pub mod actions]`, `[lib.rs:pub mod app]`, `[lib.rs:pub mod commands]`,
  `[lib.rs:pub mod hotkey]`, `[lib.rs:pub mod latency]`, `[lib.rs:pub mod layer]`,
  `[lib.rs:pub mod layout]`, `[lib.rs:pub mod query]`, `[lib.rs:pub mod results]`,
  `[lib.rs:pub mod settings]`, `[lib.rs:pub mod source]`, `[lib.rs:pub mod text]`,
  `[lib.rs:pub mod theme]`, `[lib.rs:pub mod tools]`, `[lib.rs:pub mod usage]` —
  the public module surface.
- `[lib.rs:pub use actions::Action]`, `[lib.rs:pub use app::{App, Mode, Preview}]`,
  `[lib.rs:pub use commands::{COMMANDS, Command}]`, `[lib.rs:pub use hotkey::DoubleCtrl]`,
  `[lib.rs:pub use latency::{Latency, LatencyMeter}]`, `[lib.rs:pub use layer::{Layer, Split}]`,
  `[lib.rs:pub use layout::Layout]`, `[lib.rs:pub use query::{Query, ShortFilter}]`,
  `[lib.rs:pub use results::{Results, Row}]`, `[lib.rs:pub use settings::{Fzf, Hotkey, Overlay, Settings, TapKey}]`,
  `[lib.rs:pub use source::{Answer, Source}]`, `[lib.rs:pub use text::{Lang, Text}]`,
  `[lib.rs:pub use theme::{Palette, Rgb, Theme}]`,
  `[lib.rs:pub use tools::{ContentBackend, content_backend}]` — the re-exported
  types.
- `[actions.rs:Action]`, `[app.rs:App]`, `[app.rs:Mode]`, `[app.rs:Preview]`,
  `[commands.rs:Command]`, `[commands.rs:COMMANDS]`, `[hotkey.rs:DoubleCtrl]`,
  `[layout.rs:Layout]`, `[query.rs:Query]`, `[query.rs:ShortFilter]`,
  `[results.rs:Results]`, `[results.rs:Row]`, `[settings.rs:Settings]`,
  `[source.rs:Source]`, `[source.rs:Answer]`, `[text.rs:Lang]`,
  `[text.rs:Text]`, `[theme.rs:Theme]`, `[tools.rs:ContentBackend]`,
  `[usage.rs:History]`, `[latency.rs:LatencyMeter]`, `[layer.rs:Layer]`.

## Invariants
- `[lib.rs:no_std]` — `no_std` + `alloc`; no `std` feature and no
  `extern crate std` in production code.
- `[settings.rs:impl Default for Hotkey]` — the default global shortcut is `Hotkey::Combo { modifiers: mods::ALT, key: 0x20 }` (Alt+Space) on every desktop; the X11 backend translates it to `Mod1Mask` + `XK_space` and a window manager that already owns the chord answers `BadAccess`, which the launcher says rather than hides.
- `[app.rs:DEFAULT_VISIBLE_ROWS]` — `pub const DEFAULT_VISIBLE_ROWS: usize = 12`.
- `[hotkey.rs:DEFAULT_WINDOW_MS]` — `pub const DEFAULT_WINDOW_MS: u64 = 400`,
  the double-Control window.
- `[layout.rs:BASE_DPI]` — `pub const BASE_DPI: u32 = 96`;
  `[layout.rs:SCALE_PERCENT]` — `pub const SCALE_PERCENT: u32 = 100`.
- `[usage.rs:MAX_ENTRIES]` — `pub const MAX_ENTRIES: usize = 512`;
  `[usage.rs:RECENT_SECONDS]` — 30 days.
- `[commands.rs:COMMAND_PREFIX]` — `pub const COMMAND_PREFIX: &str = ">"`.
- `[settings.rs:HEADER]` — `pub const HEADER: &str = "# rgy settings v1"`.

## Side Effects
- None (no explicit side effects in the source)

## Verification
- Run: `cargo test --workspace --no-fail-fast --locked`
