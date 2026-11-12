# crates/engine

## Overview
The `rgy-engine` crate: orchestration glue around the other engines. Its
manifest names the package `rgy-engine` (edition 2024), declares
`default = ["std"]` and a `std` feature that turns on `rgx-core/std` and
`rgx-core/tv-runtime`, and depends on the vendored `rgx-core` (package
`rgy-vendored-core`). `rgy-discovery` and `rgy-name-core` are dev-dependencies
for the examples only; the library itself does not walk anything. One
`[[example]]` (`name_bench`) is declared explicitly.

## Subdirectories & Boundaries
- [examples](./examples/EGM.md) — four name-engine / rgx-core benchmark and
  case-study harnesses.
- [src](./src/EGM.md) — the six `.rs` library sources: warm window, console,
  adapter-view policy, path expansion and the TV pack store.

## Interface
- `[Cargo.toml:package.name]` — package `rgy-engine`.
- `[lib.rs:pub mod warm]`, `[lib.rs:pub mod console]`,
  `[lib.rs:pub mod adapter_view]`, `[lib.rs:pub mod paths]`,
  `[lib.rs:pub mod tv]` — the public module surface (from `src`).
- `[warm.rs:Window]`, `[warm.rs:keep_warm]`, `[console.rs:init]`,
  `[adapter_view.rs:policy_from]`, `[paths.rs:expand_home]`,
  `[tv.rs:grep_tv]` — representative public functions (from `src`).
- `[name_bench.rs:main]`, `[name_proto.rs:main]` — harness entry points (from
  `examples`).

## Invariants
- `[lib.rs:no_std]` — `#![no_std]` + `extern crate alloc`; `std` is declared
  only behind `#[cfg(feature = "std")]` (from `src`).
- `[Cargo.toml:features.default]` — `default = ["std"]`, which turns on
  `rgx-core/std` and `rgx-core/tv-runtime`.
- `[Cargo.toml:[[example]]]` — `name_bench` is the explicitly declared
  example.
- `[warm.rs:Window]` — a closed warm window is `until == 0` (from `src`).

## Side Effects
- `[warm.rs:keep_warm]` — a `std` spinner thread (from `src`).
- `[console.rs:init]` — Windows console code-page switch and `WriteConsoleW`
  fallback (from `src`).
- `[adapter_view.rs:resolve]` — reads `RGY_ADAPTER_VIEW` / `RGX_ADAPTER_VIEW`
  environment variables (from `src`).
- `[tv.rs:grep_tv]` — on-disk TV pack-store search (from `src`).
- `[name_bench.rs:main]`, `[name_proto.rs:materialize]` — corpus/index files
  written to disk, timing and stdout/stderr report (from `examples`).

## Verification
- Run: `cargo test --workspace --no-fail-fast --locked`
