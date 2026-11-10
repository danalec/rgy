<!-- The release page, in the repository.

     This file IS the body of the release: the Forgejo publish script and the
     GitHub mirror both read it, so the two pages say the same thing, and the
     promise the README makes -- "the release page is the changelog" -- is
     kept by the tree and not by a memory.

     Two markers below are for the scripts, and the rendered page does not
     show them. `<!-- assets: ... -->` names the files the release carries,
     and the publish step compares that list with the files it is about to
     upload: a page that names an archive the release does not carry cannot
     be published. `<!-- platform -->` is where each publisher puts its own
     sentence about where the history lives.

     The text below is the page. It is written for the person who lands on
     the release with nothing else in hand. -->

Whole-filesystem name search and content search in one stack — a daemon, a CLI
and a launcher, native underneath and no_std above it.

**Which file?** — asked over the whole filesystem, then **what is inside it?**
in the same query:

```sh
searchctl --root ~ "invoice ext:pdf"
searchctl --root ~ "invoice grep:tesla"
```

The release carries four binaries: `searchctl` (the command-line tool),
`searchd` (the daemon: JSON lines over stdio, a pipe or a unix socket),
`rgy-autostart` (put the daemon in place at login) and `rgxy` (the launcher
over the daemon: a floating window where there is a desktop, the terminal where
there is not).

## Install

One line, and it installs the tool, the daemon and the launcher:

```sh
# Windows
irm https://raw.githubusercontent.com/danalec/rgy/main/install.ps1 | iex

# Linux, macOS and FreeBSD
curl -fsSL https://raw.githubusercontent.com/danalec/rgy/main/install.sh | bash
```

The installer checks the archive's SHA256 before it unpacks anything, puts the
binaries in `~/.local/bin` (Linux and FreeBSD) or
`%LOCALAPPDATA%\Programs\rgy` (Windows), adds that directory to the PATH, and
registers the daemon and the launcher to start at login on Windows, Linux and
macOS (FreeBSD has no login item yet). On Windows the installer also runs the
logon task immediately with `schtasks /Run`, so the daemon is up in the current
session instead of only after the next logon. The digest is pinned inside the
installer, so a substituted download is refused before anything is unpacked.

**Already installed and the daemon is not running?** The installers gained a
post-install switch that registers (and, on Windows, starts) the daemon with no
download: `install.sh --activate` / `install.ps1 -Activate`. The same switch
works after a Scoop, AUR, extracted-archive or crates.io install, which never
run the one-line installer. `rgy-autostart doctor` reports what is registered,
whether the daemon is up, and the command that fixes it.

### Package managers

- **Scoop** (Windows) installs this release from the owner's bucket, and the
  same bucket carries `rgx`, the upstream engine this repository is built on.
  Its `post_install` asks how you want the daemon enabled: register it for the
  next logon, register it and start it now, or do nothing.

  ```sh
  scoop bucket add danalec_scoop-alts https://github.com/danalec/scoop-alts
  scoop install danalec_scoop-alts/rgy
  ```

- **AUR** (Arch Linux and derivatives) carries three binary packages at
  **0.1.10.3-1**: `rgy-search-bin` (portable), `rgy-search-avx2-bin` (AVX2)
  and `rgy-search-znver4-bin` (AMD Zen 4). They install the prebuilt Linux
  archives, so no Rust toolchain is needed, and they conflict with one
  another because all three install `/usr/bin/searchctl`. Their `.install`
  scriptlet asks the same post-install question and explains that a root
  package install cannot register a daemon in your user session.

  ```sh
  yay -S rgy-search-bin             # or paru -S rgy-search-bin
  # yay -S rgy-search-avx2-bin     # AVX2 build
  # yay -S rgy-search-znver4-bin   # AMD Zen 4 build
  ```

  The PKGBUILDs live in their own AUR repositories, not in this tree.
