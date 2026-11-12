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
  **0.1.10.1-1**: `rgy-search-bin` (portable), `rgy-search-avx2-bin` (AVX2)
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
- **winget** (Windows) and **Homebrew** (macOS) carry their manifests in this
  repository — `.winget/` and `Formula/rgy-search.rb` — rewritten from the
  archives on publish day. The winget submission
  ([#474460](https://github.com/microsoft/winget-pkgs/pull/474460)) is open
  awaiting a moderator, and the Homebrew tap is not published, so those two
  commands install nothing yet. The MANUAL.md that travels in the archive
  states each one plainly in its "The packages" section.
- **Nix** — `flake.nix` installs the release archives with `nix build
  .#rgy-search` (and the AVX2 / Zen 4 outputs); `nix flake check` verifies the
  pinned SRI hashes against the real downloads.
- **crates.io** carries all thirty-two crates at **0.1.10** — the registry
  takes three numbers, while the release tag carries a fourth. The publish
  order is computed from the manifests and the index is waited on before
  each crate goes up (`scripts/publish-crates.sh`).
- The **FreeBSD** archive is picked by `install.sh` from `uname -s` by
  itself; there is no login item there yet.

## What changed in 0.1.10.1

The crates stay at **0.1.10** — the registry takes three numbers, the fourth
is the fix count of the line. This is a fix release of the installer,
packaging and documentation; no Rust source changed, so the binaries are
byte-identical to 0.1.10.0 apart from the documents the archives carry.

- **Post-install daemon activation.** `install.sh --activate` and
  `install.ps1 -Activate` register (and, on Windows, start) the daemon on an
  install that is already in place, with no download. A normal install now
  uses the same shared function, so an install made with `--no-autostart`, or
  an `rgy-autostart uninstall`, is put back with the same words and result.
- **Windows starts the daemon now.** `rgy-autostart install` writes a Task
  Scheduler `ONLOGON` task (or the weaker `HKCU` Run fallback), which does
  **not** start the daemon in the current session. `install.ps1` now runs
  `schtasks /Run /TN rgy-searchd` after registering — the exact `fix:` line
  `rgy-autostart doctor` prints — so `rgxy` works immediately. Linux
  (`enable --now`) and macOS (`launchctl bootstrap`) already started at once.
- **Scoop and AUR ask the question.** Scoop's `post_install` offers three
  answers (next logon / register and start now / do nothing), guarded for
  non-interactive installs. The three AUR `.install` scriptlets ask the same
  question and explain that a root install cannot register a per-user daemon.
  `docs/post-install-choice.md` is the canonical reference for both.
- **`searchctl` is not systemd.** README and MANUAL now say explicitly that
  it is a plain CLI with no systemd dependency, that `searchd` is a separate
  optional binary, and that `rgy-autostart` uses a systemd **user** unit on
  Linux only as one way to start it (Task Scheduler / launchd / no login item
  elsewhere).
- **Index vs daemon, stated precisely.** `searchctl` needs an *index* (built
  on first query, or with `searchctl --root ~ --reindex --count '*'`, rgy's
  `updatedb`); `rgxy` needs a *running* `searchd`, not an index. Indexing once
  does not make `rgxy` work. A verified daemon-free way to try `rgxy` — start
  `searchd` in the foreground on a temporary endpoint — is documented, plus
  a systemd user timer that refreshes the index once a day instead of running
  the daemon. `locate` / `updatedb` are compared in a table.
- **See the indexing state.** MANUAL now documents the startup stderr lines
  (`walking the tree`, `scanning… N dirs, M entries` every 500 ms,
  `indexed N entries → <path>`), `rgy-autostart doctor` as the connectivity
  check, and the `{"op":"status"}` frame (`docs`, `event_id`, `catch_up`,
  `feed`, `tv`) with a copy-paste command for the unix socket — including
  `feed: "gone"`, a frozen snapshot that `doctor` cannot detect.
- **Nix flake.** `flake.nix` / `flake.lock` install the 0.1.10.1 archives for
  four systems with `autoPatchelfHook` on Linux, `nix flake check` smoke tests
  and a `devShell`; `.gitattributes` pins the Nix files to LF.

## The archives

- `rgy-v0.1.10.1-x86_64-windows.zip` — portable x86_64 Windows.
- `rgy-v0.1.10.1-avx2-windows.zip` — for a CPU with AVX2.
- `rgy-v0.1.10.1-znver4-windows.zip` — for a Zen 4 CPU (AVX-512).
- `rgy-v0.1.10.1-aarch64-windows.zip` — Windows on arm64.
- `rgy-v0.1.10.1-x86_64-linux.tar.gz` — glibc 2.31 or newer.
- `rgy-v0.1.10.1-avx2-linux.tar.gz` — for a CPU with AVX2.
- `rgy-v0.1.10.1-znver4-linux.tar.gz` — for a Zen 4 CPU (AVX-512).
- `rgy-v0.1.10.1-x86_64-linux-musl.tar.gz` — statically linked.
- `rgy-v0.1.10.1-aarch64-linux.tar.gz` — Linux on arm64.
- `rgy-v0.1.10.1-x86_64-freebsd.tar.gz` — FreeBSD 15.
- `rgy-v0.1.10.1-aarch64-apple-darwin.tar.gz` — macOS on Apple silicon.
- `rgy-v0.1.10.1-x86_64-apple-darwin.tar.gz` — macOS on Intel.
- `SHA256SUMS` — the digest of each archive.

The AVX2 and the Zen 4 builds are for the CPUs that can run them and nobody
else: the Zen 4 one starts with an illegal instruction on a baseline x86_64
CPU, and the AVX2 one on a CPU older than Haswell. The portable builds run
anywhere, and they pick their kernels at run time. The SIMD variants are
`-C target-cpu=x86-64-v3` (AVX2) and `-C target-cpu=znver4` (Zen 4), applied
only to the target's own artifacts so the build scripts still run on the
building machine.

All twelve archives are built and smoked on the runner that has their
platform, and one combined `SHA256SUMS` names every one of them. The
installers, the winget manifest and the Homebrew formula pin the real
digests of the archives they install. The CI release matrix that produced
them is the same matrix that gates the tree; the Windows and Linux x86_64
variants were also built and checked locally before the run.

Apache-2.0.

<!-- assets: rgy-v0.1.10.1-x86_64-windows.zip rgy-v0.1.10.1-avx2-windows.zip rgy-v0.1.10.1-znver4-windows.zip rgy-v0.1.10.1-aarch64-windows.zip rgy-v0.1.10.1-x86_64-linux.tar.gz rgy-v0.1.10.1-avx2-linux.tar.gz rgy-v0.1.10.1-znver4-linux.tar.gz rgy-v0.1.10.1-x86_64-linux-musl.tar.gz rgy-v0.1.10.1-aarch64-linux.tar.gz rgy-v0.1.10.1-x86_64-freebsd.tar.gz rgy-v0.1.10.1-aarch64-apple-darwin.tar.gz rgy-v0.1.10.1-x86_64-apple-darwin.tar.gz SHA256SUMS -->
<!-- platform -->
