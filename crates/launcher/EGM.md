# crates/launcher

## Overview
The `rgy-launcher` crate: the launcher's pure model — the query, the results,
the actions, the hotkey, the geometry, the usage history and the text/theme
vocabulary. Its manifest names the package `rgy-launcher` (edition 2024) and
declares `default = []` with **no `std` feature**: the model is the decision,
and the window, font and daemon client are the machine, living in the binary
and the platform crates. `src/lib.rs` declares `#![no_std]` and goes into the
bare-metal gate with the rest of the library tree.

## Subdirectories & Boundaries
- [src](./src/EGM.md) — the sixteen `.rs` sources of the model.

## Interface
- `[Cargo.toml:package.name]` — package `rgy-launcher`.
- `[lib.rs:pub use app::{App, Mode, Preview}]`,
  `[lib.rs:pub use query::{Query, ShortFilter}]`,
  `[lib.rs:pub use results::{Results, Row}]`,
  `[lib.rs:pub use actions::Action]`, `[lib.rs:pub use layout::Layout]`,
  `[lib.rs:pub use source::{Answer, Source}]` — the re-exported model types
  (from `src`).
- `[app.rs:App]`, `[layout.rs:Layout]`, `[hotkey.rs:DoubleCtrl]`,
  `[settings.rs:Settings]` (from `src`).

## Invariants
- `[lib.rs:no_std]` — `#![no_std]` + `extern crate alloc`, with no `extern
  crate std` (from `src`).
- `[Cargo.toml:features.default]` — `default = []`; no `std` feature is
  declared.
- `[hotkey.rs:DEFAULT_WINDOW_MS]` — the double-Control window is 400 ms (from
  `src`).
- `[layout.rs:BASE_DPI]` — the base DPI is 96 (from `src`).
- `[usage.rs:MAX_ENTRIES]` — the usage history holds at most 512 entries (from
  `src`).

## Side Effects
- None (no explicit side effects in the source)

## Verification
- Run: `cargo test --workspace --no-fail-fast --locked`
