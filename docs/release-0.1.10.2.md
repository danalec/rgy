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

Whole-filesystem name search and content search in one stack â€” a daemon, a CLI
and a launcher, native underneath and no_std above it.

**Which file?** â€” asked over the whole filesystem, then **what is inside it?**
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
  **0.1.10.2-1**: `rgy-search-bin` (portable), `rgy-search-avx2-bin` (AVX2)
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
  repository â€” `.winget/` and `Formula/rgy-search.rb` â€” rewritten from the
  archives on publish day. The winget submission
  ([#474460](https://github.com/microsoft/winget-pkgs/pull/474460)) is open
  awaiting a moderator, and the Homebrew tap is not published, so those two
  commands install nothing yet. The MANUAL.md that travels in the archive
  states each one plainly in its "The packages" section.
- **Nix** â€” `flake.nix` installs the release archives with `nix build
  .#rgy-search` (and the AVX2 / Zen 4 outputs); `nix flake check` verifies the
  pinned SRI hashes against the real downloads.
- **crates.io** carries all thirty-two crates at **0.1.10** â€” the registry
  takes three numbers, while the release tag carries a fourth. The publish
  order is computed from the manifests and the index is waited on before
  each crate goes up (`scripts/publish-crates.sh`).
- The **FreeBSD** archive is picked by `install.sh` from `uname -s` by
  itself; there is no login item there yet.

## What changed in 0.1.10.2

The crates stay at **0.1.10** â€” the registry takes three numbers, the fourth
is the fix count of the line. This release is the **launcher's window on
Linux and FreeBSD**: the source of `rgxy`, its daemon client and the shared
model changed, so the binaries are new.

- **A floating window on Linux and FreeBSD.** `rgxy` used to draw a Win32
  overlay on Windows and fall back to the terminal everywhere else. One X11
  backend now serves both unix kernels: an override-redirect popup, an
  XEmbed tray icon, a passive root grab for the hotkey, and a single-instance
  guard (an abstract socket on Linux, a `/tmp` socket on FreeBSD, which has
  none). The window driver in `bins/rgxy/src/gui.rs` is still one driver,
  reached through `src/platform.rs`.
- **The window is drawn by the tree.** The frame is computed into a
  `u32`-per-pixel surface with integer source-over blending, antialiased
  rounded corners, a vertical gradient and a soft inner shadow, then handed
  to Xlib with one `XPutImage`. Text is FreeType (`dlopen`ed, with guarded
  LP64 offsets and the built-in 8x8 font as fallback); icons are the
  freedesktop theme's own PNGs, decoded by the crate's own inflate + row
  filters. A query highlights the characters the ranking matched, the
  selected row is a pill with an accent bar, and the latency meter is a
  strip of bars that eases towards each new sample instead of jumping.
- **Settings and themes, on both systems.** The twelve-row settings panel
  (`>settings`) adjusts the palette, width, rows, position, opacity, hotkey
  and the terminal/picker themes; every change is written to
  `settings.tsv` and applied at once. All six named palettes were rendered
  on both kernels, and all twelve rows were exercised end to end. Opacity
  (`_NET_WM_WINDOW_OPACITY`) and the desktop's work area
  (`_NET_WORKAREA`) were measured under a real X11 compositor. Changing
  the shortcut re-arms the grab in the same process, without a restart.
- **Alt+Space is the default shortcut** on Windows, Linux and FreeBSD â€”
  one registered chord instead of a double tap. A window manager that
  already owns it refuses the grab with `BadAccess`; the launcher says so
  and the settings file can name another. The terminal interface gained the
  same actions (open, open folder, copy path, paste, settings) behind one
  `Runner` trait.
- **Keyboard correctness on X11.** `XLookupKeysym(event, 0)` is the
  unshifted level: `Shift+.` read as `.`, so every `>` command
  (`settings`, `menu`, `preview`) was unreachable and capitals arrived
  lowercase. Text now comes from `XLookupString`, with a pure, tested
  decision that keeps the keysym path for special keys and Ctrl/Alt.
- **FreeBSD specifics.** Xlib is resolved at runtime (`dlopen`), so no
  target needs a `libX11.so` dev symlink. The kernel's `O_*`/`SOCK_*`
  flags, `SIGCHLD` and the 106-byte `sockaddr_un` are behind `cfg`; the
  daemon says honestly that its index is a snapshot there, since FreeBSD
  has no change journal. The launcher was built and driven on a real
  FreeBSD 15.1 VM, with the window, tray, settings, themes, menu and
  preview all exercised.
- **Fixes carried along.** A frame with no alpha yet was being written as
  almost transparent (`0x01010101`) under a compositor; both ends of the
  range now drop the opacity property. The `double <key>` line without a
  window no longer panics now that the default is a combo. The index
  build's transient pages are returned to the OS on Windows, and the saved
  index is mapped from the file instead of keeping the anonymous build
  blob.
## The archives

- `rgy-v0.1.10.2-x86_64-windows.zip` â€” portable x86_64 Windows.
- `rgy-v0.1.10.2-avx2-windows.zip` â€” for a CPU with AVX2.
- `rgy-v0.1.10.2-znver4-windows.zip` â€” for a Zen 4 CPU (AVX-512).
- `rgy-v0.1.10.2-aarch64-windows.zip` â€” Windows on arm64.
- `rgy-v0.1.10.2-x86_64-linux.tar.gz` â€” glibc 2.31 or newer.
- `rgy-v0.1.10.2-avx2-linux.tar.gz` â€” for a CPU with AVX2.
- `rgy-v0.1.10.2-znver4-linux.tar.gz` â€” for a Zen 4 CPU (AVX-512).
- `rgy-v0.1.10.2-x86_64-linux-musl.tar.gz` â€” statically linked.
- `rgy-v0.1.10.2-aarch64-linux.tar.gz` â€” Linux on arm64.
- `rgy-v0.1.10.2-x86_64-freebsd.tar.gz` â€” FreeBSD 15.
- `rgy-v0.1.10.2-aarch64-apple-darwin.tar.gz` â€” macOS on Apple silicon.
- `rgy-v0.1.10.2-x86_64-apple-darwin.tar.gz` â€” macOS on Intel.
- `SHA256SUMS` â€” the digest of each archive.

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

<!-- assets: rgy-v0.1.10.2-x86_64-windows.zip rgy-v0.1.10.2-avx2-windows.zip rgy-v0.1.10.2-znver4-windows.zip rgy-v0.1.10.2-aarch64-windows.zip rgy-v0.1.10.2-x86_64-linux.tar.gz rgy-v0.1.10.2-avx2-linux.tar.gz rgy-v0.1.10.2-znver4-linux.tar.gz rgy-v0.1.10.2-x86_64-linux-musl.tar.gz rgy-v0.1.10.2-aarch64-linux.tar.gz rgy-v0.1.10.2-x86_64-freebsd.tar.gz rgy-v0.1.10.2-aarch64-apple-darwin.tar.gz rgy-v0.1.10.2-x86_64-apple-darwin.tar.gz SHA256SUMS -->
<!-- platform -->
