# rgy-discovery examples

## Overview
A single benchmark example, `walk_bench.rs`, that times the portable walk's
sequential oracle against the fanned-out walk across `[1, 2, 4, 8, 16]`
threads and emits CSV on stdout, so a discovery regression can gate a PR the
way the name-search baseline does. `[walk_bench.rs:main]` parses
`<root> [rounds]`, warms the page cache, then times both walks. This is a
`std` example binary (it imports `[walk_bench.rs:Instant]`,
`[walk_bench.rs:std::env::args]`, `[walk_bench.rs:std::path::Path]`), not part
of the `no_std` library it measures.

## Interface
- `[walk_bench.rs:main]` — benchmark entry point; parses `<root> [rounds]` from `[walk_bench.rs:std::env::args]`, times each thread count, and prints one CSV row per count (header `threads,entries,min_ms,median_ms,speedup`).
- `[walk_bench.rs:rgy_discovery::walk::scan_sequential]` — the sequential oracle used to warm the page cache; with `threads = 1` the timed walk takes this same inline path.
- `[walk_bench.rs:rgy_discovery::walk::scan]` — the fanned-out portable walk that is timed per thread count.

## Invariants
- `[walk_bench.rs:rounds]` — clamped with `.max(1)`, so `samples` is never empty before `samples[0]` and `samples[samples.len() / 2]` are indexed.
- `[walk_bench.rs:rgy_discovery::walk::scan_sequential]` — the page cache is warmed with a sequential scan before any timing, so the comparison measures walk cost rather than first-touch I/O.
- `[walk_bench.rs:baseline]` — the first (`threads = 1`) sample is the baseline every `speedup` value divides by.

## Side Effects
- `[walk_bench.rs:rgy_discovery::walk::scan]` and `[walk_bench.rs:rgy_discovery::walk::scan_sequential]` — enumerate directories and read file metadata under `<root>` through the portable walk.
- `[walk_bench.rs:main]` — writes the CSV header and rows to stdout and `root={root} rounds={rounds}` to stderr.
- `[walk_bench.rs:std::env::args]` — reads the process command line.

## Verification
- Run: `cargo test --workspace --no-fail-fast --locked`
