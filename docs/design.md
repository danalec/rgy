# rgy — design

The decisions this workspace is built on, and the rules that keep it
honest. The code cites this file; when a comment says "§2.2", this is
what it means.

What is *not* here: the plan that produced these decisions (it landed
end to end — see [STATUS.md](STATUS.md)), and the work that is still
open (that is the last section of [STATUS.md](STATUS.md)).

## 1. The performance targets

Name-search metrics follow the fsearch plan, content metrics follow rgx's
rg-parity bars, and the merge adds the cross-layer ones. The corpus is
~5M entries for names — Chromium and kernel trees plus a fixture set of
containers for content.

| Metric | Target | Reference point |
|---|---|---|
| Name search, median @ 5M | **≤ 0.5 ms** (fsearch table: 0.13 ms) | fsearch |
| Typing a whole filename (all keystrokes) | **≤ 8 ms total** | fsearch 3.0 ms |
| Content query via index (trigram/pack), median | **≤ 5 ms** | fsearch 2.3 ms |
| Content query via index, p90 worst tree | **≤ 25 ms** | fsearch Chromium p90 2.3 ms |
| Raw content scan (rgx engine, big file) | **≥ rg parity** (rgx already +32…111% on engine rows) | rgx `benchmark/status.md` |
| Walker (`--files`-style enumeration) | **≥ rg parity** (rgx already +37.5%) | rgx |
| Mixed query (name filter + content), median | **≤ 10 ms** via indexes | new |
| Daemon warm query (name or content) | **≤ 8 ms** | fsearch CLI 3.3 ms |
| Live change reflected in name index | **≤ 200 ms** | fsearch ~0.1 s |
| First full index, 5M files | ≤ 60 s (Windows MFT faster; the Linux walk is the floor) | fsearch 49 s @ 8.3M |
| Peak memory, first index | **≤ 1.2 GB** | fsearch 0.9 GB |
| Resident name layer @ 5M | **≤ 64 MB** | fsearch 63 MB |
| Content index on disk | ≤ 2.5× corpus text (bloom + postings; packs hold the bytes once) | fsearch 2.4× |

**The perf gate is the number.** The bench harness blocks a PR on a >20%
regression in any gated row of
[`benchmark/baseline-names.md`](../benchmark/baseline-names.md). rgx's
harness rules carry over: thread-matched rounds, corpus sha256
fingerprints, min-of-N with warmups, and counts proven equal before
timing.

What a single run cannot give is added by
[`scripts/bench-names.py`](../scripts/bench-names.py): the worker width is
pinned, the measurement is several independent runs, the statistic is the
best of them, and the spread is recorded beside the number. The reason is
measured, not asserted — on this host the same code moved the `mod` row's
min between 65 µs and 116 µs across three runs at 32 workers, and the
single-run table did not reproduce in the commit that wrote it. A row whose
spread is wider than the threshold is undecidable, and the gate says so
rather than calling it either way.

## 2. The merge decisions

Three projects went into one workspace, and the decisions below are why
the result is one project and not three.

### 2.1 Name search: fsearch's engine wins, zen-rs's syntax wins

- fsearch's `index.rs` / `query.rs` are adopted wholesale: interned
  names, DFS range layout, `name_mask` + class bitmaps, fzf-v1 scoring,
  the typo regime, NEON plus a **new x86_64 SSE2 wide scorer** (fsearch
  is NEON-only; Windows and Linux are x86-first), top-k with a score
  floor, NameCache and pools.
- zen-rs's filter vocabulary sits on top of fsearch's token modes:
  `ext/dir/type` (including `type:link` and the media kinds), `size:`
  semantics (directories never match a size bound), `|` alternation,
  globs, a fail-closed unknown `type:` with a warning, and `re:` as a
  path regex through rgx-core (not a vendored copy — see §2.4).
- Persistence: the index blob is **v3** (names blob, rows, masks,
  bitmaps, meta, event id), mmap-loadable in ≤ 50 ms @ 5M. It is the one
  on-disk index, and its layout is the cross-language contract in
  [on-disk-formats.md](on-disk-formats.md).

### 2.2 Discovery: three backends behind one seam

`rgy_discovery::Source` (full enumeration as DFS directory blocks) and
`rgy_discovery::Watch` (change batches, or `Rescan` when the feed loses
history) are the seam; the platform crates are behind it.

