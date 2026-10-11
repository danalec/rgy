# rgy

Unified search stack: whole-filesystem name search (fsearch + everything-rs) and content search (rgx), on Windows, Linux, and macOS.

See `IMPLEMENTATION_PLAN.md` for the full architecture and feature matrix.

## Workspace status

The name half of the merge is landed and usable; the content half is next.

| Piece | Where | State |
|---|---|---|
| Name index v2 (intern, DFS, bitmaps, mmap blob) | `crates/name-core` (`name_index`) | ✅ from fsearch |
| Query syntax merge (fsearch tokens + everything-rs filters) | `crates/name-core` (`name_query`) | ✅ `type:`/`dir:`/`size:`/`re:`/globs/\|, fail-closed |
| fzf-v1 scorer + typos + top-k + NameCache + **SSE2 wide scorer** | `crates/name-core` (`name_query`) | ✅ parity-tested vs scalar |
| Live overlay | `crates/name-core` (`live`) | ✅ portable `lstat` |
| One regex engine (vendored copy deleted) | `crates/name-core` (`regex`) → `rgx-core` | ✅ |
| Discovery seam (`Source`/`Watch`, DFS `Listing` blocks) | `crates/discovery` | ✅ portable walk |
| Windows backend (MFT + USN, RE-audit fixes) | `crates/discovery-windows` | ✅ `MftSource` / `UsnWatch` |
| Linux backend (getdents64 + inotify) | `crates/discovery-linux` | ✅ typechecked on `x86_64-unknown-linux-gnu` |
| Content index (trigram + bloom) | `crates/name-core` (`content`) | ✅ transported from fsearch |
| rgx engine, TV packs, adapters | `crates/rgx-*` | ✅ verbatim |
| `searchctl` (name CLI, rg exit codes, `--json`) | `bins/searchctl` | ✅ |
| `searchd` (JSON-lines daemon over stdio) | `bins/searchd` | ✅ name ops; `grep:` awaits Phase 4 |
| everything-rs bin / TUI (unchanged behaviour) | `bins/everything-rs`, `bins/search-ui` | ✅ |
| Bench harness + baseline | `crates/engine/examples`, `benchmark/` | ✅ Phase 0 gate |
| macOS backend (getattrlistbulk + FSEvents) | — | ⬜ stays in `../fsearch` until verified on macOS |
| Content confirm via rgx engine + `grep:`/`regex:`/`sym:` | — | ⬜ Phase 4 |
| Named pipe / UDS fronts, `--serve` compat, searchctl content | — | ⬜ Phase 5 |
| keep-warm / mimalloc / big-alloc / PGO | — | ⬜ Phase 6 |

Perf gate: `cargo run --release -p engine --example name_bench` — a >20%
regression on any `benchmark/baseline-names.md` row fails the PR.

## Try it

```sh
cargo run --release -p searchctl -- --root src "walk type:rs"
echo '{"op":"search","q":"walk","limit":5}' | cargo run --release -p searchd -- --root src
```

## Related projects

| Project | Path | What it provides |
|---|---|---|
| `rgx` | `../rgx` | content search engine, adapters, Tesla Valve packs |
| `fsearch` | `../fsearch` | macOS name discovery + fzf-v1 scoring |
| `everything-rs` | `../everything/everything-rs` | Windows NTFS MFT/USN indexer |
