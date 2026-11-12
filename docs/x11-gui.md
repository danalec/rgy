# rgxy on X11 (Linux and FreeBSD)

`rgxy` is the floating launcher over the rgy search daemon. On Windows it is a
Win32 overlay drawn with GDI; on **Linux and FreeBSD** it is the same launcher
over an X11 window, drawn in pure Rust. Both desktop kernels speak X11, so one
backend serves both. The window driver itself is **one** driver:
`bins/rgxy/src/gui.rs` is shared, and `bins/rgxy/src/platform.rs` names the
backend — `rgy-launcher-windows` on Windows, `rgy-launcher-x11` on Linux and
FreeBSD. Both export the same `Window`, `Event`, `Key`, `View`, `Hook`, `Combo`,
`Sole` and `actions`, so the model, the daemon client, the settings panel, the
preview, the usage history and the bounded worker pools are identical on every
host with a window.

## The settings panel and the hotkey

The panel is `>settings` (or `Ctrl+S` in the terminal interface). Its twelve
rows are navigated with the arrows and changed with Left/Right; every change is
written to `settings.tsv` and applied at once. **Changing the shortcut does not
need a restart**: the driver drops the old grab and installs the new one in the
same process. Measured under openbox on FreeBSD 15.1: with `combo ctrl+space`
armed, one Right on the hotkey row rewrote the file to `double alt 400`; the
old Ctrl+Space then stopped toggling the hidden window and a double tap of Alt
showed it again.

## Running it

A bare `rgxy` is the resident launcher. It needs an **X11 display**:

- a classical X11 session sets `DISPLAY`;
- a Wayland session reaches it through **XWayland**, which sets `DISPLAY` too;
- a Wayland-only session (no XWayland) has no X server to connect to, and the
  launcher falls back to the terminal interface, saying why.

There is no per-user setup: `rgxy` opens its own display connection, checks it
in `session_available()`, and takes one claim per session before it builds
anything.

```sh
rgxy                 # resident launcher, hidden until Alt+Space or the tray icon
rgxy --show          # open it immediately
rgxy --no-hook       # run without the global hotkey
rgxy --tui           # the terminal interface, over the same source
rgxy --query report  # one search, printed, and out
rgxy            # then type >settings for the settings panel
```

## What it is made of

| | |
|---|---|
| window | an override-redirect X11 popup; the frame is computed in client memory and handed over with one `XPutImage` |
| pixels | a `u32`-per-pixel surface computed by the crate: integer source-over blending, antialiased rounded corners, a vertical gradient and a soft inner shadow |
| text | **FreeType**, `dlopen`ed, with the glyph fields read through guarded LP64 offsets; the built-in 8x8 bitmap font is the fallback when the library or the font file is missing |
| icons | the freedesktop **theme icons**, decoded by the crate (inflate and the five PNG row filters, bit depth 8, no interlacing); a drawn shape is the fallback |
| rows | a selection pill with an accent bar, one icon per kind, the matched characters picked out in the match colour, and a scrollbar when the answer is longer than the panel |
| tray | a freedesktop **XEmbed** system-tray icon (`_NET_SYSTEM_TRAY_S0`) with a three-item menu: Search, Settings, Quit |
| hotkey | **Alt+Space** by default — a passive `XGrabKey` on the root window (`Mod1Mask` + `XK_space`). The settings panel can change it to a double tap of Control/Alt/Shift or to another registered combo; the file spells it `combo alt+space`, `double ctrl 400`, and so on. |
| one instance | Linux: an **abstract** Unix socket named with the uid and a hash of `DISPLAY`. FreeBSD has no abstract sockets, so a filesystem socket in `/tmp` is used instead, with a connect-first probe and `unlink` of a stale file. A second `rgxy` asks the first to show itself and leaves. |
| actions | `xdg-open` for a result, the containing folder for *reveal*, the X `CLIPBOARD` selection for copy and paste |

The crate is `no_std` + `alloc`. All Xlib and libc calls are declared by hand in
`crates/launcher-x11/src/x.rs`; there is no X11 binding crate, no `windows`
crate, no `libc` crate and no GUI toolkit. Xlib is resolved at **runtime** with
`dlopen("libX11.so.6")` + `dlsym`, so no target — glibc, musl, aarch64, FreeBSD —
needs a `libX11.so` development symlink at build time. If the library is absent
at runtime, every wrapper answers zero and the launcher takes the terminal
interface. That is the same graceful answer the tree gives a machine with no
desktop on Windows.

## The two kernels

The differences are confined to `x.rs` and `sole.rs`, behind `#[cfg]` on the
OS; nothing else in the crate knows which kernel it is on. They were taken
from FreeBSD's own headers (`sys/sys/un.h`, `sys/sys/socket.h`,
`sys/sys/fcntl.h`, `include/unistd.h`, `sys/sys/signal.h`):

| | Linux | FreeBSD |
|---|---|---|
| `O_NONBLOCK` | `0o4000` | `0x0004` |
| `O_CLOEXEC` | `0o2000000` | `0x0010_0000` |
| `SOCK_CLOEXEC` | `0o2000000` | `0x1000_0000` |
| `SOCK_NONBLOCK` | `0o4000` | `0x2000_0000` |
| `SIGCHLD` | 17 | 20 |
| `sockaddr_un` | `{u16 family, [u8;108]}` = 110 bytes | `{u8 len, u8 family, [u8;104]}` = 106 bytes |
| abstract sockets | yes | no |

