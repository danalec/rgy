# Baseline: name search + walk (Phase 0 perf gate)

## How this table is measured

Host: AMD Ryzen 9 7950X3D (16 cores / 32 logical), Windows 11 Pro N
10.0.26300. Measured on 2026-10-24, from the commit the rank key lands
on — the code before it. The Linux rows below were taken by the same
recipe, on the Linux host, from the same commit.

    python scripts/bench-names.py measure --runs 7 --threads 4 \
        --names 200000 --names 5000000 --gate 200000 \
        --out benchmark/baseline-names.csv

It is the same code the previous recording took, and the machine measured
it slower the second time: the `dat$` and `file` rows came out at 174 µs
and 175 µs on the 23rd and at 188 µs and 185 µs on the 24th, for a
difference in the machine's state and not in the code. A reference taken
while the machine was in another state is not a reference, so it was taken
again from the same commit, and the gate reads a change against that.

The bench under it is `cargo run --release -p rgy-engine --example name_bench`:
a fixed synthetic corpus, 30 timed rounds after 3 warmup rounds, and the
match count proven equal before the timing starts (a fast wrong answer is a
failure). What the script adds is the part a single run cannot give:

- **A fixed worker width.** The search is a parallel scan, so the width of
  the pool is part of what is measured. It is pinned to 4 threads
  (`RAYON_NUM_THREADS`) and recorded in the baseline; a comparison against a
  baseline taken at another width is refused, not reported.
- **K independent runs, and the best of them.** Seven runs of the bench,
  each with its own min-of-30. The statistic the gate compares is the
  **best** of the seven: noise only ever makes a run slower, so the best run
  is the one closest to the code's real cost.
- **The spread, beside the number.** The spread is the dispersion of the
  per-run minima, relative to the best of them. It is the width of the noise
  floor the gate has to be read against, and it is in the table for that
  reason.

**A row whose spread is wider than the 20% threshold cannot decide the
gate.** Noise and a regression look the same from here, so the script says
`UNDECIDABLE` and exits 2 rather than calling either. That is the state the
previous table was in the whole time, silently.

Why 4 threads and not the machine's 32: on this host, with the other work
that runs on it, the `mod` row measured over five runs of the same code at
200k names:

    4 threads    102.1 108.6 108.5 107.2 106.1 us    spread  6.4%
    8 threads     81.2  70.1  74.0  71.3  76.9 us    spread 15.8%
   16 threads     60.5  56.6  83.1  83.4  77.5 us    spread 47.3%
   32 threads     62.8 117.6 105.3 117.3  68.1 us    spread 87.3%

The wide pool is faster and unmeasurable: at 32 workers the spread is 87.3%,
more than four times the threshold the gate applies, so no comparison
against it decides anything. At 4 workers the same code spreads 6.4% -- under
the threshold -- and a thread-matched measurement is the only kind two hosts,
or two machines, can be compared on at all.

The other end of the same knob is the one to reach for when the host is
busy. The same rows measured on a host with other work on it spread
22–42% at four workers -- wider than the threshold, so the comparison is
UNDECIDABLE -- and 1–8% at one worker, where it decides. A narrow pool is
slower and measurable, and an A/B that has to be believed is taken there:
the `RankAdj` change (the ranking adjustment travels with the entry) was
measured at one worker, nine runs, best of nine, and every row came out
3.4–4.6% faster. At four workers on a busy host the same change is inside
the noise, and the honest report is that it is.

The cost of that choice is in the numbers: the gate's rows are the 4-thread
ones, which are slower than what the product pays on an idle 32-thread box.
The gate is a regression detector, not a latency claim.

## The gate's tables

A baseline is a machine as well as a number. The same Rust is not the same
machine code on Linux and on Windows — the code generator is the OS's — and
the measurement is a memory-bound one, so the CPU's memory system is in it
too. A baseline recorded on one host cannot rule on another, so each table
below is one host, the rows carry the host they were taken on, and the gate
compares against the rows of the host it runs on. The host is the OS and the
CPU, normalized: `windows-amd-ryzen-9-7950x3d-16-core-processor` and
`linux-amd-ryzen-7-255`. A gate run on a machine the baseline has no rows
for says so and exits 3, rather than comparing two machines.

### Windows: AMD Ryzen 9 7950X3D, Windows 11 Pro

<!-- BEGIN: bench-names-table host=windows-amd-ryzen-9-7950x3d-16-core-processor -->

Corpus: 200,000 names, 4 threads, best of 7 runs.

| query | hits | min µs | med µs | p90 µs | spread | plan |
|---|---|---|---|---|---|---|
| `'report_00000.pdf` | 1 | 0 | 0 | 0 | — | selective |
| `^src` | 57,000 | 110 | 134 | 158 | 16% | full |
| `dat$` | 114,000 | 188 | 212 | 234 | 5% | full |
| `f1` | 80,547 | 161 | 173 | 195 | 4% | full |
| `file` | 114,000 | 185 | 212 | 236 | 7% | full |
| `mod` | 57,000 | 110 | 136 | 167 | 13% | full |
| `rep` | 29,000 | 78 | 99 | 127 | 14% | full |
| `report` | 29,000 | 72 | 96 | 124 | 13% | full |

