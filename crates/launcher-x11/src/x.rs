// SPDX-License-Identifier: Apache-2.0

//! The Xlib and libc calls the X11 launcher makes on Linux and FreeBSD,
//! declared by hand.
//!
//! This is the seam. The tree does not link an X11 binding crate: it does
//! its platform calls by hand, in one place, once. Only the calls the
//! launcher uses are declared — a declaration that is not used is a
//! declaration that can be wrong.
//!
//! One seam serves both hosts: the 62 Xlib wrappers, the `xfn!` macro and
//! the `SYMBOLS` table are shared verbatim, because the two kernels expose
//! the same Xlib ABI. What differs is the libc constant set (`O_*` versus
//! `SOCK_*` flag values, and `SIGCHLD`) and the shape of `sockaddr_un` —
//! FreeBSD's carries a length byte, a one-byte family and a 104-byte path,
//! and FreeBSD has no abstract sockets. Those two differences sit behind
//! `#[cfg(target_os = ...)]` on the constants and on `SockaddrUn`; no Xlib
//! call changes between the hosts.
//!
//! The seam also carries the client-side image path: `XCreateImage` wraps a
//! buffer the launcher owns in an `XImage`, `XPutImage` blits the whole
//! picture in one call, and `XDefaultVisual` / `XDefaultDepth` say what the
//! screen's own visual can hold. Text stops being a built-in bitmap font
//! the same way — `font.rs` dlopens `libfreetype.so.6` with a symbol table
//! of its own, so anti-aliased glyphs are rasterised at runtime and
//! FreeType, like Xlib, is never linked at build time.
//!
//! The Xlib calls are NOT linked at build time. There is no link attribute
//! against `libX11`: the development symlink `libX11.so` is missing on the
//! CI runner and on the musl and aarch64 cross builds, so a static `-lX11`
//! would fail to link there. Instead each call is a Rust wrapper that
//! resolves its function pointer at runtime with `dlopen`/`dlsym` from the
//! runtime soname `libX11.so.6`, which is present on desktop Linux. The
//! final binary therefore needs no native library beyond libc. When the
//! library (or a symbol) is missing, every wrapper answers with a zero
//! value, which lets `Window::new` and `session_available()` fall back to
//! the terminal UI — the honest answer on a machine without X11.
//!
//! The libc calls stay plain `unsafe extern "C"` declarations with no link
//! attribute: every binary this crate ends up in already carries libc, and
//! `dlopen`/`dlsym`/`dlclose` live in libc itself on glibc >= 2.34 and on
//! musl.
//!
//! Every struct the platform writes is the platform's own, whole.
//! `XEvent` is Xlib's union of twenty-four `long`s, not a Rust enum: Xlib
//! writes the whole union, so the struct keeps the whole union and its
//! fields are read back by offset through the accessors below. The size
//! and the offsets of every struct are asserted at compile time, so a
//! layout that drifts fails the build rather than the stack it overwrote.

// The names here are X11's own: `XID`, `KeySym`, `XK_BackSpace`, `pollfd`,
// `XSetWindowAttributes`. Renaming them to satisfy a lint would make the
// seam harder to read against the documentation it implements. The
// constants carry X11's mixed-case spelling too, hence the third allow.
#![allow(non_snake_case, non_camel_case_types)]
#![allow(non_upper_case_globals)]
#![allow(clippy::upper_case_acronyms)]
// The seam is the ONE place the Xlib surface is declared, and the runtime
// resolver resolves the whole table together: a call the current bodies do
// not make is still a call the seam owns, and a table that grew and shrank
// with the bodies would make every index a place to drift. The lint's
// answer here is the module's own allow, not sixty-two small ones.
#![allow(dead_code)]

use core::ffi::{c_char, c_int, c_long, c_short, c_uchar, c_uint, c_ulong, c_ushort, c_void};
use core::mem::{offset_of, size_of};
use core::sync::atomic::{AtomicI8, Ordering};

// --- the platform's own types ------------------------------------------------

/// The opaque connection to the X server: Xlib's `Display`, never opened up.
pub type Display = core::ffi::c_void;
/// Xlib's own resource identifier, a `unsigned long` on this ABI.
pub type XID = core::ffi::c_ulong;
/// A window, one of Xlib's resource identifiers.
pub type Window = XID;
/// Anything that can be drawn into, one of Xlib's resource identifiers.
pub type Drawable = XID;
/// An off-screen drawing surface, one of Xlib's resource identifiers.
pub type Pixmap = XID;
/// A named interned string, one of Xlib's resource identifiers.
pub type Atom = XID;
/// A colour map, one of Xlib's resource identifiers.
pub type Colormap = XID;
/// A mouse cursor, one of Xlib's resource identifiers.
pub type Cursor = XID;
/// A loaded font, one of Xlib's resource identifiers.
pub type Font = XID;
/// A timestamp in milliseconds, Xlib's `Time`.
pub type Time = core::ffi::c_ulong;
/// A symbolic key, Xlib's `KeySym`.
pub type KeySym = core::ffi::c_ulong;
/// A physical key code, Xlib's `KeyCode`.
pub type KeyCode = core::ffi::c_uchar;
/// A graphics context: an opaque pointer Xlib hands back from `XCreateGC`.
pub type GC = *mut core::ffi::c_void;
/// A visual: Xlib's opaque `Visual` struct. Never opened up; the launcher
/// always passes a null `Visual*` to `XCreateWindow`, which asks for the
/// parent's own. Declaring it as the struct (and taking `*mut Visual`
/// below) keeps the parameter a single pointer, the way Xlib's header
/// spells it.
pub type Visual = core::ffi::c_void;
/// Xlib's own boolean, an `int`.
pub type Bool = core::ffi::c_int;
/// Xlib's own status, an `int`: zero is `Success`.
pub type Status = core::ffi::c_int;
/// Xlib's own byte pointer, used by the extension interfaces.
pub type XPointer = *mut core::ffi::c_char;

// --- the protocol numbers -------------------------------------------------

// The event types, event masks, modifier masks and keysyms are X11's
// protocol facts, not platform handles: they live in `crate::keys`, an
// un-gated module whose tests run on every host, and whose integers are
// plain Rust types (a `c_ulong` is four bytes on Windows and eight on
// Linux). Re-exporting them here keeps one number in one place while the
// Linux bodies can still say `x::KeyPress` and `x::XK_Return`.
pub use crate::keys::*;

