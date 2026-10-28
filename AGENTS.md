# AGENTS.md — working notes for rgy

This repository is a merge: **fsearch**'s name engine and query
ergonomics, **zen-rs**'s Windows discovery, and **rgx**'s content engine,
adapters and pack store, in one workspace with one protocol and one
allocator.

The rules below are the ones that are easy to break by accident. Each one
says what the rule is and why it is there. If a rule here contradicts what
you were asked to do, the request wins — but say in the change that the
rule was broken and why.

## 1. The three trees, and what may be changed in them

| tree | whose it is | what you may do |
|---|---|---|
| `crates/rgx-*`, `crates/rar-codec` | **upstream rgx's**, transported verbatim | nothing, by hand. They change only through the sync below. |
| `crates/name-core`, `crates/discovery*`, `crates/par`, `crates/engine`, `crates/proto`, `crates/autostart`, `bins/*` | this repository's | freely |
| `tests/`, `scripts/`, `tools/`, `benchmark/`, `docs/` | this repository's | freely |

**The upstream trees are upstream's code.** A local edit there is a fork
that nobody asked for, and it is the reason the licence story is hard: a
file whose SPDX header says Apache-2.0 while its content is somebody
else's work is a licence claim this project cannot make.

**A directory name is not a crate name.** The fifteen rgy crates publish
as `rgy-<directory>` (`crates/proto` → `rgy-proto`, `crates/name-core` →
`rgy-name-core`, `crates/discovery-windows` → `rgy-discovery-windows`,
`crates/par` → `rgy-par`),
because `proto`, `discovery`, `name-core`, `engine`, `autostart`, `par` and
`zen-rs` are taken on crates.io. The **library target names follow the
package names** (`rgy_discovery`, `rgy_name_core`), so the Rust paths
are `rgy_discovery::…`. That is deliberate: a library whose target name
stayed `discovery` would collide with the crates.io crate of that name,
and two dependencies sharing one lib name is `E0464` on the consumer's
side. The **shipped binaries keep their names** (`searchctl`, `searchd`,
`rgy-autostart`) — only the package names moved.

### The sync is the only way they change

```sh
scripts/sync-rgx-upstream.sh --upstream ../rgx          # copy + patch + manifest
scripts/sync-rgx-upstream.sh --upstream ../rgx --dry-run # what would change
```

It copies from the upstream **commit**, not from its working tree: the
manifest pins a commit, so the copy has to be that commit's bytes. It
applies the local patches, and it rewrites
[`scripts/vendored-sources.tsv`](scripts/vendored-sources.tsv) — the
manifest is generated, never hand-edited, because a manifest that falls
behind the tree it describes is worse than none.

### A divergence from upstream is a patch, and only a patch

```text
scripts/rgx-local-patches/<path>.patch
```

One patch per file, applied on top of the copy. A patch is a bug upstream
has not fixed, or a fix upstream has not taken, and it has to be
re-derived every time upstream touches that file — the patch mechanism
fails loudly when it no longer applies, rather than quietly dropping the
fix.

There are two today, both overflows that upstream's release-mode CI
cannot see (`1u64 << 64` in the HFS+ encoder bitmap, and
`frac * 86_400` in the Firebird creation date). Both are debug-mode
panics, and this repository's CI runs the suite in debug.

## 2. The scripts have twins, and the twins are checked

Every `.ps1` has a `.sh` beside it, and they are **twins**: the same
output, on the same input, with the same exit code.

The test is to **run both and diff the result** — not to compare exit
codes, not to eyeball the code. A pair that prints the same text on
Windows and on Linux is a pair that can be trusted on a machine you did
not test on.

What the twins get wrong most often:

- **Paths.** A Windows path has backslashes; a POSIX one does not. A
  script that assumes one flavour of path is a script that works on one
  platform.
- **Line endings.** A `.sh` checked out with CRLF does not run on Linux
  at all. The `.gitattributes` says `*.sh text eol=lf` for exactly that.
- **Process handling.** `Start-Process` and `&` are not the same, and a
  pipeline that closes early (`| head`) kills the child with SIGPIPE on
  one platform and not on the other.
- **`awk -v` with a Windows path** mangles the backslashes; the escape
  has to be doubled.

