# crates/launcher-x11/src

## Overview

Source of the `rgy-launcher-x11` backend: one X11 implementation shared by
Linux and FreeBSD. `lib.rs` declares the crate `#![no_std]` + `alloc`
unconditionally and splits the tree in two: the un-gated decisions
(`types.rs`, `keys.rs`, `decode.rs`, `geometry.rs`, `mark.rs`) have tests
that run on every host, while the Xlib bodies (`x.rs`, `runtime.rs`,
`window.rs`, `raster.rs`, `render.rs`, `paint.rs`, `font.rs`, `icons.rs`,
`tray.rs`, `hotkey.rs`, `sole.rs`, `session.rs`, `actions.rs`) are behind
`#[cfg(any(target_os = "linux", target_os = "freebsd"))]`. `hook` and `combo`
are inline re-export modules of `hotkey.rs` so the module names match the
Windows backend.

`x.rs` is the one seam: 63 Xlib calls declared by hand as Rust wrappers that
resolve their addresses with `dlopen("libX11.so.6")` + `dlsym` on first use,
libc calls in a bare `extern "C"` block (no link attribute anywhere), and the
platform's structs whole with compile-time size/offset assertions. All 63 symbol names are verified against `nm -D libX11.so.6` on both kernels.

**The frame is computed in client memory, not by the server.** `raster.rs` is
a plain `u32`-per-pixel surface with integer source-over blending, a 4x4
supersampled antialiased rounded rectangle and an inner shadow; `render.rs`
lays the panel out on it; `window.rs` hands the finished buffer to the server
with one `XPutImage`. The one server-side primitive left is the tray's
`XFillRectangles` (`paint.rs:pub fn text`), which draws three menu labels once.

**Typography and icons come from the machine.** `font.rs` dlopens
`libfreetype.so.6` and reads a face's fields through the LP64 layout of
`FT_FaceRec`/`FT_GlyphSlotRec`, guarded at load time: a face whose
`num_glyphs`, `units_per_EM`, ascender or descender cannot be right is given
back, so a wrong layout can only cost the bitmap fallback and never a read
through the wrong offsets. `icons.rs` carries its own PNG decoder — inflate,
the five row filters and the four colour types a theme icon uses — because
`libpng` reports errors with `longjmp`, which is undefined behaviour across
Rust frames; the icons themselves are the freedesktop theme's own files under
`/usr/share/icons` and `/usr/local/share/icons`. `paint.rs` keeps the 8x8
bitmap font as the fallback for a machine with neither.

The per-kernel facts are here too, `#[cfg]`-gated on the OS: `O_NONBLOCK`,
`O_CLOEXEC`, `SOCK_CLOEXEC`, `SOCK_NONBLOCK`, `SIGCHLD`, and the two
`sockaddr_un` layouts (Linux 110 bytes, no length byte; FreeBSD 106 bytes
with `sun_len`, a one-byte family and a 104-byte path). FreeBSD has no
abstract Unix sockets, so `sole.rs` uses a filesystem socket in `/tmp`
there, with a connect-first probe, `unlink` of a stale file and `unlink`
on drop. Nothing else in the crate knows which kernel it is on.

## Subdirectories & Boundaries
- No child directories. Every source file is a module of this crate; the parent contract is [../EGM.md](../EGM.md).

## Interface
- `[lib.rs:pub use window::{Awareness, Compositor, Event, Key, View, Window}]`, `[lib.rs:pub use sole::{Sole, Taken}]`, `[lib.rs:session_available]`, `[lib.rs:put_the_console_away]`.
- `[types.rs:pub enum Key]`, `[types.rs:pub enum Event]`, `[types.rs:pub struct View]`, `[types.rs:pub enum Control]`, `[types.rs:pub enum Compositor]`, `[types.rs:pub enum Awareness]` — the shared vocabulary.
- `[window.rs:Window::new]`, `[window.rs:Window::pump]`, `[window.rs:Window::set_view]`, `[window.rs:Window::resize]`, `[window.rs:Window::show]`, `[window.rs:Window::hwnd]`, `[window.rs:Window::work_area_height]`, `[window.rs:pub fn wake]`.
- `[render.rs:pub struct Renderer]`, `[render.rs:Renderer::new]`, `[render.rs:Renderer::draw]`, `[render.rs:Renderer::resize]`, `[render.rs:pub enum IconFamily]`.
- `[raster.rs:pub struct Surface]`, `[raster.rs:Surface::blend]`, `[raster.rs:Surface::fill_round_rect]`, `[raster.rs:Surface::blend_mask]`, `[raster.rs:Surface::inner_shadow]`, `[raster.rs:pub fn argb]`, `[raster.rs:pub fn lerp]`, `[raster.rs:pub fn premultiplied_to_x11]`.
- `[font.rs:pub struct Font]`, `[font.rs:Font::load]`, `[font.rs:Font::load_bold]`, `[font.rs:Font::load_small]`, `[font.rs:Font::from_path_size]`, `[font.rs:Font::glyph]`, `[font.rs:Font::measure]`, `[font.rs:pub fn candidate_paths]`.
- `[icons.rs:pub fn decode_png]`, `[icons.rs:pub fn inflate]`, `[icons.rs:pub fn load]`, `[icons.rs:Icon::from_rgba]`, `[icons.rs:pub fn candidate_dirs]`, `[icons.rs:pub const ICON_SIZE]`.
- `[paint.rs:pub fn text]`, `[paint.rs:pub fn rectangles]`, `[paint.rs:pub fn glyph]` — the fallback font and the tray's labels.
- `[tray.rs:pub enum Command]`, `[tray.rs:pub struct Tray]`, `[tray.rs:pub fn menu_event]`; `[hotkey.rs:pub struct Hook]`, `[hotkey.rs:pub struct Combo]`, `[hotkey.rs:pub fn combo_keysym]`, `[hotkey.rs:pub fn combo_mask]`.
- `[sole.rs:pub enum Taken]`, `[sole.rs:pub struct Sole]`, `[sole.rs:pub fn socket_name]`, `[sole.rs:pub fn display_hash]`, `[sole.rs:pub fn ask_the_running_one_to_show]`; `[session.rs:pub fn available]`; `[actions.rs:pub fn open]`, `[actions.rs:pub fn reveal]`, `[actions.rs:pub fn copy_path]`, `[actions.rs:pub fn paste_text]`, `[actions.rs:pub enum Clipboard]`.
- `[decode.rs:pub fn key_of]`, `[decode.rs:pub fn modifier_of]`, `[geometry.rs:pub fn spot_on]`, `[mark.rs:pub fn magnifier]`, `[keys.rs:pub const XK_Control_L]`, `[runtime.rs:wake]`.

