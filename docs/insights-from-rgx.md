# Insights carried over from rgx

rgx (`../rgx`) has already paid for the lessons below — measured on its own
corpora, documented in its `AGENTS.md` / `ROADMAP.md` / `src/mmap.rs`. rgy
inherits them so the same bill is not paid twice. (Its `rar-codec` rewrite
settled: this workspace's copies of the `rgx-*` crates are upstream's own,
transported verbatim, and `benchmark/winrar-oracle.md` records what the
WinRAR oracle verified.)

## Memory mapping

**A mapping is a promise about the file.** Truncating a mapped file turns a
read past the new EOF into `SIGBUS` (unix) or an access violation
(Windows) — not an error return — and `memmap2` checks nothing. rgx's
answer is `MappedFile::live_len`: clip every slice to the minimum of the
mapping length, any caller limit, and the file length *re-queried from the
same open handle*. rgy's `name_index::Blob` applies the load-time half
(header `total` verified against the file length — `map.len() < total`
rejects), and the residual post-load race is stated rather than claimed
away: on Windows `SetEndOfFile` on a mapped section is refused outright
(`ERROR_USER_MAPPED_FILE`, 1224), so the crash cannot happen against our
own mapping; on unix a shrink between load and read is the remaining
window.

**`Mmap::map` can fail, and a failure is not "no index".** Windows sharing
and commit limits, empty files, and network paths all reject a mapping.
`Blob::from_file` falls back to reading the bytes into an owned buffer —
both the name index (`Index::load`) and content segments use it. The
build side is symmetric: `BlobMut::anon` falls back to a Vec instead of
panicking a 5M-name build under commit pressure.

**On Windows a mapping measured 1.51× SLOWER than `read()` for scans**
(rgx ROADMAP §3.3: 178.04 ms mmap vs 117.71 ms read on a 246 MB file;
zero overlap; the ~12% figure in its code understates). The TV pack
searcher's `ParallelBuffered` mode beat mmap by 44% and is the default
there. rgy's positions by access shape:

| workload | shape | choice |
|---|---|---|
| name index (bitmaps, per-entry tables) | random access, page-in on demand | mmap first, owned fallback |
| TV packs (`--tv-dir`) | sequential pack scans | `ReadMode::ParallelBuffered` (the measured winner) |
| content segments | postings + doc slices | mmap first, owned fallback |
| query-time file verify (`content::verify`) | one-shot reads | plain reads (never mapped) |

**Do not ship a threshold constant nothing calls.** rgx's
`AUTO_MMAP_THRESHOLD` carries a measurement table and has no call site —
"a constant with a measurement table and no call site reads like evidence
for a behaviour that does not happen". rgy has no promotion threshold:
mapping is chosen by access shape, not by size.

**Big allocations** (index builds, pack segments) use an anonymous map so
the bytes return to the OS on drop (`BlobMut::anon`) — fsearch's big-block
rule; the Vec fallback covers map refusal.

## CLI and `--serve` contract

**Exit codes are rg's: 0 match, 1 no match, 2 error.** No fourth state.
Scripts and shims that invent one break drop-in use. searchd's `--serve`
front already reports these three in its end sentinel (`status` 0/1/2).

**`--serve` is a line protocol.** One JSON argv-array per line on stdin;
stdout is framed by per-process sentinels: `{"rgx":"ready","tag":…,"pid":N}`
once at start, then per query `{"rgx":"begin","seq":N,"tag":…}` / rg `--json`
match lines / `{"rgx":"end","seq":N,"tag":…,"status":0|1|2,"matched":bool[,
"error":…]}`. `tag` is per-process (rgx derives it from nanos ^ pid<<32),
`seq` increments per query. The reader is the in-house `rgx_core::djson`, not
`serde_json`. In rgx the **whole grammar is accepted per query**
(`Args::try_parse_from`) and the adapter registry + page cache survive
between queries — that is what makes a daemon worth running.

**searchd's `--serve` shim keeps the shape and drops the argv.**
`serve_pattern` recovers only `-F` and `-e`/the first positional; every
other token is silently discarded (`a if a.starts_with('-') => {}`). So
`-i`, `-n`, `-A/-B/-C`, `--sort`, `--glob`, type filters, path positionals
(no per-query scoping — it always greps `--root`), `--files`, `-l`, `-c`
and the rest do nothing, with no warning. It also answers content grep
only (plain engine or TV store): no adapter registry, so a PDF/archive/
Office hit rgx would return is invisible. Sentinels, `seq`/`tag`/`status`
and the match-line shape are right; the silent flag subset is the trap.
Widen it to the grammar, or make the end sentinel say what was dropped.

## Interactive UI

**The meter measures the typing loop, not a bench cell.** `search-ui` is
per-keystroke whole-disk name search: every character is a full
`name_query::Searcher` pass (fzf-v1 + typos + top-k + NameCache) over the
live index. A smoothed mean hides exactly the shape the sparkline exists to
show.

