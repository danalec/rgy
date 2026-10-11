# Insights carried over from rgx

rgx (`../rgx`) has already paid for the lessons below — measured on its own
corpora, documented in its `AGENTS.md` / `ROADMAP.md` / `src/mmap.rs`. rgy
inherits them so the same bill is not paid twice. (Its `rar-codec` is under
active development there: this workspace's copy is verbatim and should be
re-synced from `../rgx/crates/rar-codec` when that settles.)

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

## Other rgx lessons already reflected here

- One regex engine (`rgx-core`); the regex crate survives only for
  patterns rgx's parser refuses and content `regex:` Unicode queries.
- Harness rules: fixed corpora with digests, warmups, min-of-N, counts
  proven equal before timing (`engine/examples/name_bench`).
- mimalloc for the general heap on Windows (MSVCRT `HeapAlloc` serialises
  parallel walks); both bins set `#[global_allocator]`.
- Release profile follows rgx's (`lto = "fat"`, `codegen-units = 1`,
  `strip`); `pgo.sh` is fsearch's pattern trained on the name bench.

## Still open (Phase 6 polish, named honestly)

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
  target on the machine that built this workspace).
