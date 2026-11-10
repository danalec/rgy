# Formula

## Overview
One Homebrew formula, `rgy-search.rb`, defining `RgySearch < Formula` — the
Homebrew package for the `rgy` binaries. The package is named `rgy-search`
because `rgy` is taken on crates.io; the binaries keep the names they have
everywhere else (`searchctl`, `searchd`, `rgy-autostart`, `rgxy`). The formula
names the 0.1.10.3 macOS archives by `url` and pins their real `sha256`
digests, rewritten by `scripts/publish-release.sh` from the combined
SHA256SUMS of the CI release run.

## Interface
- `RgySearch` (`< Formula`) — the formula class, carrying `desc`, `homepage`,
  `license "Apache-2.0"` and `version "0.1.10.3"` — `[rgy-search.rb:RgySearch]`.
- `on_arm` / `on_intel` — per-architecture `url` and `sha256` blocks —
  `[rgy-search.rb:on_arm]`, `[rgy-search.rb:on_intel]`.
- `install` — `bin.install` of `searchctl`, `searchd`, `rgy-autostart`, `rgxy`
  and `bin.install_symlink "rgy" => "searchctl"` — `[rgy-search.rb:install]`.
- `test do` — writes `invoice.txt` into `testpath` and asserts that
  `searchctl --root ... --index ... --count invoice` prints `1` —
  `[rgy-search.rb:test]`.

## Invariants
- The formula is named `rgy-search`, not `rgy`, and its version is pinned to
  `"0.1.10.3"` — `[rgy-search.rb:RgySearch]`, `[rgy-search.rb:version]`.
- The `sha256` values are real digests of the macOS archives built and smoked
  on the CI runners; `scripts/publish-release.sh` rewrites them from the
  combined SHA256SUMS in `.dist/` — `[rgy-search.rb:sha256]`.
- `rgy` is installed as a symlink to `searchctl`, not a copy —
  `[rgy-search.rb:install]`.
- The formula declares `license "Apache-2.0"` and carries an
  `SPDX-License-Identifier: Apache-2.0` header — `[rgy-search.rb:RgySearch]`.

## Side Effects
- Homebrew fetches the archive named by each `url` at install time —
  `[rgy-search.rb:on_arm]`, `[rgy-search.rb:on_intel]`.
- `bin.install` writes four binaries into the Homebrew prefix and
  `bin.install_symlink` creates the `rgy` symlink — `[rgy-search.rb:install]`.
- `test do` writes `invoice.txt` into `testpath` and shells out to the
  installed `searchctl` — `[rgy-search.rb:test]`.

## Verification
- Run: `cargo test --workspace --no-fail-fast --locked`
