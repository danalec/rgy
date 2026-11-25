# crates/engine/src

## Overview
The library sources of `rgy-engine`: orchestration glue — the keep-warm
window, console output encoding, adapter-view policy resolution, `~` path
expansion, and the TV pack-store grep. The crate root declares `#![no_std]`
with `extern crate alloc`; `tv` is `#[cfg(feature = "std")]`, and the `std`
feature turns on `rgx-core/std` and `rgx-core/tv-runtime`.

## Interface
- `[lib.rs:no_std]` — crate root `#![no_std]` and `extern crate alloc`.
- `[lib.rs:pub mod warm]`, `[lib.rs:pub mod console]`,
  `[lib.rs:pub mod adapter_view]`, `[lib.rs:pub mod paths]`,
  `[lib.rs:pub mod tv]`, `[lib.rs:pub use warm::{keep_warm, warm_stop}]`.
- `[warm.rs:Window]`, `[warm.rs:State]`, `[warm.rs:keep_warm]`,
  `[warm.rs:warm_stop]` — the warm window and its spinner.
- `[console.rs:Mode]`, `[console.rs:mode_for]`, `[console.rs:init]`,
  `[console.rs:init_with_input]`, `[console.rs:Console]`,
  `[console.rs:stdout]`, `[console.rs:stderr]`, `[console.rs:spell]` — console
  encoding and UTF-8/UTF-16 output.
- `[adapter_view.rs:VIEW_ENV]`, `[adapter_view.rs:UPSTREAM_VIEW_ENV]`,
  `[adapter_view.rs:policy_from]`, `[adapter_view.rs:resolve]`,
  `[adapter_view.rs:apply]`, `[adapter_view.rs:apply_from]` — adapter-view
  policy resolution.
- `[paths.rs:home_dir]`, `[paths.rs:expand_home]`,
  `[paths.rs:expand_home_with]` — `~` expansion.
- `[tv.rs:TvFile]`, `[tv.rs:grep_tv]` — the TV pack-store grep (std).

## Invariants
- `[lib.rs:no_std]` — `no_std` + `alloc`; `extern crate std` is declared only
  behind `#[cfg(feature = "std")]`.
- `[lib.rs:pub mod tv]` — `tv` is `#[cfg(feature = "std")]`; the keep-warm
  window's arithmetic stays `no_std` while its spinner thread does not.
- `[warm.rs:Window]` — a closed window is `until == 0`, so a search closes it
  with one store.
- `[adapter_view.rs:policy_from]` — rgy's own `RGY_ADAPTER_VIEW` wins over
  rgx's `RGX_ADAPTER_VIEW`; an unknown own value resolves to `Source`, and the
  fallback is not consulted.
- `[console.rs:Mode]` — a redirected stream keeps raw UTF-8 bytes; only a real
  console is switched to UTF-8 / `WriteConsoleW`.
- `[paths.rs:expand_home]` — a leading `~` expands to the home directory on
  every platform and in every shell.

## Side Effects
- `[warm.rs:keep_warm]` — the keep-warm spinner is a `std` thread that waits on
  the window; `[warm.rs:warm_stop]` cancels it.
- `[console.rs:init]` — switches the Windows console output code page to
  UTF-8; `[console.rs:Console]` falls back to `WriteConsoleW` with UTF-16.
- `[adapter_view.rs:resolve]`, `[adapter_view.rs:apply]` — read the
  `RGY_ADAPTER_VIEW` / `RGX_ADAPTER_VIEW` environment variables and call
  `rgx_core::set_policy`.
- `[paths.rs:home_dir]` — reads the platform's home directory.
- `[tv.rs:grep_tv]` — searches a TV pack store on disk through
  `rgx_core::tv_runtime::searcher::search_store` and loads the store manifest.

## Verification
- Run: `cargo test --workspace --no-fail-fast --locked`