- **Windows**: zen-rs's MFT streaming + USN, with its trap suite
  carried verbatim (FRN = `stream_offset / rec_size`, echoed-FRN
  rejection, `OVERLAPPED` reads, 44-byte USN input, resume-USN peel,
  record-size derivation) and the `zen-c` RE-AUDIT fixes folded in (M1
  cluster floor, M2 resident `ValueOffset`, D1 USN offsets, M3 extension
  records, D2 resume advance) — they are shared bugs.
- **macOS**: fsearch's `walk.rs` + `fsevents.rs`, including the
  `no_materialize` iCloud guard, gated-folder / Full Disk Access
  handling, and relist-on-history-loss. The guard now asks the file
  itself (`SF_DATALESS` in `st_flags`) before the open, so a dataless
  file is skipped with the reason, like the Windows one, instead of
  failing the read.
- **Linux**: a `getdents64` bulk walk (8 threads, `O_NOFOLLOW`, skip
  rules) plus recursive `inotify` (`IN_Q_OVERFLOW` → `Rescan`), and
  `statx` for the metadata. fanotify FID is a later hardening.
- **FreeBSD**: the same seam, with `getdirentries` and `d_type` behind
  it — see §4.

The pick is a speed decision, never a semantics one: each backend is
held against `scan_sequential` by the dispatch's differential test, and
the portable walk answers everywhere else and whenever a native backend
refuses.

### 2.3 Content: layer the indexes, don't choose

The two designs solve different problems and compose.

1. **Candidate selection (which files can match):** fsearch's trigram
   postings + per-file 5/7-gram bloom. This is a *retrieval filter*: it
   turns "scan the corpus" into "scan 0.1% of the corpus" for
   literal-ish patterns.
2. **Corpus storage (bytes to search):** rgx's **Tesla Valve pack
   store** — corpus bytes packed, mmap'd, incremental updates, atomic
   manifest, warm `MappedStore` pool for the daemon. The pack searcher
   never runs the adapter registry by default (deliberately); the daemon
   path always has the direct engine and the adapters beside it.
3. **Token queries (`foo AND bar` docs):** rgx's token inverted index
   stays the specialised path it already is.
4. **Per-file confirmation:** the rgx engine (literal/hybrid/fixed/VM +
   the SIMD nibble prefilter) — strictly better than fsearch's verify
   path (memchr/regex), so verification moves to rgx. fsearch's
   "verify-on-disk, best-first in batches" scheduling survives as the
   policy around it.
5. **Containers:** the rgx adapters apply to *direct* search and to
   `--tv-pre-extract` pack builds. A trigram index over extracted text
   (rather than raw bytes) is a later experiment — extraction changes
   what a trigram means, and the pack path already covers recurrent
   container queries.

`sym:` (definitions) becomes a small posting table built by the same
content worker that builds the trigrams, not a regex pass at query time.

### 2.4 One regex engine, one daemon, one allocator

- **The vendored regex copies are gone.** What was a copy is now a
  dependency on `rgx-core` — the same lineage it was copied from. The
  byte-regex crate stays only where the Unicode mode is needed for
  `regex:` content queries; the path `re:` uses rgx-core.
- **One protocol:** the JSON-lines op model (`{"q", "limit"}`,
  `{"op":"grep"|"status"|"…"}`) is `rgy-proto`, spoken over a named pipe
  (Windows), a UDS (Unix) and stdio. rgx's `--serve` (an argv-array per
  line) stays as a **compatibility front** that translates to the same
  engine calls, so rg harnesses keep working; rgx's `--json` output stays
  as it is.
- **One escaper.** A JSON-lines protocol is a framing: one response per
  line. RFC 8259 §7 says the quotation mark, the reverse solidus and
  every C0 control (U+0000–U+001F) MUST be escaped, and a C0 control
  left raw is a line break inside the frame — a grep line out of a TSV
  file is enough to break it. `rgy_proto::escape_json_string` is the one
  escaper in this repository's crates: `rgy-proto`, `searchd` and `searchctl`
  all call it, and the reader in `rgy-proto` knows every escape the RFC
  defines and refuses the ones it does not. `crates/rgx-core` has its own
  (`djson::escape_json_string`), which is upstream's; a test in `searchd`
  holds the two to the same bytes.