Corpus: 5,000,000 names, 4 threads, best of 7 runs.

| query | hits | min µs | med µs | p90 µs | spread | plan |
|---|---|---|---|---|---|---|
| `'report_00000.pdf` | 1 | 1 | 1 | 1 | — | selective |
| `^src` | 1,428,000 | 2501 | 2624 | 3158 | 2% | full |
| `dat$` | 2,856,000 | 4371 | 4553 | 5551 | 1% | full |
| `f1` | 1,639,704 | 2876 | 2980 | 3405 | 2% | full |
| `file` | 2,856,000 | 4341 | 4574 | 5496 | 3% | full |
| `mod` | 1,428,000 | 2509 | 2616 | 3252 | 2% | full |
| `rep` | 716,000 | 1582 | 1642 | 2020 | 1% | full |
| `report` | 716,000 | 1564 | 1682 | 2060 | 3% | full |

<!-- END: bench-names-table host=windows-amd-ryzen-9-7950x3d-16-core-processor -->

### Linux: AMD Ryzen 7 255, CachyOS

<!-- BEGIN: bench-names-table host=linux-amd-ryzen-7-255 -->

Corpus: 200,000 names, 4 threads, best of 7 runs.

| query | hits | min µs | med µs | p90 µs | spread | plan |
|---|---|---|---|---|---|---|
| `'report_00000.pdf` | 1 | 0 | 0 | 0 | — | selective |
| `^src` | 57,000 | 108 | 141 | 191 | 10% | full |
| `dat$` | 114,000 | 174 | 208 | 278 | 5% | full |
| `f1` | 80,547 | 143 | 166 | 240 | 5% | full |
| `file` | 114,000 | 170 | 212 | 304 | 8% | full |
| `mod` | 57,000 | 108 | 139 | 186 | 2% | full |
| `rep` | 29,000 | 77 | 97 | 112 | 16% | full |
| `report` | 29,000 | 76 | 96 | 131 | 10% | full |

Corpus: 5,000,000 names, 4 threads, best of 7 runs.

| query | hits | min µs | med µs | p90 µs | spread | plan |
|---|---|---|---|---|---|---|
| `'report_00000.pdf` | 1 | 1 | 1 | 1 | — | selective |
| `^src` | 1,428,000 | 2637 | 2904 | 3191 | 5% | full |
| `dat$` | 2,856,000 | 4250 | 4469 | 4798 | 6% | full |
| `f1` | 1,639,704 | 2929 | 3062 | 3237 | 4% | full |
| `file` | 2,856,000 | 4258 | 4636 | 5209 | 5% | full |
| `mod` | 1,428,000 | 2643 | 2809 | 3322 | 4% | full |
| `rep` | 716,000 | 1778 | 1922 | 2190 | 13% | full |
| `report` | 716,000 | 1770 | 1949 | 2251 | 4% | full |

<!-- END: bench-names-table host=linux-amd-ryzen-7-255 -->

**On the Windows host the gate rules on the 200k rows; the 5M rows are
measured and reported, not gated.** The 5M measurement does not reproduce on
this host: the spread across the seven runs of the same code reaches 52% on
`rep` and 39% on `^src`, and two sessions of identical code disagree by 28%
on `mod` — wider than the 20% the gate applies. A gate that fires on noise
is worse than no gate, so the 5M rows are recorded, printed and compared, and
the script says `REPORTED` instead of failing. The 200k rows, at a 3–8%
spread, are the ones that can carry a verdict, and they have held across four
sessions of the same code.