/// The `CWBackPixel` bit of `XSetWindowAttributes`' value mask.
pub const CWBackPixel: c_ulong = 1 << 1;
/// The `CWBorderPixel` bit of `XSetWindowAttributes`' value mask.
pub const CWBorderPixel: c_ulong = 1 << 3;
/// The `CWOverrideRedirect` bit of `XSetWindowAttributes`' value mask.
pub const CWOverrideRedirect: c_ulong = 1 << 9;
/// The `CWEventMask` bit of `XSetWindowAttributes`' value mask.
pub const CWEventMask: c_ulong = 1 << 11;
/// The `CWColormap` bit of `XSetWindowAttributes`' value mask.
pub const CWColormap: c_ulong = 1 << 13;
/// The `CWCursor` bit of `XSetWindowAttributes`' value mask.
pub const CWCursor: c_ulong = 1 << 14;

/// `ZPixmap`: the image format whose pixels are stored as integers.
///
/// The format `XCreateImage` is asked for: a rectangle of pixel values, one
/// picture's worth, as opposed to `XYBitmap`'s one bit per pixel or
/// `XYPlane`'s bit-planes. The launcher holds ARGB pixels, so this is the only
/// format it asks for.
pub const ZPixmap: c_int = 2;

/// Take the parent's own value for a window attribute.
pub const CopyFromParent: c_int = 0;
/// A window that can be drawn into and shown on the screen.
pub const InputOutput: c_int = 1;
/// The timestamp that means "now" to Xlib.
pub const CurrentTime: Time = 0;
/// Do not wait for a grab to become possible.
pub const GrabModeAsync: c_int = 1;
/// Let the grabbed event continue on to its normal destination.
pub const ReplayKeyboard: c_int = 2;
/// Replace a property instead of appending to it.
pub const PropModeReplace: c_int = 0;
/// The predefined atom for the `STRING` type.
pub const XA_STRING: Atom = 31;
/// The predefined atom for the `ATOM` type.
pub const XA_ATOM: Atom = 4;
/// The predefined atom for the `CARDINAL` type.
pub const XA_CARDINAL: Atom = 6;
/// The status Xlib returns when a call worked.
pub const Success: c_int = 0;
/// The resource identifier that names nothing.
pub const None: XID = 0;
/// Give the focus back to the parent window when the focus window goes away.
pub const RevertToParent: c_int = 2;
/// The `poll` event that says a descriptor has something to read.
pub const POLLIN: i16 = 0x001;
/// The Unix-domain socket family.
pub const AF_UNIX: c_int = 1;
/// A byte-stream socket.
pub const SOCK_STREAM: c_int = 1;
/// The file-status flag that makes a descriptor non-blocking (`pipe2`).
///
/// `pipe2` takes `O_*` flags, whereas `socket()`/`accept4()` take the
/// `SOCK_*` flags below. On Linux `O_NONBLOCK` and `SOCK_NONBLOCK` happen
/// to share a value; on FreeBSD they do not, so the two families are
/// declared apart and each call uses its own.
#[cfg(target_os = "linux")]
pub const O_NONBLOCK: c_int = 0o4000;
/// The `O_NONBLOCK` file-status flag on FreeBSD (4, not Linux's 2048).
#[cfg(target_os = "freebsd")]
pub const O_NONBLOCK: c_int = 0x0004;

/// Close a descriptor on `exec`: the `O_*` flag `pipe2` takes.
///
/// `pipe2` takes `O_*` flags, whereas `socket()`/`accept4()` take
/// `SOCK_*` flags. On Linux `O_CLOEXEC` and `SOCK_CLOEXEC` happen to share
/// the value `0o2000000`; on FreeBSD they do not, so the self-pipe must be
/// given this `O_*` constant rather than `SOCK_CLOEXEC`.
#[cfg(target_os = "linux")]
pub const O_CLOEXEC: c_int = 0o2000000;
/// Close a descriptor on `exec` on FreeBSD: `pipe2`'s `O_*` flag, whose
/// value differs from FreeBSD's `SOCK_CLOEXEC` and from Linux's `O_*` value.
#[cfg(target_os = "freebsd")]
pub const O_CLOEXEC: c_int = 0x0010_0000;

/// Close a socket on `exec` (`socket()`/`accept4()` take this `SOCK_*`).
#[cfg(target_os = "linux")]
pub const SOCK_CLOEXEC: c_int = 0o2000000;
/// Close a socket on `exec` on FreeBSD (`SOCK_*` = 0x1000_0000, not Linux's
/// 0o2000000 and not this host's `O_CLOEXEC`).
#[cfg(target_os = "freebsd")]
pub const SOCK_CLOEXEC: c_int = 0x1000_0000;

/// Put the socket in non-blocking mode (`accept4()`'s `SOCK_*` flag).
#[cfg(target_os = "linux")]
pub const SOCK_NONBLOCK: c_int = 0o4000;
/// Put the socket in non-blocking mode on FreeBSD (`SOCK_*` = 0x2000_0000,
/// not Linux's 0o4000 and not this host's `O_NONBLOCK`).
#[cfg(target_os = "freebsd")]
pub const SOCK_NONBLOCK: c_int = 0x2000_0000;

/// Do not wait for a child that has not exited yet.
pub const WNOHANG: c_int = 1;
/// The signal handler that ignores the signal.
pub const SIG_IGN: usize = 1;
/// The signal sent when a child exits (Linux 17, FreeBSD 20).
#[cfg(target_os = "linux")]
pub const SIGCHLD: c_int = 17;
/// The signal sent when a child exits on FreeBSD (20, not Linux's 17).
#[cfg(target_os = "freebsd")]
pub const SIGCHLD: c_int = 20;

// --- the platform's own structs ----------------------------------------------

