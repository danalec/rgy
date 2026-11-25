# rgy-discovery-freebsd

## Overview
The FreeBSD discovery backend crate (`[Cargo.toml:package.name]` is
`rgy-discovery-freebsd`, edition 2024). Its root holds only the manifest; the
library is in [src](./src/EGM.md). `[Cargo.toml:rgy-discovery]` is pulled in
with `default-features = false` so the `no_std` seam cannot bring `std` in
through the door, and `[Cargo.toml:dev-dependencies]` re-adds
`rgy-discovery` with the `std` feature only as the portable-walk oracle the
reader is compared against — a dev-dependency a normal build does not see.

## Subdirectories & Boundaries
- [src](./src/EGM.md) — `[src/walk.rs:FreeBsdSource]` / `[src/walk.rs:scan]`, the `getdirentries` bulk walk behind `[src/lib.rs:#![cfg(target_os = "freebsd")]]`.

## Interface
- `[src/lib.rs:FreeBsdSource]` / `[src/lib.rs:scan]` — the `rgy_discovery::Source` and its free-function walk.
- `[src/walk.rs:Dirent]`, `[src/walk.rs:OFF_RECLEN]`, `[src/walk.rs:OFF_TYPE]`, `[src/walk.rs:OFF_NAME]` — the declared `struct dirent` and its `offset_of!` field offsets.

## Invariants
- `[src/lib.rs:#![no_std]]` / `[src/lib.rs:#![cfg(target_os = "freebsd")]]` — `no_std` is declared before the crate-level `cfg`; off-target the crate compiles empty.
- `[src/walk.rs:OFF_RECLEN]` — dirent rows are stride-walked by `d_reclen` at `offset_of!`-derived offsets.
- `[src/walk.rs:getdirentries]` — a 64 KiB buffer is used so a filesystem does not answer `EINVAL`.
- `[Cargo.toml:dev-dependencies]` — the portable-walk oracle is a dev-dependency, so a normal build does not see the `std` edge.

## Side Effects
- `[src/walk.rs:getdirentries]`, `[src/walk.rs:lstat]`, `[src/walk.rs:open]`, `[src/walk.rs:close]` — kernel calls declared by hand in `[src/walk.rs:unsafe extern "C"]`.
- `[src/walk.rs:scan]` — bulk directory reads and per-entry `lstat` metadata.

## Verification
- Run: `cargo test --workspace --no-fail-fast --locked`
