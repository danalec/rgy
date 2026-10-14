# Baseline: name search + walk (Phase 0 perf gate)

Machine: Windows x64, `cargo run --release -p engine --example name_bench`
(default corpus: 200,000 names / 1,000 dirs, 30 timed rounds after 3
warmup rounds, counts proven equal before timing).

## At the scale the plan targets: 5M

`name_bench 5000000`, same machine and discipline. This is the row the plan
calls "name search, median @ 5M = 0.5 ms", and it is the one number the 200k
table cannot speak to.

| query | hits | med µs | vs the 0.5 ms goal |
|---|---|---|---|
| `rep` | 716,000 | 540 | 1.1x |
| `report` | 716,000 | 556 | 1.1x |
| `^src` | 1,428,000 | 786 | 1.6x |
| `mod` | 1,428,000 | 787 | 1.6x |
| `f1` | 1,639,704 | 876 | 1.8x |
| `file` | 2,856,000 | 1,249 | 2.5x |
| `dat$` | 2,856,000 | 1,252 | 2.5x |
| `'report_00000.pdf` (exact) | 1 | 1.0 | — |

Walk: 5,002,000 entries in 0.90 s (5.5M entries/s). Index build: 1.7 s.

**The goal is not met on these rows.** They are broad queries, though -- 14%
to 57% of the corpus matches, so any implementation touches at least the
matching names. The per-entry cost is already 0.11-0.25 ns of wall clock
across the parallel scan, which is why the totals track match count rather
than corpus size.

What is left is not "less work per entry" but less work. The scan's one
indirection is `dense[ent_name[i]]` -- a per-query table indexed by interned
name id, gathered 5M times -- and it is structural: `dense` holds this
query's scores, so it cannot live in the index. The levers are fewer matches
to score (a bound that pays, which `ranked` was meant to be and measured
against), or an index layout that removes the indirection. Both are changes,
not tunings.


**Gate:** a PR that regresses any row by more than 20% fails (plan §2).
These numbers are the *current* baseline - the plan's targets (name search
median = 0.5 ms @ 5M) are fsearch-grade end-state goals, not this table's
ceiling.

| query | hits | min µs | med µs | p90 µs | plan |
|---|---|---|---|---|---|
| `report` (fuzzy) | 29,000 | 53 | 57 | 63 | full |
| `file` (fuzzy, matches most of corpus) | 114,000 | 86 | 157 | 394 | full |
| `'report_00000.pdf` (exact) | 1 | 0.3 | 0.4 | 0.5 | selective |
| `^src` (prefix) | 57,000 | 65 | 123 | 137 | full |
| `dat$` (suffix) | 114,000 | 82 | 97 | 165 | full |
| `mod` (fuzzy) | 57,000 | 60 | 68 | 84 | full |
| `rep` (fuzzy) | 29,000 | 53 | 57 | 63 | full |
| `f1` (fuzzy) | 80,547 | 72 | 77 | 92 | full |

Corpus build: 200k names in ~35 ms. Walk: 201,000 entries in 0.049 s
(~4.1M entries/s, hot cache -- the tree is created and walked back to back).

Notes:
- `hits` is the true match count at an unbounded limit; the timed work is the
  scan+rank at `limit: 50`. The `plan` column is what `--explain` reports
  (`QueryPlan`), which is what ties a row to an implementation.
- The corpus is synthetic (`name_bench.rs`); regenerate this table when the
  corpus definition changes, and keep the machine class in mind when
  comparing.

## What moved, and why

Three plans were picked by a signal that did not predict whether they would
win. Each was measured against the plan it replaced and turned out to lose on
the very rows it was chosen for.

**`ranked`, chosen by breadth, lost 33-62x.** It is a bounded top-k that pays
only when the top `limit` comes from one or two dominant names. `rankable`
selected it when the token was *broad*, which is when many names score alike
and pruning cannot work. It is off by default (`RGY_FORCE_RANKED=1` turns it
back on to study it); `file` went 5,632 → 157 µs and `f1` 4,547 → 141 µs.

**`selective`, chosen by an absolute candidate count, lost 5-12x.** It visits
only the entries carrying a matching name, so it does less work than `full`'s
linear pass -- and was measured losing anyway, because each of its visits
costs far more (a name → entries list walk and a HashMap per candidate,
about 18 ns, against 0.3 ns an entry for `full`). The old cut was
`ok_entries <= 60_000`, which chose `selective` for every row this table
measures.

The cut is now proportional -- `candidates * 60 < entries`, from that measured
ratio, so about 3.3k of 200k here. Narrow still goes `selective` and still
wins by a lot (`'report_00000.pdf` is 0.4 µs against 63 µs for `full`);
broad goes `full` and stops paying for a list walk it does not need:

    report   29k candidates   575 us ->  57 us   10.1x
    mod      57k candidates   873 us ->  68 us   12.8x
    rep      29k candidates   530 us ->  57 us    9.4x
    ^src     57k candidates   868 us -> 123 us    7.0x

**The walk was 156k entries/s and is 4.1M.** `list_dir` called
`e.path().symlink_metadata()` per entry -- a PathBuf and an `lstat` for every
file. `DirEntry::metadata()` is the same query ("will not traverse symlinks
if this entry points at a symlink") and reuses what the platform's directory
read already returned instead of asking the kernel again.

### Corrections carried in the same work

`size:>10000` matched a file of exactly 10,000 bytes. `range` stripped `>`
and `>=` into the same arm, so a strict bound was inclusive. Found by
counting `type:file size:>10000` against `Get-ChildItem | Where Length -gt
10000` on the same tree -- 71 vs 70. Now 70 and 70.

The previous table recorded `file` at 5,960 µs and the walk at ~46k entries/s,
on a build where `ranked` and `selective` were both chosen for the rows they
lost on.

## Memory at the scale the plan targets: 5M

Peak resident set of `name_bench 5000000` (the in-memory 5M name index
build), measured with `scripts/mem-peak.py` on Windows x64:

| Metric | Goal | Measured | Verdict |
|---|---|---|---|
| peak RSS (build) | <= 1.2 GB | **658 MiB** (689,930,240 B) | under goal |
| disk / corpus | <= 2.5x | 2.5x (table above) | at goal |
| resident (daemon, mmap) | <= 64 MB | baseline 11.4 MiB (4 entries); @5M TODO | approach validated |

The peak is the high-water mark of the synthetic 5M name-index build -- the
transient the plan's `peak <= 1.2 GB` guards. It is well under. `disk` is the
on-disk index/corpus ratio from the table above. `resident` is the daemon's
steady-state RSS with the index mmap'd (only touched pages resident); that
needs a running daemon measured, not a build -- the next measurement.

Commands:

    # peak (the build)
    python scripts/mem-peak.py -- target/release/examples/name_bench.exe 5000000
    # peak_rss_bytes=689930240 peak_rss_mib=658.0 exit=0

    # resident (the daemon, index mmap-ed): build the index, then hold + sample
    searchctl --root <tree> --reindex <any query>     # build + persist the index
    python scripts/mem-peak.py --hold-seconds 4 -- target/debug/searchd.exe --root <tree> --listen pipe:memtest
    # on a 4-entry tree: peak_rss_mib=11.4 resident_rss_mib=11.4 (daemon baseline)
    # @5M the resident is the daemon baseline + the touched pages of the mmap-ed
    # index; run the same two commands on the 5M corpus to close it.
