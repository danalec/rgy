# crates/name-core

## Overview
The `rgy-name-core` crate: the name index, the query filters and the fuzzy
ranking, plus the generated Unicode tables and the two table generators that
produce them. The manifest names the package `rgy-name-core` (edition 2024),
declares `default = ["std"]` and a `std` feature that turns on
`rgy-par/std`, `rgx-core/std` and `rgy-discovery/std`, and holds one
`[[example]]` (`mmap_vs_read`) gated on `required-features = ["std"]`. It
depends on `rgy-discovery`, `rgy-par` and the vendored `rgx-core` (package
`rgy-vendored-core`), and uses `hashbrown` only as a dev-dependency oracle.
All of it is Apache-2.0: the three sources once transported from fsearch
(`content`, `name_index`, `name_query`) were replaced by independent
implementations, and `LICENSE-MIT-FSEARCH` was removed with the last of them.

## Subdirectories & Boundaries
- [examples](./examples/EGM.md) — the single `mmap_vs_read` sequential-scan
  benchmark.
- [src](./src/EGM.md) — the 23 `.rs` library sources: index, query, ranking,
  fold, NFC, readings, overlay, session and lookup.
- [tools](./tools/EGM.md) — the two Python UCD table generators.

## Interface
- `[Cargo.toml:package.name]` — package `rgy-name-core`; consumers import it
  as `rgy_name_core` (seen in the engine examples' `use rgy_name_core::...`).
- `[lib.rs:pub mod name_index]`, `[lib.rs:pub mod name_query]`,
  `[lib.rs:pub mod rank]`, `[lib.rs:no_materialize]` — the library's public
  module surface (rolled up from `src`).
- `[name_query.rs:Query]`, `[name_query.rs:Searcher]`,
  `[name_index.rs:Index]`, `[fold.rs:fold]` — core engine types (from `src`).
- `[mmap_vs_read.rs:main]` — the benchmark entry point (from `examples`).
- `[gen_nfc_table.py:main]`, `[gen_readings_table.py:main]` — the generator
  entry points (from `tools`).

## Invariants
- `[lib.rs:no_std]` — `#![no_std]` + `extern crate alloc`; `std` is declared
  only behind `#[cfg(all(feature = "std", not(test)))]` (from `src`).
- `[Cargo.toml:features.default]` — `default = ["std"]`; the `std` feature is
  what links the filesystem, the threads and the clock.
- `[Cargo.toml:required-features]` — `mmap_vs_read` requires the `std` feature.
- `[Cargo.toml:package.license]` — `Apache-2.0`; the product carries no
  third-party source file (from `src/lib.rs` docs). The v2 blob magic
  `FSIDX015` is kept as a data-format contract, not as code.

## Side Effects
- `[clock.rs:clock_gettime]`, `[clock.rs:GetSystemTimeAsFileTime]` —
  hand-declared platform clocks (from `src`).
- `[scan_priority.rs:unsafe extern "C"]` — hand-declared platform ABI calls
  (from `src`). `content.rs` is std-gated and declares no platform ABI; the
  hand-declared extern blocks live in `clock.rs` and `scan_priority.rs`.
- `[placeholder.rs:attributes]`, `[live.rs:lstat]` — OS attribute/stat reads
  (from `src`).
- `[mmap_vs_read.rs:main]` — temp corpus file I/O, `sync` spawn and stdout
  report (from `examples`, std-only).
- `[gen_nfc_table.py:main]`, `[gen_readings_table.py:main]` — build-time reads
  of UCD files and writes of generated `.rs` files (from `tools`).

## Verification
- Run: `cargo test --workspace --no-fail-fast --locked`