`pipe2` takes `O_*` flags on both kernels while `socket()`/`accept4()` take
`SOCK_*` flags — on Linux the close-on-exec values happen to coincide, on
FreeBSD they do not, so the seam keeps both names. `pipe2`, `accept4`,
`unlink`, `getuid`, `fork`, `setsid`, `execvp` and the `dl*` functions all
exist in FreeBSD's libc; `RTLD_LAZY` is 1 on both.

## What a reader should know

- **X11, not native Wayland.** There is no `wl_surface`, no `zwlr_layer_shell`
  and no StatusNotifierItem D-Bus client. Wayland users need XWayland. A
  StatusNotifierItem implementation is the natural next step for a session
  with no X server at all.
- **The tray needs a host.** The freedesktop XEmbed tray selection must have an
  owner (a panel or a tray applet). If it does not, the icon is refused, the
  launcher still runs on the hotkey, and the reason is kept in the line under
  the field.
**Alt+Space is the default on every desktop, but a window manager may
already own it.** `openbox`, GNOME, KDE and COSMIC bind Alt+Space to the
window menu; a passive `XGrabKey` of a key another client grabbed fails
with `BadAccess`. The launcher says so in the line under the field and
keeps running — the tray icon still opens it. On such a desktop put a
different shortcut in `settings.tsv`, for example:

```sh
hotkey	combo ctrl+space
hotkey	double ctrl 400
```

On a bare X server (or one whose WM does not take Alt+Space) the default
works while the window is hidden: measured on FreeBSD 15.1, Escape hid
the window and Alt+Space showed it again.

- **The hotkey is a passive grab.** Alt+Space is a registered combo, so it is
  one gesture rather than two taps and it works while the window is hidden.
  Grabbed keys are replayed with `XAllowEvents(ReplayKeyboard)` so the user's
  own Alt+Space still reaches the window manager;
  another program that grabbed the same key can still win the grab.
- **The font is ASCII plus derived Latin-1.** Printable ASCII has real 8x8
  glyphs; accented letters (`ã`, `ç`, `é`, …) are the base letter with an
  accent row, and anything else is spelled `?`.
- **Reveal opens the folder.** X11 has no Explorer `/select,` equivalent; the
  containing folder is opened instead. `xdg-open` comes from `xdg-utils`
  (a port/package on FreeBSD); if it is not installed the refusal is said on
  the line under the field.
- **A resident launch from a terminal keeps that terminal.** Windows gives
  its own console back with `FreeConsole`; an X11 session has no console
  window to close, and `rgxy` does not daemonize itself. Start it from a
  desktop entry, a login item or a compositor autostart (or with `&`).
- **A missing `libX11.so.6` is not a build failure**, because Xlib is loaded
  at runtime. It is a machine without an X server, and that machine gets the
  terminal interface.

## Verification

The X11 bodies are behind
`#[cfg(any(target_os = "linux", target_os = "freebsd"))]`; the pure decisions
(vocabulary, keysym decoder, geometry, icon pixels, protocol numbers) are
un-gated and tested on every host.

```sh
cargo check -p rgy-launcher-x11 -p rgxy --target x86_64-unknown-linux-gnu
cargo clippy -p rgy-launcher-x11 -p rgxy --target x86_64-unknown-linux-gnu --all-targets -- -D warnings
cargo check -p rgy-launcher-x11 -p rgxy --target x86_64-unknown-freebsd
cargo clippy -p rgy-launcher-x11 -p rgxy --target x86_64-unknown-freebsd --all-targets -- -D warnings
cargo check -p rgy-launcher-x11 -p rgxy --target x86_64-unknown-linux-musl
cargo check -p rgy-launcher-x11 -p rgxy --target aarch64-unknown-linux-gnu
cargo fmt --all -- --check
```

The bare-metal `no_std` gate in `.github/workflows/ci.yml` names
`-p rgy-launcher-x11` with the rest of the library tree; the `freebsd-check`
and `freebsd-suite` jobs build and run the whole workspace on FreeBSD. All 58
Xlib symbol names in `x.rs`'s `SYMBOLS` table were checked against
`nm -D --defined-only libX11.so.6` on an Ubuntu glibc 2.43 host (including the
ones that are macros in `Xlib.h`: `XDefaultScreen`, `XDefaultRootWindow`,
`XRootWindow`, `XConnectionNumber`, `XDisplayWidth`, `XDisplayHeight`, which
libX11 does export as functions), and `dlopen`/`dlsym` are in libc there.

**Honest limit:** this port was compiled, linted and cross-checked for
x86_64-gnu, x86_64-musl, aarch64-gnu and x86_64-freebsd, and the Windows
host's full suite passes, but a live X server session was not available while
it was written — the CachyOS box on the LAN was unreachable. A hand test at a
real X11 desktop on **both** Linux and FreeBSD (open the window, click the tray
icon, paste, press the hotkey, start a second `rgxy`) is still needed before
calling the systray, the FreeBSD filesystem socket and the passive-grab hotkey
verified at runtime. The FreeBSD CI VM runs the suite but has no display.