## Invariants
- `[lib.rs:#![no_std]]` — unconditional; a crate-level `cfg` would hide the portable half from the only host this house can run the tests on.
- `[x.rs:size_of::<XEvent>()]` — `XEvent` is `24 * size_of::<c_ulong>()`; `XSetWindowAttributes` is 112 bytes with `event_mask` at 72 and `override_redirect` at 88 (LP64, both kernels).
- `[x.rs:size_of::<SockaddrUn>()]` — 110 on Linux, 106 on FreeBSD; each has its own compile-time assertion and struct definition.
- `[x.rs:set_image_size]` — an `XImage` is reused and re-advertised by writing its first two `int`s; the pixels behind it come from `malloc`, because an image's buffer is Xlib's to free.
- `[window.rs:apply_opacity]` — the compositor property is dropped at both ends of the range (`alpha <= 1` or `>= 255`): a `View` that no frame has reached has alpha 0 and must mean "solid", not one 255th opaque. Measured under `xcompmgr`: alpha 64 blended to `#c50607` over red, alpha 255 left `#18191c` solid.
- `[render.rs:Renderer::draw]` — the panel is opaque everywhere inside the rounded shape and transparent at the corners, which is why the fill is drawn one inset row at a time instead of as a rectangle.
- `[font.rs:face_looks_sane]` — a face is only read once `num_glyphs`, `units_per_EM`, the ascender and the descender all land in ranges a real face uses; a refusal is the bitmap font, never a wrong read.
- `[icons.rs:decode_png]` — bit depth 8 and no interlacing, and anything else is `None`; the decoder refuses rather than guesses.
- `[icons.rs:Icon::from_rgba]` — the scale-down is a coverage-weighted average, so a transparent pixel cannot darken the icon's colour.
- `[decode.rs:typed_key]` — a printable key is what `XLookupString` spells, not the unshifted keysym: `Shift+.` must be `>` or every `>` command (settings, menu, preview) is unreachable. Control and Alt keep the keysym path.
- `[decode.rs:key_of]` — control letters are actions before characters; a modifier keysym answers `None`.
- `[geometry.rs:spot_on]` — the window never falls outside the work area; `clamp_anchor` pins an anchor to 0..1000.
- `[mark.rs:magnifier]` — the lens is a ring and the mark is the accent's blue; the same bytes the Windows tray uses.
- `[sole.rs:socket_name]` — an abstract name on Linux, a `/tmp` path on FreeBSD; both carry the uid and a hash of `DISPLAY` so two sessions never collide.
- `[hotkey.rs:Hook::drop]` + `[gui.rs:rearm]` — a settings change drops the armed grab and installs the new one in the same process. Measured under openbox on FreeBSD 15.1: `combo ctrl+space` -> `double alt 400` changed at runtime; the old chord stopped toggling the hidden window and the new double tap showed it.
- `[hotkey.rs:on_key_press]` — passive root grabs are replayed with `XAllowEvents(ReplayKeyboard)` so the user's Control still works.
- `[actions.rs:serve_selection_request]` — the launcher owns CLIPBOARD until another client takes it; every request is answered.

## Side Effects
- `[x.rs:XOpenDisplay]`, `[x.rs:XCreateWindow]`, `[x.rs:XCreateImage]`, `[x.rs:XPutImage]`, `[x.rs:XLookupString]`, `[x.rs:XFillRectangles]`, `[x.rs:XGrabKey]`, `[x.rs:XSetSelectionOwner]`, `[x.rs:XConvertSelection]`, `[x.rs:XSendEvent]` — the X11 platform calls, each a runtime-resolved wrapper.
- `[x.rs:pipe2]`, `[x.rs:poll]`, `[x.rs:fork]`, `[x.rs:execvp]`, `[x.rs:socket]`, `[x.rs:bind]`, `[x.rs:unlink]`, `[x.rs:open]`, `[x.rs:read]`, `[x.rs:malloc]`, `[x.rs:free]` — the libc calls; `unlink` is FreeBSD's stale-socket cleanup, and `open`/`read` are how an icon file is read.
- `[font.rs:dlopen]` — `libfreetype.so.6`, resolved once; a machine without it keeps the bitmap font. `[icons.rs:read_file]` — reads a theme PNG, capped at 512 KiB.
- `[tray.rs:Tray::new]` — docks the icon and draws it; `[hotkey.rs:Hook::install]` — takes a global key grab; `[sole.rs:Sole::take]` — binds the session's socket; `[actions.rs:open]` — starts `xdg-open` in a detached process; `[window.rs:Window::repaint]` — one `XPutImage` per frame.

## Verification
- Run: `cargo test --workspace --no-fail-fast --locked`