/// The window attributes Xlib reads as a whole when `XCreateWindow` or
/// `XChangeWindowAttributes` is given a value mask.
///
/// The struct is the platform's, whole: every field Xlib's own header
/// names is here, in Xlib's order, and the value mask says which of them
/// Xlib is allowed to read. A field left out is a shorter struct than the
/// one Xlib reads — the stack that holds it, and Xlib's own read past its
/// end.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct XSetWindowAttributes {
    /// The pixmap used as the window's background.
    pub background_pixmap: Pixmap,
    /// The pixel used as the window's background.
    pub background_pixel: c_ulong,
    /// The pixmap used as the window's border.
    pub border_pixmap: Pixmap,
    /// The pixel used as the window's border.
    pub border_pixel: c_ulong,
    /// Where the window's contents go when it is resized.
    pub bit_gravity: c_int,
    /// How the window itself is placed when its parent is resized.
    pub win_gravity: c_int,
    /// Whether Xlib keeps the window's contents off-screen.
    pub backing_store: c_int,
    /// Which planes Xlib keeps when it backs the window.
    pub backing_planes: c_ulong,
    /// The pixel Xlib uses for the planes it keeps.
    pub backing_pixel: c_ulong,
    /// Whether Xlib saves the pixels under the window.
    pub save_under: Bool,
    /// The events the window asks for.
    pub event_mask: c_long,
    /// The events the window does not let propagate to its parent.
    pub do_not_propagate_mask: c_long,
    /// Whether the window manager is told to leave this window alone.
    pub override_redirect: Bool,
    /// The colour map the window uses.
    pub colormap: Colormap,
    /// The cursor shown over the window.
    pub cursor: Cursor,
}

/// A rectangle in Xlib's drawing calls, in pixels.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct XRectangle {
    /// The left edge.
    pub x: c_short,
    /// The top edge.
    pub y: c_short,
    /// The width.
    pub width: c_ushort,
    /// The height.
    pub height: c_ushort,
}

/// Xlib's event, kept as the whole union it really is.
///
/// `XEvent` is a union of twenty-four `long`s in Xlib's own header, and
/// Xlib writes the whole union on every `XNextEvent`. It is deliberately
/// not a Rust enum: a Rust enum would be a different size, and Xlib would
/// write past it. The fields are read back by offset through the accessors
/// below, each of which names the byte offset and the Xlib field it reads.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct XEvent {
    /// The union's twenty-four words, as Xlib left them.
    pub data: [c_ulong; 24],
}

/// The descriptor `poll` watches, as libc lays it out.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct pollfd {
    /// The descriptor to watch.
    pub fd: c_int,
    /// The events the caller asks about.
    pub events: c_short,
    /// The events libc says happened.
    pub revents: c_short,
}

/// A Unix-domain socket address on Linux: a 2-byte family and a path.
///
/// The path is the platform's own 108-byte buffer, whole: libc writes into
/// it with `bind` and `connect`, so a shorter array would be a buffer libc
/// writes past. Linux's `sockaddr_un` is `{ u16 sun_family; char[108] }`
/// = 110 bytes, with no length byte.
#[cfg(target_os = "linux")]
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct SockaddrUn {
    /// The address family, `AF_UNIX` for these calls.
    pub family: c_ushort,
    /// The filesystem path, NUL-terminated by the caller.
    pub path: [u8; 108],
}

/// A Unix-domain socket address on FreeBSD: a length byte, a 1-byte family
/// and a 104-byte path.
///
/// BSD `sockaddr` structs carry a leading `sun_len` byte and a one-byte
/// `sa_family_t`, so FreeBSD's `sockaddr_un` is
/// `{ u8 sun_len; u8 sun_family; char sun_path[104] }` = 106 bytes.
/// `sun_len` must be set to the whole address length and `sun_path` must be
/// NUL-terminated (FreeBSD has no abstract sockets); `sole::address` does
/// both. The 104-byte buffer is kept whole because libc writes into it with
/// `bind` and `connect`.
#[cfg(target_os = "freebsd")]
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct SockaddrUn {
    /// The whole address length (`sun_len`), set by the caller.
    pub len: u8,
    /// The address family, `AF_UNIX` for these calls (a one-byte `u8`).
    pub family: u8,
    /// The filesystem path, NUL-terminated by the caller.
    pub path: [u8; 104],
}

// Xlib and libc write these structs whole and read them back by their own
// layout, so the layout is fixed here at compile time: a field left out,
// or one placed at the wrong offset, is a buffer the platform writes past
// or a value read out of the wrong word. The `XSetWindowAttributes`
// numbers are the LP64 (x86_64 Linux) layout this launcher is built for;
// `XEvent` is asserted against the word size, so it holds on either ABI.
const _: () = assert!(size_of::<XSetWindowAttributes>() == 112);
const _: () = assert!(offset_of!(XSetWindowAttributes, event_mask) == 72);
const _: () = assert!(offset_of!(XSetWindowAttributes, override_redirect) == 88);
const _: () = assert!(size_of::<XRectangle>() == 8);
const _: () = assert!(size_of::<XEvent>() == 24 * size_of::<c_ulong>());
const _: () = assert!(size_of::<pollfd>() == 8);
#[cfg(target_os = "linux")]
const _: () = assert!(size_of::<SockaddrUn>() == 110);
#[cfg(target_os = "freebsd")]
const _: () = assert!(size_of::<SockaddrUn>() == 106);

// --- reading the event union by offset ---------------------------------------

/// `XAnyEvent.type` (byte 0, word 0 low half): which event arrived.
pub fn ev_type(e: &XEvent) -> c_int {
    e.data[0] as c_int
}

/// `XAnyEvent.window` (byte 32, word 4): the window the event is for.
pub fn ev_window(e: &XEvent) -> Window {
    e.data[4]
}

/// `XKeyEvent.root` (byte 40, word 5): the root of the event's screen.
pub fn ev_root(e: &XEvent) -> Window {
    e.data[5]
}

/// `XKeyEvent.time` (byte 56, word 7): when the event happened.
pub fn ev_time(e: &XEvent) -> Time {
    e.data[7]
}

/// `XKeyEvent.state` (byte 80, word 10 low half): the modifier mask.
pub fn ev_state(e: &XEvent) -> u32 {
    e.data[10] as u32
}

/// `XKeyEvent.keycode` (byte 84, word 10 high half): the physical key.
pub fn ev_keycode(e: &XEvent) -> u32 {
    (e.data[10] >> 32) as u32
}

/// `XButtonEvent.button` (byte 84, word 10 high half): the button number.
pub fn ev_button(e: &XEvent) -> u32 {
    (e.data[10] >> 32) as u32
}

/// `XKeyEvent.x` (byte 64, word 8 low half): the pointer's x coordinate.
pub fn ev_x(e: &XEvent) -> i32 {
    e.data[8] as i32
}

/// `XKeyEvent.y` (byte 68, word 8 high half): the pointer's y coordinate.
pub fn ev_y(e: &XEvent) -> i32 {
    (e.data[8] >> 32) as i32
}

/// `XConfigureEvent.width` (byte 56, word 7 low half): the new width.
pub fn ev_width(e: &XEvent) -> i32 {
    e.data[7] as i32
}

