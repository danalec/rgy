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

On Windows, Scoop installs the same release from the owner's bucket — the
package is `rgy`, and the same bucket carries `rgx`, the upstream engine this
repository is built on:

```sh
scoop bucket add danalec/scoop-alts
scoop install rgy
```

## What changed in 0.1.9.1

This is a fix release on the 0.1.9 line: the fourth number is the fix count,
the crates stay at **0.1.9** — the registry takes three — and the fixes are in
the archives.

- **The release carries every archive the CI builds, and not two of them.**
  The 0.1.9.0 release was the two portable x86_64 archives this machine cut;
  the other nine were built by the CI on the runners that have the platforms
  and published nowhere, so macOS and aarch64 had no archive and the installer
  said so. This release carries all twelve: the portable, AVX2 and Zen 4
  Windows builds, the aarch64 Windows one, the glibc, static musl and Zen 4
  Linux builds, the aarch64 Linux one, FreeBSD, and both macOS arches. Each is
  built and smoked on the machine that has the platform, kept by the run that
  built it, and uploaded with the release, so the installer and the package
  managers pin an archive that exists for every platform they name.
- **FreeBSD has an archive of its own.** The backend was there — the
  `getdirentries` walk — and the release carried nothing for it: the platform is
  now a row of its own, built inside a FreeBSD VM on an ubuntu runner (there is
  no FreeBSD runner, and a cross-link to a libc the runner does not have is a
  binary nobody can vouch for), and `install.sh` picks it from `uname -s` with
  no argument. There is no wired change feed on FreeBSD yet, so the index
  there is a snapshot until the next rebuild.
- **The scope's stats do not answer with an empty scope when the window is
  already gone.** The stats run on their own thread, and the caller waited for
  them with nothing left on the clock: whether the answer was the empty scope
  — which reads as *there are no files* — or the refusal depended on whether
  the thread reached its send before the caller reached its zero-length
  receive. The window is checked before the worker starts now, and a gone
  window is the refusal the caller's own message names.

## The archives

- `rgy-v0.1.9.1-x86_64-windows.zip` — portable x86_64 Windows.
- `rgy-v0.1.9.1-avx2-windows.zip` — for a CPU with AVX2.
- `rgy-v0.1.9.1-znver4-windows.zip` — for a Zen 4 CPU (AVX-512).
- `rgy-v0.1.9.1-aarch64-windows.zip` — Windows on arm64.
- `rgy-v0.1.9.1-x86_64-linux.tar.gz` — glibc 2.31 or newer.
- `rgy-v0.1.9.1-avx2-linux.tar.gz` — for a CPU with AVX2.
- `rgy-v0.1.9.1-znver4-linux.tar.gz` — for a Zen 4 CPU (AVX-512).
- `rgy-v0.1.9.1-x86_64-linux-musl.tar.gz` — statically linked.
- `rgy-v0.1.9.1-aarch64-linux.tar.gz` — Linux on arm64.
- `rgy-v0.1.9.1-x86_64-freebsd.tar.gz` — FreeBSD 15.
- `rgy-v0.1.9.1-aarch64-apple-darwin.tar.gz` — macOS on Apple silicon.
- `rgy-v0.1.9.1-x86_64-apple-darwin.tar.gz` — macOS on Intel.
- `SHA256SUMS` — the digest of each archive.

The AVX2 and the Zen 4 builds are for the CPUs that can run them and nobody
else: the Zen 4 one starts with an illegal instruction on a baseline x86_64
CPU, and the AVX2 one on a CPU older than Haswell. The portable builds run
anywhere, and they pick their kernels at run time.

Apache-2.0.

<!-- assets: rgy-v0.1.9.1-x86_64-windows.zip rgy-v0.1.9.1-avx2-windows.zip rgy-v0.1.9.1-znver4-windows.zip rgy-v0.1.9.1-aarch64-windows.zip rgy-v0.1.9.1-x86_64-linux.tar.gz rgy-v0.1.9.1-avx2-linux.tar.gz rgy-v0.1.9.1-znver4-linux.tar.gz rgy-v0.1.9.1-x86_64-linux-musl.tar.gz rgy-v0.1.9.1-aarch64-linux.tar.gz rgy-v0.1.9.1-x86_64-freebsd.tar.gz rgy-v0.1.9.1-aarch64-apple-darwin.tar.gz rgy-v0.1.9.1-x86_64-apple-darwin.tar.gz SHA256SUMS -->
<!-- platform -->
