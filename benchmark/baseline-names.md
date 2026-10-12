# Baseline: name search + walk (Phase 0 perf gate)

Machine: Windows x64, `cargo run --release -p engine --example name_bench`
(default corpus: 200,000 names / 1,000 dirs, 30 timed rounds after 3
warmup rounds, counts proven equal before timing).

**Gate:** a PR that regresses any row by more than 20% fails (plan §2).
These numbers are the *current* baseline - the plan's targets (name search
median = 0.5 ms @ 5M) are fsearch-grade end-state goals, not this table's
ceiling.

| query | hits | min µs | med µs | p90 µs | plan |
|---|---|---|---|---|---|
| `report` (fuzzy) | 50 (top-k) | 456 | 575 | 877 | selective |
| `file` (fuzzy, matches most of corpus) | 50 (top-k) | 92 | 157 | 189 | full |
| `'report_00000.pdf` (exact) | 1 | 0.3 | 0.4 | 0.6 | selective |
| `^src` (prefix) | 50 | 829 | 868 | 1358 | selective |
| `dat$` (suffix) | 50 | 86 | 153 | 168 | full |
| `mod` (fuzzy) | 50 | 836 | 873 | 1353 | selective |
| `rep` (fuzzy) | 50 | 428 | 530 | 883 | selective |
| `f1` (fuzzy) | 50 | 123 | 141 | 160 | full |

Corpus build: 200k names in ~38 ms. Walk: 201,000 entries in 0.054 s
(~3.7M entries/s).

Notes:
- `hits` is capped by `Query`'s `limit: 50` top-k; the timed work is the
  scan+rank, not the result copy. The `plan` column is what `--explain`
  reports (`QueryPlan`), which is what pins a row to an implementation.
- The corpus is synthetic (`name_bench.rs`); regenerate this table when
  the corpus definition changes, and keep the machine class in mind when
  comparing.

## What moved, and why

Two rows used to be an order of magnitude worse and two numbers used to be
wrong. Both were found by measuring against something independent, not by
reading.

**`file` and `f1` were 5,632 µs and 4,547 µs.** They were the only rows the
`ranked` plan was chosen for, and it lost to the `full` plan it replaces:
62x and 33x slower, with identical match counts and results. `rankable`
selected it on *breadth* (`RANKED_MIN`), which is the wrong signal -- the
bounded top-k pays only when the top `limit` comes from one or two dominant
names, and fires worst exactly when many names score alike. It is off by
default (`RGY_FORCE_RANKED=1` turns it back on to study it); the
implementation and its tests are kept. On real trees it never produced a
result at all -- it bailed and `full` ran anyway.

**The walk was 156,293 entries/s and is 3,732,424.** `list_dir` called
`e.path().symlink_metadata()` per entry -- a PathBuf and an `lstat` for
every file. `DirEntry::metadata()` is the same query ("will not traverse
symlinks if this entry points at a symlink") and reuses what the
platform's directory read already returned instead of asking the kernel
again.

### Corrections carried in the same work

`size:>10000` matched a file of exactly 10,000 bytes. `range` stripped `>`
and `>=` into the same arm, so a strict bound was inclusive. Found by
counting `type:file size:>10000` against `Get-ChildItem | Where Length -gt
10000` on the same tree -- 71 vs 70. Now 70 and 70.

The previous baseline recorded the walk at ~46k entries/s and `file` at
5,960 µs; the intermediate table (walk 156k, `file` 5,632 µs on `ranked`)
predates this one and is superseded by it.
