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

# Linux and macOS
curl -fsSL https://raw.githubusercontent.com/danalec/rgy/main/install.sh | bash
```

The installer checks the archive's SHA256 before it unpacks anything, puts the
binaries in `~/.local/bin` (Linux) or `%LOCALAPPDATA%\Programs\rgy` (Windows),
adds that directory to the PATH, and registers the daemon and the launcher to
start at login. The digest is pinned inside the installer, so a substituted
download is refused before anything is unpacked.

On Windows, Scoop installs the same release from the owner's bucket — the
package is `rgy`, and the same bucket carries `rgx`, the upstream engine this
repository is built on:

```sh
scoop bucket add danalec/scoop-alts
scoop install rgy
```

## What changed in 0.1.9.0

This release carries a feature, a fix the users saw every login, and a line of
decoder fixes. The number grew a digit: the release is **0.1.9.0**, the crates
are **0.1.9**, and the fourth place counts the fixes this line will carry until
it turns at 0.2.0 — the registry takes three numbers only, and a line of fixes
wanted a number of its own.

- **The launcher got a settings panel, themes of its own, and a hotkey the
  user chooses.** `>settings` in the window opens the panel: the width, the
  language, the hotkey and the colours, and what the panel says the file
  says — one store, read at start-up and written whole. The themes are the
  window's and the terminal's, independently: a dark desktop does not force a
  dark terminal and the other way round. The hotkey is the user's own — the
  double-tap is generalised to any key, and a combination (a modifier and a
  key together) is registered with the platform, so both ways of summoning
  the launcher are the user's to pick. The picker follows: `fzf`'s colour
  flags pass through to it, and its free arguments too.
- **The resident launcher no longer leaves a console window behind.** A
  launch from a folder, a shortcut or the login — none of which has a
  terminal — used to keep the console Windows made for it on the screen,
  a black window the user had to close by hand. The launcher gives that
  console back before it says its first word, and only that console: one a
  terminal shared stays where it is, because taking it would take the
  terminal the user is typing in.
- **The launcher's memory of what was used survives a busy moment.** The
  usage history is a file, and a file another process holds open for a
  moment used to be read as a store that could not be read — and moved
  aside, so the memory started again. A refusal that lasts a moment is
  waited for now, and a store that is busy is not a store that is broken.
- **Nine decoders refuse what they cannot parse, by name, instead of
  panicking on the arithmetic.** A malformed file used to reach an addition
  or a subtraction that ran past the end of its number, and a debug build
  stopped there — a release build answered a value that was not the file's.
  The brotli reader refused a copy shorter than the dictionary word it
  names; the BigTIFF walker checked every sum before it walked to a page;
  the CGM reader read its pad byte only when the file has one and held
  every walk at the end of the bytes, with the JP2 and JPEG XL readers;
  the DEFLATE reader checked its three tables for claiming more code space
  than there is; the gzip layer said the reason the layer underneath
  refused instead of flattening it into a note; the cpio reader took a
  member's name with the NUL that ends it. The Firebird reader's day
  fraction and the HFS+ volume header's bit sixty-four were this
  repository's own fixes, carried as patches — upstream took both, and the
  patches are retired: what the two trees carry now is one code, at 0.1.9.
- **The crates are at 0.1.9.** The thirty-two crates the binaries are made
  of are published at 0.1.9 under this repository's names — the vendored
  trees as `rgy-vendored-core`, the `rgy-vendored-adapters-*` family and
  `rgy-vendored-rar-codec` — and the requirement lines that name them say
  0.1.9 too: a requirement left at the number of a release whose bytes are
  not the bytes here installs the release before this one, silently, at
  the user's machine.

## The archives

- `rgy-v0.1.9.0-x86_64-windows.zip` — the four binaries, the `rgy` alias and
  the documents.
- `rgy-v0.1.9.0-x86_64-linux.tar.gz` — the same four binaries, the `rgy`
  alias and the documents.
- `SHA256SUMS` — the digest of each archive.

The other nine archives are not in this release: the CI builds them on their
own runners — the macOS ones on a Mac, the aarch64 ones on an arm64 machine —
and it publishes none of them. The two archives here are the portable x86_64
ones; on macOS and on aarch64 the installer says so and installs nothing.

Apache-2.0.

<!-- assets: rgy-v0.1.9.0-x86_64-windows.zip rgy-v0.1.9.0-x86_64-linux.tar.gz SHA256SUMS -->
<!-- platform -->