**The cold first query is an outlier — keep it out of the rolling window.**
Index page-in, first NameCache fill, and warm-up all land on sample 1. Fold
it into the median and one open stains the next N keystrokes; drop it
silently and the chart lies about the only sample a user actually waited
on. Report it separately as `first`, keep it visible, keep it out of the
percentiles.

**p50/p90 are the honest numbers; "average latency" is not.** Nearest-rank
percentiles over the warm samples: p50 for the feel, p90 for the tail the
keystroke still pays. `candidates-skipped` and the rest of the diagnostics
come later; the meter contract is `first` + p50 + p90 from day one — a UI
that cannot state its own median cannot be tuned.

## Walk traps

Three from `benchmark/findings.md`, all priced on the `crates/` row.

**Ignore cost is the rule-set COMPILE, not the match.** The walk is 85.7%
of a 12.43 ms run; `--no-ignore` takes it to 3.54 ms, so 7.01 ms is
`.gitignore` matching against 0.88 ms of read + engine. The mechanism is
~58 µs per pattern, once per `.gitignore` — holding patterns at 65 and
moving entries 1→1024 moves 4.55 → 7.86 ms, where a per-(entry × pattern)
cost would move 16×. It is gated on `.git` presence: a tree without one
has its `.gitignore` compiled and never consulted.

**One unshaped line forfeits the whole file's fast path.** The fast-shape
builder demanded *every* file glob look like `*.ext` or `pre*`; this
repository's own ignore file is 17 of 35, and one anchored line cost all
35 the fast path. Fixing the shape check took the `crates` walk 10.73 →
6.09 ms. Gate the fast path per rule, not per file.

**`--sort path` must order at the source.** It originally sorted `--files`
while search hits stayed in walk order (caught by a flaky equivalence
test); the first fix then buffered every matched file at the printer —
unbounded on a pattern matching a whole tree. The shipped shape is
`Builder::ordered` in the walker, `--sortr path` walks backwards, the
printer streams one message at a time: peak working set flat across 16×
the matched files (6432 / 6480 / 6716 KiB at 500 / 2000 / 8000). rgy's
name-index rows are already path-ordered (`index.rs`), so the listing side
never needs that buffer; the trap returns the moment an rg-compatible
content walk sorts at a sink.

## Vendoring

**rgx never copies a file from any upstream.** That is rgx's rule for
*its own* trees: they are independent reimplementations, written from the
upstream source or the specification for the API contract and the
per-platform algorithm. Every vendored file carries a `//!` provenance
header naming the upstream and the commit it was based on.

**rgy is the other way round, and deliberately.** rgy does not reimplement
rgx: it *is* rgx's engine and adapters, transported verbatim. `crates/rgx-*`
and `crates/rar-codec` are upstream rgx's crates, byte for byte, and
`scripts/sync-rgx-upstream.sh` is what puts them there. The licence follows
upstream's (Apache-2.0, with five files whose layout was transcribed from
a third party), so `COPYING` and `NOTICE` say so and `LICENSE-MIT-FSEARCH`
carries the notice of the one upstream whose code is carried here.

The earlier shape — rgy's copies of the rgx crates being a *curated
subset*, each file with a provenance header of its own — cost 210 upstream
commits of drift before it was noticed, and it was the drift that made the
provenance headers wrong rather than right: a header that names an
upstream is a claim that this file is ours, and it stopped being true for
every file upstream touched.

**Where rgy must differ from upstream, the difference is a patch.**
`scripts/rgx-local-patches/` holds one patch per divergence, applied on top
of the copy, and `scripts/apply-vendored-edits.*` derives each patched
file from upstream's current content plus its patch. Two patches are live:
an overflow in upstream's HFS+ encoder bitmap (`1u64 << 64`) and an
overflow in upstream's Firebird creation-date arithmetic
(`frac * 86_400` in a `u32`). Both are upstream's, both are invisible to
upstream's CI because it runs the suite in release mode where the overflow
checks are off, and both are debug-mode panics in rgy, whose CI runs the
suite the way it is meant to be run.

**`scripts/check-vendoring.*` is a four-rule gate**, exit 1 on violation:
(1) no upstream reference in any non-comment line of `Cargo.toml` /
`Cargo.lock` / `.cargo/config.toml`; (2) no `[patch]` in
`.cargo/config.toml`; (3) a provenance header on every vendored tree's root
module (`vendored from` or `independent reimplementation`); (4) no
unlisted crates.io dependency.

