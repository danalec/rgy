# rgy

Whole-filesystem name search and content search in one stack: fsearch's name
engine and query ergonomics, zen-rs's Windows discovery, rgx's content engine,
adapters and pack store. Windows, Linux and macOS.

**Which file?** — asked over the whole filesystem, then **what is inside
it?** in the same query.

```sh
searchctl --root ~ "invoice ext:pdf"      # which file
searchctl --root ~ "invoice grep:tesla"   # and what is inside it
```

## The docs

| file | what it is | for whom |
|---|---|---|
| [MANUAL.md](MANUAL.md) | how to use it: the query language, the flags, the daemon, the pack store | everyone |
| [FORMATS.md](FORMATS.md) | what it reads inside a file, format by format | everyone |
| [docs/README.md](docs/README.md) | the map of the rest: the scoreboard, the parity matrices, the design, the on-disk formats | maintainers |
| [benchmark/](benchmark/) | the measured tables and the write-ups behind them | everyone |

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
| Pack-store control | `searchd --tv-dir`, `rgx-tv build/search/update/inspect` | ✅ `--tv-build`, `--engine auto\|tv\|rgx` |
| Forensic triage (magic ID, carving, strings, entropy, x509) | — | ✅ `inspect` adapter |

**The tech-savvy pick is rgx.** If you already think in `rg` flags and
regexes, script against `--json` submatches, pick engines, build pack stores,
or triage binaries — take [rgx](https://github.com/danalec/rgx).

**The broad, simple pick is rgy.** It answers both everyday questions over
the whole filesystem in one plain query language — words, plus
`ext:`/`type:`/`size:`/`dir:`/`re:`, `|` to alternate — with one daemon
underneath and nothing to configure.

> **Where the numbers live.** rgy's are targets until the plan's per-OS
> verification lands — name lookup **≤ 0.5 ms** median at 5M entries
> ([`docs/design.md`](docs/design.md) §1) — with
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

There are packages for all three operating systems — winget, Homebrew and
the tarball — but none of them has been published yet. The
[manual](MANUAL.md#2-install-it) has the commands and the state.

## Try it

```sh
# name: fuzzy words, filters narrow, hits ranked
cargo run --release -p rgy-searchctl -- --root crates "walk ext:rs"
# content: `grep:` literal, `regex:`, `sym:` — output is path:line:text
cargo run --release -p rgy-searchctl -- --root crates "grep:Listing"
# both in one query: the name narrows what is read, the content confirms
cargo run --release -p rgy-searchctl -- --root crates "walk ext:rs grep:Listing"
# the daemon: JSON lines in, hits frames out, index warm between queries
echo '{"op":"grep","q":"regex:fn.list"}' | cargo run --release -p rgy-searchd -- --root crates
# rgx --serve compat: one JSON argv-array per line, begin/end framing
echo '["-e","Listing"]' | cargo run --release -p rgy-searchd -- --root crates --serve
# transports: --listen uds:/tmp/rgy.sock (unix) or pipe:rgy-search (windows)
```

`type:` names kinds (`file`/`dir`/`link`, media kinds like `image`); a bare
extension goes in `ext:`. An unknown `type:` fails closed — warning on
stderr, zero matches, exit 1.

The [manual](MANUAL.md) has the rest: the query language, the flags, the
daemon, and the pack store.

## Status

The merge is landed end to end: name search,
content search, one daemon/CLI, and the polish layer. What is left is
verification on machines this host does not have — macOS above all.

**[docs/parity.md](docs/parity.md)** has both matrices: the per-OS one — 53
features, none of them carrying a gap (a `G` cell — work required) — and what
Listary does that rgy does not. A gap that is not on the known-gaps list fails
CI, so a new one cannot appear quietly. The degraded cells (`D`) are the limits
that are documented rather than closed.

The perf gate fails a PR on a >20% regression against any
[`benchmark/baseline-names.md`](benchmark/baseline-names.md) row.

The crates are on crates.io, all fourteen at 0.1.0: `rgy-name-core`,
`rgy-discovery`, `rgy-engine`, `rgy-proto` and the rest. They carry the
`rgy-` prefix because `name-core`, `discovery`, `engine`, `proto`, `par`
and `zen-rs` are taken on crates.io.

## Related projects

| Project | Repo | What it provides |
|---|---|---|
| `rgx` | [github.com/danalec/rgx](https://github.com/danalec/rgx) | content search engine, adapters, Tesla Valve packs |

## License

Apache-2.0 for the product and the end-to-end harness, MIT OR Apache-2.0 for
the tooling around it — the full map is in `COPYING`:

- **Apache-2.0 only**: the index base (`crates/name-core`, `crates/discovery*`,
  `crates/engine`, `crates/proto`), the vendored rgx engine and adapters
  (`crates/rgx-*`, `crates/rar-codec`), the binaries (`bins/*`) and the e2e
  harness (`tests/`). Every file carries its `SPDX-License-Identifier:` header,
  and the manifests state the same terms.
- **MIT OR Apache-2.0**: the tooling (`scripts/`, `tools/`, `benchmark/`).
  Commercial use is licensed under Apache-2.0; the MIT option remains for
  anyone who prefers its terms.

Three files in `crates/name-core` are transported from `fsearch` (MIT) and keep
its terms; the vendored trees are Apache-2.0, and the layouts they encode that
were learned from other projects are recorded in `NOTICE`.
Full texts: `LICENSE-APACHE`, `LICENSE-MIT`. Third-party notices: `NOTICE`
and the notice of the one upstream whose code is carried here, in `LICENSE-MIT-FSEARCH`.
