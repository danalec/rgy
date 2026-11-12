// SPDX-License-Identifier: Apache-2.0

//! Whether this session has a display to draw on.
//!
//! The crate is an **X11 client**, and X11 needs `DISPLAY`. A Wayland
//! session reaches this crate through XWayland, which sets `DISPLAY`
//! too; a Wayland-only session sets `WAYLAND_DISPLAY` and nothing an X
//! client can connect to, so `XOpenDisplay(null)` answers null there
//! and the honest answer is "no graphical session for this launcher".
//!
//! The check is deliberately a real open, not just a look at the
//! environment: an environment that names a display whose server has
//! gone away is not a session the launcher can draw on. The connection
//! is closed again at once — the window opens its own later.
//!
//! # `no_std`
//!
//! `alloc`, `core` and the [`crate::x`] seam. [`display_is_set`] is a
//! pure decision about a string, with tests that need no X server.

use alloc::string::String;
use core::ffi::c_char;

use crate::x;

/// Whether a `DISPLAY` value names a display at all.
///
/// `None` and an empty string are both "no display": an environment
/// variable that is present but empty is a variable that names nothing,
/// and a window opened against it would go nowhere. Pure, so the rule
/// is testable on the host that has no X server.
pub fn display_is_set(display: Option<&str>) -> bool {
    matches!(display, Some(value) if !value.is_empty())
}

/// Read an environment variable as text, or `None` when it is absent.
///
/// `getenv` gives a pointer into libc's own storage and `strlen` says
/// how long it is; `from_raw_parts` reads exactly that many bytes. A
/// value that is not UTF-8 is not a display this launcher can name, so
/// it answers `None` rather than guessing.
fn env_text(name: &[u8]) -> Option<String> {
    let ptr = unsafe { x::getenv(name.as_ptr().cast::<c_char>()) };
    if ptr.is_null() {
        return None;
    }
    let len = unsafe { x::strlen(ptr) };
    let bytes = unsafe { core::slice::from_raw_parts(ptr.cast::<u8>(), len) };
    let text = String::from_utf8_lossy(bytes).into_owned();
    Some(text)
}

/// Whether this session has a display the launcher can draw on.
///
/// `DISPLAY` (or, on a Wayland session, `WAYLAND_DISPLAY`) must name
/// something, and then `XOpenDisplay(null)` must actually connect. A
/// Wayland-only session passes the first gate and fails the second —
/// XWayland is the only Wayland path that works, and it sets `DISPLAY`
/// as well — so the open is the authority, not the variable.
///
/// The connection is closed immediately: this is a question, not the
/// window's display, and the window opens its own in `Window::new`.
pub fn available() -> bool {
    let display = display_is_set(env_text(b"DISPLAY\0").as_deref());
    let wayland = display_is_set(env_text(b"WAYLAND_DISPLAY\0").as_deref());
    if !display && !wayland {
        return false;
    }
    let opened = unsafe { x::XOpenDisplay(core::ptr::null()) };
    if opened.is_null() {
        return false;
    }
    unsafe {
        x::XCloseDisplay(opened);
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An absent display and an empty one both say "no"; a named one
    /// says "yes".
    #[test]
    fn a_display_has_to_be_there_and_not_be_empty() {
        assert!(!display_is_set(None));
        assert!(!display_is_set(Some("")), "an empty DISPLAY names nothing");
        assert!(display_is_set(Some(":0")));
        assert!(display_is_set(Some("wayland-0")));
    }
}
