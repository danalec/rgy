# rgy

Whole-filesystem name search and content search in one stack, rgx's content engine,
adapters and pack store.

[![rgy -- a demo on YouTube](https://img.youtube.com/vi/bLDYOjMUQGs/hqdefault.jpg)](https://www.youtube.com/watch?v=bLDYOjMUQGs)

**Which file?** — asked over the whole filesystem, then **what is inside
it?** in the same query.

```sh
searchctl --root ~ "invoice ext:pdf"      # which file
searchctl --root ~ "invoice grep:tesla"   # and what is inside it
```

## See it work

Two faces, one daemon. Click either one to watch it move.

[![the rgxy window over the daemon: the query "invoice doc:", the results the daemon ranked, the timings, and the actions on the right](docs/screenshots/gui-filters.png)](docs/videos/gui-filters.mp4)

[![the terminal interface on Linux, talking to a live daemon: the query "rs", the header's count of the daemon's index, the results the daemon ranked, and the timings](docs/screenshots/rgxy.png)](docs/videos/term-tui.mp4)

## Install

The release is **0.1.9.1**. It is on [GitHub](https://github.com/danalec/rgy/releases/tag/rgy-v0.1.9.1), and the crates are on crates.io at 0.1.9 — the release's number carries a fourth place, the fix count of the line, and the registry takes three.

**One line**, and it installs the tool, the daemon and the launcher:

```sh
# Windows
irm https://raw.githubusercontent.com/danalec/rgy/main/install.ps1 | iex

# Linux, macOS and FreeBSD
curl -fsSL https://raw.githubusercontent.com/danalec/rgy/main/install.sh | bash
```

The installer picks the archive that matches the machine, **checks its
SHA256**, unpacks the binaries into `~/.local/bin` (Linux, macOS and FreeBSD)
or `%LOCALAPPDATA%\Programs\rgy` (Windows), adds that directory to your PATH,
and registers the daemon to start at login on Windows, Linux and macOS
(FreeBSD has no login item yet). The launcher gets a login entry on Windows
alone, where it is a resident floating window; on the other systems it is the
terminal interface, a login entry has no terminal, and there is nothing to
register. The digest
is pinned inside the installer, so a substituted download is refused before
anything is unpacked — and an archive the release does not carry is refused
too. **This release carries every archive the CI builds** — portable, AVX2
and Zen 4 Windows, aarch64 Windows, glibc, musl and Zen 4 Linux, aarch64
Linux, FreeBSD and both macOS arches — and the CI builds each one on the
runner that has the platform.

**From crates.io:**

```sh
cargo install rgy-search --version 0.1.9         # gives you `searchctl`
```

That is the whole install for the tool you actually type, and it drops the
`searchctl` binary into `~/.cargo/bin` — already on your PATH if Rust was
installed the usual way. It needs **Rust 1.89 or newer** and a **C
compiler** (the allocator is C); the release archives and the installers
need neither.

The other binaries are separate crates, because most people only want the
first one:

```sh
cargo install rgy-searchd --version 0.1.9       # the daemon: searchd
cargo install rgy-autostart --version 0.1.9     # the login item: rgy-autostart
cargo install rgxy --version 0.1.9            # the window: rgxy
```

**From the release** — the archive carries the four binaries, the `rgy`
alias and the documents, and it needs neither Rust nor a compiler:

| platform | archive |
|---|---|
| Windows x86_64 | `rgy-v0.1.9.1-x86_64-windows.zip` |
| Linux x86_64 | `rgy-v0.1.9.1-x86_64-linux.tar.gz` |
| FreeBSD x86_64 | `rgy-v0.1.9.1-x86_64-freebsd.tar.gz` |

The release page carries a `SHA256SUMS` beside the archives, and every arch
the CI builds has one: aarch64 Windows, AVX2 and Zen 4 x86_64, static musl,
aarch64 Linux and both macOS arches are on it too.

Nothing else: no runtime, no service, nothing to configure. The parts that
differ per operating system (the Windows MFT walk, the Linux change feed, the
macOS FSEvents feed, the FreeBSD `getdirentries` walk) are compiled in and
chosen by `cfg` at build time, so one package carries everything it needs.

## Your first five minutes

Point it at a folder and give it a query in quotes:

```sh
searchctl --root ~ "invoice"
```

The first run walks the tree and builds an index of the names; every run
after it reuses that index for **30 minutes** — `--max-age` moves that
window, and an index older than it is rebuilt. The two progress lines go to
**stderr**, so they never pollute the output you pipe:

```
searchctl: rebuilding — no index on disk
searchctl: indexed 3 entries → C:\Users\danalec\AppData\Local\rgy\caf45dc303365b6b.idx
C:\Users\danalec\Documents\invoice_2024.pdf
```

The index lives in the platform's cache folder — `%LOCALAPPDATA%\rgy` on
Windows, `$XDG_CACHE_HOME/rgy` (or `~/.cache/rgy`) on Linux,
`~/Library/Caches/rgy` on macOS. `RGY_INDEX_DIR` moves the cache base, and
the `rgy` folder still goes inside it.

### The query

A query is a list of words, and **every word narrows the answer** — a file
has to satisfy all of them.

| you write | rgy looks for |
|---|---|
| `invoice` | anything close to "invoice" — a typo still hits |
| `'invoice` | a name that *contains* "invoice" |
| `^invoice` | a name that *starts* with "invoice" |
| `invoice$` | a name that *ends* with "invoice" |
| `=invoice` | a name with "invoice" as a whole *word* |
| `"invoice pdf"` | a name that *contains* "invoice pdf", the space and all |
| `!invoice` | anything that is **not** close to "invoice" |

Those are the name matchers. These narrow by a property of the file:

| filter | means | example |
|---|---|---|
| `ext:` | the file extension | `ext:pdf` |
| `type:` | what kind of thing it is | `type:dir`, `type:link`, `type:image` |
| `size:` | how big it is | `size:+10m` (at least 10 MB), `size:-1k` (at most 1 KB) |
| `dir:` | the folder it is in | `dir:documents` |
| `in:` | where it is | `in:downloads` |
| `mtime:` | when it changed | `mtime:-7d`, `mtime:today`, `mtime:2024-01-01` |
| `re:` | the path as a regular expression | `re:\.rs$` |
| `limit:` | how many hits | `limit:20` |
| `sort:` | the order of the answers | `sort:score` (the default), `sort:name`, `sort:mtime` |

And three operators look **inside** the file, with `path:line:text` for the
output — the shape `rg` prints:

| operator | means |
|---|---|
| `grep:` | the literal text |
| `regex:` | a regular expression |
| `sym:` | the symbols in the file, for code |

Two more things the query understands: `report|invoice` means "report" **or**
"invoice", and a term with `*` or `?` globs the **whole path** — `*.pdf`
reaches a PDF at any depth, and a path always starts with `/`, so the name
alone is `*scan?`.

An unknown `type:` fails closed: a warning on stderr, zero matches, exit 1.
It never falls back to "search everything".

Three ways to take the answer away:

```sh
searchctl --root ~ "invoice" --count        # how many hits
searchctl --root ~ "invoice" --json         # one JSON object per hit
searchctl --root ~ "invoice" --limit 100    # more than the 50 default
```

Exit codes: **0** found something, **1** nothing matched, **2** the run
failed.

## The binaries

| binary | what it does |
|---|---|
| `searchctl` | the command-line tool — the one you type |
| `searchd` | the daemon: it holds the index warm between queries |
| `rgxy` | the launcher: a floating window on Windows, the terminal interface on Linux, macOS and FreeBSD |
| `rgy-autostart` | puts the daemon in place at login, and takes it out again |

`rgy` is another name for `searchctl` — the same binary. The release
archives and the installers put it there; `cargo install rgy-search`
gives you `searchctl` and nothing else.

### The daemon

`searchctl` on its own is enough for everything you type by hand: it walks
the tree, so it finds the files that were created while the machine was
off. The daemon is for scripts, and for a machine you search again and
again — it holds the index warm between queries.

```sh
searchd --root ~                                        # one JSON request per line on stdin
echo '{"op":"search","q":"invoice"}' | searchd --root ~

rgy-autostart install      # the daemon beside the tool, indexing your home
rgy-autostart status       # what is registered
rgy-autostart doctor       # what is registered, whether it is up, and what to do
rgy-autostart uninstall    # take it out
```

The installers register the launcher with the daemon on Windows, so the
window is there after the next login. On Linux, macOS and FreeBSD the
launcher is the terminal interface and has no login entry: the window is
Windows-only today, and a terminal interface needs a terminal. When you
install by hand on Windows, ask for it:

```sh
rgy-autostart install --launcher   # the daemon and the window, at login (Windows)
```

`install` needs nothing: the daemon is the `searchd` beside the tool, and
the tree it indexes is your home directory. The tree is always written into
the entry, because the directory a login entry starts in is not your home —
it is `C:\Windows\System32` on Windows and `/` on macOS, and a daemon that
inherited that would walk the wrong tree. To index another tree:
`rgy-autostart install --args "--root /data"`.

Nothing is registered in the administrator's account: it goes into your own
session — a Task Scheduler task or an `HKCU` run value on Windows, a systemd
**user** unit on Linux, a launchd agent on macOS. Running the command *is*
the consent; no dialog, no elevation. FreeBSD has no login item yet:
`rgy-autostart install` says so and writes nothing.

The entry names where the daemon listens: a socket on Linux, macOS and
FreeBSD, a named pipe on Windows. The daemon reads requests from standard
input by default, and a login entry has no standard input — a daemon
registered that way would start, walk the tree, and exit. `doctor` prints the
line.

### The clients

`rgxy` is the daemon's **client**: the index lives in `searchd`, and a
keystroke is a question sent down a pipe. That is why the launcher is there
in a few milliseconds — it is not building an index, it is asking one that
is already built.

It has two faces, and the binary is the same on every platform: a floating
graphical window on Windows — resident, where two taps of Control open it —
and the terminal interface on Linux, macOS and FreeBSD, where it is the
command you run in a terminal. **The window is Windows-only today, on every
desktop**: a Linux machine with a desktop gets the terminal interface like
every other one. The two pictures at the top are the two faces.

```sh
rgxy                  # the launcher: resident window on Windows (Control twice); TUI elsewhere
rgxy --query invoice  # one search, printed, and out — the scriptable face
rgxy --tui            # the terminal interface (the launcher on Linux, macOS and FreeBSD)
rgxy --pick --query invoice   # hand the results to your own fzf
```

In the field:

| you write | the launcher does |
|---|---|
| `invoice` | the **name** layer: the engine ranks the names |
| `grep:total` | the **content** layer: which files *say* it |
| `invoice grep:total` | both: the files called invoice, that say total |
| `doc:invoice` | the documents called invoice |
| `file:` `folder:` `doc:` `pic:` `video:` `audio:` | the short filters |
| `>open` `>folder` `>copy` `>preview` `>menu` | the commands |

`Control` twice opens the window, over whatever is on screen. In the
window the keys are all of them: arrows, `Enter` to open, `Control+Enter`
for the folder, `Escape` to close, `Control+C` to copy the path,
`Control+P` to preview the file, `Control+K` for the actions.

On Windows the launcher also leaves a magnifying glass in the notification
area — the row of icons at the right of the taskbar — so that it can be
found when the shortcut is not remembered. A click on the icon, or a double
click, opens the window; the right button opens a menu with **Search** and
**Quit**. The icon never closes the window: a double click is two clicks,
and a window that opens and closes again is a window that is not there.

The daemon's order is the relevance order and it rules; on top of it the
launcher remembers what **you** opened, and a result you used recently comes
up a few places — never more than three. It is a nudge and not a takeover:
what it passes goes down one place to make room.

The window is a Windows thing for now — the double-`Control` hotkey is a
low-level keyboard hook, and the hook this repository has is the Windows
one. Everywhere else the terminal interface is the launcher, over the same
daemon: the same commands (`>open`, `>folder`, `>copy`, `>preview`,
`>menu`) and the same keys (`Control+P`, `Control+K`), with what it
cannot do said out loud — a terminal has no window to open a file in and
no clipboard to copy the path into, so it prints the path. `Control+C` is
the one key that is not the launcher's there: in a terminal it is how a
user quits, so it quits — and the copy is the `>copy` command.

## When it goes wrong

- **No results, and a warning on stderr** — an unknown `type:`, or a filter
  that does not apply. It fails closed, on purpose.
- **No results, no warning** — the name is not close to what you typed. Drop
  the quotes to go fuzzy, and check the spelling of the filter: `ext:pdf` and
  `ext:.pdf` are the same filter, and the leading dot is optional.
- **The first run is slow** — it is walking the tree. Every run after it is
  fast.
- **A PDF search finds nothing** — the words are not in the file. `grep:` asks
  the format adapters first, and a PDF's text layer is read through the
  adapter, so the words of a PDF are words the query can match. A **scanned**
  PDF has no text layer at all: its pages are images, and they are read only
  when `tesseract` is installed. See [FORMATS.md](FORMATS.md).
- **A content answer that says it is partial** — the daemon says so, in the
  answer: `the daemon did not read N file(s): the answer is partial` when a
  budget ran out or a file was a cloud placeholder, and `the answer stops at
  the limit` when the list was longer than the limit. The two are different
  holes, and the sentence says which one it is. The answer carries the
  reasons as well, so a reader knows whether the fix is a bigger budget or a
  file that is not there yet.

The [manual](MANUAL.md) has the whole query language, every flag, the daemon
protocol and the pack store.

## rgy or rgx?

[rgx](https://github.com/danalec/rgx) is the content-search specialist: an
`rg` replacement that reads *inside* files — PDFs, Office documents,
archives, mail, media metadata, 3D models, LLM checkpoints — with `rg`'s
flags, output and exit codes, the Tesla Valve pack store, and engine knobs
for harnesses. The `rgx-*` crates in this workspace are its engine and
adapters, transported verbatim.

rgy is the broader question: **which file?** — asked over the whole
filesystem, then **what is inside it?** in the same query.

| | rgy | rgx |
|---|---|---|
| Which file, anywhere on the disk | ✅ name index, fzf-v1 ranking, typos, live overlay | walk + path globs |
| What is inside it | ✅ `grep:` / `regex:` / `sym:` | ✅ rg patterns |
| Inside PDF / Office / archives / mail / 3D / LLM | ✅ the same adapters, read through the search path | ✅ |
| Name and content in one query | ✅ `report ext:pdf grep:tesla` | pattern + globs |
| Drop-in `rg` (flags, output, exit codes) | exit codes only | ✅ |
| Persistent index across runs | ✅ `--index` (pinned root, `--max-age`) | walk per run (pack store opt-in) |
| Pack-store control | `searchd --tv-dir`; a workspace build leaves `rgx-tv` in `target/release/` | ✅ in the `rgx` binary: `--tv-build`, `--engine auto\|tv\|rgx` |
| Forensic triage (magic ID, carving, strings, entropy, x509) | ✅ the same adapter, read through the search path | ✅ |

**The tech-savvy pick is rgx.** If you already think in `rg` flags and
regexes, script against `--json` submatches, pick engines, build pack stores,
or triage binaries — take [rgx](https://github.com/danalec/rgx).

**The broad, simple pick is rgy.** It answers both everyday questions over
the whole filesystem in one plain query language — words, plus
`ext:`/`type:`/`size:`/`dir:`/`re:`, `|` to alternate — with one daemon
underneath and nothing to configure.

> **Where the numbers live.** rgy's are targets until the per-OS
> verification lands — name lookup **≤ 0.5 ms** median at 5M entries — and
> the measurements so far do not meet that row: at 5M entries the broad
> queries measure 1.1× to 2.5× it on the Windows host. The write-ups, with
> the host and the date on every number, are in
> [`benchmark/`](benchmark/). rgx's are measured and dated in its
> [`benchmark/`](https://github.com/danalec/rgx/tree/main/benchmark); quote
> them with their run date and host.

## Build and try it from the source

Rust **1.89** or newer: the tree's SIMD kernels use the AVX-512 target
features, which landed in 1.89, and the manifests declare that floor so cargo
says so before it compiles anything. The workspace mixes editions on purpose:
the `rgy` crates and binaries are edition 2024, the vendored `rgx` crates stay
on 2021 so they keep diffing cleanly against upstream.

```sh
cargo build --release            # the whole workspace
```

```sh
# name: fuzzy words, filters narrow, hits ranked
cargo run --release -p rgy-search -- --root crates "walk ext:rs"
# content: `grep:` literal, `regex:`, `sym:` — output is path:line:text
cargo run --release -p rgy-search -- --root crates "grep:Listing"
# both in one query: the name narrows what is read, the content confirms
cargo run --release -p rgy-search -- --root crates "walk ext:rs grep:Listing"
# the daemon: JSON lines in, hits frames out, index warm between queries
echo '{"op":"grep","q":"regex:fn.list"}' | cargo run --release -p rgy-searchd -- --root crates
# rgx --serve compat: one JSON argv-array per line, begin/end framing
echo '["-e","Listing"]' | cargo run --release -p rgy-searchd -- --root crates --serve
# transports: --listen uds:$HOME/.cache/rgy/searchd.sock (unix, created 0600)
#             or pipe:rgy-search (windows)
```

`type:` names kinds (`file`/`dir`/`link`, media kinds like `image`); a bare
extension goes in `ext:`. An unknown `type:` fails closed — warning on
stderr, zero matches, exit 1.

## Status

The release is out: **0.1.9.1**, tagged `rgy-v0.1.9.1`, with every archive the
CI builds beside a `SHA256SUMS`, and all thirty-two crates on crates.io at
0.1.9 — the release's number carries a fourth place, the fix count of the
line, and the registry takes three. The release page is the changelog — it is
[`docs/release-0.1.9.1.md`](docs/release-0.1.9.1.md), the file the release
publishes.

### The packages

The **packages** — the winget manifest, the Scoop manifest and the Homebrew
formula — are called **rgy-search**, and not `rgy`, because `rgy` is taken on
crates.io (a GameBoy emulator library from 2019). The command-line tool's
crate carries the same name, so the package manager and the registry say one
thing: beside it are `rgy-searchd`, `rgy-autostart` and `rgxy`, and the
binaries keep the names they have everywhere else. The project itself is
still rgy.

| platform | package | state |
|---|---|---|
| Windows | Scoop | **out** — `scoop bucket add danalec/scoop-alts` and `scoop install rgy`. The same bucket carries `rgx`, the upstream engine this repository is built on |
| Windows | winget | the manifest in this repository validates and is refreshed by every release; the submission to `microsoft/winget-pkgs` ([#469679](https://github.com/microsoft/winget-pkgs/pull/469679)) was closed unmerged — `winget install danalec.rgy-search` installs nothing today |
| macOS | Homebrew | the formula's digests are placeholders, and the tap is not published — `brew install danalec/rgy/rgy-search` installs nothing today |

The merge is landed end to end: name search,
content search, one daemon/CLI, and the polish layer — and now the
window over the daemon, `rgxy`. What is left is
verification on machines this host does not have — the live feeds above
all: the release builds and smokes every archive, but the FSEvents watcher
has never run on a Mac and there is no wired feed on FreeBSD, so what keeps
the index current there is the next rebuild, not a live one.

The perf gate fails a PR on a >20% regression against the gated
[`benchmark/baseline-names.md`](benchmark/baseline-names.md) rows. A row whose
measurement does not reproduce on the host is measured and reported instead.

### The crates

All thirty-two are on crates.io at 0.1.9. Five are the programs — `rgy-search`,
`rgy-searchd`, `rgy-autostart`, `rgxy` and `rgy-zen` — and twenty-seven are
the libraries under them: `rgy-name-core`, `rgy-discovery`,
`rgy-discovery-dispatch`, `rgy-discovery-linux`, `rgy-discovery-macos`,
`rgy-discovery-freebsd`, `rgy-discovery-windows`, `rgy-engine`, `rgy-proto`,
`rgy-launcher`, `rgy-launcher-windows`, `rgy-pick`, `rgy-adapters`,
`rgy-par`, `rgy-search-ui`, and the twelve the upstream's engine is
transported as: `rgy-vendored-core`, `rgy-vendored-adapters-archive`,
`rgy-vendored-adapters-docs`, `rgy-vendored-adapters-inspect`,
`rgy-vendored-adapters-llm`, `rgy-vendored-adapters-mail`,
`rgy-vendored-adapters-media`, `rgy-vendored-adapters-office`,
`rgy-vendored-adapters-pdf`, `rgy-vendored-adapters-tabular`,
`rgy-vendored-adapters-threed` and `rgy-vendored-rar-codec`.

Two owners, two prefixes. A crate that is this project's is `rgy-*`, and the
front is `rgxy`: the launcher, the one program the user runs with the daemon
behind it. The twelve trees that come from the upstream are published as
`rgy-vendored-*`, and the reason is the registry's, not this project's taste: a
crate on crates.io has one name and one version, and the upstream's names —
`rgx-core`, the `rgx-adapters-*` family and `rar-codec` — are the upstream's to
publish. Until this release the two projects took turns on that line — 0.1.2
and 0.1.4 and 0.1.6 are this project's copies, 0.1.3 and 0.1.5 are the
upstream's — each one stepping over the number the other had taken, because a
number one repository has taken is a number the other cannot have. This
release is the one where the two meet on it: the copies take a name of their
own, and the numbers stay the same.

The code inside the transported crates is still the upstream's, and a dependant
writes the same code either way: the crate's Rust name is the upstream's
(`rgx_core::`), so the sources are the upstream's byte for byte and not one
line of them was touched to make the package answer to another name. The
package carries this project's name on the registry; the library keeps the
name its own code says. What the copy carries on top is this tree's fixes —
the EXR attribute-size overflow among them, which no published version of that
adapter had before 0.1.8: every earlier one on crates.io read the attribute's
size as a signed `i32`, and a file with a large enough attribute overflows it.

The number is **0.1.9**, and it is the upstream's line as much as this
project's: the upstream published 0.1.9 with the two fixes this repository had
carried as patches taken into it — the Firebird reader's day fraction and the
HFS+ volume header's bit sixty-four — and this project's copy is taken from
the upstream's 0.1.9, so the two stand at the same version with the same
fixes in them. A dependant that asks for `rgy-vendored-core ^0.1.9` gets this
project's copy; one that asks for `rgx-core ^0.1.9` gets the upstream's.
Before this release the two could not both be had — they wanted the same name
at the same version, and the registry gives that to one of them.

The versions this project published under the upstream's names — 0.1.2, 0.1.4
and 0.1.6 — stay where they are, and they are not yanked: they are the
artifacts of older builds, and the registry keeps what it keeps. The upstream's
own first versions — 0.1.0 and 0.1.1 — are **yanked**: crates.io does not
delete, so a yank is the most it does, and it is reversible. Those are the
versions the 0.1.2 artifacts of this project resolved, and a yanked dependency
breaks every published crate that names it: `cargo install rgy-search` asked
for `rgx-adapters-archive ^0.1.1` and was told "version 0.1.1 is yanked". The
0.1.6 artifacts ask for `^0.1.6` and resolve; the 0.1.3 artifacts ask for
`^0.1.2` and still do; and the 0.1.2 artifacts resolve again now that 0.1.2 is
there. The [release page](docs/release-0.1.9.1.md) says what the change costs
and when it lands.

The command-line tool's crate was published as `rgy-searchctl` first;
`rgy-search` is the name the package managers already use, and the binary it
installs is still `searchctl`. The old name was **yanked** on 2026-10-31: a
name on crates.io is permanent, and `cargo install rgy-searchctl` now says
the version was yanked instead of installing a crate this project no longer
has. The
[manual](MANUAL.md#the-library) says what each one gives you and how to
depend on it.

crates.io resolves a dependency when a crate is *installed*, not when it is
published, so the order the crates go up in is computed from the manifests
and followed by the publish script (`scripts/publish-crates.sh` and its
PowerShell twin, `scripts/publish-crates.ps1`).

The 0.1.1, 0.1.2, 0.1.3 and 0.1.4 numbers are taken as well, by earlier builds
of this same project — the registry keeps what it keeps, and a version number
is as permanent as a name. Those builds are not this tree: the protocol they
shipped has neither the limits nor the count rules that 0.1.9 has, and a
program pinned to them does not build against it. The version names the
bytes, and this release is **0.1.9** because these are the bytes.

## Related projects

| Project | Repo | What it provides |
|---|---|---|
| `rgx` | [github.com/danalec/rgx](https://github.com/danalec/rgx) | content search engine, adapters, Tesla Valve packs |

## License

Apache-2.0 for the product and the end-to-end harness, MIT OR Apache-2.0 for
the tooling around it — the full map is in `COPYING`:

- **Apache-2.0 only**: the index base (`crates/name-core`, `crates/discovery*`,
  `crates/engine`, `crates/proto`), the vendored rgx engine and adapters
  (`crates/rgx-*`, `crates/rar-codec`), the binaries (`bins/*`) and the e2e
  harness (`tests/`). Every file carries its `SPDX-License-Identifier:` header,
  and the manifests state the same terms — with one exception named here so
  the claim is not read as a claim about something else: `crates/name-core`
  states `Apache-2.0 AND MIT`, because three of its files arrived from
  `fsearch` under MIT and keep its terms.
- **MIT OR Apache-2.0**: the tooling (`scripts/`, `tools/`, `benchmark/`).
  Commercial use is licensed under Apache-2.0; the MIT option remains for
  anyone who prefers its terms.

Three files in `crates/name-core` are transported from `fsearch` (MIT) and keep
its terms; the vendored trees are Apache-2.0, and the layouts they encode that
were learned from other projects are recorded in `NOTICE`.
Full texts: `LICENSE-APACHE`, `LICENSE-MIT`. Third-party notices: `NOTICE`
and the notice of the one upstream whose code is carried here, in `LICENSE-MIT-FSEARCH`.

## Star history

<a href="https://star-history.com/#danalec/rgy&Date">

<picture>
<source media="(prefers-color-scheme: dark)" srcset="https://api.star-history.com/svg?repos=danalec/rgy&type=Date&theme=dark">
<source media="(prefers-color-scheme: light)" srcset="https://api.star-history.com/svg?repos=danalec/rgy&type=Date">
<img alt="Star History Chart" src="https://api.star-history.com/svg?repos=danalec/rgy&type=Date">
</picture>
</a>
