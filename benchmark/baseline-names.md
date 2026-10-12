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
