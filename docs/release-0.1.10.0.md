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
macOS (FreeBSD has no login item yet). The digest is pinned inside the
installer, so a substituted download is refused before anything is unpacked.

### Package managers

- **Scoop** (Windows) installs this release from the owner's bucket, and the
  same bucket carries `rgx`, the upstream engine this repository is built on:

  ```sh
  scoop bucket add danalec_scoop-alts https://github.com/danalec/scoop-alts
  scoop install danalec_scoop-alts/rgy
  ```

- **AUR** (Arch Linux and derivatives) carries three binary packages at
  **0.1.10.0-1**: `rgy-search-bin` (portable), `rgy-search-avx2-bin` (AVX2)
  and `rgy-search-znver4-bin` (AMD Zen 4). They install the prebuilt Linux
  archives, so no Rust toolchain is needed, and they conflict with one
  another because all three install `/usr/bin/searchctl`:

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
- **crates.io** carries all thirty-two crates at **0.1.10** — the registry
  takes three numbers, while the release tag carries a fourth. The publish
  order is computed from the manifests and the index is waited on before
  each crate goes up (`scripts/publish-crates.sh`).
- The **FreeBSD** archive is picked by `install.sh` from `uname -s` by
  itself; there is no login item there yet.

## What changed in 0.1.10.0

The crates stay at **0.1.10** — the registry takes three numbers, the fourth
is the fix count of the line — and this release carries the binary archives
for every platform the CI builds.

- **Paired with upstream rgx 0.1.10.** Every manifest, requirement line and
  `Cargo.lock` moved together, the EGM checker adopted the upstream's
  `--fix`/`-Fix`, `decision`-in-status and path-safety hardening (a 55-case
  self-test), and the vendored patches were re-derived. The vendored drift
  that remains is manifest versions and newly tracked upstream contracts —
  no `.rs` differs — so the re-pin is deferred by decision, not by accident.
- **The UI review is closed.** Ten verified defects were fixed with tests,
  then the remaining fourteen: bounded worker pools for stat, reader and
  preview threads, one long-lived stderr writer, a worker that cannot start
  degrading instead of panicking, a stateful decoder for bracketed paste, a
  bounded resize tick, the picker's execute bit and Windows extension order,
  the preview decision separated from the icon, compile-time assertions for
  every platform-written struct, the settings panel's width, and a write-only
  field removed. The launcher's per-operation write threads now go through a
  one-worker bounded pool, and the stores stay in submission order.
- **A pasted line break is text, not the enter key.** The Windows terminal
  seam asks for `ENABLE_VIRTUAL_TERMINAL_INPUT` only on a pseudoconsole,
  which implements bracketed paste; a legacy conhost keeps the key-record
  path because raw mode — not the flag — is what takes its paste away. A
  shared queue of decoded keys makes `poll` see every key a read already
  produced, so a paste arrives whole instead of one character per later
  keystroke. Measured at a real keyboard in Windows Terminal: ten events in
  two milliseconds with `Char('\r')` for the line break and no `Enter`.
- **The paste check is a CI gate.** On Linux and macOS a pseudo-terminal
  drives the same `poll`/`read`/decoder path and exits non-zero on a fail;
  the Windows halves stay a hand test, with their measurements recorded in
  `.egm/2026-11-07_DRIFT_REPORT.md`.
- **macOS audited.** Every termios flag, `NCCS`/`VMIN`/`VTIME`,
  `TIOCGWINSZ`, the 72-byte Darwin struct and `nfds_t` were checked against
  the platform's own headers, and a test now pins the numbers the tree
  declares by hand.

## The archives

- `rgy-v0.1.10.0-x86_64-windows.zip` — portable x86_64 Windows.
- `rgy-v0.1.10.0-avx2-windows.zip` — for a CPU with AVX2.
- `rgy-v0.1.10.0-znver4-windows.zip` — for a Zen 4 CPU (AVX-512).
- `rgy-v0.1.10.0-aarch64-windows.zip` — Windows on arm64.
- `rgy-v0.1.10.0-x86_64-linux.tar.gz` — glibc 2.31 or newer.
- `rgy-v0.1.10.0-avx2-linux.tar.gz` — for a CPU with AVX2.
- `rgy-v0.1.10.0-znver4-linux.tar.gz` — for a Zen 4 CPU (AVX-512).
- `rgy-v0.1.10.0-x86_64-linux-musl.tar.gz` — statically linked.
- `rgy-v0.1.10.0-aarch64-linux.tar.gz` — Linux on arm64.
- `rgy-v0.1.10.0-x86_64-freebsd.tar.gz` — FreeBSD 15.
- `rgy-v0.1.10.0-aarch64-apple-darwin.tar.gz` — macOS on Apple silicon.
- `rgy-v0.1.10.0-x86_64-apple-darwin.tar.gz` — macOS on Intel.
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

<!-- assets: rgy-v0.1.10.0-x86_64-windows.zip rgy-v0.1.10.0-avx2-windows.zip rgy-v0.1.10.0-znver4-windows.zip rgy-v0.1.10.0-aarch64-windows.zip rgy-v0.1.10.0-x86_64-linux.tar.gz rgy-v0.1.10.0-avx2-linux.tar.gz rgy-v0.1.10.0-znver4-linux.tar.gz rgy-v0.1.10.0-x86_64-linux-musl.tar.gz rgy-v0.1.10.0-aarch64-linux.tar.gz rgy-v0.1.10.0-x86_64-freebsd.tar.gz rgy-v0.1.10.0-aarch64-apple-darwin.tar.gz rgy-v0.1.10.0-x86_64-apple-darwin.tar.gz SHA256SUMS -->
<!-- platform -->
