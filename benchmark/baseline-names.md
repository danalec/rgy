# Baseline: name search + walk (Phase 0 perf gate)

Machine: Windows x64, `cargo run --release -p engine --example name_bench`
(default corpus: 200,000 names / 1,000 dirs, 30 timed rounds after 3
warmup rounds, counts proven equal before timing).

**Gate:** a PR that regresses any row by more than 20% fails (plan §2).
These numbers are the *current* baseline — the plan's targets (name search
median ≤ 0.5 ms @ 5M) are fsearch-grade end-state goals, not this table's
ceiling. NameCache/pools/keep-warm (PR #12) and the 5M-scale bitmaps are
what closes the gap.

| query | hits | min µs | med µs | p90 µs |
|---|---|---|---|---|
| `report` (fuzzy) | 50 (top-k) | 552 | 596 | 796 |
| `file` (fuzzy, matches most of corpus) | 50 (top-k) | 4920 | 5130 | 5360 |
| `'report_00000.pdf` (exact) | 1 | 0.4 | 0.4 | 0.7 |
| `^src` (prefix) | 50 | 894 | 1003 | 1212 |
| `dat$` (suffix) | 50 | 114 | 175 | 191 |
| `mod` (fuzzy) | 50 | 811 | 1029 | 1309 |
| `rep` (fuzzy) | 50 | 524 | 561 | 625 |
| `f1` (fuzzy) | 50 | 4058 | 4358 | 4570 |

Corpus build: 200k names in ~40 ms. Walk: 201,000 entries in 4.36 s
(~46k entries/s, portable std walk — the platform fast paths land in
`discovery-macos` / MFT streaming).

Notes:
- `hits` is capped by `Query`'s `limit: 50` top-k; the timed work is the
  scan+rank, not the result copy.
- The corpus is synthetic (`name_bench.rs`); regenerate this table when
  the corpus definition changes, and keep the machine class in mind when
  comparing.
