# rgy Blueprint

## System Overview

`rgy` is a virtual Rust workspace (`[Cargo.toml:workspace]`, `resolver = "2"`)
that answers "which file?" over a whole filesystem and "what is inside it?" in
the same query. `[Cargo.toml:workspace.members]` lists 33 members: 27 library
crates under `crates/`, five shipped binaries under `bins/` and the `tests`
e2e harness crate. The root manifest's own comment states the architecture as
"One workspace, thin bins, one protocol", adds that `crates/rgx-*` are
"transported verbatim from ../rgx" and that `name-core` and
`discovery-windows` hold the zen-rs code with the RE-audit fixes.

`[Cargo.toml:workspace.package]` pins `edition = "2021"` and
`license = "Apache-2.0"` for the workspace; `[Cargo.toml:profile.release]`
sets `opt-level = 3`, `lto = "fat"`, `codegen-units = 1`, `strip = true`.

This bootstrap writes `EGM.md` contracts, not `README.md`: the repository's
human-maintained `README.md` (the map), `MANUAL.md` and `FORMATS.md` are left
untouched. The 51 contracts in trees this repository owns are tracked, so a
fresh CI checkout holds the same inputs as a working tree; `.gitignore`
ignores only the out-of-scope copies inside the vendored upstream trees
(`crates/rgx-*/**/EGM.md` and `crates/rar-codec/**/EGM.md`). The
`benchmark/`, `docs/` and `tests/` trees were pruned from the bootstrap
because they hold no source files by the skill's extension list or are
test-only; `bins/` and `crates/` are the two blueprint subtrees.

## Local Interface & Invariants

The root holds one in-scope source file by the skill's extension list:
`install.sh`, the unix installer (`install.ps1` is its PowerShell twin;
`pgo.sh` / `pgo.ps1` are gitignored scratch). Verified symbols and flags:

- `[install.sh:version]` = `rgy-v0.1.10.3`, the four-number release tag the crates' three-number 0.1.10 line ships as; `[install.sh:install_dir]` = `$HOME/.local/bin`; `[install.sh:base_url]` = the GitHub releases download URL.
- `[install.sh:expected_linux_x86_64]`, `[install.sh:expected_linux_aarch64]`, `[install.sh:expected_macos_x86_64]`, `[install.sh:expected_macos_aarch64]`, `[install.sh:expected_freebsd_x86_64]` — one pinned uppercase-hex SHA256 digest per platform/architecture, each a real digest of an archive built and smoked on the runner that has that platform. `publish-release.sh` rewrites them from the combined SHA256SUMS of the CI release run.
- `[install.sh:placeholder]` — the all-zero digest standing for "an archive nobody has published yet".
- `[install.sh:rgy_read_options]` — the fallback option parser used when the installer is piped into a shell with no tree to read `scripts/lib/options.sh` from; it refuses an unknown option with `unknown option:` and exit 2.
- `[install.sh:profile_path_entries]`, `[install.sh:already_on_path]` — PATH helpers.
- `[install.sh:skip_checksum]`, `[install.sh:no_autostart]`, `[install.sh:no_launcher]`, `[install.sh:activate]`, `[install.sh:uninstall]` — option flags; `activate` is the post-install switch that registers and arms the daemon on an install that is already in place (no download), and `uninstall` is the switch that reverses the install.
- `[install.sh:rgy_register_daemon]` — the one function both `--activate` and the end of a normal install call: it resolves `rgy-autostart` next to the install dir first and on the PATH second (a package manager put the binaries on the PATH, not in `install_dir`), runs `rgy-autostart install` with `--launcher` when `rgxy` is present, and prints the same result for both callers. A normal install ignores its exit code; `--activate` propagates it.
- `[install.sh:rgy_uninstall]` — the uninstall branch: `rgy-autostart uninstall` takes out both login entries, the twelve copied files and the `docs/` tree are removed by name, and the exact two-line PATH block is filtered out of the profile. The install directory itself is never removed.
- `[install.sh:tmp]` = `mktemp -d`; `[install.sh:stage]`, `[install.sh:stage_root]` — the unpack staging directory.
- `[Cargo.toml:workspace.members]`, `[Cargo.toml:workspace.package]`, `[Cargo.toml:profile.release]`.

Local invariants read from the file:

