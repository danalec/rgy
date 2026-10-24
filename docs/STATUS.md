# rgy — status

What is built, what is measured, and what is still unverified. The tables
are the merge's scoreboard: the [README](../README.md) is the map, and this
is the detail behind it.

The merge is landed end to end: name search,
content search, one daemon/CLI, and the polish layer. What is left is
verification on machines this host does not have.

### Name search

| Piece | Where | State |
|---|---|---|
| Name index v2 (intern, DFS, bitmaps, mmap blob) | `crates/name-core` (`name_index`) | ✅ from fsearch; the blob is `FSIDX013`, and the ranking adjustment each entry inherits from its directory lives in the entry (the `RankAdj` section) rather than one hop away in the directory table — 3.4–4.6% faster on every row of the name bench |
| Query syntax merge (fsearch tokens + zen-rs filters) | `crates/name-core` (`name_query`) | ✅ `type:`/`dir:`/`size:`/`re:`/globs/\|, fail-closed |
| fzf-v1 scorer + typos + top-k + NameCache + **SSE2 wide scorer** | `crates/name-core` (`name_query`) | ✅ parity-tested vs scalar |
| Live overlay | `crates/name-core` (`live`) | ✅ portable `lstat`; the base's root-relative paths and the overlay's openable ones are crossed, so a re-listed folder answers each file once. The daemon merges the overlay through `search_alternatives` and the CLI through `merge_hits`, so **every** live change is answered — the keyed-by-index merge it replaced dropped one change per file and made the CLI index the base with the overlay's `u32::MAX` sentinel (a panic); both paths are pinned by `every_live_change_is_answered` |
| Index persistence (sidecar-pinned root) | `crates/name-core` (`persist`) | ✅ |
| Path fold (ASCII case + Unicode NFC, UAX #15) | `crates/name-core` (`fold`, `nfc`) | ✅ tables generated from the UCD; 20,171 UCD conformance cases pass |

### Discovery

| Piece | Where | State |
|---|---|---|
| Seam (`Source`/`Watch`, DFS `Listing` blocks) | `crates/discovery` | ✅ portable walk |
| Walk selector (`source_for`, per-platform by `cfg`) | `crates/discovery-dispatch` | ✅ differential vs `scan_sequential`, green on Windows + Linux |
| Windows backend (MFT + USN, RE-audit fixes) | `crates/discovery-windows` | ✅ `MftSource` / `UsnWatch`; the path a USN record names is rebuilt from the MFT — the logic is pinned by tests, and the read against a real MFT needs a volume handle (an administrator) |
| Linux backend (getdents64 + inotify) | `crates/discovery-linux` | ✅ suite green on a real kernel |
| macOS backend (getattrlistbulk + FSEvents) | `crates/discovery-macos` | ⬜ transported, unverified — needs a real Mac |
| FreeBSD backend (getdirentries + `d_type`) | `crates/discovery-freebsd` | ✅ suite green on a FreeBSD 15.1 kernel (2026-10-14) |

> **The platform walks are selected, not stranded.** `source_for(root,
> threads)` picks the native backend per platform — the MFT walk when the
> root is a volume root on Windows, `getdents64` on Linux, `getattrlistbulk`
> on macOS, `getdirentries` on FreeBSD — and the portable walk answers
> everywhere else and whenever a native backend refuses. The pick is a speed
> decision, never a semantics one: each backend is held against
> `scan_sequential` by the dispatch's differential test. What remains
> unverified is macOS (needs a real Mac); the FreeBSD reader is verified
> against a FreeBSD 15.1 kernel (2026-10-14).

### Content

| Piece | Where | State |
|---|---|---|
| Content index (trigram + bloom) | `crates/name-core` (`content`) | ✅ transported from fsearch |
| Cloud placeholders (never hydrated) | `crates/name-core` (`placeholder`) | ✅ the recall bits on Windows, `SF_DATALESS` in `st_flags` on macOS — asked at the open, so the bytes are never fetched |
| Confirm via rgx engine + `grep:`/`regex:`/`sym:` | `crates/name-core` (`content`), `crates/rgx-core` | ✅ |
| rgx engine, TV packs, adapters | `crates/rgx-*` | ✅ upstream's crates, verbatim (re-synced 2026-10-22) |
| One regex engine (vendored copy deleted) | `crates/name-core` (`regex`) → `rgx-core` | ✅ (`rgx-core` stays `no_std`) |
| Pack compaction | `crates/rgx-core` (`tv_runtime`) | ✅ opt-in, `--compact` |

> **`grep:`/`regex:`/`sym:` search file bytes today, not containers.** The
> `rgx-adapters-*` families are transported verbatim, but no binary wires
> them into the search path yet — searching *inside* PDF/Office/archives is
> [rgx](https://github.com/danalec/rgx)'s job until they are wired in.

### Binaries and tooling

| Piece | Where | State |
|---|---|---|
| `searchctl` (name + content CLI, rg exit codes, `--json`) | `bins/searchctl` | ✅ |
| `searchd` (JSON-lines daemon) | `bins/searchd` | ✅ name + `grep:`/`regex:`/`sym:` ops |
| The JSON-lines protocol and its one escaper | `crates/proto` | ✅ RFC 8259 §7: the quotation mark, the reverse solidus and every C0 control are escaped, so a path, a query or a grep line out of a TSV file reaches the wire as **one** line and the framing cannot be broken by the content. `rgy-proto`, `searchd` and `searchctl` share the escaper, and a test holds it byte-for-byte equal to the upstream `djson::escape_json_string` |
| Named pipe / UDS fronts, `--serve` compat, `--tv-dir` | `bins/searchd` | ✅ |
| zen-rs bin / TUI (unchanged behaviour) | `bins/zen-rs`, `bins/search-ui` | ✅ |
| `rgy-autostart` (login item, per platform) | `crates/autostart` | ✅ startup task / HKCU Run, systemd user unit, launchd agent |
| The crates on crates.io | `crates/*/Cargo.toml`, `bins/*/Cargo.toml` | ✅ all fourteen published at 0.1.0 on 2026-10-23 — see below |
| The Listary plan | `docs/listary-parity.toml`, `scripts/listary-parity.py` | ⬜ 22 gaps and 7 degraded rows, each with work and a phase — see below |
| Packaging (winget, brew, tarballs) | `scripts/build-release.*`, `scripts/publish-release.*`, `.winget/`, `Formula/` | ✅ machinery in place; **no release published yet** |
| Release flow (the one-line installers, the Linux build over ssh) | `install.sh`, `install.ps1`, `scripts/release-linux.sh` | ✅ the installers pin the digest of the artifact they install; the pins are release data |
| Homebrew submission (the pull request to homebrew-core) | `scripts/publish-homebrew.*` | ✅ the machinery; the pull request waits for the first release to exist |
| keep-warm / mimalloc / big-alloc (`BlobMut::anon`) | `crates/engine`, `crates/name-core` | ✅ |
| PGO (trained on the name bench) | `pgo.sh` (unix), `pgo.ps1` (Windows) | ✅ validated end to end on Windows |
| Bench harness + baselines | `crates/engine/examples`, `crates/name-core/examples`, `benchmark/` | ✅ |
| The perf gate itself | `scripts/bench-names.py`, `benchmark/baseline-names.csv` | ✅ a fixed worker width, seven runs, the best of them, and the spread beside the number |
| The CI (the gates, on three OSes) | `.github/workflows/ci.yml`, `.gitea/workflows/drift.yml` | ✅ written and pinned to the named images (Windows Server 2022, macOS 15 on arm64, Ubuntu 24.04); ⬜ **never run** — see below |

### Perf gate

`python scripts/bench-names.py check --baseline benchmark/baseline-names.csv
--runs 7` — a >20% regression on any **gated** row of
`benchmark/baseline-names.md` fails the PR.

The gate is not a single run of the bench. A measurement is seven
independent runs at a fixed worker width (4 threads), the statistic is the
**best** of them, and the spread across the runs is recorded next to the
number. That is because the noise is wider than the threshold otherwise: on
this host the same code moved the `mod` row's min between 65 µs and 116 µs
across three runs at 32 workers, and the previous table — a single run per
row — did not reproduce even in the commit that wrote it.

Two things follow from it, and both are in the script:

- a row whose spread is wider than the 20% threshold is **undecidable**, and
  the gate says so instead of guessing;
- the 200k rows are gated (a 3–8% spread) and the 5M rows are **measured and
  reported, not gated** (a 5–52% spread on this host, and two sessions of
  identical code disagreeing by 28%). A gate that fires on noise is worse
  than no gate.

`benchmark/mmap-vs-read.md` records the mmap-vs-read measurements
(Linux + Windows) behind the access-shape choices in
[`docs/insights-from-rgx.md`](insights-from-rgx.md).

### What is left

The open work, ranked by what it blocks. The reasoning behind each row is
in [design.md](design.md); the matrix the rows come from is
[parity.md](parity.md).

A row that carries a `G` cell is a gap: work required. **There are none.**
All 53 features of the matrix now ship as `N`, `P` or `D` on all three
operating systems, and the list of known gaps in
[`docs/parity.toml`](parity.toml) is empty.

That is not the same as "nothing is left". A `D` cell is a documented
limit: the feature works, and the limit is written down where a user meets
it. Below are the limits that are still work, and the work that is not a
parity row at all.

The CI gate (`python scripts/parity-matrix.py --check`) keeps it that way:
it fails when a gap appears that is not on the known-gaps list, and when a
listed gap has been closed and the list forgot.

#### The Listary plan

[docs/listary-parity.md](listary-parity.md) is the plan: 33 capabilities
recovered from the reverse engineering of Listary 7.1.0.45, one row each,
with the work that closes every gap and the phase it belongs to. It asks a
different question from the matrix above — that one asks "does this work on
each OS", this one asks "does rgy do what Listary does".

Sixteen are gaps, eight are degraded, and nine are already at parity: the
NTFS MFT reader, the USN journal monitor, the multi-volume enumeration,
the folder walker, the atomic index snapshot, the u32-second modification
time, the fail-closed regex and the parallel scoring came in with the merge
or were already here.

The scope is the **engine's performance** — the ranking machinery, the
matchers, the priority, the query surface, the index shape and pinyin. The
product surface Listary also has (the HTTP API, the suggestion sources, the
index schedule, the diagnostics) is out of scope: it is features, not
performance, and it stays in the report rather than in the plan.

The phases run in a dependency order, and it is not the order the rows are
written in:

1. **The rank key** — Listary's ranking is a packed 64-bit composite, not
   a tuple. Everything that ranks follows from that one decision.
2. **The matchers** — six matchers instead of one, and the positions they
   matched. The positions are what makes a match visible.
3. **The priority rules** — the priority becomes a rule system the user
   writes. It needs the parent chain from phase 5, so it is written before
   it is implemented.
4. **The query surface** — the grammar, the relative time facets and the
   replay path.
5. **The index shape** — five indexes over a validated parent-chain
   forest. The on-disk format stays rgy's own.
6. **Pinyin** — the matcher family and the data behind it.

The gate (`python scripts/listary-parity.py --check`) fails when a gap has
no work or no phase, and when a parity claim names no place in the code. It
does not fail because a gap is open: a gap is the work.

**How the measurements are taken.** The name bench is measured at a fixed
worker width, and the width decides whether the number means anything. On
this host the four-thread rows spread 22–42% between runs — wider than the
20% threshold the gate rules on, so a four-thread comparison here is
UNDECIDABLE and the script says so rather than guessing. At one thread the
same rows spread 1–8% and the comparison is decidable. The A/B behind the
`RankAdj` section was taken at one thread for that reason: nine runs, best
of nine, and every row faster by 3.4–4.6%.

#### The limits that are documented

These are not gaps: the matrix says `D`, and the limit is written down
where a user will meet it. They are here because each one is still work,
and because the reason matters more than the label.

**1.3 — Restart replay** · D (windows) · D (linux) · D (macos)

The catch-up after a restart is a **relist**, and the program says so
instead of pretending otherwise.

What was wrong: `--max-age` had no default, so a saved index was reused
**however old it was**, and the watch started at the journal's head — so
the changes made while the daemon was off were never delivered. A daemon
would come up, print "reusing index", and answer on a snapshot of
yesterday's tree, with a file created since simply missing and nothing
saying the index was a day old.

What is the truth now:

- the reuse has a **default maximum age of 30 minutes**, and an index
  older than that is rebuilt by walking the tree;
- the reason reaches the user — *"rebuilding — index is 25h old"* — and
  when the index *is* reused, the line says how old it is and what that
  means (*"no change journal on this platform, so the changes made since
  then are not in it"*);
- `--max-age 0` still means "never reuse", and an explicit `--max-age`
  still wins in both directions;
- the **status answer** says which of the two happened: `replayed` or
  `relisted`.

What is still not the full feature: the journal **resume** is wired (the
feed starts at the saved watermark, which travels in the index header) and
the backlog now reaches the index — each record's path is rebuilt from the
MFT, so the folder to re-list is known. The age limit still applies to a
resumed index, because it carries the walk's metadata until a change lands,
and the code says so at the resume site. On Linux the limit is permanent:
there is no journal to resume from, so the catch-up is a relist.

**1.2 — The Windows feed names a file, and the path is rebuilt** ·
P (windows) · P (linux) · P (macos)

The USN journal delivers the changes, and what it does not deliver is the
*path*: a record names the file and its parent's file reference. The path is
rebuilt from the MFT, one record per level, with the ancestors remembered
because every change in a folder asks for the same ones.

Two things came out of that, and both are worth stating:

- the record's parent is the folder the change happened in, so the folder
  to re-list is exact — and a file with several hard links is re-listed
  where it changed, not where its canonical name points;
- the feed is wider than the tree: the journal is the volume's, and the
  daemon indexes a folder of it. A change outside the indexed tree is
  dropped, because applying it would put entries in the index that the
  tree never had.

What is left is the platform's, not the code's: the volume handle needs an
administrator, so on a shell that cannot open one the feed stays closed and
the daemon answers on its snapshot.

**1.12 — The NFC fold changes what an ASCII query matches** ·
P (windows) · P (linux) · P (macos)

Every comparison now runs on a fold of both sides: the ASCII case fold
that was always there, plus a real Unicode NFC normalization (UAX #15,
with the tables generated from the UCD). A name stored as NFD — what
macOS gives — now matches a query typed as NFC, which is the whole point
of the row.

The price is one behaviour change, and it is worth knowing about: the
ASCII query `cafe` no longer matches the file `café.txt`. NFC absorbs the
base letter into the composed `é`, so the folded query is `café` and the
folded name is `café` — they match — but a query with no accent is a
different string. On Windows and Linux nothing changes, because their
names were already NFC and `cafe` never matched `café.txt` there either.
On macOS this removes a match that exists today, which is the price of
the three platforms behaving alike.

#### What is not a parity row

- **The adapters in the search path.** The `rgx-adapters-*` families are
  in the tree and read inside PDF, Office, archives, mail, media, 3D and
  the model formats — but no binary wires them into the search path yet.
  That is the biggest single piece of work left, and it is not a per-OS
  parity question: it is the same on all three operating systems, and the
  matrix says so (row 3.5: the families are there and portable; what rgy's
  own binaries do not do is read *inside* them). It is the difference
  between "rgy searches bytes" and "rgy searches documents".
  [FORMATS.md](../FORMATS.md) has the format by format reach; the upstream's
  own [FORMATS.md](https://github.com/danalec/rgx/blob/main/FORMATS.md) has
  what each format gives.
- **The trigram index over extracted text.** An experiment, not a gap: a
  trigram means something different over extracted text than over raw
  bytes, and the pack path already covers the recurrent container
  queries.
- **Publishing to a registry.** The rgx repository is published
  (`https://github.com/danalec/rgx`, answering 200 since 2026-10-23, and
  the blob links in this file resolve), and the rgx crates are on
  crates.io: all twelve — `rgx-core`, `rar-codec` and the ten
  `rgx-adapters-*` — at **0.1.0**, confirmed against the crates.io API on
  2026-10-23.

  rgy's own crates are on crates.io too, all fourteen at **0.1.0**:
  `rgy-proto`, `rgy-discovery`, `rgy-name-core`, `rgy-engine`,
  `rgy-autostart`, the four `rgy-discovery-{windows,linux,macos,freebsd}`,
  `rgy-discovery-dispatch`, `rgy-searchctl`, `rgy-searchd`,
  `rgy-search-ui` and `rgy-zen`. The names carry the `rgy-` prefix
  because `proto`, `discovery`, `name-core`, `engine`, `autostart` and
  `zen-rs` are all taken on the registry (checked against the crates.io
  API on 2026-10-23), and the library target names follow the package
  names (`rgy_discovery`, `rgy_name_core`, …). That is not cosmetic: the
  crates.io crate named `discovery` already publishes a library target
  called `discovery`, and a consumer depending on both would hit E0464
  **on their side**, not a warning here. The shipped binaries keep their
  names (`searchctl`, `searchd`, `rgy-autostart`); only the package names
  moved.

  The *distribution* keeps a different name: `rgy-search`, because `rgy`
  is taken on crates.io (a GameBoy emulator library); on winget and
  homebrew the name is free, and one name across the registries is worth
  more than the shorter one. The project, the repository (`danalec/rgy`)
  and the binaries keep the name `rgy`.

  What is left is the repository itself. The `repository` metadata on
  every crate points at `https://github.com/danalec/rgy`, which does not
  answer yet (404) — the repository still lives only on the private
  forgejo, and crates.io metadata is fixed at publish time, so the URL
  is set now and the repository follows.
- **The first release.** The packaging is in place — the winget manifest,
  the Homebrew formula, the scripts that build the archives and publish
  them — but nothing has been published yet, so the manifest and the
  formula name a release that does not exist.
- **A Mac.** The macOS column is the one column that has never run on the
  operating system it describes. The code is transported and compiles
  there — checked for both `aarch64-apple-darwin` and `x86_64-apple-darwin`
  — but nobody has watched it work.
- **The CI has never run.** The workflow is written and pinned to the
  images this project is verified against: Windows Server 2022, macOS 15 on
  arm64 (the only place the NEON scorer and the macOS discovery backend
  would ever run) and Ubuntu 24.04, with the `lld-link` step, the
  `--locked` builds and the format gate. It has not run once, because the
  repository lives only on the private forgejo, and the workflow asks for
  GitHub-hosted labels while the only runner this project registers there
  is a self-hosted Linux one. The matrix becomes a gate the day the
  repository is on a host with those runners. Until then it is a promise.
