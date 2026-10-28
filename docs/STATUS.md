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
| Name index v2 (intern, DFS, bitmaps, mmap blob) | `crates/name-core` (`name_index`) | ✅ from fsearch; the blob is `FSIDX014`, the ranking adjustment each entry inherits from its directory lives in the entry (the `RankAdj` section) rather than one hop away in the directory table — 3.4–4.6% faster on every row of the name bench — and the priority tier that entry inherits rides in the kind byte the search reads anyway |
| Query syntax merge (fsearch tokens + zen-rs filters) | `crates/name-core` (`name_query`) | ✅ `type:`/`dir:`/`size:`/`re:`/globs/\|, fail-closed |
| fzf-v1 scorer + typos + top-k + NameCache + **SSE2 wide scorer** | `crates/name-core` (`name_query`) | ✅ parity-tested vs scalar |
| Live overlay | `crates/name-core` (`live`) | ✅ portable `lstat`; the base's root-relative paths and the overlay's openable ones are crossed, so a re-listed folder answers each file once. The daemon merges the overlay through `search_alternatives` and the CLI through `merge_hits`, so **every** live change is answered — the keyed-by-index merge it replaced dropped one change per file and made the CLI index the base with the overlay's `u32::MAX` sentinel (a panic); both paths are pinned by `every_live_change_is_answered` |
| Index persistence (sidecar-pinned root) | `crates/name-core` (`persist`) | ✅ |
| Path fold (ASCII case + Unicode NFC, UAX #15) | `crates/name-core` (`fold`, `nfc`) | ✅ tables generated from the UCD; 20,171 UCD conformance cases pass |
| The tree without `std` | `crates/*` (every crate this repository owns) | ✅ the index, the query parser, the ranking, the fold, the transliterations, the protocol, the selector, the engine and the four platform backends are `alloc` and nothing else — the lot builds for `x86_64-unknown-none` (bare metal, no OS at all), and the CI checks it on every push. The `std` feature adds what needs a filesystem: the content grep, the live overlay, the persisted index, the mmap, the clock, the threads, the portable walk, the console and the pack store |

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
| rgx engine, TV packs, adapters | `crates/rgx-*` | ✅ upstream's crates, verbatim (re-synced 2026-10-27, at upstream `4d35a02`, the `rgx-v0.1.1` release) |
| One regex engine (vendored copy deleted) | `crates/name-core` (`regex`) → `rgx-core` | ✅ (`rgx-core` stays `no_std`) — the name path (`re:`, `path:`, the rules) and the content path's query plan both run on rgx-core's AST; the `regex` crate is left only as the content grep's fallback for the patterns rgx's byte parser refuses (Unicode properties, say), which is the same gap upstream's rgx still has |
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
| The Listary plan | `docs/listary-parity.toml`, `scripts/listary-parity.py` | ⬜ 0 gaps and 12 degraded rows, each with work and a phase — see below |
| Packaging (winget, brew, tarballs) | `scripts/build-release.*`, `scripts/publish-release.*`, `.winget/`, `Formula/` | ✅ machinery in place; **no release published yet** |
| Release flow (the one-line installers, the Linux build over ssh) | `install.sh`, `install.ps1`, `scripts/release-linux.sh` | ✅ the installers pin the digest of the artifact they install; the pins are release data |
| Homebrew submission (the pull request to homebrew-core) | `scripts/publish-homebrew.*` | ✅ the machinery; the pull request waits for the first release to exist |
| keep-warm / mimalloc / big-alloc (`BlobMut::anon`) | `crates/engine`, `crates/name-core` | ✅ |
| PGO (trained on the name bench) | `pgo.sh` (unix), `pgo.ps1` (Windows) | ✅ validated end to end on Windows |
| Bench harness + baselines | `crates/engine/examples`, `crates/name-core/examples`, `benchmark/` | ✅ |
| The perf gate itself | `scripts/bench-names.py`, `benchmark/baseline-names.csv` | ✅ a fixed worker width, seven runs, the best of them, and the spread beside the number |
| The parallel primitives (rayon gone) | `crates/par` (`rgy-par`) | ✅ `no_std` and dependency-free: `map_collect`, `for_each`, the k-way merge sort and the split that writes a chunk per worker. The width is the user's: `--threads` wins, then `RGY_NUM_THREADS`, then four for the scan and the machine's cores for the query |
| The whole tree is `no_std` | `crates/*`, `crates/discovery-*` | ✅ every crate this repository owns declares `#![no_std]`; the `no-std` job builds the lot for `x86_64-unknown-none`, and each platform backend is proved on its own OS by the suite job |
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

[docs/listary-parity.md](listary-parity.md) is the plan: 35 capabilities
recovered from the reverse engineering of Listary 7.1.0.45, one row each,
with the work that closes every gap and the phase it belongs to. It asks a
different question from the matrix above — that one asks "does this work on
each OS", this one asks "does rgy do what Listary does".

None are gaps, twelve are degraded, and twenty-three are already at parity:
the NTFS MFT reader, the USN journal monitor, the multi-volume enumeration,
the folder walker, the atomic index snapshot, the u32-second modification
time, the fail-closed regex and the parallel scoring came in with the merge
or were already here — and the transliteration shadow, the rank key and the
matchers below.

**The span that earned the score.** The matchers now say which characters
they matched, and the UI shows it: the TUI paints the span in the
highlight's colour and `searchctl --json` carries the characters as a
`match` array. The positions are computed on demand, for the rows that are
shown — the scan does not carry them, so the hot path does not pay for
them. The matcher works on bytes (rgy's index is bytes), so the bytes it
matched are snapped to the characters they belong to: a character is
matched whole or not at all.

**The matchers.** A quoted run is a phrase (`"invoice pdf"` is one token and
the name has to carry the words together, with the space between them) and
`=port` asks for a whole word (`port` is the word of `report_port.txt` and
not of `report`). Both are modes, like `'x`, `^x` and `x$`: the fuzzy
match already forgives what the keyword one refuses, and running every
matcher on every candidate would cost the scan a third of itself.

The negation was wrong while the modes were added: `!invoice` negated an
*exact* match, so it still answered `invoce` — and the manual said it
meant "not close to". The negation now asks the same question the match
does, so what `!x` keeps is what `x` would not have found.

**The order of the results.** `sort:` picks it: `score` (the default, the
ranking), `name`, `mtime` or `none`. The choice orders the hits the cap
kept; it does not pick them — the cap still takes the best by score, so
`sort:mtime` is "the newest of the best matches", not "the newest
matches". An unknown choice fails closed, like an unknown `type:`.

**The priority rules.** The table of directory names that decided what a
place was worth was in the source, and now it is not: the rules live in a
file beside the index (`<index>.rules`), each a level, a regex and the two
flags that say who the rule is for. The table is the default rule set, so
the behaviour that shipped is the behaviour that comes out of the box, and
a user who writes no rules gets it.

The rules are read when the index is built, so a record's tier is decided
once and stored with the record — the search reads it per candidate
instead of replaying the rules for every candidate it looks at. A rule
that does not parse is an error, not a warning: a rule the user wrote and
rgy dropped is a rule that does not work.

**The rank key.** The order of the results is now one `u64`, and the
priority is in its high bits: `Ignored < Low < Normal < Internal < High <
HighFile`, the ladder Listary's disk-search priority uses (§4.6). A priority
that is only a term added to the score loses to a better name — `config.json`
in `node_modules` beat `configuration.txt` in `Documents` — and the ranking
corpus (`crates/name-core/tests/ranking.rs`) is what says so. That corpus is
the instrument the repo did not have: the name bench measures how long a
query takes and says nothing about whether the answer is the right one.

The tier is read from the index, not computed: computing it cost 13% of the
scan, because the prior it comes from is a load and the tier is a chain of
compares on top of it. The tier rides in the kind byte the search reads
anyway, and the index format is `FSIDX014` because of it.

The key costs 6% to 9% of the scan (9 runs at one thread, before and after
on the same host: 395→424 µs on `^src`, 690→751 µs on `dat$`). That is
inside the gate's 20%, and it is the price of a ranking that says what the
user meant.

**The session.** A keystroke does not have to walk the index again. The
daemon keeps a session per client (the protocol carries the id), and a
query that narrows the previous one is answered by re-ranking the previous
result set. The re-rank is exact or it does not happen: the query has to
be able to match no more entries than the previous one, and the previous
result set has to be complete — a search that hit its cap did not find
everything, and the entries the cap dropped may be the ones the next
keystroke wants. The answers are the answers a fresh search gives, which
is the whole property, and a test pins it over the keystroke sequences a
user types.

**The measurement does not support the claim, and the matrix says so.**
On a 200k-name corpus the re-ranked keystroke is 0.97x the fresh one, and
the reason is structural: rgy's `full` scan is a linear pass over the
entry array at about 0.3 ns an entry, while a re-rank visits the
candidates one by one at about 18 ns — and the name table, which is where
the cost of a broad query is, is already narrowed by the `NameCache`. So
the re-rank re-pays what the cache already saved. It is therefore
guarded: it runs only when the previous set is at most a sixtieth of the
scope, the same rule the engine's own `selective` plan follows. A session
is never a pessimisation.

**The replay path is the part that pays.** A query that is the same query
— the same text and the same filters — is answered from the session's
answer, with no search at all. The rule is the whole query and not the
selection alone: a query whose text changed has a different answer, and
replaying the old one would be a stale answer.

**The four axes of a path.** A word says which part it matches: `name:`,
`ext:`, `folder:` and `attr:`. The name axis is what a bare word has
always meant, so the queries that were already there are unchanged. The
folder axis matches a folder of the path (the name need not hold the
word), the attribute axis matches the file's attributes by name (`dir`,
`file`, `link`, `hidden`, `mount`), and the extension axis matches the
extension of the name. A prefix that is also a filter keeps the filter's
meaning — `ext:pdf` is the extension, `ext:^pdf` is the extension axis.

**The five lookup indexes** are a surface over the same store, not a second
copy of it: `file_name` and `folder_name` (the two are apart, which rgy's
flat index was not), `folder_backend` (the volume that owns a folder),
`file_path` and `folder_path`. The daemon answers a `resolve` request
through it.

**A name read in another script.** `nihongo` finds 日本語, `にほんご` finds
日本語, `zhongwen` finds 中文. The mechanism is Listary's (the pinyin
shadow, §5.2 and §6.3), extended to the Japanese scripts, which Listary
has no data for. The data is the UCD's Unihan readings, generated into
`readings_table.rs` the way the NFC tables are generated: `kMandarin` for
the pinyin, `kJapanese` for the kana readings, tone marks dropped. Unihan
does not carry a kana-to-romaji table, so that one is written by hand, in
the generator, with the rules a person types by — きって is "kitte",
しゃ is "sha", スーパー is "suupaa".