**On the Linux host the gate rules on both corpora.** Those rows come from
the same commit the Windows rows come from (`121532e`, "the priority is a
level, not a term added to the score"), so the two references are one code
measured on two machines. That host's noise moves between corpora and between
recordings — the same code measured four times put the 5M spread at 4–13% in
one recording and 72–193% in another, and the 200k rows at 2–16% in one and
88% in another — so each corpus is taken from the recording in which it
reproduced, with the spread recorded beside it: 2–16% at 200k, 4–13% at 5M,
both under the threshold the gate applies. The daemon ships on that host, so
it is the one that has to rule on the 5M corpus.

**The 5M rows are a number about a machine, not about the code.** The two
trees that the `no_std` rewrite put apart measure 3% to 6% apart on this
Windows host, at the 5M corpus — and they do the same work: the same
entries, the same filter, the same `score` calls, the same arrays read (the
hashes of `ent_name`, `kind`, `parent` and `dense` are identical) and the
same answers to every query. The scan loop compiles to 483 instructions on
Windows where the older tree compiled to 376, and no change to the source
recovers it there.

On Linux (CachyOS, AMD Ryzen 7 255) the same two trees come out the other
way: the `no_std` tree is 16% to 32% faster on the 200k corpus and 11% to
18% faster on the 5M one, at one worker and at four. The same work, the
same answers, a different code generator — and the daemon ships on the
second one. So a 5M number is read the way the
gate reads every number, and a change is judged by the A/B against the same
code on the same day, on the platform the change is for.

### The width the tree asks for by itself

The tables above are taken at a fixed width of four workers, because that
is the width this host's noise lets a comparison decide. The tree does not
run at four: it runs at the width the machine gives it, and there the
story is different — and worth writing down, because the number that came
out of it was wrong for a while.

**A core is a core.** This host has sixteen cores and two threads each.
The scan is a memory-bound pass over arrays, and the second thread of a
core shares the first one's load units, its cache and its place in the
memory system. The same scan, the same corpus, the best of seven runs, in
microseconds:

| workers | 1 | 2 | 4 | 8 | 16 | 32 |
|---|---|---|---|---|---|---|
| 200,000 names | 374 | 190 | 98 | 54 | 55 | 81 |

| workers | 1 | 4 | 8 | 16 | 32 |
|---|---|---|---|---|---|
| 5,000,000 names | 9704 | 3290 | 1324 | 708 | 691 |

Sixteen workers is the machine's best at both corpora. Thirty-two is 47%
worse at 200k and 2% better at 5M. The gate's four workers are not the
tree's width, and the tree's width is not the machine's processor count.

**The tree that came before the pool measured 61 µs at thirty-two workers,
and the reason was a race.** Its worker chain handed out the work from a
shared counter, and a helper that found the counter empty went home
without working: of the thirty-two workers it asked for, fifteen to
seventeen did the job. That was counted, not inferred — the counter sat in
the closure and the tree printed it. The pool's workers all engage:
thirty-two of thirty-two. The old tree's number was the number of sixteen
workers with thirty-two asked for; the new tree's number is the number of
thirty-two workers. The new tree is the honest one, and honest was 32%
slower at 200k.

So the width the tree asks for by itself is the machine's **cores**:
`available_parallelism()` divided by the number of processors a core
carries, which the platform says — `GetLogicalProcessorInformationEx` on
Windows, the sibling list in sysfs on Linux, `hw.physicalcpu` on macOS. A
caller that wants a width of its own still gets it: `RGY_NUM_THREADS` and
`rgy_par::set_workers` are the user's, and the gate's four workers are the
gate's. The width does not cost the index build anything either: the 5M
build measured 1964 ms with sixteen workers against 2003 with thirty-two.

With that, the two trees as they ship — each at its own width, the same
machine, the same day, interleaved, the best of seven:

| corpus | rows | new/old |
|---|---|---|
| 200,000 | all eight | 0.70 – 0.93 |
| 5,000,000 | all eight | 0.87 – 1.05 |

The 5M rows that come out above one are inside the noise this host has at
that corpus (its spread is 4–13% there); the rows that come out below one
are outside it.

Notes:
- `hits` is the true match count at an unbounded limit; the timed work is
  the scan+rank at `limit: 50`. The `plan` column is what `--explain`
  reports (`QueryPlan`), which is what ties a row to an implementation.
- `min µs` is the best of the seven runs. `med µs` and `p90 µs` are the
  medians across the seven runs of the bench's own median and p90. `spread`
  is the dispersion of the per-run minima.
- A spread of "—" is a row below 10 µs, where the percentage is the timer's
  resolution talking rather than the code's cost. The gate does not rule on
  rows that small.
- The corpus is synthetic (`name_bench.rs`); regenerate this table when the
  corpus definition changes, and keep the machine class in mind when
  comparing.
- `host` is the machine the row was taken on — the OS and the CPU,
  normalized. `bench-names.py check` compares against the rows of the host it
  runs on, and refuses (exit 3) a baseline that has no rows for it; `--host`
  names the host when a machine has more than one name.
- The table is generated from `benchmark/baseline-names.csv`. The
  CSV is the source of truth; `bench-names.py render --check` fails when the
  two have drifted.

**Gate:** a PR that regresses any row by more than 20% fails.

    python scripts/bench-names.py check --baseline benchmark/baseline-names.csv --runs 7

Exit codes: 0 pass, 1 regression, 2 undecidable (the noise floor is wider
than the threshold), 3 not comparable (a different worker width, a row that is
not measured at all, or a host the baseline has no rows for).

## The wide pool, as it was measured before

The rows below are the previous table, kept because the write-up below
quotes them. They were measured with the machine's own worker pool — 32
threads on this host — and they do not reproduce: three runs of the same
code moved the `mod` row's min between 65 µs and 116 µs, and the commit
that wrote them did not reproduce them either. They are a record of a
measurement, not a baseline.

### At the scale the plan targets: 5M

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

### The 200k rows, as they were measured before

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

The `min` column reproduces and the `med` column does not: on the same code,
`mod` measured a min of 63 µs against the 60 recorded, and a median of 130 µs
against the 68 recorded. The median of a noisy run is the number that moves;
the minimum is the one that survives. That is the whole reason the gate
compares minima and why it takes several runs to get one.

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
index is persisted through `rgy_name_core::persist` — the root sidecar, and
the root's digest in the index's own header, so `searchd` will trust it — and
the run stops before the walk fixture, which writes one file per corpus
entry. `exit=1` in hold mode is mem-peak killing the daemon on schedule.

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
