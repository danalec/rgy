# crates/name-core/examples

## Overview
One example program, `mmap_vs_read.rs`: a sequential-scan benchmark that
compares `std::fs::read` against `rgx_core::memmap2::Mmap` over a deterministic
in-memory corpus. Its module docs say it closes an open measurement in rgx's
own numbers (mmap measured 1.51x slower than `read()` for scans on Windows)
and reports hot-cache min-of-N results; cold rows are reported only when the
process can write `/proc/sys/vm/drop_caches`. The crate manifest registers it
as the `mmap_vs_read` example behind `required-features = ["std"]`.

## Interface
- `[mmap_vs_read.rs:main]` — the bench entry point; parses an optional MiB
  argument, builds the corpus, times both paths and prints the report.
- `[mmap_vs_read.rs:checksum]` — FNV-1a over every byte, forcing a real touch
  of each page so neither path is optimized into a no-op.
- `[mmap_vs_read.rs:synthetic_bytes]` — deterministic LCG corpus generator.
- `[mmap_vs_read.rs:stats]` — min and median of a sample array.
- `[mmap_vs_read.rs:drop_caches]` — Linux-root-only cold-cache drop gate.
- `[mmap_vs_read.rs:ROUNDS]` — `const ROUNDS: usize = 15`.
- `[mmap_vs_read.rs:Mmap]` — the mmap path is `rgx_core::memmap2::Mmap`,
  imported from the vendored `rgx-core`, not defined locally.

## Invariants
- `[mmap_vs_read.rs:main]` — both paths prove the same `checksum` against the
  corpus before any timing ("counts proven equal before timing"); a divergence
  aborts with `assert_eq!`.
- `[mmap_vs_read.rs:ROUNDS]` — 15 rounds, min-of-N, one warmup per path.
- `[mmap_vs_read.rs:drop_caches]` — cold rows are marked unavailable rather
  than faked when `/proc/sys/vm/drop_caches` cannot be written.

## Side Effects
- `[mmap_vs_read.rs:main]` — creates a temp corpus directory and
  `corpus.bin`, writes and `sync_all`s it, then removes the file and directory.
- `[mmap_vs_read.rs:drop_caches]` — spawns `sync` through
  `std::process::Command` and writes `/proc/sys/vm/drop_caches` (root only).
- `[mmap_vs_read.rs:main]` — prints a Markdown results table to stdout.

## Verification
- Run: `cargo test --workspace --no-fail-fast --locked`