- **One allocator story:** mimalloc for the general heap (rgx measured
  5–12% on 32-worker walks) + fsearch's big-block rule (≥ 1 MiB →
  mmap/VirtualAlloc, returned on free) for index builds and pack segments
  + `release_memory` after the big phases.
- **Latency:** the keep-warm spinner (a 250 ms window, cancelled by the
  search itself) wraps the daemon, and the QoS/priority pools come
  through the platform seam.

### 2.5 The tree without `std`, and the parallelism written by hand

- **The whole library tree is `no_std`.** Every crate this repository owns
  — the name engine, the seam, the protocol, the selector, the engine and
  the four platform backends — declares `#![no_std]` and is `alloc` and
  nothing else. What genuinely needs a filesystem — the content grep, the
  persisted index, the mmap, the threads, the keep-warm spinner, the
  console, the pack store — is behind the `std` feature, and the `no_std`
  build does not have it. The CI builds that configuration for
  `x86_64-unknown-none`: a bare-metal target with no operating system at
  all, so nothing in the chain can quietly reach for a file, a thread or a
  clock, because there is nothing there to reach for.
- **The platform calls are written by hand, and they are written once.**
  The seam is where a platform call lives: `rgy_discovery::dir` (list a
  folder, stat a path), `rgy_discovery::file` (read a file, write one,
  resolve a path) and `rgy_discovery::path` (the bytes, the joining, and
  the Windows spelling). The portable walk, the live overlay's `lstat`, the
  scope filter's path resolution and the macOS backend's root all go
  through them, so there is one implementation of each and no `std` behind
  it. A call that needs the operating system is a call the tree declares:
  `clock_gettime(2)` in `rgy_name_core::clock`, `GetSystemTimeAsFileTime`
  on Windows, `realpath(3)` and `open(2)` in the seam.
- **A path is bytes.** `std::path` does not exist in `no_std`, and the
  tree does not miss it: the seam's root is a `Vec<u8>`, the platform
  backends take the same shape, and `rgy_discovery::path` owns the joining
  and the C strings. The separator is `/` everywhere, and the bytes are the
  bytes the filesystem gave — a Unix path is not UTF-8, and the tree does
  not pretend otherwise.
- **The smaller Cortex-M targets would do the same job**, except that
  rgx-core keeps a 64-bit atomic in its instruction cache and those
  cores do not have one. That is a limit of the upstream tree, not of
  this one, and the upstream tree is not edited by hand.
- **The parallelism is written here, not borrowed.** rayon needs `std`,
  and its thread pool is the one thing a `no_std` build cannot have.
  `crates/par` (`rgy-par`) is the replacement: `no_std`, no dependencies,
  and only the primitives the tree actually uses — map and collect, the
  per-worker split of a slice, the k-way merge sort, and the two
  synchronisation primitives a `no_std` tree needs (a lock and a cell
  that spin, because there is nothing to park on). Under `std` the chunks
  run on threads and the lock is the OS one; without it they run in order,
  in the same order, so the two configurations agree on the answer.
- **The width belongs to the user.** `--threads` on `searchctl` and
  `searchd`, `RGY_NUM_THREADS` in the environment, and the default is
  four for the scan and the machine's cores for the query. The flag wins
  over the environment, and the environment over the default. Four for
  the scan is not timidity: walking a tree is disk work, and eight
  workers on one disk queue behind each other instead of reading any
  faster. The query is arithmetic, so it does want every core.
- **The macOS backend walked the tree with rayon and does not any more.**
  Its walk is a recursive descent that opens every child relative to its
  parent's descriptor, so the number of open descriptors follows the
  *depth* of the tree rather than its width. That shape is now a shared
  stack of work over `rgy-par`'s workers: the same `openat` trick, the
  same bound on the descriptors, and the same `no_std` answer — parallel
  with the `std` feature, one worker without it.
- **One crate in the tree is `std`, and it is not ours.**
  `crates/rgx-adapters-docs` is the upstream's, transported verbatim, and
  it spawns external programs (`soffice`, `tesseract`) — which is `std` by
  definition, and its own manifest says so.

### 2.6 UX surfaces

- `searchctl` (names): fsearch's one-shot ergonomics + zen-rs's filters
  + rg-compatible exit codes where they do not conflict; `--json` from
  rgx.
- `rgy-search-ui`: zen-rs's terminal UI (the sparkline stays), the name path
  **wired** to `rgy_name_core::name_query::Searcher` (fzf-v1 + typos + top-k
  + NameCache) + content grep. The latency meter reports `first` (the
  cold sample, kept out of the rolling window) and p50/p90 by
  nearest-rank.
