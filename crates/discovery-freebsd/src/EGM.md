# rgy-discovery-freebsd src — the FreeBSD walk

## Overview
The FreeBSD discovery backend: a `getdirentries` bulk walk exposed as a
`[lib.rs:rgy_discovery::Source]`. `[lib.rs:#![no_std]]` is deliberately
declared before `[lib.rs:#![cfg(target_os = "freebsd")]]` — a crate-level
`cfg` that is false empties the crate, and an emptied crate has no `no_std`
to declare. `[lib.rs:pub use walk::{FreeBsdSource, scan}]` is the crate's
whole public face; `[walk.rs:scan]` parses dirent rows at `offset_of!`
offsets from the whole `struct dirent` the platform writes.

## Interface
- `[lib.rs:FreeBsdSource]` / `[lib.rs:scan]` — re-exports of `[walk.rs:FreeBsdSource]` and `[walk.rs:scan]`.
- `[walk.rs:impl rgy_discovery::Source for FreeBsdSource]` — `enumerate` delegates to `scan(&self.root)`.
- `[walk.rs:Dirent]`, `[walk.rs:Timespec]`, `[walk.rs:Stat]` — the `#[repr(C)]` layouts of the platform structs.
- `[walk.rs:OFF_RECLEN]`, `[walk.rs:OFF_TYPE]`, `[walk.rs:OFF_NAME]` — `offset_of!`-derived dirent field offsets.
- `[walk.rs:O_RDONLY]`, `[walk.rs:O_DIRECTORY]`, `[walk.rs:DT_DIR]`, `[walk.rs:DT_REG]`, `[walk.rs:DT_LNK]`, `[walk.rs:S_IFDIR]` — platform constants.

## Invariants
- `[lib.rs:#![no_std]]` / `[lib.rs:#![cfg(target_os = "freebsd")]]` — `no_std` is declared before the crate-level `cfg`; off-target the crate compiles empty and the portable walk is the `Source`.
- `[walk.rs:OFF_RECLEN]` / `[walk.rs:OFF_TYPE]` / `[walk.rs:OFF_NAME]` — dirent rows are parsed at `offset_of!`-derived offsets, never hand-remembered, and stride-walked by `d_reclen`; a layout mistake is a build or differential-test failure, not a wrong kind byte.
- `[walk.rs:getdirentries]` — the 64 KiB buffer is above any filesystem block size, because `getdirentries` needs `nbytes` >= the block size or some filesystems answer `EINVAL`.
- `[walk.rs:Dirent]` — the whole `struct dirent` is declared, including the padding the header puts between fields.
- `[walk.rs:rgy_discovery::path::join]` — paths are joined as bytes and handed to the kernel unchanged.

## Side Effects
- `[walk.rs:open]`, `[walk.rs:close]`, `[walk.rs:lstat]`, `[walk.rs:getdirentries]` — kernel calls declared by hand in `[walk.rs:unsafe extern "C"]`.
- `[walk.rs:scan]` — opens directories, reads bulk dirent blocks, and takes one `lstat` per entry for size/mtime (there is no `getattrlistbulk` on FreeBSD).
- `[walk.rs:lstat]` — per-entry metadata read from the kernel.

## Verification
- Run: `cargo test --workspace --no-fail-fast --locked`
