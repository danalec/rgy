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

Measured with `scripts/mem-peak.py` on Windows x64 (release builds): the
synthetic 5M corpus of `name_bench`, and the existing `crates/` tree (698
entries, nothing generated) as the daemon's fixed cost.

| Metric | Goal | Measured | Verdict |
|---|---|---|---|
| peak RSS (build + persist) | <= 1.2 GB | **593.6 MiB** (622,473,216 B) | under goal |
| peak RSS (full harness run) | <= 1.2 GB | 658 MiB (689,930,240 B) | under goal |
| disk / corpus | <= 2.5x | 2.5x (table above) | at goal |
| resident, daemon @ 5M (mmap) | <= 64 MB | **147.5 MiB** (154,636,288 B) | **over goal, 2.3x** |
| resident, daemon fixed cost (release) | — | 10.2 MiB (10,682,368 B) @ 698 entries | — |

The resident row is the daemon's steady-state RSS with the 5M index mmap'd
(`searchd` printing `reusing index`, no rebuild), held idle and sampled
twice: 154,636,288 B and 154,652,672 B. "Loaded" here already includes the
working set a first query needs: `Live::new` calls `Index::prefault`, which
faults the arrays every query scans (name bitmaps, name offsets, names,
entry->name, kind, parent) and builds the memo tables the scorer consults.
Idle beyond that touches nothing. The earlier debug-build baseline of the
same recipe read 11.4-11.6 MiB; release is 10.2 MiB.

**The goal is not met.** The cost scales with the corpus at ~29 B/entry
(10.2 MiB fixed + 137 MiB over 5,002,001 entries; the same recipe on a
201,001-entry index reads 16.2 MiB, consistent to ~10%). The goal is
12.8 B/entry all-in at 5M. Two notes on what the number is and is not:

- The synthetic corpus gives every entry a distinct name, so the per-name
  half of the index (names, name offsets, name info, name bitmaps) sits at
  its worst case. The index interns names -- its design note is "7.5M
  entries share ~2M distinct names" -- so a real corpus measures lower.
  Direction only: no real-corpus number is claimed here.
- The per-entry half (entry->name, kind, parent, name->entries) does not
  shrink with interning. Getting under 64 MB at 5M is layout work -- a more
  compact per-entry table -- not a policy tweak. Not prefaulting moves the
  same pages into the first broad query and trades this row for the
  typing-latency row instead of paying less.

The persisted blob itself is 355,225,344 B (71 B/entry on disk) for
5,002,001 entries.

Commands:

    # the artifact: build + persist the synthetic 5M index (no corpus on disk)
    python scripts/mem-peak.py -- target/release/examples/name_bench.exe 5000000 \
        --save-index %TEMP%/rgy-5m.idx --root %TEMP%/rgy-5m-root
    # name_bench: saved ...\rgy-5m.idx (5002001 entries) for root ...\rgy-5m-root
    # peak_rss_bytes=622473216 peak_rss_mib=593.6 exit=0

    # the daemon's fixed cost, over the existing crates/ tree
    python scripts/mem-peak.py --hold-seconds 4 -- target/release/searchd.exe \
        --root crates --listen pipe:memtest
    # searchd: reusing index ...\rgy\6c8fe7269d389acd.idx (698 entries)
    # peak_rss_bytes=10682368 peak_rss_mib=10.2 resident_rss_mib=10.2 exit=1

    # resident @ 5M: the daemon mmap-ing the synthetic index
    python scripts/mem-peak.py --hold-seconds 4 -- target/release/searchd.exe \
        --root %TEMP%/rgy-5m-root --index %TEMP%/rgy-5m.idx --listen pipe:memtest
    # searchd: reusing index ...\rgy-5m.idx (5002001 entries)
    # peak_rss_bytes=154652672 peak_rss_mib=147.5 resident_rss_mib=147.5 exit=1

`name_bench --save-index <file> --root <dir>` exists for this recipe: the
index is persisted through `name_core::persist` (root sidecar included, so
`searchd` will trust it) and the run stops before the walk fixture, which
writes one file per corpus entry. `exit=1` in hold mode is mem-peak killing
the daemon on schedule.

### Item 3, measured: the indirection is not the lever

The full scan loads `dense[ent_name[i]]` per entry -- two dependent loads --
and item 3 asked which way to kill it: a per-entry `Short` array (~+11 MB
resident at 5M, which the row above cannot absorb) or walking names instead
of entries. Before restructuring anything, the per-entry array was built
and measured: compose `dense[ent_name[i]]` once per query, then scan that
view sequentially. Same session, release, `--no-walk`, medians in µs:

| query | fused gather (today) | per-entry array | |
|---|---|---|---|
| `rep` | 578 | 3,386 | 5.9x slower |
| `report` | 581 | 3,000 | 5.2x slower |
| `mod` | 817 | 3,823 | 4.7x slower |
| `^src` | 852 | 3,246 | 3.8x slower |
| `f1` | 950 | 3,709 | 3.9x slower |
| `dat$` | 1,338 | 3,729 | 2.8x slower |
| `file` | 1,353 | 3,744 | 2.8x slower |

(A parallel build was running during these rows, so all of them sit above
the checked-in table above; the comparison is back-to-back within one
session, and the gaps hold on `min_us` too. Hit counts match exactly.)

The gather is not slow because it is dependent -- it is nearly free because
the `score()` work between two gathers hides its latency. Paying it in a
separate pass instead costs that whole pass (~2.5 ms of gathers at 5M,
against 0.6-1.4 ms rows). So the array buys no latency and asks for ~20 MB
more resident (a `Short` is 4 bytes, not 2). Walking names removes the
gather but replaces it with name -> entries list walks -- the shape
`selective`'s measured 5-12x loss was made of. The indirection stays; the
5M rows are bound by match volume (14-57% of the corpus matches), and the
lever is fewer scored entries, not a different per-entry shape. On the tie
order the old trade-off note worried about: it cannot move -- `top_k` keys
are `(score, !idx)` and `search()` re-sorts by `(score, idx)`, so both
traversals answer identically.
