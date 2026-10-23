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
| Name index v2 (intern, DFS, bitmaps, mmap blob) | `crates/name-core` (`name_index`) | ✅ from fsearch |
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
| The JSON-lines protocol and its one escaper | `crates/proto` | ✅ RFC 8259 §7: the quotation mark, the reverse solidus and every C0 control are escaped, so a path, a query or a grep line out of a TSV file reaches the wire as **one** line and the framing cannot be broken by the content. `proto`, `searchd` and `searchctl` share the escaper, and a test holds it byte-for-byte equal to the upstream `djson::escape_json_string` |
| Named pipe / UDS fronts, `--serve` compat, `--tv-dir` | `bins/searchd` | ✅ |
| zen-rs bin / TUI (unchanged behaviour) | `bins/zen-rs`, `bins/search-ui` | ✅ |
| `rgy-autostart` (login item, per platform) | `crates/autostart` | ✅ startup task / HKCU Run, systemd user unit, launchd agent |
| Packaging (winget, brew, tarballs) | `scripts/build-release.*`, `scripts/publish-release.*`, `.winget/`, `Formula/` | ✅ machinery in place; **no release published yet** |
| Release flow (the one-line installers, the Linux build over ssh) | `install.sh`, `install.ps1`, `scripts/release-linux.sh` | ✅ the installers pin the digest of the artifact they install; the pins are release data |
| Homebrew submission (the pull request to homebrew-core) | `scripts/publish-homebrew.*` | ✅ the machinery; the pull request waits for the first release to exist |
| keep-warm / mimalloc / big-alloc (`BlobMut::anon`) | `crates/engine`, `crates/name-core` | ✅ |
| PGO (trained on the name bench) | `pgo.sh` (unix), `pgo.ps1` (Windows) | ✅ validated end to end on Windows |
| Bench harness + baselines | `crates/engine/examples`, `crates/name-core/examples`, `benchmark/` | ✅ |
| The perf gate itself | `scripts/bench-names.py`, `benchmark/baseline-names.csv` | ✅ a fixed worker width, seven runs, the best of them, and the spread beside the number |

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
  the blob links in this file resolve), and the rgx crates are going to
  crates.io. rgy's own crates are not published yet. The *distribution*
  package is called `rgy-search`, because `rgy` is taken on crates.io (a
  GameBoy emulator library); on winget and homebrew the name is free, and
  one name across the registries is worth more than the shorter one. The
  project, the repository (`danalec/rgy`) and the binaries keep the name
  `rgy`.

  The names of the internal crates are a separate problem, and it is not
  solved by publishing them. Checked against the crates.io API on
  2026-10-23: `rgy`, `engine`, `proto`, `discovery`, `zen-rs` and
  `autostart` are **all taken**; `rgy-search`, `rgx-core` and `rar-codec`
  are free. So every internal crate needs a `rgy-` prefix (or another
  name) before it can be published at all, and the order matters:
  `name-core` depends on `rgx-core` by path, so `rgx-core` has to exist
  first. `cargo publish --dry-run -p rgx-core` passes today (127 files,
  compiles, stops at the upload); `-p name-core` fails, because a
  path dependency without a version requirement cannot be published. The
  crate names are the decision; the machinery is in place.
- **The first release.** The packaging is in place — the winget manifest,
  the Homebrew formula, the scripts that build the archives and publish
  them — but nothing has been published yet, so the manifest and the
  formula name a release that does not exist.
- **A Mac.** The macOS column is the one column that has never run on the
  operating system it describes. The code is transported and compiles
  there; nobody has watched it work.