/// `XConfigureEvent.height` (byte 60, word 7 high half): the new height.
pub fn ev_height(e: &XEvent) -> i32 {
    (e.data[7] >> 32) as i32
}

/// `XClientMessageEvent.message_type` (byte 40, word 5): the atom.
pub fn ev_message_type(e: &XEvent) -> Atom {
    e.data[5]
}

/// `XClientMessageEvent.format` (byte 48, word 6 low half): 8, 16 or 32.
pub fn ev_message_format(e: &XEvent) -> c_int {
    e.data[6] as c_int
}

/// `XClientMessageEvent.data.l[index]` (byte 56 + 8 * index, word 7 + index).
///
/// `index` must be less than five: Xlib's message union holds five `long`s.
pub fn ev_client_data(e: &XEvent, index: usize) -> c_long {
    e.data[7 + index] as c_long
}

/// `XSelectionRequestEvent.requestor` (byte 40, word 5).
pub fn ev_selection_requestor(e: &XEvent) -> Window {
    e.data[5]
}

/// `XSelectionRequestEvent.selection` (byte 48, word 6).
pub fn ev_selection_selection(e: &XEvent) -> Atom {
    e.data[6]
}

/// `XSelectionRequestEvent.target` (byte 56, word 7).
pub fn ev_selection_target(e: &XEvent) -> Atom {
    e.data[7]
}

/// `XSelectionRequestEvent.property` (byte 64, word 8).
pub fn ev_selection_property(e: &XEvent) -> Atom {
    e.data[8]
}

/// `XSelectionRequestEvent.time` (byte 72, word 9).
pub fn ev_selection_time(e: &XEvent) -> Time {
    e.data[9]
}

/// Word 7 (byte 56) of a `SelectionNotify` (`XSelectionEvent`): the atom
/// the launcher reads back from the notification.
pub fn ev_notify_property(e: &XEvent) -> Atom {
    e.data[7]
}

/// Fill an `XEvent` as Xlib's `XClientMessageEvent`, ready for `XSendEvent`.
///
/// Xlib writes the union whole, so the fields are placed by word exactly as
/// the accessors above read them: type at word 0, window at word 4, message
/// type at word 5, format at word 6, and the five `long`s of the message's
/// own union at words 7 through 11.
pub fn set_client_message(
    e: &mut XEvent,
    window: Window,
    message_type: Atom,
    format: c_int,
    data: &[c_long; 5],
) {
    e.data[0] = ClientMessage as c_ulong;
    e.data[4] = window;
    e.data[5] = message_type;
    e.data[6] = format as c_ulong;
    for (dst, src) in e.data[7..12].iter_mut().zip(data.iter()) {
        *dst = *src as c_ulong;
    }
}

/// Set the width and height a preallocated `XImage` advertises.
///
/// `XImage` keeps `width` and `height` as its first two fields, and
/// `XPutImage` reads exactly them (plus `bytes_per_line`) to walk the
/// caller's buffer. So a frame at a new size is these two writes, and the
/// image, its `malloc` buffer and the library's idea of it are all reused:
/// nothing is created or destroyed per frame, which is what keeps a
/// resident launcher from leaking an image per keystroke.
///
/// The offsets are Xlib's public `XImage` layout: two `int`s, then `xoffset`,
/// and they are the same on LP64 and ILP32 because both fields are `int`s at
/// the start of the struct. `bytes_per_line` is left alone: it is the stride
/// the buffer behind the image was allocated with, and a width that no longer
/// fits inside it is the caller's own contract, not something this call can
/// check.
///
/// A null `image` is ignored, so a failed `XCreateImage` needs no check at
/// every call site, and a size no `int` can hold is clamped rather than wrapped
/// into a negative one.
pub fn set_image_size(image: *mut c_void, width: u32, height: u32) {
    if image.is_null() {
        return;
    }
    // A width or a height beyond `int` is a window beyond any screen: clamp it
    // rather than let the cast wrap into a negative extent.
    let width = width.min(i32::MAX as u32);
    let height = height.min(i32::MAX as u32);
    // SAFETY: `image` is an XImage created by `XCreateImage` (the caller's
    // contract), and the write stays inside its first eight bytes.
    unsafe { write_image_size(image, width, height) }
}

/// Write `XImage`'s `width` and `height`, at byte 0 and byte 4.
///
/// The write is unaligned, so it is defined whatever the image was allocated
/// over, and it is a function of its own so that the raw write carries one
/// safety contract in one place rather than sitting inside a public safe
/// signature.
///
/// # Safety
///
/// `image` must be a live `XImage` created by `XCreateImage`.
unsafe fn write_image_size(image: *mut c_void, width: u32, height: u32) {
    // SAFETY: the caller guarantees a live XImage, whose first two fields are
    // `int width; int height;`.
    unsafe {
        let base = image.cast::<u8>();
        core::ptr::write_unaligned(base.cast::<c_int>(), width as c_int);
        core::ptr::write_unaligned(base.add(4).cast::<c_int>(), height as c_int);
    }
}

// --- the Xlib calls ----------------------------------------------------------
//
// Xlib is resolved at runtime, not linked at build time. `libX11.so.6` is
// the runtime soname present on desktop Linux, while the `libX11.so`
// development symlink (and with it a static `-lX11`) is absent on the CI
// runner and on the musl and aarch64 cross builds. The `dl*` helpers come
// from libc, so the final binary needs no native library beyond libc.
//
// The order of `SYMBOLS` is a contract: entry `i` is resolved into `PTRS[i]`,
// and the `xfn!` wrapper with index `i` is the only reader of that slot.

/// The runtime soname of Xlib; the trailing NUL makes it a C string.
const XLIB: &[u8] = b"libX11.so.6\0";
/// `RTLD_LAZY`; `RTLD_LOCAL` is 0 on Linux, so it is left out.
const RTLD_LAZY: c_int = 1;
/// The number of Xlib calls the launcher resolves.
const XLIB_COUNT: usize = 63;