- `searchd`: owns the indexes, the keep-warm, and the follower takeover.

### 2.7 The release: the upstream's platforms, and the CPUs it tunes for

The upstream rgx's release is the reference for what a release is here: one
portable build for every CPU, the tuned ones for the CPUs that can run
them, and every operating system the project ships for. rgy builds the same
set.

| | portable | AVX2 | Zen 4 (AVX-512) | arm64 |
|---|---|---|---|---|
| Windows | `x86_64-windows.zip` | `avx2-windows.zip` | `znver4-windows.zip` | `aarch64-windows.zip` |
| Linux (gnu) | `x86_64-linux.tar.gz` | `avx2-linux.tar.gz` | `znver4-linux.tar.gz` | `aarch64-linux.tar.gz` |
| Linux (musl) | `x86_64-linux-musl.tar.gz` | — | — | — |
| macOS | `aarch64-apple-darwin.tar.gz`, `x86_64-apple-darwin.tar.gz` | — | — | — |

Three rules about it:

- **The portable archives keep their names.** `install.sh` asks for
  `x86_64-linux.tar.gz` and `install.ps1` for `x86_64-windows.zip`, and the
  published 0.1.0 carries those names. A rename would point the installers
  at an archive that does not exist, so the tuned archives sit beside them
  instead of replacing them.
- **The installers install the portable one, by name.** The pin they carry
  is the portable archive's digest looked up by its exact name: with three
  Windows zips in a release, a match by suffix picks whichever comes first
  in the digests file, and that is the AVX2 one — an installer that
  downloads a binary tuned for a CPU it cannot assume.
- **The archive's name is a property of the target and the flavour, never
  of the machine that happened to run the build.** Two machines building
  the same target produce the same archive name.

The tuned builds are worth what the upstream measured for them (+1% on the
corpus, +2.5% on the big-file literal, +5% on the big-file regex) and not
one microsecond more: the vendored tree picks its byte kernels at *runtime*,
by the CPU probe in `rgx-core`'s own `detect.rs`, so the compile-time
target buys nothing on the path the name bench measures (187 µs against
184 µs on `^src`, which is noise). They are built because the upstream
builds them and a user on a Zen 4 should not have to wait for a runtime
dispatch to find out what the compiler already knew.

## 3. The semantic traps

These are the rules that break "parity" when they are ignored. Each one
names the parity row it belongs to.

1. **Case sensitivity (row 1.11)** — the same query means different
   things per filesystem. Rule: query strings carry smart-case; the index
   stores a folded mask plus the original; exact mode (`'`) matches the
   host OS's semantics, and the docs say so.
2. **Unicode normalization (row 1.12)** — macOS NFD against a typed NFC;
   Windows preserves case but collapses some legacy short names; Linux is
   raw bytes. Rule: every comparison runs on a fold of the path (ASCII
   case-fold + NFC); display and I/O keep the original bytes/UTF-16.
