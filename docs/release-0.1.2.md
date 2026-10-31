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

## What changed in 0.1.2

- **The launcher has a face.** On Windows it leaves a magnifying glass in the
  notification area: a click or a double click opens the window, and the right
  button opens a menu with **Search** and **Quit**. A double click opens the
  window and never closes it again — a double click is two clicks.
- **The search refuses to grow while the daemon is gone.** The characters and
  the paste are refused; erasing still works, so the field can always be
  cleared. A field that shows a query nobody is running is a lie.
- **A folder is answered as a folder.** The daemon answers a hit's kind from
  the index, so a folder created after the last compaction is no longer drawn
  with a file's icon.
- **The counts on the wire are ruled.** A fractional count, a count that does
  not add up and a negative journal position are refused, and the answer says
  *whole* only when every file was read — with the reasons printed.
- **A `~` is the home on every platform.** The tree expands it itself, so
  `--root ~` is your home directory in PowerShell as well as in a POSIX shell.
  A path that does not expand looks for a folder named `~`, and the answer
  looks exactly like a tree with nothing in it.
- **The crates are at 0.1.2.** The twenty crates the binaries are made of are
  published at 0.1.2. The command-line tool's crate is `rgy-search`; it was
  published first as `rgy-searchctl`, and that name stays in the index, because
  a name on crates.io is permanent and the only thing one can do with an old
  one is *yank* it.

## The archives

- `rgy-v0.1.2-x86_64-windows.zip` — the four binaries, the `rgy` alias and the
  documents.
- `rgy-v0.1.2-x86_64-linux.tar.gz` — the same four binaries, the `rgy` alias and
  the documents.
- `SHA256SUMS` — the digest of each archive.

The other nine archives are not in this release: the CI builds them on their
own runners — the macOS ones on a Mac, the aarch64 ones on an arm64 machine —
and it publishes none of them. The two archives here are the portable x86_64
ones; on macOS and on aarch64 the installer says so and installs nothing.

Apache-2.0.

<!-- assets: rgy-v0.1.2-x86_64-windows.zip rgy-v0.1.2-x86_64-linux.tar.gz SHA256SUMS -->
<!-- platform -->