Two limits are written down rather than papered over. A name is read
character by character, so 日本語 is read ニチホンゴ and not the word's
ニホンゴ: the compound reading belongs to the word, and the UCD does not
carry it. The fuzzy matcher skips the extra character, so `nihongo` still
finds it — but that is the matcher's tolerance doing the work, not the
data. And a character's other readings (日 is also ひ) are not tried.

The scope is the **engine's performance** — the ranking machinery, the
matchers, the priority, the query surface, the index shape and the
transliteration. The product surface Listary also has (the HTTP API, the
suggestion sources, the index schedule, the diagnostics) is out of scope:
it is features, not performance, and it stays in the report rather than in
the plan.

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
6. **Pinyin and the Japanese scripts** — the matcher family and the data
   behind it. The shadow that carries them is built; the Japanese side
   keeps two written-down limits (the compound readings of a word, and a
   character's alternative readings).

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

#### The content index is alive on both sides

Found on 2026-10-24, and it was a bigger piece than a cleanup.

The trigram content index came in with fsearch's `content.rs`, and its
**reader** was live: `searchctl` opens `<index>.content/` and hands it to
`content::answer`, which uses the postings to rule most files out before
they are read. But nothing in the product ever **wrote** it —
`Content::sync`, `build_segment`, `merge`, `diff`, `changed_dirs`,
`alloc_id`, `push`, `replace` and `merge_plan` were called only by the
tests — and the daemon's `grep:` did not read it either: it went
`content::scan_paths` + `content::verify`, which is the scan with no
index. So the directory the CLI looks for was never created, and the
read-avoidance it was written for was never in effect.

Both halves are now in the daemon:

- the **`grep:`** goes through `content::answer` with the daemon's index,
  so the postings rule the files out and only the candidates are opened;
- a **content thread** builds and keeps the index. It starts when the
  first content search asks for one — a daemon that is never asked to
  grep never pays for it — and it builds in batches, because the build
  reads the files it indexes and a burst that reads the whole disk would
  make every keystroke of a search running beside it slower.

The CLI's comment ("the daemon keeps it fresh") is true now. The union is
the scan's exact answer either way: the index answers for the files it
holds a fresh doc for, and every other candidate is read from disk.

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
- **The `std` corners that are left, and why.** The library tree is `alloc`
  and nothing else; what still leans on the standard library is behind a
  feature, and there are two kinds of it:
  - **Ours, and it is a capability rather than a leftover.** The threads
    (the parallel walk and the query), the mmap of the name blob, the
    content grep's file reads, the persisted index's cache folder and the
    environment it asks for. Each one needs an operating system to be
    interesting, and each one has a working shape without it: the walk
    runs on one worker, the index is read into memory instead of mapped,
    the grep does not run at all.
  - **Upstream's, and it travels with the crate.** The content grep keeps
    the `regex` crate as the fallback for the patterns rgx's byte parser
    refuses (Unicode properties, say). The name path and the content
    path's query plan both run on rgx-core; the fallback is the same gap
    upstream's own rgx still has. And `crates/rgx-adapters-docs` is the
    upstream's crate, transported verbatim, and it spawns external
    programs (`soffice`, `tesseract`) — `std` by definition, and its own
    manifest says so. It is the one crate the bare-metal gate does not
    name.
- **The whole tree is `no_std`, and the CI says so.** Every crate this
  repository owns — the name engine, the seam, the protocol, the selector,
  the orchestration engine, the login-item tool and the four platform
  backends — declares `#![no_std]`, and the `no-std` job builds the lot for
  `x86_64-unknown-none`: a bare-metal target with no operating system at
  all, so nothing in the chain can quietly reach for a file, a thread or a
  clock. The platform backends' own bodies are proved by the suite job,
  which builds each one for its own OS — where the `#![no_std]` at the root
  is the proof, because a `std::` path in the body does not compile.
- **The second round: the platform calls, written by hand.** The first round
  made the tree compile without `std`. The second asked what such a build is
  *missing*, and the answer was the operating system. The seam owns that
  now: `rgy_discovery::dir` (list a folder, stat a path),
  `rgy_discovery::file` (read a file, write one, resolve a path) and
  `rgy_discovery::path` (the bytes, the joining, the Windows spelling). The
  portable walk was rewritten over those calls, and it came out of `std`
  with them: `rgy_discovery::walk::scan` answers in a bare-metal build, so
  the selector's `Kind::None` — the "no walk on this target" answer — is
  gone. The name layer got the same treatment: its own clock
  (`clock_gettime(2)`, `GetSystemTimeAsFileTime` on Windows), the index
  format separated from the I/O (`to_bytes`/`from_bytes`), the live
  overlay's `lstat` and the scope filter's path resolution. What is still
  behind `std` is the list above, and every entry in it is a capability
  rather than a gap.
  - **What the migration changed in public API.** The roots of the sources
    are bytes now (`Vec<u8>`), because `std::path` does not exist in
    `no_std` and the tree did not need it: `rgy_discovery::path` owns the
    joining and the C strings, and the separator is `/` everywhere. The
    Linux inotify watch takes `&[u8]` where it took a `&Path`.
  - **The `ERS_USE_RECORDS` escape hatch moved.** The Windows backend no
    longer reads the environment — a library that reads the environment is
    a library that cannot be tested. The daemon and the CLI read it and
    hand the decision to `Indexer::with_records`, and the default is the
    same behaviour the variable used to select.
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

  `rgy-par` is the fifteenth, and it is **not published yet**: it was
  written for the `no_std` migration, so it has no release of its own to
  speak of. It publishes with the next release of the crates that depend
  on it.

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
- **The FreeBSD column has run, and it found two things that are not
  ours.** The whole workspace suite now runs on a real FreeBSD 15.1
  kernel: a `vmactions/freebsd-vm`-shaped VM, brought up with `anyvm` on
  the Unraid host with KVM, with `pkg install rust git` and the same two
  commands the CI job runs — 64 test targets, none failing. The autostart's
  CLI tests were running on platforms where the autostart does not exist;
  they are now filtered by platform, and the refusal is itself under
  test. Two failures are upstream's, in the vendored `rgx-core`, and both
  are about the host rather than about FreeBSD. The JSON scanner's test
  asserts the AVX2 path on any `x86_64`; the image boots with QEMU's
  baseline CPU when the rich one will not start it, and that baseline has
  no AVX2 — so the assertion cannot hold there, and the test should ask
  the machine instead of the architecture. And the fuzz harness's watchdog
  is ten seconds of wall clock, which a round crosses when `cargo test`
  runs the targets in parallel — and the worker thread the watchdog
  abandons keeps spinning, so the test binary never exits: on the Linux
  box that took the whole VM down. The same test passes three times out of
  three on an idle FreeBSD guest, so the input that does it is not the
  same one every time. Both belong in rgx, not here — and both were
  fixed there: the AVX2 test asks the machine instead of the
  architecture, and the watchdog has a round budget and stops the
  worker it abandons. A third, worse one came out of the same box:
  the LZMA decoder sized its literal table by the number of
  probabilities where it wanted the number of rows, so a 21-byte
  header asking for lc=8 and lp=4 asked for 4,831,838,208 bytes
  where the model needs 6 MiB — 768 times too much. All three are
  fixed upstream, and the vendored copy carries them as of the
  `rgx-v0.1.1` sync.
- **The CI has never run.** The workflow is written and pinned to the
  images this project is verified against: Windows Server 2022, macOS 15 on
  arm64 (the only place the NEON scorer and the macOS discovery backend
  would ever run) and Ubuntu 24.04, with the `lld-link` step, the
  `--locked` builds and the format gate. It has not run once, because the
  repository lives only on the private forgejo, and the workflow asks for
  GitHub-hosted labels while the only runner this project registers there
  is a self-hosted Linux one. The matrix becomes a gate the day the
  repository is on a host with those runners. Until then it is a promise.