## 3. The licence is in the headers, and the headers are checked

The per-file `SPDX-License-Identifier:` header is the authority. The
[`COPYING`](COPYING) and [`NOTICE`](NOTICE) documents agree with it, and
[`tests/license.rs`](tests/license.rs) reads the file list out of git and
fails on the first file that says otherwise.

- The **product** and the **e2e harness** are Apache-2.0. The **tooling**
  (`scripts/`, `tools/`, `benchmark/`) is MIT OR Apache-2.0.
- Three files in `crates/name-core` are transported from `fsearch` (MIT,
  another author's work) and keep its terms; their notice is
  `LICENSE-MIT-FSEARCH`, and the licence test lists them as explicit
  exceptions.
- The vendored rgx trees carry upstream's terms, which are Apache-2.0, and
  upstream's per-crate `LICENSE-APACHE` travels with each published
  crate.

A file that changes licence has to change in three places at once: its
header, its crate manifest, and the audit's exception list. Change one and
the audit says so.

## 4. The parity matrix is generated

[`docs/parity.md`](docs/parity.md) comes from
[`docs/parity.toml`](docs/parity.toml) through
[`scripts/parity-matrix.py`](scripts/parity-matrix.py). Never edit the
Markdown; edit the TOML and regenerate:

```sh
python scripts/parity-matrix.py > docs/parity.md
```

A `G` cell is a gap — work required — and it fails CI until it is ported
or documented as degraded. A row that claims parity without the
verification behind it is worse than a `G`.

## 5. The perf gate is a number, and the number is the gate

```sh
python scripts/bench-names.py check --baseline benchmark/baseline-names.csv --runs 7
```

A **>20% regression** on any gated row of
[`benchmark/baseline-names.md`](benchmark/baseline-names.md) fails the PR.
The measurements live in [`benchmark/`](benchmark/), and
the write-ups say what was measured, on what host, on what date. A number
without its host and its date is a rumour.

The gate is not one run of the bench. A measurement is several independent
runs at a **fixed worker width**, the statistic is the **best** of them, and
the spread across the runs is recorded next to the number — because the
noise is otherwise wider than the threshold, and a gate that fires on noise
is worse than no gate. A row whose spread is wider than the threshold is
undecidable, and the script says so instead of guessing.

The width is a machine too, and not the same thing twice. The gate
measures at four workers, because that is the width this host's noise lets
a comparison decide; the tree runs at the width the machine gives it, and
that width is the machine's **cores**, not its processors — a core with two
threads is one core, and the scan is a memory-bound pass that measured 47%
slower with the second thread of every core put to work. A caller that
wants a width of its own (`RGY_NUM_THREADS`, `rgy_par::set_workers`) still
gets it.

The gate is also a machine. A baseline recorded on one host cannot rule on
another — the code generator is the OS's, and the measurement is a
memory-bound one, so the CPU's memory system is in it too — so every row
carries the host it was taken on, the gate compares against the rows of the
host it runs on, and the write-up carries a table per host. `--host` names
the host when a machine has more than one name.

The table is generated from [`benchmark/baseline-names.csv`](benchmark/baseline-names.csv),
the way `docs/parity.md` is generated from `docs/parity.toml` (§4): the CSV
is the source of truth, and `bench-names.py render --check` fails when the
two have drifted.

## 6. Build before you test

```sh
cargo build --workspace          # the e2e harness resolves the binaries by path
cargo test --workspace --no-fail-fast
```

`cargo test --workspace` alone is not a build-first. On a clean tree the
e2e tests race the rest of the workspace and spawn nothing; the harness
says so in the failure, but the fix is the build above.

## 7. The tree is `no_std`, and a build says so

- **Every crate this repository owns is `no_std` + `alloc`.** `crates/par`,
  `crates/discovery`, `crates/name-core`, `crates/proto`, `crates/engine`,
  `crates/discovery-dispatch` and the four platform backends declare
  `#![no_std]`, and what they depend on — `hashbrown`, `rgx-core`,
  `rgy-par`, `libc`, the `windows` crate — is `no_std` as well. A `std`
  dependency in one of them is a decision, not an accident: it means the
  tree stops being linkable into an embedded search box, which is the whole
  reason it is `no_std`. Say so in the change, and put the dependency
  behind the `std` feature if the feature can hold it.
- **What needs a filesystem is behind the `std` feature, and the list is
  short.** The content grep (it reads files with `std::fs` and runs
  threads), the persisted index (it writes files and asks the environment
  for the cache folder), the cloud placeholders (they ask the OS), the
  threads and the mmap of the name blob; the engine's keep-warm spinner,
  the console and the pack store; the login item's files and the platform's
  own tools. Everything else — the index, the query parser, the ranking, the
  fold, the NFC tables, the transliterations, the protocol, the parallel
  primitives, the portable walk, the live overlay, the clock, the path
  resolution, the decision of which mechanism arms the daemon — runs on
  bytes the caller owns.
- **A platform call is written by hand, in the seam, once.** The seam owns
  the calls the tree needs from the operating system: `rgy_discovery::dir`
  (list a folder, stat a path), `rgy_discovery::file` (read a file, write
  one, resolve a path) and `rgy_discovery::path` (the bytes, the joining,
  the Windows spelling). The portable walk, the live overlay's `lstat`, the
  scope filter's resolution and the macOS backend's root all go through
  them. Two rules: the call is declared, not brought in (`clock_gettime`,
  `realpath`, `CreateFileW` — the house pattern, in
  `crates/name-core/src/clock.rs` and `crates/discovery/src/file.rs`), and
  a struct the platform writes is the platform's struct, whole — the
  `WIN32_FIND_DATAW` without its `cAlternateFileName` is 28 bytes short of
  what `FindFirstFileW` writes, and the stack says so.
- **A crate-level `cfg` comes after `#![no_std]`, never before.** A crate-level
  `#![cfg(...)]` that is false empties the crate — and an emptied crate has
  no `no_std` to declare, so it is compiled as a crate that wants the
  standard library. On a bare-metal target that is `E0463: can't find
  crate for std`, in the backend whose body does not even run on the host.
  `crates/discovery-freebsd` has both attributes, and the order is the
  rule: `#![no_std]` first, `#![cfg(target_os = "freebsd")]` second.
- **A path is bytes.** `std::path` does not exist in `no_std`, and the
  tree does not need it: the seam's root is a `Vec<u8>`, the platform
  backends take the same shape, and `rgy_discovery::path` owns the joining
  and the C strings. The separator is `/` everywhere, and the bytes are
  the bytes the filesystem gave — a Unix path is not UTF-8, and the tree
  does not pretend otherwise.
- **The gate is a build for bare metal, on every push.**
  `cargo build --release --locked --no-default-features -p rgy-par
  -p rgy-discovery -p rgy-name-core -p rgy-proto -p rgy-engine
  -p rgy-discovery-dispatch -p rgy-autostart -p rgy-discovery-linux
  -p rgy-discovery-freebsd -p rgy-discovery-windows -p rgy-discovery-macos
  --target x86_64-unknown-none`. That target has no operating system at
  all, so nothing in the chain can quietly reach for a file, a thread or a
  clock: there is nothing there to reach for. The Cortex-M targets would
  do the same job, except that rgx-core keeps a 64-bit atomic and those
  cores do not have one — a limit of the upstream tree, not of this one.
- **The list is the whole tree, and it is the whole list on purpose.** A
  tree is `no_std` when the last crate in it is, and a list that stops at
  the crates that were easy stops exactly where the work gets interesting.
  The platform backends are in it too, and what it proves for them is the
  shape of their tree; what proves their *bodies* is the suite job, which
  builds each one for its own OS, where the `#![no_std]` at the root of
  the crate is the proof — a `std::` path in the body does not compile.
- **The product is `std`, and that is the point.** `searchctl`,
  `searchd`, `rgy-autostart` and the zen CLI read files, spawn threads and
  talk over pipes. A CLI that cannot open a file is not a CLI. The rule is
  about the library tree, not about the binaries.
- **One crate in the tree is `std`, and it is not ours.**
  `crates/rgx-adapters-docs` is the upstream's, transported verbatim, and
  it spawns external programs (`soffice`, `tesseract`) — which is `std` by
  definition, and its own manifest says so. It is the one crate the
  bare-metal gate does not name, and the reason is in the crate.
- **The upstream trees are already `no_std`** — `rgx-core`, `rar-codec`
  and eight of the ten `rgx-adapters` — so the tree on top of them has
  no `std` to inherit. The one `std` dependency left inside it is the
  `regex` crate: optional, behind the `std` feature, and only the
  content grep's fallback for the patterns rgx's byte parser refuses.

## 8. The docs have one job each

| file | job |
|---|---|
| `README.md` | the map: what this is, and where everything else lives |
| `MANUAL.md` | how to use it |
| `FORMATS.md` | what it reads inside a file |
| `AGENTS.md` | this file |
| `docs/design.md` | the decisions the code is built on, and the rules that keep it honest |
| `docs/STATUS.md` | the scoreboard: what is built, measured, unverified — and what is left |
| `docs/listary-parity.md` | what Listary does that rgy does not, and the work that closes it: the implementation plan |
| `docs/` | everything else, one document per subject |

The README is short on purpose. When a section grows past a screen, it
belongs in its own document and the README links to it.

**There is no plan document, and adding one is a step backwards.** The
merge is landed; the plan that produced it is gone, and what it left
behind lives in the two documents that are still true: `docs/design.md`
(the decisions, which the code cites by section) and `docs/STATUS.md` (the
scoreboard, and the work that remains). A new document earns its place by
answering a question a reader has *now*, not by recording how the answer
was reached.

**The audience is not assumed to be technical.** A person who wants to
find their invoices should be able to read the manual and get an answer
without knowing what an MFT is. Put the explanation first and the
mechanism second; keep the tables for the people who need them.

## 9. The working tree is shared

This repository is worked on in parallel — by people and by agents. A file
may change under you between the moment you read it and the moment you
write it. Read a file immediately before writing it, and never overwrite
a change you did not make without saying so.

## 10. Gotchas

- **An unknown `type:` fails closed.** A warning on stderr, zero matches,
  exit 1. It does not search everything.
- **The `rgx` crates stay on edition 2021** so they keep diffing cleanly
  against upstream. The rgy crates and binaries are edition 2024. Do not
  "harmonise" this.
- **`rgx-tv` is not in a default build.** It is a target of the vendored
  `rgx-core` behind `required-features = ["tv-runtime"]`, so it needs
  `cargo build -p rgx-core --bin rgx-tv --features tv-runtime`.
- **The pack store's tuning flags stop at the daemon's edge.**
  `searchd` calls the pack searcher with `SearchConfig::default()`, so
  `--engine`, `-j`, `--stats`, `-A/-B/-C` and the mmap choice are only
  reachable by running `rgx-tv search` directly.
- **Fixtures are byte-compared.** `**/testdata/**`, `**/fixtures/**` and
  `*.expected` are `-text` in the `.gitattributes`, and a checkout that
  rewrote them would make the suite green in the working tree and red in
  a fresh clone. That happened once already.
- **Windows needs the long-path form** (`\\?\`) and UTF-16 at the
  boundary; the internal representation is UTF-8 with `/` separators.
- **The host hides the code of the other platforms.** A module behind
  `#[cfg(target_os = ...)]` in a crate the tree shares is not compiled
  here, so a green suite on this Windows host says nothing about it. The
  `scan_priority` module broke exactly that way — the import was renamed
  and the use was not — and only the Linux build saw it. Before calling a
  change green, check it for every platform the tree targets:
  `cargo check --workspace --target x86_64-unknown-linux-gnu`,
  `--target x86_64-unknown-freebsd`, `--target x86_64-apple-darwin`, and
  a real run on the Linux box (the `suite` job, or the CachyOS host).
- **A struct the platform writes is the platform's struct, whole.**
  `WIN32_FIND_DATAW` without its `cAlternateFileName[14]` is 28 bytes
  short of what `FindFirstFileW` writes, and the stack says so with an
  access violation that looks like anything but the cause. The size is
  checked at compile time, in `crates/discovery/src/dir.rs`.
- **macOS stores filenames in NFD.** A query typed in NFC has to match,
  and that folding layer does not exist yet — it is row 1.12 of the parity
  matrix, and it is a `G`.
