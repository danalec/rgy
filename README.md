# rgy

Whole-filesystem name search and content search in one stack, rgx's content engine,
adapters and pack store.

**Which file?** — asked over the whole filesystem, then **what is inside
it?** in the same query.

```sh
searchctl --root ~ "invoice ext:pdf"      # which file
searchctl --root ~ "invoice grep:tesla"   # and what is inside it
```

## Install

**From crates.io, today:**

```sh
cargo install rgy-searchctl     # gives you `searchctl`
```

That is the whole install for the tool you actually type. It needs **Rust
1.85 or newer** and a **C compiler** (the allocator is C), and it drops the
`searchctl` binary into `~/.cargo/bin` — already on your PATH if Rust was
installed the usual way.

The other two binaries are separate crates, because most people only want
the first one:

```sh
cargo install rgy-searchd       # the daemon: searchd
cargo install rgy-autostart     # the login item: rgy-autostart
```

The **packages** — the winget manifest and the Homebrew formula — are called
**rgy-search**, and not `rgy`, because `rgy` is taken on crates.io (a GameBoy
emulator library from 2019). The crates are `rgy-searchctl`, `rgy-searchd`
and `rgy-autostart`, and the binaries keep the names they have everywhere
else. The project itself is still rgy.

### The packages that are not out yet

The packaging is in place — the winget manifest, the Homebrew formula, the
release script, the one-line installers at the top of the repository — but
the first release has not been pushed, so these install nothing today:

| platform | package | command |
|---|---|---|
| Windows | winget | `winget install danalec.rgy-search` |
| Windows | one-line installer | `irm https://raw.githubusercontent.com/danalec/rgy/main/install.ps1 \| iex` |
| macOS | Homebrew | `brew tap danalec/rgy https://github.com/danalec/rgy` then `brew install danalec/rgy/rgy-search` |
| macOS | one-line installer | `curl -fsSL https://raw.githubusercontent.com/danalec/rgy/main/install.sh \| bash` |
| Linux | one-line installer | `curl -fsSL https://raw.githubusercontent.com/danalec/rgy/main/install.sh \| bash` |

The installers download the release archive, **check its SHA256**, unpack the
three binaries into `~/.local/bin` (Linux and macOS) or
`%LOCALAPPDATA%\Programs\rgy` (Windows), add that directory to your PATH,
and register the daemon to start at login. The archive that matches the
machine is the one that gets installed: the operating system and the
architecture decide it. The digest is pinned inside the installer, so a
substituted download is refused before anything is unpacked.

**When they land, prefer them**: they carry the same three binaries and the
documents, and they need neither Rust nor a compiler on the machine. Until
then, `cargo install` is the way in.

### Requirements

Rust **1.85** or newer, and a C compiler. Nothing else — no runtime, no
service, nothing to configure. The parts that differ per operating system
(the Windows MFT walk, the Linux change feed, the macOS FSEvents feed) are
compiled in and chosen by `cfg` at build time, so one package carries
everything it needs.

## Your first five minutes

Point it at a folder and give it a query in quotes:

```sh
searchctl --root ~ "invoice"
```

The first run walks the tree and builds an index of the names; every run
after it reuses that index. The two progress lines go to **stderr**, so they
never pollute the output you pipe:

```
searchctl: rebuilding — no index on disk
searchctl: indexed 3 entries → C:\Users\danalec\AppData\Local\rgy\caf45dc303365b6b.idx
C:\Users\danalec\Documents\invoice_2024.pdf
```

The index lives in the platform's cache folder — `%LOCALAPPDATA%\rgy` on
Windows, `$XDG_CACHE_HOME/rgy` on Linux, `~/Library/Caches/rgy` on macOS.
`RGY_INDEX_DIR` puts it somewhere else.

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
| `size:` | how big it is | `size:+10m` (bigger than 10 MB), `size:-1k` |
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
"invoice", and `*.pdf` / `scan?` glob the name.

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
| `rgxy` | the window: a floating launcher over the daemon |
| `rgy-autostart` | puts the daemon in place at login, and takes it out again |

`rgy` is another name for `searchctl` — the same binary.

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

`install` needs nothing: the daemon is the `searchd` beside the tool, and
the tree it indexes is your home directory. The tree is always written into
the entry, because the directory a login entry starts in is not your home —
it is `C:\Windows\System32` on Windows and `/` on macOS, and a daemon that
inherited that would walk the wrong tree. To index another tree:
`rgy-autostart install --args "--root /data"`.

Nothing is registered in the administrator's account: it goes into your own
session — a Task Scheduler task or an `HKCU` run value on Windows, a systemd
**user** unit on Linux, a launchd agent on macOS. Running the command *is*
the consent; no dialog, no elevation.

The entry names where the daemon listens: a socket on Linux and macOS, a
named pipe on Windows. The daemon reads requests from standard input by
default, and a login entry has no standard input — a daemon registered
that way would start, walk the tree, and exit. `doctor` prints the line.

### The window

`rgxy` is the daemon's **client**: the index lives in `searchd`, and a
keystroke is a question sent down a pipe. That is why the window is there
in a few milliseconds — it is not building an index, it is asking one that
is already built.