- `[install.sh:expected_linux_x86_64]` — the digest is pinned inside the installer itself, "so a tampered or substituted download is refused before anything is unpacked"; `scripts/publish-release.sh` rewrites the pins from the artifacts it has just built.
- `[install.sh:placeholder]` — "A platform whose archive this release does not carry would hold the placeholder digest, and that is refused by name, before anything is downloaded".
- `[install.sh:rgy_read_options]` — the option vocabulary, digest helper and refusal behaviour are shared with `install.ps1` through `scripts/lib/options.sh` and `scripts/lib/digest.sh`, so the two twins answer the same question the same way.
- `[install.sh:rgy_uninstall]` — "--uninstall takes out exactly what install put in"; the same URL installs and uninstalls, so there is no second script to keep in step with this one.
- `[install.sh:rgy_register_daemon]` — "--activate is a post-install action, so it never reaches the download"; the daemon registration is one function shared by the post-install switch and a normal install, so an install made with `--no-autostart`, a package-manager install, or an `rgy-autostart uninstall` is put back with the same words and the same result.

## Subdirectories & System Boundaries

- [bins](./bins/EGM.md) — five shipped thin binaries: `rgxy`, `search-ui`, `searchctl`, `searchd`, `zen-rs` (`[Cargo.toml:workspace.members]`).
- [crates](./crates/EGM.md) — 27 library crates: fifteen owned (`rgy-*` / `rgy_*`, edition 2024) and twelve vendored upstream trees (`rgy-vendored-*` / `rgx_*`, edition 2021).
- [Formula](./Formula/EGM.md) — the Homebrew formula `rgy-search.rb`.
- [scripts](./scripts/EGM.md) — release, publish, sync, vendoring, twin-check and perf-gate shell/Python tooling, plus [lib](./scripts/lib/EGM.md) helpers.
- [tools](./tools/EGM.md) — Python probe, calibration and smoke helpers.

Contracts were written for 95 directories discovered by breadth-first scan and
pruned bottom-up; hidden/system directories (`.git`, `.release-tree`, `target`,
`.worktrees`, `.github`, …) and test-only/fixture directories (`tests/`,
`testdata/`, `fixtures/`) were excluded and carry no links here.

## Global Invariants

- `[Cargo.toml:workspace.members]` — the 33 listed paths are the complete workspace membership; the workspace root is virtual (root manifest comment), which is why the `tests` harness is its own member crate.
- `[Cargo.toml:workspace.package]` — `edition = "2021"`, `license = "Apache-2.0"`; the owned crates override to `edition = "2024"` in their own manifests while the vendored trees stay on edition 2021.
- `[crates/rgx-core/Cargo.toml:include]` — vendored manifests carry the negation `"!/src/**/EGM.md"` with a comment calling EGM.md "the working documents of the review loop ... gitignored ... not source"; contracts inside `src/` are expected and excluded from published crates.
- `[crates/par/Cargo.toml:package.description]` — `rgy-par` declares no dependencies on purpose (manifest comment) and gates threads behind `[crates/par/Cargo.toml:features.std]`.
- `[crates/discovery/src/dir.rs:Win32FindDataW]` — the Windows directory struct is fixed at compile time (`size_of::<Win32FindDataW>() == 592`), with `c_alternate_file_name: [u16; 14]`; a platform-written struct is written whole.

## Global Side Effects

- `[install.sh:base_url]` — downloads a release tarball over HTTPS, verifies its SHA256, unpacks four binaries into `[install.sh:install_dir]`, prepends that directory to PATH, and registers the daemon to start at login; `[install.sh:tmp]` allocates a temporary staging directory with `mktemp -d`.
- `[crates/rgx-adapters-docs/Cargo.toml:package.description]` — shells out to external programs (`soffice`, `tesseract`) on the host.
- `[crates/discovery-windows/src/win.rs:CreateFileW]` — the Windows backend declares its Win32 calls by hand in `unsafe extern "system"` rather than linking the `windows` crate; the module and its manifest carry no such dependency.
- `[Cargo.toml:profile.release]` — `lto = "fat"` and `codegen-units = 1` shape the release build; a PGO build adds a trained profile on top (root manifest comment).
- Per-crate filesystem, platform-call and process side effects are rolled up in each crate's own `EGM.md`.

## System Verification

- Run: `cargo test --workspace --no-fail-fast --locked`
