# Formula

## Overview
One Homebrew formula, `rgy-search.rb`, defining `RgySearch < Formula` — the
Homebrew package for the `rgy` binaries. The package is named `rgy-search`
because `rgy` is taken on crates.io; the binaries keep the names they have
everywhere else (`searchctl`, `searchd`, `rgy-autostart`, `rgxy`). The formula
is STAGED: the `url` values name the 0.1.10.0 macOS archives and the `sha256`
values are placeholders that `scripts/publish-release.sh` rewrites from the
real archives on publish day.

## Interface
- `RgySearch` (`< Formula`) — the formula class, carrying `desc`, `homepage`,
  `license "Apache-2.0"` and `version "0.1.10.0"` — `[rgy-search.rb:RgySearch]`.
- `on_arm` / `on_intel` — per-architecture `url` and `sha256` blocks —
  `[rgy-search.rb:on_arm]`, `[rgy-search.rb:on_intel]`.
- `install` — `bin.install` of `searchctl`, `searchd`, `rgy-autostart`, `rgxy`
  and `bin.install_symlink "rgy" => "searchctl"` — `[rgy-search.rb:install]`.
- `test do` — writes `invoice.txt` into `testpath` and asserts that
  `searchctl --root ... --index ... --count invoice` prints `1` —
  `[rgy-search.rb:test]`.

## Invariants
- The formula is named `rgy-search`, not `rgy`, and its version is pinned to
  `"0.1.10.0"` — `[rgy-search.rb:RgySearch]`, `[rgy-search.rb:version]`.
- The `sha256` values are all-zero STAGED placeholders; the real digests and
  URLs are rewritten by `scripts/publish-release.sh` from the archives in
  `.dist/` on publish day — `[rgy-search.rb:sha256]`.
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