3. **Separator and prefix shapes (rows 1.13/1.14)** — `C:\`, `\\?\C:\`,
   `/`, and a trailing `\` or `/` must collapse to one internal identity.
4. **Link identity (rows 1.9/1.10)** — 8.3 names, hardlinks, firmlinks,
   junctions and symlinks are alias rows over one identity on their own
   OS; the merge must not pretend an NTFS junction equals a POSIX
   symlink in `type:`.
5. **Live-feed honesty (rows 1.2/1.3)** — "reflected ≤ 200 ms" is true on
   Windows and macOS; on Linux after a reboot it means "relisted", not
   "replayed". The API distinguishes the two in `Status`.
6. **Privilege surfaces (row 4.14)** — Admin on Windows (raw volume),
   Full Disk Access on macOS (gated folders), nothing on Linux (walk as
   the user). The daemon never prompts; it degrades to `skipped` lists.
7. **A FIFO must not block a scan.** Under the scan root, `open()` on a
   FIFO with no writer would block forever. The native backends carry
   `O_NONBLOCK` and `O_NOFOLLOW`, so the open fails fast instead
   (`ENXIO`), and a symlink's target is refused rather than followed.

## 4. The FreeBSD decisions

Decided 2026-10-13, and they still bind.

| # | Question | Decision | What it locks in |
|---|---|---|---|
| 1 | FreeBSD version floor | **Always the newest stable** — 15.1-RELEASE as of 2026-10 | `O_SEARCH` (13+) and the current `dirent` layouts are available; the CI/VM image pins the newest release and re-pins on each one |
| 2 | Does `zen-rs` run on FreeBSD? | **Deferred** — out of this port's "done" until tested | `zen-rs` is Windows-only by construction (MFT + USN); a `zen-rs` over the portable walk is a separate project |
| 3 | Is the native backend seam worth wiring? | **Yes — parity with the other OSes** | `source_for(root, threads)` selects windows/linux/macos/freebsd/portable by `cfg`; `rgy-discovery-freebsd` (getdirentries + `d_type`) is in scope |
| 4 | v1 acceptance bar | **The same bar as the other OSes** — the suite green on a real kernel | "It compiles" is not v1 |
| 5 | Where verification runs, and what performance means | **Correctness in a VM, performance on bare metal** | the suite may run in a VM (`vmactions/freebsd-vm`, a Gitea runner, or a local one); the latency/RSS bars in §1 are the FreeBSD bar, and a VM timing is not evidence for them |

## 5. What is already fine on FreeBSD

Recorded so it is not re-investigated:

- `searchd`'s `#[cfg(unix)] serve_uds` — Unix domain sockets work.
  `serve_pipe` is Windows-only and says so instead of misbehaving.
- `rgy_discovery::interrupt` — the `#[cfg(unix)]` arm is `signal(2)`, which
  FreeBSD has.
- `rgy_discovery::walk` — portable by construction; `scan_sequential` is the
  oracle and is platform-independent.
- `Blob`'s fallback path is exercised and correct, so the mmap backend
  is an optimisation, not a rescue.

## 6. The verification gates

- **Build before you test.** The e2e harness resolves the binaries by
  path, so `cargo test --workspace` on a clean tree races the rest of the
  workspace and spawns nothing. `cargo build --workspace` first. See
  [AGENTS.md](../AGENTS.md).
- **Both parents' oracles.** rgx: the differential oracle against the
  `regex` crate, the CLI differential against ripgrep, the adapter
  fixtures, the pack round-trips, the corpus sha256 discipline. fsearch:
  the typo suite, the scoring goldens, the bloom zero-false-negative
  tests, the MFT/USN trap regressions.
- **Merge-specific:** the query-semantic diff table (old zen-rs query →
  new unified query → the same result sets on a fixture volume image),
  the name-cache extension property tests, and the protocol golden files
  (`rgy-proto` request → reply) for the daemon, stdio and `--serve`.
- **Privileged and integration (opt-in):** the real-volume A/B
  (`ERS_USE_RECORDS`), the USN/FSEvents/inotify replay, the two-process
  daemon handoff, and the keep-warm behaviour under load (no starvation
  of the UI thread).
- **The parity matrix.** One test ID per row of
  [parity.md](parity.md); each runs on the CI matrix. A `G` cell fails
  CI until it is `P`/`N` or flipped to `D` with a linked issue and a note
  in the README. The behavioural corpus is one fixture tree (links, case
  twins, unicode names, deep paths, gated dirs, container files) checked
  into `docs/fixtures` and materialized per OS, with the expected results
  per OS beside it.
- **The status surface.** `searchctl status` prints the per-OS
  capability flags (journal replay: yes/no, QoS: native/approx,
  privilege: full/gated) so a "degraded" is visible at runtime and not
  only in the docs.
- **The vendoring rules and the drift, in two different CI files.** The
  vendoring rules ([`check-vendoring.*`](../scripts/check-vendoring.sh))
  need nothing but the tree, so they gate in
  [`.github/workflows/ci.yml`](../.github/workflows/ci.yml) on all three
  operating systems — which exercises both twins, because each OS runs
  its native one. The drift check
  ([`check-drift.*`](../scripts/check-drift.sh)) needs an upstream
  *checkout*, and the upstream is a private repository on the forgejo, on
  the LAN: `github.com/danalec/rgx` does not exist, and a runner that
  cannot reach it has no answer to give. So the drift check lives in
  [`.gitea/workflows/drift.yml`](../.gitea/workflows/drift.yml), on a
  self-hosted runner on the LAN. Gitea reads `.gitea/workflows/` and
  GitHub reads `.github/workflows/`, so each check gates where it can
  and neither pretends to gate where it cannot.