/// The 62 Xlib symbols the launcher uses, in a FIXED order: entry `i`
/// belongs to the `xfn!` wrapper whose index is `i`. Each is a NUL-terminated
/// C string exactly matching its C symbol name.
#[rustfmt::skip]
const SYMBOLS: [&[u8]; XLIB_COUNT] = [
    b"XOpenDisplay\0", // 0
    b"XCloseDisplay\0", // 1
    b"XInitThreads\0", // 2
    b"XDefaultScreen\0", // 3
    b"XDefaultRootWindow\0", // 4
    b"XRootWindow\0", // 5
    b"XConnectionNumber\0", // 6
    b"XDisplayWidth\0", // 7
    b"XDisplayHeight\0", // 8
    b"XFlush\0", // 9
    b"XSync\0", // 10
    b"XPending\0", // 11
    b"XNextEvent\0", // 12
    b"XCreateSimpleWindow\0", // 13
    b"XCreateWindow\0", // 14
    b"XChangeWindowAttributes\0", // 15
    b"XSelectInput\0", // 16
    b"XMapWindow\0", // 17
    b"XMapRaised\0", // 18
    b"XUnmapWindow\0", // 19
    b"XDestroyWindow\0", // 20
    b"XMoveResizeWindow\0", // 21
    b"XResizeWindow\0", // 22
    b"XStoreName\0", // 23
    b"XCreateGC\0", // 24
    b"XFreeGC\0", // 25
    b"XSetForeground\0", // 26
    b"XSetBackground\0", // 27
    b"XSetLineAttributes\0", // 28
    b"XFillRectangle\0", // 29
    b"XFillRectangles\0", // 30
    b"XDrawRectangle\0", // 31
    b"XDrawLine\0", // 32
    b"XClearWindow\0", // 33
    b"XCreatePixmap\0", // 34
    b"XFreePixmap\0", // 35
    b"XCopyArea\0", // 36
    b"XInternAtom\0", // 37
    b"XChangeProperty\0", // 38
    b"XGetWindowProperty\0", // 39
    b"XDeleteProperty\0", // 40
    b"XGetSelectionOwner\0", // 41
    b"XSetSelectionOwner\0", // 42
    b"XConvertSelection\0", // 43
    b"XSendEvent\0", // 44
    b"XGrabKey\0", // 45
    b"XUngrabKey\0", // 46
    b"XAllowEvents\0", // 47
    b"XKeysymToKeycode\0", // 48
    b"XKeycodeToKeysym\0", // 49
    b"XLookupKeysym\0", // 50
    b"XStringToKeysym\0", // 51
    b"XSetInputFocus\0", // 52
    b"XGetInputFocus\0", // 53
    b"XQueryPointer\0", // 54
    b"XGrabPointer\0", // 55
    b"XUngrabPointer\0", // 56
    b"XFree\0", // 57
    b"XCreateImage\0", // 58
    b"XPutImage\0", // 59
    b"XDefaultVisual\0", // 60
    b"XDefaultDepth\0", // 61
    b"XLookupString\0", // 62
];

/// The length of `SYMBOLS`, used as the pointer table's length.
const XL: usize = SYMBOLS.len();
// A missing wrapper is a compile error: the count is fixed at 63.
const _: () = assert!(SYMBOLS.len() == 63);
const _: () = assert!(XL == XLIB_COUNT);

// `PTRS` is a `static mut` rather than a lock or a `OnceCell`: this crate is
// `no_std`, and every Xlib call happens on the launcher's single window
// thread. `STATE` gates it — only the thread that wins the 0 -> 2
// compare-exchange writes the slots, and every reader waits for `STATE` to
// leave 2 before touching them, so there is no concurrent write and no torn
// read. A `static mut` read or write is therefore sound here.
static mut PTRS: [*mut c_void; XL] = [core::ptr::null_mut(); XL];
/// Load state: 0 = not tried, 2 = loading, 1 = ready, -1 = failed.
static STATE: AtomicI8 = AtomicI8::new(0);

/// Is Xlib present on this machine?
///
/// A false answer means "no X11 on this machine": every Xlib wrapper returns
/// a zero value and the caller falls back to the terminal UI.
pub fn available() -> bool {
    ensure_loaded();
    STATE.load(Ordering::Acquire) == 1
}

/// Resolve every symbol once, on the first Xlib call.
fn ensure_loaded() {
    let state = STATE.load(Ordering::Acquire);
    if state == 1 || state == -1 {
        return;
    }
    if STATE
        .compare_exchange(0, 2, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        // Another thread is loading (or has already finished); wait only
        // while it is still in the loading state, then return.
        while STATE.load(Ordering::Acquire) == 2 {
            core::hint::spin_loop();
        }
        return;
    }
    // We own the 0 -> 2 transition: load every symbol, then publish.
    let ok = unsafe { load_all() };
    STATE.store(if ok { 1 } else { -1 }, Ordering::Release);
}

/// `dlopen` the runtime soname and resolve every symbol into `PTRS`.
///
/// SAFETY: only the `STATE == 2` winner calls this, once, on the window
/// thread, so `PTRS` is written by exactly one thread before `STATE` is
/// published as ready.
unsafe fn load_all() -> bool {
    // SAFETY: `XLIB` and every `SYMBOLS` entry are NUL-terminated C strings,
    // and `handle` is a live library handle for the whole loop.
    unsafe {
        let handle = dlopen(XLIB.as_ptr().cast(), RTLD_LAZY);
        if handle.is_null() {
            return false;
        }
        for (i, name) in SYMBOLS.iter().enumerate() {
            PTRS[i] = dlsym(handle, name.as_ptr().cast());
            if PTRS[i].is_null() {
                // A symbol is missing. Leave the library loaded: the process
                // may still call a wrapper, which must answer zero rather
                // than jump through a stale pointer into an unloaded library.
                return false;
            }
        }
    }
    // Success: deliberately do NOT `dlclose`. The pointers stay valid for
    // the process's whole life, which is exactly how long the launcher
    // needs them.
    true
}

/// Return the resolved pointer for symbol `i`, or null when Xlib is absent.
fn sym(i: usize) -> *mut c_void {
    ensure_loaded();
    if STATE.load(Ordering::Acquire) != 1 {
        return core::ptr::null_mut();
    }
    // SAFETY: `STATE == 1` means every `PTRS` slot was filled by `load_all`.
    unsafe { PTRS[i] }
}

// Transmuting a data pointer to a function pointer needs equal sizes; this
// holds on every target this crate is built for.
const _: () = assert!(size_of::<*mut c_void>() == size_of::<usize>());

