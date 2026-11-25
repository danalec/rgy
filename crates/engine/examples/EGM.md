# crates/engine/examples

## Overview
Four example/benchmark programs that exercise the name engine and rgx-core's
matcher. `name_bench.rs` is the Phase 0 name-search + walk latency harness
whose CSV output the perf gate diffs; `name_proto.rs` is the name-tier
protocol case-study harness (five modes: `materialize`, `build`, `q-index`,
`q-walk`, `floor`); `session_bench.rs` measures a re-ranked keystroke against
a fresh search; `tesla_valve.rs` is an empirical harness for the "Tesla
Valve" SIMD prefilter and corpus n-gram indexing architecture. They consume
`rgy-name-core`, `rgy-discovery` and the vendored `rgx-core`, which the engine
manifest declares as dev-dependencies.

## Interface
- `[name_bench.rs:main]` — entry point; `--csv`, `--save-index`, `--root`,
  `--no-walk` modes.
- `[name_bench.rs:synthetic_listings]`, `[name_bench.rs:stats]`,
  `[name_bench.rs:print_csv]`, `[name_bench.rs:per_files]` — corpus,
  statistics, CSV output, width helper.
- `[name_proto.rs:main]`, `[name_proto.rs:materialize]`,
  `[name_proto.rs:matches_ref]`, `[name_proto.rs:usage]`,
  `[name_proto.rs:COUNT_LIMIT]` — the five-mode harness and its reference
  matcher.
- `[session_bench.rs:main]`, `[session_bench.rs:synthetic_listings]`,
  `[session_bench.rs:best]` — fresh-vs-session timing.
- `[tesla_valve.rs:main]`, `[tesla_valve.rs:test_tesla_simd_choker]`,
  `[tesla_valve.rs:test_tesla_corpus_index]`,
  `[tesla_valve.rs:build_sig256]`, `[tesla_valve.rs:build_query256]`,
  `[tesla_valve.rs:collect_rs_files]`, `[tesla_valve.rs:NOISE_THRESHOLD_COST]`
  — the Tesla Valve harness.

## Invariants
- `[name_bench.rs:synthetic_listings]` — a fixed deterministic corpus;
  warmups, min-of-N, and "counts proven equal before timing" (a fast wrong
  answer is a failure); a >20% regression on any row fails the PR.
- `[name_proto.rs:COUNT_LIMIT]` — `const COUNT_LIMIT: usize = 10_000_000`,
  reported unsaturated so the count gate compares real counts.
- `[name_proto.rs:matches_ref]` — the two query arms are suppressed unless
  their match counts agree exactly (the paper's count gate).
- `[session_bench.rs:best]` — best of several rounds over a fixed synthetic
  corpus.
- `[tesla_valve.rs:NOISE_THRESHOLD_COST]` — `const NOISE_THRESHOLD_COST: u64 =
  3_800_000_000`.

## Side Effects
- `[name_bench.rs:main]` — with `--save-index` persists a synthetic index
  through `rgy_name_core::persist`; with the walk fixture materializes one file
  per corpus entry on disk; prints a Markdown/CSV table to stdout and a human
  summary to stderr.
- `[name_proto.rs:materialize]` — writes the deterministic corpus as a real
  tree; `[name_proto.rs:main]` builds and persists a name index.
- `[tesla_valve.rs:collect_rs_files]` — walks a directory collecting `.rs`
  files; `[tesla_valve.rs:test_tesla_corpus_index]` imports `std::fs`.
- `[name_bench.rs:main]`, `[session_bench.rs:main]`,
  `[name_proto.rs:main]`, `[tesla_valve.rs:main]` — all import
  `std::time::Instant` for timing.

## Verification
- Run: `cargo test --workspace --no-fail-fast --locked`