**`scripts/vendored-sources.tsv` is written by the sync, not by hand.** It
records the relationship that actually exists — these trees came from
those trees at that commit — and every pin in it is a commit, so
`scripts/check-drift.*` can ask "has the upstream moved past the pin?" and
get an answer. The old manifest listed, per file, the *specification* the
file was written from (`RFC 1950`, `FIPS-180-4`, and so on), and the drift
check could never verify a row like that: a specification version is not a
git commit, so every one of them read as drift and the check reported 81
of 115 rows broken, which says nothing at all. **A row pinned `-` is a
failure on purpose** — it says "no established sync point", and a manifest
that quietly omits a file is worse than one that admits it has not been
looked at, because the omission reads as "nothing to do here". Never
hand-write a commit to make the gate green.

## Gotchas

- **`--tv-build` is a boolean — no value.** The tree comes from the first
  positional (`rgx --tv-build .`, not `--tv-build .`); argv fixup re-binds
  positionals as paths in `--files`/`--tv-build` modes for the same reason.
- **`.rgxrc` is loaded before argv is parsed.** Search order:
  `$RGX_CONFIG_PATH` → `./.rgxrc` → `~/.rgxrc` → `~/.config/rgx/rgxrc`.
  Each non-empty non-comment line is prepended to argv (whitespace split,
  shell-style quoting), so **CLI flags win over the file**. A missing file
  is fine; an unreadable one warns. "Why does rgx ignore my flag" starts
  with "is there a `.rgxrc`?".
- **Open codec gap: RAR 7.x, PPMd *filters*, VM-program filters and
  encryption do not decode.** PPMd var-H *blocks* do (`ppmd7.rs` +
  `unpack3.rs`). An early adapter dropped a refused member with
  `if let Ok(bytes) = archive.extract(e)` — and a silently absent entry
  looks identical to a nonexistent one in a search result. The fix keeps
  refusals and names them (note member beside real content, listing
  fallback when nothing extracts); rgy's shared `rar.rs` already carries
  it. Still no decode-against-known-good PPMd test: `rar-codec` encodes
  store-only, so no PPMd-encoded RAR can be crafted here.
- **`--file` is not accepted**, matching rg. Path flags take `-` for stdin.
  Do not add it for symmetry.

## Extraction cache pricing

`benchmark/archive-extract-cache.md` (7950X3D, `RGXFIXTURE` over `crates/`,
min-of-N; 4.3 ms spawn floor measured and reported separately):

| cell | ms | note |
|---|---:|---|
| first run, no cache | 538 | the published `archive-extract` shape |
| first run, `--cache` (fills 163 outputs) | 1057 | **2.0× to fill** |
| cached run | 66 | **8.2×** vs no-cache; 16.1× vs filling |

- **Break-even is the third query**: `1057 + 66·(N−1) < 538·N` holds for
  N ≥ 3. Repeated sessions (`--serve`, gate loops) get a 66 ms query from
  run 3 on; a one-shot search pays 2× for nothing.
- **Counts are identical across cache cells (20/20/20).** The cache changes
  what the answer costs, never the answer. A cache that changes a count is
  a bug, not a trade.
- **Off by default is correct, not an oversight**: a wrong default serves
  stale bytes, and the fill is real. When rgy grows an extraction memo,
  keep it opt-in — and any table that varies adapter behaviour must keep
  `--cache` off on both sides or report both cells; mixing them compares
  different work.

## Other rgx lessons already reflected here

- One regex engine (`rgx-core`); the regex crate survives only for
  patterns rgx's parser refuses and content `regex:` Unicode queries.
- Harness rules: fixed corpora with digests, warmups, min-of-N, counts
  proven equal before timing (`engine/examples/name_bench`).
- mimalloc for the general heap on Windows (MSVCRT `HeapAlloc` serialises
  parallel walks); both bins set `#[global_allocator]`.
- Release profile follows rgx's (`lto = "fat"`, `codegen-units = 1`,
  `strip`); `pgo.sh` is fsearch's pattern trained on the name bench.

## Still open, named honestly

- ~~PGO validated end-to-end on Windows~~ — **done**: `pgo.ps1` drives the
  same two-pass shape as `pgo.sh` (instrumented `name_bench` trainer →
  `llvm-profdata` merge → profile-use release build); validated on
  Windows. Both scripts `cd` to their own directory first.
- ~~A Linux measurement of the mmap-vs-read ordering~~ — **done**:
  `benchmark/mmap-vs-read.md`, bench at
  `crates/name-core/examples/mmap_vs_read`. Linux reverses the Windows
  ordering (mmap 0.72× the read() cost cold, 0.63× hot-inclusive), and
  the resident-scan rows show the scan itself costs the same either way —
  the delta is the read() copy vs page faults. Windows first-touch cold
  stays unmeasured (needs a kernel driver to drop the standby list).
- `discovery-macos` is imports-only transport, unverified (no macOS
  target on the machine that built this workspace). It is the first row
  of the macOS limits in [STATUS.md](STATUS.md).
