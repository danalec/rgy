# scripts

## Overview
Release, vendoring, benchmark, contract-gate and smoke automation for the
workspace: 22 source files (15 `.sh` and 7 `.py`). The scripts publish to
crates.io and Homebrew, build and pack the release archives, sync and check
the vendored rgx trees, gate the name-search perf baseline, validate the EGM
contracts (`check-egm-contracts.py` and its twin wrappers), and compare the
`.sh` / `.ps1` twin pairs.
Each `.sh` here is documented as the twin of a `.ps1` beside it (the `.ps1`
files are not source by this skill's extension list): the tree's rule is that
the two produce the same text and the same exit code on the same input, and
`check-twins.sh` runs both and diffs the result. The shared helpers live in
the child `scripts/lib` and are sourced by nearly every script here.

## Interface
- `[lib](./lib/EGM.md)` — the shared shell helper library (`rgy_read_options`, `rgy_sha256`, `rgy_release_version`) sourced by the scripts here.
- Release/publish switches, declared through `rgy_read_options`: `[build-release.sh:--skip-tests]`, `[build-release.sh:--version]`, `[build-release.sh:--variant]`; `[publish-release.sh:--dry-run]`, `[publish-release.sh:--variant]`; `[publish-crates.sh:--dry-run]`; `[publish-homebrew.sh:--version]`; `[release-linux.sh:--host]`; `[fetch-ci-assets.sh:--dir]`.
- Vendoring tooling: `[sync-rgx-upstream.sh:--upstream]`, `[sync-rgx-upstream.sh:--prune]`, `[apply-vendored-edits.sh:--upstream]`, `[derive-vendored-patches.py:--upstream]`, `[derive-vendored-patches.py:--check]`, `[check-drift.sh:--upstream]`, `[check-vendoring.sh:rgy_read_options]`.
- Benchmark gates: `[bench-names.py:measure]`, `[bench-names.py:check]`, `[bench-names.py:render]`; `[rgy-name-protocol.sh:--scales]`, `[rgy-name-protocol.sh:--rounds]`, `[rgy-name-protocol.sh:--queries]`; `[mem-peak.py:--hold-seconds]`; `[publish-plan.py:--registry]`, `[publish-plan.py:wait_for]`; `[bump-version.py:--check]`.
- Helpers and checks: `[check-twins.sh:PROBE]`, `[check-twins.sh:normalize()]`, `[smoke-serve.sh:json_str()]`, `[gen_xz_fixtures.py:PLAIN]`, `[gen_xz_fixtures.py:CHECK]`, `[test-publish-release.sh:fail()]`.
- EGM contract gate: `[check-egm-contracts.py:validate()]`, `[check-egm-contracts.py:generated_problems()]`, `[check-egm-contracts.py:self_test()]`, with thin twins `[check-egm-contracts.sh]`, `[check-egm-contracts.ps1]`; driven by `[../.egmrc]`.
- Locally defined functions: `[check-drift.sh:upstream_blob()]`, `[check-drift.sh:patch_verdict()]`, `[check-vendoring.sh:find_files()]`, `[build-release.sh:asset_of()]`, `[publish-release.sh:json_escape()]`, `[sync-rgx-upstream.sh:apply_local_patch()]`, `[rgy-name-protocol.sh:get_verdict()]`.

## Invariants
- The tree's twin rule: every `.ps1` has a `.sh` beside it and the two are
  twins — the same text and the same exit code; `check-twins.sh` runs both with
  an option no script knows and diffs the normalized output —
  `[check-twins.sh:PROBE]`, `[check-twins.sh:normalize()]`.
- An option no script knows must be refused with `exit 2` before the script
  touches anything — `[check-twins.sh:PROBE]`.
- `--dry-run` prints the plan and publishes nothing —
  `[publish-crates.sh:--dry-run]`.
- The perf gate is `THRESHOLD_PCT = 20.0` (a >20% regression fails),
  `THREADS = 4`, and `MIN_GATE_US = 10.0` below which a row is not gated; the
  statistic is the best of `--runs` and exit code `2` means undecidable noise —
  `[bench-names.py:THRESHOLD_PCT]`, `[bench-names.py:THREADS]`, `[bench-names.py:MIN_GATE_US]`.
- The publish order is a dependency order computed from `cargo metadata`,
  with the launcher faces (`FACES = ("rgxy",)`) last and a crate that can
  never be published refused — `[publish-plan.py:FACES]`, `[publish-plan.py:workspace_packages()]`.
- Unless `--skip-tests`, the build fails on any clippy warning
  (`-D warnings`) and any failing test, and every cargo call carries
  `--locked` — `[build-release.sh:--skip-tests]`.
- The drift gate compares upstream's blob at the pin with the repository's
  index; a patch that no longer applies is a failure —
  `[check-drift.sh:upstream_blob()]`, `[check-drift.sh:patch_verdict()]`.
- The sync copies byte-for-byte from the upstream commit and removes the
  `EXCLUDE` paths; it does not merge — `[sync-rgx-upstream.sh:is_excluded()]`.
- The publish `variant` is one of `portable|avx2|znver4` —
  `[publish-release.sh:--variant]`.

## Side Effects
- `release-linux.sh` pushes the ref to forgejo, runs `ssh`/`scp` against
  `RGY_RELEASE_HOST`, falls back to `git archive | scp`, and writes `.dist/` —
  `[release-linux.sh:RGY_RELEASE_HOST]`.
- `publish-homebrew.sh` checks the artifact with `curl`, clones
  `Homebrew/homebrew-core`, commits, pushes and opens a pull request with
  `gh pr create` — `[publish-homebrew.sh:CORE_REPO]`.
- `publish-crates.sh` runs `cargo publish -p <name> --locked` and needs
  `CARGO_REGISTRY_TOKEN` — `[publish-crates.sh:cargo publish]`.
- `publish-release.sh` reads forgejo credentials via `git credential fill`,
  uploads release assets and drives GitHub through `gh` —
  `[publish-release.sh:fetch_credentials()]`.
- `fetch-ci-assets.sh` calls `gh run view` / `gh run download` and rewrites
  `.dist/` — `[fetch-ci-assets.sh:gh run download]`.
- `build-release.sh` runs `cargo build`/`clippy`/`test` and writes the
  archives and `SHA256SUMS` into `.dist/` — `[build-release.sh:asset_of()]`, `[build-release.sh:smoke()]`.
- `sync-rgx-upstream.sh` and `apply-vendored-edits.sh` rewrite files under
  `crates/` and run `git apply` in a scratch tree —
  `[sync-rgx-upstream.sh:apply_local_patch()]`.
- `derive-vendored-patches.py` writes `.patch` files under its `PATCH_DIR` and
  `bump-version.py` rewrites `Cargo.toml` manifests —
  `[derive-vendored-patches.py:PATCH_DIR]`, `[bump-version.py:rewrite()]`.
- `gen_xz_fixtures.py` writes `.xz` fixture streams —
  `[gen_xz_fixtures.py:out.write_bytes()]`.
- `bench-names.py` runs the name bench and writes `BASELINE_CSV` /
  `BASELINE_MD` — `[bench-names.py:BASELINE_CSV]`, `[bench-names.py:write_csv()]`.
- `mem-peak.py` spawns a child, reads `/proc/<pid>/status` on Linux and calls
  `GetProcessMemoryInfo` on Windows — `[mem-peak.py:peak_rss_bytes()]`.
- `check-twins.sh` runs each pair under `bash` and `pwsh` —
  `[check-twins.sh:PWSH]`.

## Verification
- Run: `cargo test --workspace --no-fail-fast --locked`
