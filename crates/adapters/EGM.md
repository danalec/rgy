# crates/adapters

## Overview
The `rgy-adapters` crate: the registry that orders the ten vendored rgx
adapter families, and the reader that answers the content engine's adapter
seam. Its manifest names the package `rgy-adapters` (edition 2024) and lists
twelve dependencies — `rgx-core`, `rgy-name-core` and the ten
`rgx-adapters-*` families (package names `rgy-vendored-*`). It declares **no
`[features]` section and no `std` feature**; `src/lib.rs` has no `#![no_std]`,
and its module docs state the crate is `std` because the vendored families are
built with their formats turned on. This is the one repository crate the
bare-metal gate does not name.

## Subdirectories & Boundaries
- [src](./src/EGM.md) — the two `.rs` sources: `lib.rs` (the registry) and
  `reader.rs` (the `Adapters` reader).

## Interface
- `[Cargo.toml:package.name]` — package `rgy-adapters`.
- `[lib.rs:registry]`, `[lib.rs:pub use reader::Adapters]` — the registry
  builder and reader re-export (from `src`).
- `[reader.rs:Adapters]`, `[reader.rs:rendered]` — the `AdapterReader`
  implementation (from `src`).

## Invariants
- `[lib.rs:registry]` — the ten families are registered in upstream's order:
  archives, office, 3D, llm, media, tabular, pdf, mail, docs, inspect (from
  `src`).
- `[reader.rs:rendered]` — an empty vector means no adapter claims the file,
  and the content path reads its own bytes (from `src`).
- `[reader.rs:shared_registry]` — one registry per process, held in a
  `OnceLock` (from `src`).
- `[Cargo.toml:[dependencies]]` — twelve dependencies are declared and there
  is no `[features]` stanza; unlike the other library crates this one is
  `std` unconditionally.

## Side Effects
- `[reader.rs:read_for_adapter]` — reads files from disk through
  `content::open_regular` (from `src`).
- `[reader.rs:rendered]` — calls `rgx_core::process` to render the claimed
  bytes (from `src`).

## Verification
- Run: `cargo test --workspace --no-fail-fast --locked`
