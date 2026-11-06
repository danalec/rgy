# The platform pairs

Every `.ps1` has a `.sh` beside it, and the twins are checked by running
both and diffing the result. `scripts/check-egm-contracts.py` reads this
table (not `AGENTS.md`, which is gitignored working notes in this tree) and
fails on a listed script that is gone or a tracked `.sh`/`.ps1` pair that
is not listed. The pair list is derived from `git ls-files`, so a fresh CI
checkout and this working tree have the same inputs.

| script | what it does |
|---|---|
| `install.sh`, `install.ps1` | the one-line installers |
| `scripts/apply-vendored-edits.sh`, `scripts/apply-vendored-edits.ps1` | apply the local patches to the vendored copies |
| `scripts/build-release.sh`, `scripts/build-release.ps1` | build the release archives |
| `scripts/check-drift.sh`, `scripts/check-drift.ps1` | compare the vendored trees with upstream |
| `scripts/check-egm-contracts.sh`, `scripts/check-egm-contracts.ps1` | the EGM contract gate (thin wrappers around the Python checker) |
| `scripts/check-vendoring.sh`, `scripts/check-vendoring.ps1` | the vendoring ledger and its invariants |
| `scripts/fetch-ci-assets.sh`, `scripts/fetch-ci-assets.ps1` | fetch the CI fixtures |
| `scripts/lib/digest.sh`, `scripts/lib/digest.ps1` | hashing helper |
| `scripts/lib/options.sh`, `scripts/lib/options.ps1` | option parsing helper |
| `scripts/lib/version.sh`, `scripts/lib/version.ps1` | version helper |
| `scripts/publish-crates.sh`, `scripts/publish-crates.ps1` | publish the crates in dependency order |
| `scripts/publish-homebrew.sh`, `scripts/publish-homebrew.ps1` | publish the Homebrew formula |
| `scripts/publish-release.sh`, `scripts/publish-release.ps1` | publish the release |
| `scripts/rgy-name-protocol.sh`, `scripts/rgy-name-protocol.ps1` | the name protocol probe |
| `scripts/smoke-serve.sh`, `scripts/smoke-serve.ps1` | smoke-test the daemon |
| `scripts/test-publish-release.sh`, `scripts/test-publish-release.ps1` | prove the installers against a built archive |
