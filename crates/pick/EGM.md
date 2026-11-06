# crates/pick

## Overview
The `rgy-pick` crate: handing rgy's result records to an external picker. Its
manifest names the package `rgy-pick` (edition 2024) and declares
`default = ["std"]` with an empty `std = []` feature; there are no runtime
dependencies. Without the feature only the vocabulary and decisions remain —
which picker, what its exit status means, and the byte rules — with no line
that touches the machine.

## Subdirectories & Boundaries
- [src](./src/EGM.md) — the single `.rs` source, `lib.rs`, holding the whole
  picker contract.

## Interface
- `[Cargo.toml:package.name]` — package `rgy-pick`.
- `[lib.rs:Separator]`, `[lib.rs:Order]`, `[lib.rs:Request]`,
  `[lib.rs:NUL_CAPABLE]` — the picker vocabulary (from `src`).
- `[lib.rs:status]`, `[lib.rs:carries_line]`, `[lib.rs:carryable_in]` — the
  public rules (from `src`).
- `[lib.rs:pick_records]`, `[lib.rs:pick]`, `[lib.rs:takes_colour_flags]` —
  the std entry points (from `src`).

## Invariants
- `[lib.rs:no_std]` — `#![no_std]` + `extern crate alloc`; `std` only behind
  `#[cfg(any(test, feature = "std"))]` (from `src`).
- `[Cargo.toml:features.std]` — `std = []`.
- `[lib.rs:status]` — exit status 0 / 1 / 2 are spelled out rather than
  inherited (from `src`).
- `[lib.rs:carries_line]` — a record with a line break is left out, not split
  (from `src`).

## Side Effects
- `[lib.rs:pick_records]` — spawns the picker process and a writer thread
  (from `src`).
- `[lib.rs:scan]` — scans `PATH` and memoises the result per process (from
  `src`).
- `[lib.rs:pick]` — writes the selection to stdout (from `src`).

## Verification
- Run: `cargo test --workspace --no-fail-fast --locked`