/// Generate one Xlib wrapper: resolve symbol `$idx` and call it, or return
/// the zero value when the library is absent.
macro_rules! xfn {
    ($idx:expr, $name:ident ( $($a:ident : $t:ty),* ) -> $r:ty) => {
        #[allow(non_snake_case, clippy::too_many_arguments)]
        #[doc = concat!(
            "The Xlib `", stringify!($name), "` call, resolved at runtime.\n\n",
            "# Safety\n\n",
            "The arguments must satisfy Xlib's own contract for `",
            stringify!($name),
            "`; call it on the launcher's window thread. Returns the zero ",
            "value when no X11 library is present, so callers fall back to ",
            "the terminal interface.",
        )]
        pub unsafe fn $name($($a: $t),*) -> $r {
            const _: () = assert!(($idx as usize) < XL);
            let p = sym($idx);
            if p.is_null() {
                // No X11 on this machine: the zero answer is the platform's
                // own "it did not work" and the caller falls back to the
                // terminal interface.
                // SAFETY: every return type here is an integer or a pointer,
                // for which an all-zero bit pattern is a valid value.
                return unsafe { core::mem::zeroed::<$r>() };
            }
            // SAFETY: `p` came from `dlsym` for exactly this symbol, and the
            // function-pointer type below was declared by hand against
            // Xlib's header; a data and a function pointer are the same size
            // here (asserted above).
            unsafe {
                let f: unsafe extern "C" fn($($t),*) -> $r =
                    core::mem::transmute::<*mut c_void, unsafe extern "C" fn($($t),*) -> $r>(p);
                f($($a),*)
            }
        }
    };
}

