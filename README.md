# rgy

Unified search stack: whole-filesystem name search (fsearch + zen-rs) and content search (rgx), on Windows, Linux, and macOS.

See `IMPLEMENTATION_PLAN.md` for the full architecture and feature matrix.

## Workspace status

The merge is landed end to end (plan first-PR sequence 1–12): name search,
content search, one daemon/CLI, and the polish layer. What is left is
verification on machines this host does not have.

| Piece | Where | State |
|---|---|---|
| Name index v2 (intern, DFS, bitmaps, mmap blob) | `crates/name-core` (`name_index`) | ✅ from fsearch |
| Query syntax merge (fsearch tokens + zen-rs filters) | `crates/name-core` (`name_query`) | ✅ `type:`/`dir:`/`size:`/`re:`/globs/\|, fail-closed |
| fzf-v1 scorer + typos + top-k + NameCache + **SSE2 wide scorer** | `crates/name-core` (`name_query`) | ✅ parity-tested vs scalar |
| Live overlay | `crates/name-core` (`live`) | ✅ portable `lstat` |
| One regex engine (vendored copy deleted) | `crates/name-core` (`regex`) → `rgx-core` | ✅ (`rgx-core` stays `no_std`) |
| Discovery seam (`Source`/`Watch`, DFS `Listing` blocks) | `crates/discovery` | ✅ portable walk |
| Windows backend (MFT + USN, RE-audit fixes) | `crates/discovery-windows` | ✅ `MftSource` / `UsnWatch` |
| Linux backend (getdents64 + inotify) | `crates/discovery-linux` | ✅ suite green under WSL (real kernel) |
| Content index (trigram + bloom) | `crates/name-core` (`content`) | ✅ transported from fsearch |
| Content confirm via rgx engine + `grep:`/`regex:`/`sym:` | `crates/name-core` (`content`), `crates/rgx-core` | ✅ |
| rgx engine, TV packs, adapters | `crates/rgx-*` | ✅ verbatim (re-synced with the cleanroom codec rewrites) |
| `searchctl` (name + content CLI, rg exit codes, `--json`) | `bins/searchctl` | ✅ |
| `searchd` (JSON-lines daemon) | `bins/searchd` | ✅ name + `grep:`/`regex:`/`sym:` ops |
| Named pipe / UDS fronts, `--serve` compat, `--tv-dir` | `bins/searchd` | ✅ |
| zen-rs bin / TUI (unchanged behaviour) | `bins/zen-rs`, `bins/search-ui` | ✅ |
| keep-warm / mimalloc / big-alloc (`BlobMut::anon`) | `crates/engine`, `crates/name-core` | ✅ |
| PGO (trained on the name bench) | `pgo.sh` (unix), `pgo.ps1` (Windows) | ✅ validated end to end on Windows |
| Bench harness + baselines | `crates/engine/examples`, `crates/name-core/examples`, `benchmark/` | ✅ |
| macOS backend (getattrlistbulk + FSEvents) | `crates/discovery-macos` | ⬜ transported, unverified — needs a real Mac |

Perf gate: `cargo run --release -p engine --example name_bench` — a >20%
regression on any `benchmark/baseline-names.md` row fails the PR.
`benchmark/mmap-vs-read.md` records the mmap-vs-read measurements
(Linux + Windows) behind the access-shape choices in
`docs/insights-from-rgx.md`.

## Try it

```sh
cargo run --release -p searchctl -- --root src "walk ext:rs"
cargo run --release -p searchctl -- --root src "grep:Listing"
echo '{"op":"grep","q":"regex:fn.list"}' | cargo run --release -p searchd -- --root src
# rgx --serve compat: one JSON argv-array per line, begin/end framing
echo '["-e","Listing"]' | cargo run --release -p searchd -- --root src --serve
# transports: --listen uds:/tmp/rgy.sock (unix) or pipe:rgy-search (windows)
```

`type:` names kinds (`file`/`dir`/`link`, media kinds like `image`); a bare
extension goes in `ext:`. An unknown `type:` fails closed — warning on
stderr, zero matches, exit 1.

## Related projects

| Project | Path | What it provides |
|---|---|---|
| `rgx` | `../rgx` | content search engine, adapters, Tesla Valve packs |
| `fsearch` | `../fsearch` | macOS name discovery + fzf-v1 scoring |
| `zen-rs` | `../zen/zen-rs` | Windows NTFS MFT/USN indexer |

## License

Split-licensed — the full map is in `COPYING`:

- **Apache-2.0 only**: the Zen components (`bins/zen-rs`, `bins/search-ui`,
  `crates/discovery-windows`, `name_core::index`) and the Tesla Valve pack
  store (`rgx-core` `tv`/`tv_runtime`). Each file carries its
  `SPDX-License-Identifier: Apache-2.0` header.
- **MIT OR Apache-2.0**: everything else. Commercial use is intended
  under Apache-2.0; the MIT option remains for anyone who prefers it.

Full texts: `LICENSE-APACHE`, `LICENSE-MIT`. Third-party notices: `NOTICE`.
