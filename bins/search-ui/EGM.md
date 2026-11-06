# bins/search-ui

## Overview

Crate root of the terminal search UI library. The directory has no source of
its own: `[Cargo.toml:package.name]` is `rgy-search-ui` at
`[Cargo.toml:package.version]` 0.1.9, `[Cargo.toml:package.edition]` 2024 and
`[Cargo.toml:package.rust-version]` 1.89. The manifest declares no `[[bin]]`
or `[lib]` table; the library root is the [src](./src/EGM.md) child's
`lib.rs` ([lib.rs:pub mod term], [lib.rs:pub use ui::*]). Its workspace
dependencies are `[Cargo.toml:dependencies.rgy-name-core]`,
`[Cargo.toml:dependencies.rgy-launcher]`,
`[Cargo.toml:dependencies.rgy-engine]`,
`[Cargo.toml:dependencies.rgy-pick]` and
`[Cargo.toml:dependencies.rgy-proto]`; the
`[Cargo.toml:dev-dependencies.crossterm]` entry (0.28.1) is the differential
oracle that does not ship in a binary. All implementation is in the
[src](./src/EGM.md) child contract.

## Subdirectories & Boundaries
- [src](./src/EGM.md) — the library: `lib.rs`, the interactive `ui.rs`, the hand-declared terminal seam `term.rs` and the head-of-file `preview.rs`.

## Interface
- `[Cargo.toml:package.name]` — `rgy-search-ui`; `[Cargo.toml:package.version]` 0.1.9; `[Cargo.toml:package.edition]` 2024; `[Cargo.toml:package.rust-version]` 1.89.
- `[Cargo.toml:dependencies.rgy-name-core]`, `[Cargo.toml:dependencies.rgy-launcher]`, `[Cargo.toml:dependencies.rgy-engine]`, `[Cargo.toml:dependencies.rgy-pick]`, `[Cargo.toml:dependencies.rgy-proto]` — the workspace dependencies; `[Cargo.toml:dev-dependencies.crossterm]` (0.28.1) and `[Cargo.toml:dev-dependencies.rgy-discovery]` are dev-only.
- Rolled up from [src](./src/EGM.md): [lib.rs:pub mod term], [lib.rs:pub use ui::*], [ui.rs:pub fn run], [ui.rs:pub fn run_with_source], [ui.rs:pub fn run_with_source_lang_and_theme], [term.rs:pub struct Screen], [term.rs:pub trait Paint], [term.rs:pub fn enable_raw_mode], [term.rs:pub fn decode], [preview.rs:pub fn of].

## Invariants
- `[Cargo.toml:package.edition]` is 2024 and `[Cargo.toml:package.rust-version]` is 1.89; `[Cargo.toml:dev-dependencies.crossterm]` is dev-only, so no binary links it ([term.rs:pub const RESET], [term.rs:pub fn enable_raw_mode] are declared in the tree's own seam).
- Rolled up from [src](./src/EGM.md): [ui.rs:impl Drop for TerminalGuard] restores raw mode on panic or early error, [ui.rs:mod palette] sends 24-bit RGB only when advertised, [ui.rs:const MAX_RESULTS] is 20_000, [preview.rs:const MAX_BYTES] is 64 KiB, [term.rs:pub enum Decoded] and [term.rs:pub fn decode] decode keys.

## Side Effects
- Rolled up from [src](./src/EGM.md): [ui.rs:impl TerminalGuard] / [ui.rs:impl Drop for TerminalGuard] (raw mode, alternate screen, console reset writes), [term.rs:pub struct Screen] (ANSI escape writes), [term.rs:mod imp] (unix `tcgetattr`/`tcsetattr`/`ioctl` and Windows `GetConsoleMode`/`ReadConsoleInputW`/`WaitForSingleObject` calls), [ui.rs:mod palette] (environment reads), [ui.rs:fn run_app] (key reads, frame paints, stdout path), [preview.rs:fn read_head] (file open and stat).

## Verification
- Run: `cargo test --workspace --no-fail-fast --locked`