// One wrapper per symbol, in SYMBOLS' order. Names, argument types and
// return types are byte-for-byte the old `extern "C"` declarations. The
// signatures are Xlib's C ABI verbatim, so rustfmt is told to leave each
// on one line rather than wrap it into a shape the macro would not match;
// that also keeps the formatting identical across rustfmt versions.
#[rustfmt::skip]
xfn!(0, XOpenDisplay(name: *const c_char) -> *mut Display);
#[rustfmt::skip]
xfn!(1, XCloseDisplay(display: *mut Display) -> c_int);
#[rustfmt::skip]
xfn!(2, XInitThreads() -> Status);
#[rustfmt::skip]
xfn!(3, XDefaultScreen(display: *mut Display) -> c_int);
#[rustfmt::skip]
xfn!(4, XDefaultRootWindow(display: *mut Display) -> Window);
#[rustfmt::skip]
xfn!(5, XRootWindow(display: *mut Display, screen: c_int) -> Window);
#[rustfmt::skip]
xfn!(6, XConnectionNumber(display: *mut Display) -> c_int);
#[rustfmt::skip]
xfn!(7, XDisplayWidth(display: *mut Display, screen: c_int) -> c_int);
#[rustfmt::skip]
xfn!(8, XDisplayHeight(display: *mut Display, screen: c_int) -> c_int);
#[rustfmt::skip]
xfn!(9, XFlush(display: *mut Display) -> c_int);
#[rustfmt::skip]
xfn!(10, XSync(display: *mut Display, discard: Bool) -> c_int);
#[rustfmt::skip]
xfn!(11, XPending(display: *mut Display) -> c_int);
#[rustfmt::skip]
xfn!(12, XNextEvent(display: *mut Display, event: *mut XEvent) -> c_int);
#[rustfmt::skip]
xfn!(13, XCreateSimpleWindow(display: *mut Display, parent: Window, x: c_int, y: c_int, width: c_uint, height: c_uint, border_width: c_uint, border: c_ulong, background: c_ulong) -> Window);
#[rustfmt::skip]
xfn!(14, XCreateWindow(display: *mut Display, parent: Window, x: c_int, y: c_int, width: c_uint, height: c_uint, border_width: c_uint, depth: c_int, class: c_int, visual: *mut Visual, valuemask: c_ulong, attributes: *mut XSetWindowAttributes) -> Window);
#[rustfmt::skip]
xfn!(15, XChangeWindowAttributes(display: *mut Display, w: Window, valuemask: c_ulong, attributes: *mut XSetWindowAttributes) -> c_int);
#[rustfmt::skip]
xfn!(16, XSelectInput(display: *mut Display, w: Window, event_mask: c_long) -> c_int);
#[rustfmt::skip]
xfn!(17, XMapWindow(display: *mut Display, w: Window) -> c_int);
#[rustfmt::skip]
xfn!(18, XMapRaised(display: *mut Display, w: Window) -> c_int);
#[rustfmt::skip]
xfn!(19, XUnmapWindow(display: *mut Display, w: Window) -> c_int);
#[rustfmt::skip]
xfn!(20, XDestroyWindow(display: *mut Display, w: Window) -> c_int);
#[rustfmt::skip]
xfn!(21, XMoveResizeWindow(display: *mut Display, w: Window, x: c_int, y: c_int, width: c_uint, height: c_uint) -> c_int);
#[rustfmt::skip]
xfn!(22, XResizeWindow(display: *mut Display, w: Window, width: c_uint, height: c_uint) -> c_int);
#[rustfmt::skip]
xfn!(23, XStoreName(display: *mut Display, w: Window, name: *const c_char) -> c_int);
#[rustfmt::skip]
xfn!(24, XCreateGC(display: *mut Display, drawable: Drawable, valuemask: c_ulong, values: *mut c_void) -> GC);
#[rustfmt::skip]
xfn!(25, XFreeGC(display: *mut Display, gc: GC) -> c_int);
#[rustfmt::skip]
xfn!(26, XSetForeground(display: *mut Display, gc: GC, foreground: c_ulong) -> c_int);
#[rustfmt::skip]
xfn!(27, XSetBackground(display: *mut Display, gc: GC, background: c_ulong) -> c_int);
#[rustfmt::skip]
xfn!(28, XSetLineAttributes(display: *mut Display, gc: GC, line_width: c_uint, line_style: c_int, cap_style: c_int, join_style: c_int) -> c_int);
#[rustfmt::skip]
xfn!(29, XFillRectangle(display: *mut Display, drawable: Drawable, gc: GC, x: c_int, y: c_int, width: c_uint, height: c_uint) -> c_int);
#[rustfmt::skip]
xfn!(30, XFillRectangles(display: *mut Display, drawable: Drawable, gc: GC, rectangles: *const XRectangle, n: c_int) -> c_int);
#[rustfmt::skip]
xfn!(31, XDrawRectangle(display: *mut Display, drawable: Drawable, gc: GC, x: c_int, y: c_int, width: c_uint, height: c_uint) -> c_int);
#[rustfmt::skip]
xfn!(32, XDrawLine(display: *mut Display, drawable: Drawable, gc: GC, x1: c_int, y1: c_int, x2: c_int, y2: c_int) -> c_int);
#[rustfmt::skip]
xfn!(33, XClearWindow(display: *mut Display, w: Window) -> c_int);
#[rustfmt::skip]
xfn!(34, XCreatePixmap(display: *mut Display, drawable: Drawable, width: c_uint, height: c_uint, depth: c_uint) -> Pixmap);
#[rustfmt::skip]
xfn!(35, XFreePixmap(display: *mut Display, pixmap: Pixmap) -> c_int);
#[rustfmt::skip]
xfn!(36, XCopyArea(display: *mut Display, src: Drawable, dest: Drawable, gc: GC, src_x: c_int, src_y: c_int, width: c_uint, height: c_uint, dest_x: c_int, dest_y: c_int) -> c_int);
#[rustfmt::skip]
xfn!(37, XInternAtom(display: *mut Display, name: *const c_char, only_if_exists: Bool) -> Atom);
#[rustfmt::skip]
xfn!(38, XChangeProperty(display: *mut Display, w: Window, property: Atom, type_: Atom, format: c_int, mode: c_int, data: *const c_uchar, nelements: c_int) -> c_int);
#[rustfmt::skip]
xfn!(39, XGetWindowProperty(display: *mut Display, w: Window, property: Atom, long_offset: c_long, long_length: c_long, delete: Bool, req_type: Atom, actual_format: *mut c_int, actual_type: *mut Atom, nitems: *mut c_ulong, bytes_after: *mut c_ulong, prop: *mut *mut c_uchar) -> c_int);
#[rustfmt::skip]
xfn!(40, XDeleteProperty(display: *mut Display, w: Window, property: Atom) -> c_int);
#[rustfmt::skip]
xfn!(41, XGetSelectionOwner(display: *mut Display, selection: Atom) -> Window);
#[rustfmt::skip]
xfn!(42, XSetSelectionOwner(display: *mut Display, selection: Atom, owner: Window, time: Time) -> c_int);
#[rustfmt::skip]
xfn!(43, XConvertSelection(display: *mut Display, selection: Atom, target: Atom, property: Atom, requestor: Window, time: Time) -> c_int);
#[rustfmt::skip]
xfn!(44, XSendEvent(display: *mut Display, w: Window, propagate: Bool, event_mask: c_long, event: *mut XEvent) -> Status);
#[rustfmt::skip]
xfn!(45, XGrabKey(display: *mut Display, keycode: c_int, modifiers: c_uint, grab_window: Window, owner_events: Bool, pointer_mode: c_int, keyboard_mode: c_int) -> c_int);
#[rustfmt::skip]
xfn!(46, XUngrabKey(display: *mut Display, keycode: c_int, modifiers: c_uint, grab_window: Window) -> c_int);
#[rustfmt::skip]
xfn!(47, XAllowEvents(display: *mut Display, event_mode: c_int, time: Time) -> c_int);
#[rustfmt::skip]
xfn!(48, XKeysymToKeycode(display: *mut Display, keysym: KeySym) -> KeyCode);
#[rustfmt::skip]
xfn!(49, XKeycodeToKeysym(display: *mut Display, keycode: KeyCode, index: c_int) -> KeySym);
#[rustfmt::skip]
xfn!(50, XLookupKeysym(event: *const XEvent, index: c_int) -> KeySym);
#[rustfmt::skip]
xfn!(51, XStringToKeysym(name: *const c_char) -> KeySym);
#[rustfmt::skip]
xfn!(52, XSetInputFocus(display: *mut Display, focus: Window, revert_to: c_int, time: Time) -> c_int);
#[rustfmt::skip]
xfn!(53, XGetInputFocus(display: *mut Display, focus: *mut Window, revert_to: *mut c_int) -> c_int);
#[rustfmt::skip]
xfn!(54, XQueryPointer(display: *mut Display, w: Window, root_return: *mut Window, child_return: *mut Window, root_x: *mut c_int, root_y: *mut c_int, win_x: *mut c_int, win_y: *mut c_int, mask_return: *mut c_uint) -> Bool);
#[rustfmt::skip]
xfn!(55, XGrabPointer(display: *mut Display, grab_window: Window, owner_events: Bool, event_mask: c_uint, pointer_mode: c_int, keyboard_mode: c_int, confine_to: Window, cursor: Cursor, time: Time) -> c_int);
#[rustfmt::skip]
xfn!(56, XUngrabPointer(display: *mut Display, time: Time) -> c_int);
#[rustfmt::skip]
xfn!(57, XFree(data: *mut c_void) -> c_int);
#[rustfmt::skip]
xfn!(58, XCreateImage(display: *mut Display, visual: *mut Visual, depth: c_uint, format: c_int, offset: c_int, data: *mut c_char, width: c_uint, height: c_uint, bitmap_pad: c_int, bytes_per_line: c_int) -> *mut c_void);
#[rustfmt::skip]
xfn!(59, XPutImage(display: *mut Display, drawable: Drawable, gc: GC, image: *mut c_void, src_x: c_int, src_y: c_int, dest_x: c_int, dest_y: c_int, width: c_uint, height: c_uint) -> c_int);
#[rustfmt::skip]
xfn!(60, XDefaultVisual(display: *mut Display, screen: c_int) -> *mut Visual);
#[rustfmt::skip]
xfn!(61, XDefaultDepth(display: *mut Display, screen: c_int) -> c_int);
#[rustfmt::skip]
xfn!(62, XLookupString(event: *const XEvent, buffer: *mut c_char, bytes_buffer: c_int, keysym: *mut KeySym, status: *mut c_void) -> c_int);

// --- the libc calls ----------------------------------------------------------
//
// No build-time link attribute: every binary this crate ends up in already
// carries libc. The three `dl*` helpers below live in libc itself on
// glibc >= 2.34 and on musl, and the names are the C names, so no name
// remapping is needed either.