- **winget** (Windows) carries its manifests in this repository's
  `.winget/`, rewritten from the archives on publish day. The submission
  ([#474460](https://github.com/microsoft/winget-pkgs/pull/474460)) is still
  open awaiting a moderator, so `winget install` does not find this release
  yet. The MANUAL.md that travels in the archive states it plainly in its
  "The packages" section.
- **Homebrew** (macOS) is published in the project's own tap,
  [danalec/homebrew-rgy](https://github.com/danalec/homebrew-rgy), not in
  `Homebrew/homebrew-core`: the latter is Homebrew's own source repository,
  and the rgy formula belongs to a tap the project owns.
  `scripts/publish-homebrew.ps1` (and its `.sh` twin) push the formula
  straight to that tap's `main` on release day.

  ```sh
  brew tap danalec/rgy
  brew install rgy-search
  ```

  The canonical formula lives in this repository at
  `Formula/rgy-search.rb`; the tap is a distribution point. Both Apple
  silicon and Intel Macs are pinned to the SHA256 of their release archives.
- **Nix** — `flake.nix` installs the release archives with `nix build
  .#rgy-search` (and the AVX2 / Zen 4 outputs); `nix flake check` verifies the
  pinned SRI hashes against the real downloads.
- **crates.io** carries all thirty-two crates at **0.1.10** — the registry
  takes three numbers, while the release tag carries a fourth. The publish
  order is computed from the manifests and the index is waited on before
  each crate goes up (`scripts/publish-crates.sh`).
- The **FreeBSD** archive is picked by `install.sh` from `uname -s` by
  itself; there is no login item there yet.

## What changed in 0.1.10.3

The crates stay at **0.1.10** — the registry takes three numbers, the fourth
is the fix count of the line. This release adds two archives, one Windows
installer, a Homebrew tap, a machine-readable licence and an explicit
boundary around files you do not trust. The X11 window, the settings panel,
the themes and the FreeBSD work shipped in 0.1.10.2; that release's page
lives in the repository, not inside this archive.

- **Fourteen archives: a generic AVX-512 build.** The `znver4` archives are
  tuned to AMD Zen 4 and enable its Zen-specific instructions
  (`avx512bf16`, `avx512bitalg`, ...). The new `avx512-windows` and
  `avx512-linux` archives are `-C target-cpu=x86-64-v4`, the portable
  AVX-512 level: they run on Zen 4 *and* on Intel Skylake-X, Ice Lake and
  newer. Both build-release twins accept the variant, and the smoke test
  skips itself on a runner whose CPU has no AVX-512 instead of dying with
  an illegal instruction. The release page now names all fourteen assets.
- **A per-user Windows MSI that starts the daemon.** One MSI, no separate
  EXE, no UAC and no Windows Service: it installs into
  `%LOCALAPPDATA%\Programs\rgy`, prepends that directory to the user's
  PATH, adds a Start Menu shortcut and has a real ARP uninstaller. After
  the files land it reproduces the Scoop `post_install` default —
  `rgy-autostart install --launcher`, the HKCU Run value, `schtasks /Run`
  and a detached hidden `searchd` — so `rgxy` works in the current
  session, not only after the next logon. A warning before the install
  says the first index build can temporarily use about 4 GB of RAM, and an
  advanced checkbox (`STARTDAEMON=0`, or unticking it) installs the files
  without registering or starting the daemon. Uninstall reverses all of it.
  MSI ignores the fourth field of `ProductVersion`, so the fix count is
  folded into the third (`0.1.10.3` —> `0.1.103`) and the ProductCode is
  a stable UUIDv5 per release. The `msi` CI job builds the package from the
  very Windows archive the release matrix smoke-tested and installs and
  removes it silently as a gate.
- **The Homebrew formula has a home.** It is published to the personal tap
  `danalec/homebrew-rgy`, not submitted as a pull request to
  `Homebrew/homebrew-core`, which is Homebrew's own source repository. The
  two publish-homebrew twins now clone that tap, commit and push `main`;
  there is no fork, no branch and no PR body. The earlier PR to
  homebrew-core was closed, and the fork was deleted.
- **The licence is unambiguous.** A root `LICENSE` carries the verbatim
  Apache-2.0 text, so GitHub's API reports `Apache-2.0` instead of
  `NOASSERTION`. Apache-2.0 is available for every file here: the tooling's
  `MIT OR Apache-2.0` always includes it, and `COPYING` remains the detailed
  map. The README states the SPDX identifier, every package manifest agrees
  (Scoop, winget, AUR, Homebrew), and `LICENSE` travels in the archives.
  The three AUR PKGBUILDs install it beside `LICENSE-APACHE`,
  `LICENSE-MIT`, `COPYING` and `NOTICE`, so a binary package carries the
  same licence file the repository declares.
- **Untrusted files, honestly bounded.** The archive, PDF, office and media
  decoders run *in the same process* on bytes a stranger may have written.
  Rust rules out most memory bugs, but not a decoder that panics, loops or
  allocates without bound. The 4 MiB read cap, nesting at four levels,
  4096 members per level and the compression decoders' `OutputCap` limit
  the damage; there is still no process isolation and no timeout on one
  parse. `RGY_ADAPTERS=off` makes the content path read only a file's own
  bytes and invoke no parser at all, while name search and plain-text
  content search keep working. README, MANUAL and FORMATS now state the
  threat model, what is bounded, what is not, and how to set the switch for
  the daemon (whose environment `rgxy` and `searchctl --daemon` inherit).
  For a corpus that may be actively hostile, the docs say plainly that no
  in-process switch replaces a container, a VM or a separate account.
- **Carried along.** `winget` is still awaiting its moderator; the FreeBSD
  archive is still picked by `install.sh` from `uname -s`; the AUR carries
  three binary packages, and the two new AVX-512 archives are not an AUR
  package yet.

## The archives

- `rgy-v0.1.10.3-x86_64-windows.zip` — portable x86_64 Windows.
- `rgy-v0.1.10.3-avx2-windows.zip` — for a CPU with AVX2.
- `rgy-v0.1.10.3-znver4-windows.zip` — for a Zen 4 CPU (AVX-512).
- `rgy-v0.1.10.3-avx512-windows.zip` — generic AVX-512 (x86-64-v4), for any AVX-512 CPU.
- `rgy-v0.1.10.3-aarch64-windows.zip` — Windows on arm64.
- `rgy-v0.1.10.3-x86_64-linux.tar.gz` — glibc 2.31 or newer.
- `rgy-v0.1.10.3-avx2-linux.tar.gz` — for a CPU with AVX2.
- `rgy-v0.1.10.3-znver4-linux.tar.gz` — for a Zen 4 CPU (AVX-512).
- `rgy-v0.1.10.3-avx512-linux.tar.gz` — generic AVX-512 (x86-64-v4), for any AVX-512 CPU.
- `rgy-v0.1.10.3-x86_64-linux-musl.tar.gz` — statically linked.
- `rgy-v0.1.10.3-aarch64-linux.tar.gz` — Linux on arm64.
- `rgy-v0.1.10.3-x86_64-freebsd.tar.gz` — FreeBSD 15.
- `rgy-v0.1.10.3-aarch64-apple-darwin.tar.gz` — macOS on Apple silicon.
- `rgy-v0.1.10.3-x86_64-apple-darwin.tar.gz` — macOS on Intel.
- `SHA256SUMS` — the digest of each archive.

The AVX2, Zen 4 and generic AVX-512 builds are for the CPUs that can run
them and nobody else: the Zen 4 one starts with an illegal instruction on a
baseline x86_64 CPU, and the AVX2 one on a CPU older than Haswell. The
generic AVX-512 one is the portable member of that family — `x86-64-v4`
is the level both Zen 4 and Intel's AVX-512 CPUs share. The portable builds
run anywhere, and they pick their kernels at run time. The SIMD variants
are `-C target-cpu=x86-64-v3` (AVX2), `-C target-cpu=znver4` (Zen 4) and
`-C target-cpu=x86-64-v4` (generic AVX-512), applied only to the target's
own artifacts so the build scripts still run on the building machine.

All fourteen archives are built and smoked on the runner that has their
platform, and one combined `SHA256SUMS` names every one of them. The
installers, the winget manifest and the Homebrew formula pin the real
digests of the archives they install. The CI release matrix that produces
them is the same matrix that gates the tree; the Windows and Linux x86_64
variants are also built and checked locally before the run. A separate CI
job builds the per-user Windows MSI from the Windows archive and keeps it
as the `rgy-msi` artifact; it is not one of these fourteen archives, and
the `SHA256SUMS` does not name it.

Apache-2.0 — the canonical text is `LICENSE` at the root, with the
detailed map in `COPYING` and third-party notices in `NOTICE`.

<!-- assets: rgy-v0.1.10.3-x86_64-windows.zip rgy-v0.1.10.3-avx2-windows.zip rgy-v0.1.10.3-znver4-windows.zip rgy-v0.1.10.3-avx512-windows.zip rgy-v0.1.10.3-aarch64-windows.zip rgy-v0.1.10.3-x86_64-linux.tar.gz rgy-v0.1.10.3-avx2-linux.tar.gz rgy-v0.1.10.3-znver4-linux.tar.gz rgy-v0.1.10.3-avx512-linux.tar.gz rgy-v0.1.10.3-x86_64-linux-musl.tar.gz rgy-v0.1.10.3-aarch64-linux.tar.gz rgy-v0.1.10.3-x86_64-freebsd.tar.gz rgy-v0.1.10.3-aarch64-apple-darwin.tar.gz rgy-v0.1.10.3-x86_64-apple-darwin.tar.gz SHA256SUMS -->
<!-- platform -->