```sh
rgxy                  # the resident launcher: Control twice opens the window
rgxy --query invoice  # one search, printed, and out — the scriptable face
rgxy --tui            # the terminal interface, for a machine with no desktop
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

`Control` twice opens the window, over whatever is on screen. The keys are
all of them: arrows, `Enter` to open, `Control+Enter` for the folder,
`Escape` to close, `Control+C` to copy the path, `Control+P` to preview
the file, `Control+K` for the actions.

The window is a Windows thing for now — the double-`Control` hotkey is a
low-level keyboard hook, and the hook this repository has is the Windows
one. Everywhere else the terminal interface is the launcher, over the same
daemon.

## When it goes wrong

- **No results, and a warning on stderr** — an unknown `type:`, or a filter
  that does not apply. It fails closed, on purpose.
- **No results, no warning** — the name is not close to what you typed. Drop
  the quotes to go fuzzy, and check the filter: `ext:pdf`, not `ext:.pdf`.
- **The first run is slow** — it is walking the tree. Every run after it is
  fast.
- **A PDF search finds nothing** — expected today. `grep:` reads the file's
  own bytes, and a PDF's text is compressed. The adapters that read inside
  PDFs are not wired into the search path yet; that is what
  [rgx](https://github.com/danalec/rgx) is for.

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
| Inside PDF / Office / archives / mail / 3D / LLM | ⬜ families vendored, search path not wired yet | ✅ |
| Name and content in one query | ✅ `report ext:pdf grep:tesla` | pattern + globs |
| Drop-in `rg` (flags, output, exit codes) | exit codes only | ✅ |
| Persistent index across runs | ✅ `--index` (pinned root, `--max-age`) | walk per run (pack store opt-in) |
| Pack-store control | `searchd --tv-dir`, `rgx-tv build/search/update/inspect` | ✅ `--tv-build`, `--engine auto\|tv\|rgx` |
| Forensic triage (magic ID, carving, strings, entropy, x509) | — | ✅ `inspect` adapter |

**The tech-savvy pick is rgx.** If you already think in `rg` flags and
regexes, script against `--json` submatches, pick engines, build pack stores,
or triage binaries — take [rgx](https://github.com/danalec/rgx).

**The broad, simple pick is rgy.** It answers both everyday questions over
the whole filesystem in one plain query language — words, plus
`ext:`/`type:`/`size:`/`dir:`/`re:`, `|` to alternate — with one daemon
underneath and nothing to configure.

> **Where the numbers live.** rgy's are targets until the per-OS
> verification lands — name lookup **≤ 0.5 ms** median at 5M entries —
> with measurements so far in [`benchmark/`](benchmark/). rgx's are measured and
> dated in its
> [`benchmark/`](https://github.com/danalec/rgx/tree/main/benchmark); quote
> them with their run date and host.

## Build and try it from the source

Rust **1.85** or newer. The workspace mixes editions on purpose: the `rgy`
crates and binaries are edition 2024, the vendored `rgx` crates stay on 2021
so they keep diffing cleanly against upstream.

```sh
cargo build --release            # the whole workspace
```

```sh
# name: fuzzy words, filters narrow, hits ranked
cargo run --release -p rgy-searchctl -- --root crates "walk ext:rs"
# content: `grep:` literal, `regex:`, `sym:` — output is path:line:text
cargo run --release -p rgy-searchctl -- --root crates "grep:Listing"
# both in one query: the name narrows what is read, the content confirms
cargo run --release -p rgy-searchctl -- --root crates "walk ext:rs grep:Listing"
# the daemon: JSON lines in, hits frames out, index warm between queries
echo '{"op":"grep","q":"regex:fn.list"}' | cargo run --release -p rgy-searchd -- --root crates
# rgx --serve compat: one JSON argv-array per line, begin/end framing
echo '["-e","Listing"]' | cargo run --release -p rgy-searchd -- --root crates --serve
# transports: --listen uds:/tmp/rgy.sock (unix) or pipe:rgy-search (windows)
```

`type:` names kinds (`file`/`dir`/`link`, media kinds like `image`); a bare
extension goes in `ext:`. An unknown `type:` fails closed — warning on
stderr, zero matches, exit 1.

## Status

The merge is landed end to end: name search,
content search, one daemon/CLI, and the polish layer. What is left is
verification on machines this host does not have — macOS above all.

The perf gate fails a PR on a >20% regression against any
[`benchmark/baseline-names.md`](benchmark/baseline-names.md) row.

The crates are on crates.io, all fourteen at 0.1.0: `rgy-name-core`,
`rgy-discovery`, `rgy-engine`, `rgy-proto` and the rest. They carry the
`rgy-` prefix because `name-core`, `discovery`, `engine`, `proto`, `par`
and `zen-rs` are taken on crates.io.

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
  and the manifests state the same terms.
- **MIT OR Apache-2.0**: the tooling (`scripts/`, `tools/`, `benchmark/`).
  Commercial use is licensed under Apache-2.0; the MIT option remains for
  anyone who prefers its terms.

Three files in `crates/name-core` are transported from `fsearch` (MIT) and keep
its terms; the vendored trees are Apache-2.0, and the layouts they encode that
were learned from other projects are recorded in `NOTICE`.
Full texts: `LICENSE-APACHE`, `LICENSE-MIT`. Third-party notices: `NOTICE`
and the notice of the one upstream whose code is carried here, in `LICENSE-MIT-FSEARCH`.
