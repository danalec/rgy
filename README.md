# rgy

Whole-filesystem name search and content search in one stack: fsearch's name
engine and query ergonomics, zen-rs's Windows discovery, rgx's content engine,
adapters and pack store. Windows, Linux and macOS.

| I want to… | Go to |
|---|---|
| know which one I want: rgy or rgx | [rgy or rgx?](#rgy-or-rgx) |
| understand the design and the feature matrix | [`docs/IMPLEMENTATION_PLAN.md`](docs/IMPLEMENTATION_PLAN.md) |
| see what landed and what is unverified | [Workspace status](#workspace-status) |
| run something right now | [Try it](#try-it) |
| know why the storage layer is shaped the way it is | [`docs/insights-from-rgx.md`](docs/insights-from-rgx.md) |
| port it to FreeBSD | [`docs/freebsd-support.md`](docs/freebsd-support.md) |

## rgy or rgx?

[rgx](https://github.com/danalec/rgx) is the content-search specialist: an
`rg` replacement that reads *inside* files — PDFs, Office documents,
archives, mail, media metadata, 3D models, LLM checkpoints — with `rg`'s
flags, output and exit codes, the Tesla Valve pack store, and engine knobs
for harnesses. The `rgx-*` crates in this workspace are its engine and
adapters, transported verbatim.

rgy is the broader question: **which file?** — asked over the whole
filesystem, then **what is inside it?** in the same query.

| | rgy | rgx |
|---|---|---|
| Which file, anywhere on the disk | ✅ name index, fzf-v1 ranking, typos, live overlay | walk + path globs |
| What is inside it | ✅ `grep:` / `regex:` / `sym:` | ✅ rg patterns |
| Inside PDF / Office / archives / mail / 3D / LLM | ⬜ families vendored, search path not wired yet | ✅ |
| Name and content in one query | ✅ `report ext:pdf grep:tesla` | pattern + globs |
| Drop-in `rg` (flags, output, exit codes) | exit codes only | ✅ |
| Persistent index across runs | ✅ `--index` (pinned root, `--max-age`) | walk per run (pack store opt-in) |
| Pack-store control | `searchd --tv-dir` | ✅ `--tv-build`, `--engine auto\|tv\|rgx` |
| Forensic triage (magic ID, carving, strings, entropy, x509) | — | ✅ `inspect` adapter |

**The tech-savvy pick is rgx.** If you already think in `rg` flags and
regexes, script against `--json` submatches, pick engines, build pack stores,
or triage binaries — take [rgx](https://github.com/danalec/rgx).

**The broad, simple pick is rgy.** It answers both everyday questions over
the whole filesystem in one plain query language — words, plus
`ext:`/`type:`/`size:`/`dir:`/`re:`, `|` to alternate — with one daemon
underneath and nothing to configure:

```sh
searchctl --root ~ "invoice ext:pdf"      # which file
searchctl --root ~ "invoice grep:tesla"   # and what is inside it
```

> **Where the numbers live.** rgy's are targets until the plan's per-OS
> verification lands — name lookup **≤ 0.5 ms** median at 5M entries
> ([`docs/IMPLEMENTATION_PLAN.md`](docs/IMPLEMENTATION_PLAN.md) §2) — with
> measurements so far in [`benchmark/`](benchmark/). rgx's are measured and
> dated in its
> [`benchmark/`](https://github.com/danalec/rgx/tree/main/benchmark); quote
> them with their run date and host.

## Requirements

Rust **1.85** or newer. The workspace mixes editions on purpose: the `rgy`
crates and binaries are edition 2024, the vendored `rgx` crates stay on 2021
so they keep diffing cleanly against upstream.

Nothing to install beyond the OS. Everything else — the allocator, the
terminal layer, the SIMD — is compiled in; the platform code is chosen by
`cfg` at build time.

## Try it

```sh
# name: fuzzy words, filters narrow, hits ranked
cargo run --release -p searchctl -- --root crates "walk ext:rs"
# content: `grep:` literal, `regex:`, `sym:` — output is path:line:text
cargo run --release -p searchctl -- --root crates "grep:Listing"
# both in one query: the name narrows what is read, the content confirms
cargo run --release -p searchctl -- --root crates "walk ext:rs grep:Listing"
# the daemon: JSON lines in, hits frames out, index warm between queries
echo '{"op":"grep","q":"regex:fn.list"}' | cargo run --release -p searchd -- --root crates
# rgx --serve compat: one JSON argv-array per line, begin/end framing
echo '["-e","Listing"]' | cargo run --release -p searchd -- --root crates --serve
# transports: --listen uds:/tmp/rgy.sock (unix) or pipe:rgy-search (windows)
```

`type:` names kinds (`file`/`dir`/`link`, media kinds like `image`); a bare
extension goes in `ext:`. An unknown `type:` fails closed — warning on
stderr, zero matches, exit 1.

## Workspace status

The merge is landed end to end (plan first-PR sequence 1–12): name search,
content search, one daemon/CLI, and the polish layer. What is left is
verification on machines this host does not have.

### Name search

| Piece | Where | State |
|---|---|---|
| Name index v2 (intern, DFS, bitmaps, mmap blob) | `crates/name-core` (`name_index`) | ✅ from fsearch |
| Query syntax merge (fsearch tokens + zen-rs filters) | `crates/name-core` (`name_query`) | ✅ `type:`/`dir:`/`size:`/`re:`/globs/\|, fail-closed |
| fzf-v1 scorer + typos + top-k + NameCache + **SSE2 wide scorer** | `crates/name-core` (`name_query`) | ✅ parity-tested vs scalar |
| Live overlay | `crates/name-core` (`live`) | ✅ portable `lstat` |
| Index persistence (sidecar-pinned root) | `crates/name-core` (`persist`) | ✅ |

### Discovery

| Piece | Where | State |
|---|---|---|
| Seam (`Source`/`Watch`, DFS `Listing` blocks) | `crates/discovery` | ✅ portable walk |
| Windows backend (MFT + USN, RE-audit fixes) | `crates/discovery-windows` | ✅ `MftSource` / `UsnWatch` |
| Linux backend (getdents64 + inotify) | `crates/discovery-linux` | ✅ suite green under WSL (real kernel) |
| macOS backend (getattrlistbulk + FSEvents) | `crates/discovery-macos` | ⬜ transported, unverified — needs a real Mac |

> **The platform crates are not wired to a dispatch yet.** Nothing selects
> between `discovery-windows` / `-linux` / `-macos` and the portable walk, so
> the portable one runs everywhere. See `docs/freebsd-support.md` §3 (P2.1);
> it is the same question a FreeBSD backend would have to answer first.

### Content

| Piece | Where | State |
|---|---|---|
| Content index (trigram + bloom) | `crates/name-core` (`content`) | ✅ transported from fsearch |
| Confirm via rgx engine + `grep:`/`regex:`/`sym:` | `crates/name-core` (`content`), `crates/rgx-core` | ✅ |
| rgx engine, TV packs, adapters | `crates/rgx-*` | ✅ verbatim (re-synced with the cleanroom codec rewrites) |
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
| Named pipe / UDS fronts, `--serve` compat, `--tv-dir` | `bins/searchd` | ✅ |
| zen-rs bin / TUI (unchanged behaviour) | `bins/zen-rs`, `bins/search-ui` | ✅ |
| keep-warm / mimalloc / big-alloc (`BlobMut::anon`) | `crates/engine`, `crates/name-core` | ✅ |
| PGO (trained on the name bench) | `pgo.sh` (unix), `pgo.ps1` (Windows) | ✅ validated end to end on Windows |
| Bench harness + baselines | `crates/engine/examples`, `crates/name-core/examples`, `benchmark/` | ✅ |

### Perf gate

`cargo run --release -p engine --example name_bench` — a >20% regression on
any `benchmark/baseline-names.md` row fails the PR.
`benchmark/mmap-vs-read.md` records the mmap-vs-read measurements
(Linux + Windows) behind the access-shape choices in
[`docs/insights-from-rgx.md`](docs/insights-from-rgx.md).

## Docs

| File | What it is |
|---|---|
| [`docs/IMPLEMENTATION_PLAN.md`](docs/IMPLEMENTATION_PLAN.md) | The plan this workspace was built from: capability map, perf targets, per-OS parity matrix, phases. |
| [`docs/insights-from-rgx.md`](docs/insights-from-rgx.md) | What the rgx side taught about storage and access shapes. |
| [`docs/freebsd-support.md`](docs/freebsd-support.md) | Porting plan for FreeBSD, with the gaps ranked by whether they are wrong or merely slow. |
| [`benchmark/`](benchmark/) | Measured tables and the write-ups behind them. |

## Related projects

| Project | Repo | What it provides |
|---|---|---|
| `rgx` | [github.com/danalec/rgx](https://github.com/danalec/rgx) | content search engine, adapters, Tesla Valve packs |

## License

Split-licensed — the full map is in `COPYING`:

- **Apache-2.0 only**: the Zen components (`bins/zen-rs`, `bins/search-ui`,
  `crates/discovery-windows`, `name_core::index`) and the Tesla Valve pack
  store (`rgx-core` `tv`/`tv_runtime`). Each file carries its
  `SPDX-License-Identifier: Apache-2.0` header.
- **MIT OR Apache-2.0**: everything else. Commercial use is intended
  under Apache-2.0; the MIT option remains for anyone who prefers it.

Full texts: `LICENSE-APACHE`, `LICENSE-MIT`. Third-party notices: `NOTICE`.