unsafe extern "C" {
    /// Write `count` bytes to a file descriptor.
    pub fn write(fd: c_int, buf: *const c_void, count: usize) -> isize;
    /// Read up to `count` bytes from a file descriptor.
    pub fn read(fd: c_int, buf: *mut c_void, count: usize) -> isize;
    /// Close a file descriptor.
    pub fn close(fd: c_int) -> c_int;
    /// Remove a name from the filesystem. On FreeBSD, `sole::take` calls
    /// this to `unlink` a stale socket file before `bind`, and `Sole`'s
    /// `Drop` calls it on a clean exit; FreeBSD has no abstract sockets,
    /// so the claim is a real path that can outlive a killed launcher.
    /// Linux's abstract name needs no file and uses no `unlink`.
    pub fn unlink(path: *const c_char) -> c_int;
    /// Make a pair of connected pipes with the given flags.
    pub fn pipe2(fds: *mut c_int, flags: c_int) -> c_int;
    /// Wait for one of a set of descriptors to become ready.
    pub fn poll(fds: *mut pollfd, nfds: usize, timeout: c_int) -> c_int;
    /// Read an environment variable; the pointer is libc's own storage.
    pub fn getenv(name: *const c_char) -> *mut c_char;
    /// The process's real user id.
    pub fn getuid() -> u32;
    /// Open a socket of a family and type.
    pub fn socket(domain: c_int, type_: c_int, protocol: c_int) -> c_int;
    /// Give a socket a name; `len` is a `socklen_t`, a `u32` on this ABI.
    pub fn bind(fd: c_int, addr: *const SockaddrUn, len: u32) -> c_int;
    /// Let a socket accept incoming connections.
    pub fn listen(fd: c_int, backlog: c_int) -> c_int;
    /// Accept a connection with the given flags; `addr` and `len` are unused
    /// by the launcher but are the platform's own out-parameters.
    pub fn accept4(fd: c_int, addr: *mut SockaddrUn, len: *mut u32, flags: c_int) -> c_int;
    /// Connect a socket to a named peer.
    pub fn connect(fd: c_int, addr: *const SockaddrUn, len: u32) -> c_int;
    /// Send bytes on a connected socket.
    pub fn send(fd: c_int, buf: *const c_void, len: usize, flags: c_int) -> isize;
    /// Make this process and a copy of it.
    pub fn fork() -> c_int;
    /// Become the leader of a new session, with no controlling terminal.
    pub fn setsid() -> c_int;
    /// Replace this process image with the program named by `file`.
    pub fn execvp(file: *const c_char, argv: *const *const c_char) -> c_int;
    /// Leave this process without running any more Rust code.
    pub fn _exit(code: c_int) -> !;
    /// Wait for a child, or merely ask whether it has exited.
    pub fn waitpid(pid: c_int, status: *mut c_int, options: c_int) -> c_int;
    /// Set a signal's handler; `SIG_IGN` is the handler that ignores it.
    pub fn signal(sig: c_int, handler: usize) -> usize;
    /// The length of a NUL-terminated C string.
    pub fn strlen(s: *const c_char) -> usize;
    /// Open a file read-only: `O_RDONLY` is 0 on both hosts, so the only
    /// argument that matters is the path.
    pub fn open(path: *const c_char, flags: c_int, ...) -> c_int;
    /// Read bytes from a file descriptor.
    /// Allocate bytes with the allocator Xlib itself uses.
    ///
    /// `XCreateImage` takes a buffer the caller owns and `XDestroyImage` gives
    /// that buffer back with libc's `free` — `XDestroyImage` is a macro in
    /// Xlib's header and not a symbol, so the launcher calls `free` here
    /// itself. A buffer for an image must therefore come from this allocator
    /// and from nowhere else: a `Vec`'s pointer handed to `XCreateImage` would
    /// be freed by libc and then a second time by the `Vec`.
    pub fn malloc(size: usize) -> *mut c_void;
    /// Give memory back to that allocator: the `free` that Xlib's
    /// `XDestroyImage` would have called.
    pub fn free(p: *mut c_void);
    /// Open a shared library by its soname; null on failure.
    pub unsafe fn dlopen(filename: *const c_char, flags: c_int) -> *mut c_void;
    /// Find a symbol in an open library; null when it is not there.
    pub unsafe fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
    /// Close an open library.
    pub unsafe fn dlclose(handle: *mut c_void) -> c_int;
}

// --- tests -------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// `XEvent` is Xlib's union of twenty-four `long`s: 192 bytes on LP64.
    #[test]
    fn xevent_is_the_union_xlib_writes() {
        assert_eq!(size_of::<XEvent>(), 24 * size_of::<c_ulong>());
        if size_of::<usize>() == 8 {
            assert_eq!(size_of::<XEvent>(), 192);
        }
    }

    /// The layout of `XSetWindowAttributes` is the platform's own.
    #[test]
    fn window_attributes_layout_is_the_platform_s() {
        if size_of::<usize>() == 8 {
            assert_eq!(size_of::<XSetWindowAttributes>(), 112);
            assert_eq!(offset_of!(XSetWindowAttributes, event_mask), 72);
            assert_eq!(offset_of!(XSetWindowAttributes, override_redirect), 88);
        }
    }

    /// The small structs libc and Xlib write are their whole sizes.
    #[test]
    fn small_structs_are_the_platform_s_sizes() {
        assert_eq!(size_of::<pollfd>(), 8);
        assert_eq!(size_of::<XRectangle>(), 8);
        #[cfg(target_os = "linux")]
        assert_eq!(size_of::<SockaddrUn>(), 110);
        #[cfg(target_os = "freebsd")]
        assert_eq!(size_of::<SockaddrUn>(), 106);
    }

    /// The accessors read the union by the same offsets Xlib wrote it at.
    #[test]
    fn accessors_read_the_union_by_word() {
        let mut e = XEvent::default();

        let state: c_ulong = 0x25;
        let keycode: c_ulong = 0x38;
        e.data[10] = state | (keycode << 32);
        assert_eq!(ev_state(&e), 0x25);
        assert_eq!(ev_keycode(&e), 0x38);
        assert_eq!(ev_button(&e), 0x38);

        let width: c_ulong = 640;
        let height: c_ulong = 480;
        e.data[7] = width | (height << 32);
        assert_eq!(ev_width(&e), 640);
        assert_eq!(ev_height(&e), 480);

        e.data[0] = 33;
        assert_eq!(ev_type(&e), ClientMessage);
    }

    /// The symbol table is well-formed: one entry per wrapper, each a
    /// NUL-terminated C string, and no two names equal. This is pure data —
    /// it never calls `dlopen`.
    #[test]
    fn symbols_match_the_wrapper_count() {
        assert_eq!(SYMBOLS.len(), XLIB_COUNT);
        for name in SYMBOLS {
            assert_eq!(name.last(), Some(&0), "symbol name must end in NUL");
        }
        for (i, name) in SYMBOLS.iter().enumerate() {
            for (j, other) in SYMBOLS.iter().enumerate().skip(i + 1) {
                assert_ne!(name, other, "duplicate symbol at {i} and {j}");
            }
        }
    }
}
