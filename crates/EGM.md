# crates

## Overview

The library half of the `rgy` workspace. `[Cargo.toml:workspace.members]` at the
repository root names 27 crate directories under `crates/`: fifteen crates this
repository owns and twelve vendored upstream trees (`crates/rgx-core`,
`crates/rar-codec` and the ten `crates/rgx-adapters-*` families). The root
manifest's own comment says `crates/rgx-*` are "transported verbatim from
../rgx" and that `name-core` and `discovery-windows` carry the zen-rs code with
the RE-audit fixes.

The naming split is visible in the manifests and is not a guess: owned crates
declare `package.name = "rgy-<dir>"` with `edition = "2024"` (for example
`[crates/par/Cargo.toml:package.name]` is `rgy-par`), while vendored crates
declare `package.name = "rgy-vendored-*"` with `[lib] name = "rgx_*"` and
`edition = "2021"` (for example `[crates/rgx-core/Cargo.toml:package.name]` is
`rgy-vendored-core` and `[crates/rgx-core/Cargo.toml:lib.name]` is `rgx_core`).
Four owned crates (`discovery-freebsd`, `discovery-linux`, `discovery-macos`,
`discovery-windows`) are the platform backends behind the portable `discovery`
seam.

## Subdirectories & Boundaries

- [adapters](./adapters/EGM.md) — `rgy-adapters`: "The rgx adapter registry: the formats a search can read into" (`[adapters/Cargo.toml:package.description]`).
- [autostart](./autostart/EGM.md) — `rgy-autostart`: "Registering the rgy search daemon to start at login, per platform" (`[autostart/Cargo.toml:package.description]`); declares the `rgy-autostart` binary.
- [discovery](./discovery/EGM.md) — `rgy-discovery`: "Platform discovery seam (Source/Watch) and the shared change vocabulary" (`[discovery/Cargo.toml:package.description]`).
- [discovery-dispatch](./discovery-dispatch/EGM.md) — `rgy-discovery-dispatch`: "The Source selector: the native walk per platform by cfg, the portable walk elsewhere" (`[discovery-dispatch/Cargo.toml:package.description]`).
- [discovery-freebsd](./discovery-freebsd/EGM.md) — `rgy-discovery-freebsd`: "FreeBSD discovery backend: getdirentries bulk walk with d_type from the dirent" (`[discovery-freebsd/Cargo.toml:package.description]`).
- [discovery-linux](./discovery-linux/EGM.md) — `rgy-discovery-linux`: "Linux discovery backend: getdents64 bulk walk + recursive inotify" (`[discovery-linux/Cargo.toml:package.description]`).
- [discovery-macos](./discovery-macos/EGM.md) — `rgy-discovery-macos`: "macOS discovery backend: getattrlistbulk walk + FSEvents watch" (`[discovery-macos/Cargo.toml:package.description]`).
- [discovery-windows](./discovery-windows/EGM.md) — `rgy-discovery-windows`: "Windows discovery backend: NTFS MFT streaming + USN journal (from zen-rs)" (`[discovery-windows/Cargo.toml:package.description]`).
- [engine](./engine/EGM.md) — `rgy-engine`: "Orchestration: keep-warm, TV pack store glue" (`[engine/Cargo.toml:package.description]`).
- [launcher](./launcher/EGM.md) — `rgy-launcher`: "The launcher's model: the query, the results, the actions, the hotkey" (`[launcher/Cargo.toml:package.description]`).
- [launcher-x11](./launcher-x11/EGM.md) — `rgy-launcher-x11`: "The launcher's window on X11 (Linux and FreeBSD): a floating overlay and a system-tray icon, drawn in pure Rust" (`[launcher-x11/Cargo.toml:package.description]`). One X11 backend, shared by the two kernels.
- [launcher-windows](./launcher-windows/EGM.md) — `rgy-launcher-windows`: "The launcher's window on Windows: a floating overlay, drawn with GDI" (`[launcher-windows/Cargo.toml:package.description]`).
- [name-core](./name-core/EGM.md) — `rgy-name-core`: "Name index, query filters, fuzzy ranking (name engine + zen query vocabulary)" (`[name-core/Cargo.toml:package.description]`).
- [par](./par/EGM.md) — `rgy-par`: "The parallelism rgy carries: split the work, run the pieces, join them" (`[par/Cargo.toml:package.description]`); declares no dependencies on purpose.
- [pick](./pick/EGM.md) — `rgy-pick`: "Handing rgy's results to an external picker (fzf and its family)" (`[pick/Cargo.toml:package.description]`).
- [proto](./proto/EGM.md) — `rgy-proto`: "JSON-lines protocol types shared by daemon, stdio, and CLI (skeleton)" (`[proto/Cargo.toml:package.description]`).
- The other twelve members are vendored upstream trees (`rar-codec`,
  `rgx-core` and the ten `rgx-adapters-*` families). They deliberately carry
  no tracked `EGM.md`: §1 of the working notes leaves those trees to
  `scripts/sync-rgx-upstream.sh`, and `.egmrc`'s `scope` excludes them from
  the contract set. A link to a contract there would be broken on a fresh
  checkout, where the ignored scratch files do not exist. Their boundaries
  are upstream's own per-crate manifests, not a contract written here.

## Interface

- `[Cargo.toml:workspace.members]` — the 28 `crates/*` directories above are the complete library membership of the workspace; `bins/*` and `tests` are separate members.
- `[Cargo.toml:workspace.package]` — `edition = "2021"`, `license = "Apache-2.0"` are the workspace defaults; the owned crates declare `edition = "2024"` in their own manifests.
- `[crates/rgx-core/Cargo.toml:lib.name]` — the vendored crates keep upstream's library target names (`rgx_core`, `rgx_adapters_*`, `rar_codec`) while publishing under `rgy-vendored-*` package names.
- The public symbols of each crate are rolled up in that crate's own `./<crate>/EGM.md`, and the symbols of each `src/` tree in `./<crate>/src/EGM.md`.

## Invariants

- `[crates/rgx-core/Cargo.toml:include]` — the published-crate include list carries the negation `"!/src/**/EGM.md"`, with a comment describing EGM.md as "the working documents of the review loop ... gitignored ... not source". The same negation appears in `[crates/rgx-adapters-inspect/Cargo.toml:include]`. Contracts inside `src/` are therefore expected by the tree and excluded from published crates.
- `[crates/par/Cargo.toml:package.description]` — `rgy-par` "has no dependencies, on purpose: this crate is the floor the rest of the workspace stands on" (manifest comment); `[crates/par/Cargo.toml:features.std]` is enabled by default (`default = ["std"]`) and gates threads; it is disabled with `--no-default-features`.
- `[Cargo.toml:workspace.members]` — no crate outside this list is a workspace member; the workspace root is virtual (root manifest comment).

## Side Effects

- `[rgx-adapters-docs/Cargo.toml:package.description]` — shells out to external tools (`soffice`, `tesseract`) on the host; this is the only vendored family whose manifest declares that behaviour.
- `[crates/par/Cargo.toml:features.std]` — the default-on `std` feature enables threads; with `--no-default-features` the work runs in order on the calling thread (manifest comment).
- The filesystem, platform-call and process side effects of each crate's source are rolled up in that crate's own `EGM.md` and its `src/EGM.md`.

## Verification

- Run: `cargo test --workspace --no-fail-fast --locked`
