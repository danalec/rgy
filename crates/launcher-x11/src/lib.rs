// SPDX-License-Identifier: Apache-2.0

//! The launcher's window on X11: Linux and FreeBSD.
//!
//! A compact overlay that appears over whatever is on screen, draws a
//! field and a list, and gives the answers back. It is plain X11 — no
//! toolkit, no window manager of its own, no runtime — declared by hand
//! in [`x`], the way the Windows launcher declares Win32. The pixels are
//! the tree's own: the glyphs, the icon and the panel are drawn here, in
//! Rust, and Xlib is only asked to put them in a drawable.
//!
//! Both Linux and FreeBSD desktops speak X11, so one backend serves both.
//! The two kernels differ in a handful of libc constants and in the shape
//! of `sockaddr_un` (FreeBSD has no *abstract* Unix sockets); those
//! differences live in [`x`] and [`sole`], behind `#[cfg]` on the OS, and
//! nowhere else.
//!
//! # `no_std`
//!
//! The crate is `no_std` + `alloc`. It does **not** empty itself off its
//! two hosts: the decisions that do not need a display — the key
//! vocabulary, the geometry, the icon's pixels, the keysym decoder — are
//! ordinary modules with ordinary tests, and they are compiled and tested
//! on every host. Only the bodies that call Xlib are behind
//! `#[cfg(any(target_os = "linux", target_os = "freebsd"))]`. A
//! crate-level `cfg` would hide the portable half from the host that is
//! the only one this house can run the tests on, and a test nobody runs is
//! a note about a test.
//!
//! # The interface is the Windows one
//!
//! [`window::Window`], [`window::Event`], [`types::View`] and the rest are
//! the same types `rgy-launcher-windows` exports. `bins/rgxy`'s window
//! driver is one driver, and it reaches the platform through a module
//! alias; a port that invented a second vocabulary would need a second
//! driver, and two drivers are two places for the same bug.

#![no_std]

extern crate alloc;

// The decisions: compiled, linted and tested on every host.
pub mod decode;
pub mod geometry;
pub mod keys;
pub mod mark;
pub mod types;

// The machine: Xlib, libc and the bodies that call them. These are empty
// off Linux and FreeBSD. Xlib is resolved at runtime from `libX11.so.6`,
// so no native library is ever linked on a host that cannot link it.
#[cfg(any(target_os = "linux", target_os = "freebsd"))]
pub mod actions;
#[cfg(any(target_os = "linux", target_os = "freebsd"))]
mod font;
#[cfg(any(target_os = "linux", target_os = "freebsd"))]
mod hotkey;
#[cfg(any(target_os = "linux", target_os = "freebsd"))]
mod icons;
#[cfg(any(target_os = "linux", target_os = "freebsd"))]
mod paint;
#[cfg(any(target_os = "linux", target_os = "freebsd"))]
mod raster;
#[cfg(any(target_os = "linux", target_os = "freebsd"))]
mod render;
#[cfg(any(target_os = "linux", target_os = "freebsd"))]
mod runtime;
#[cfg(any(target_os = "linux", target_os = "freebsd"))]
mod session;
#[cfg(any(target_os = "linux", target_os = "freebsd"))]
pub mod sole;
#[cfg(any(target_os = "linux", target_os = "freebsd"))]
mod tray;
#[cfg(any(target_os = "linux", target_os = "freebsd"))]
pub mod window;
#[cfg(any(target_os = "linux", target_os = "freebsd"))]
mod x;

// The two shapes of the global hotkey keep the Windows module names:
// `gui.rs` says `backend::hook::Hook` and `backend::combo::Combo` on every
// host, and the alias is the whole of the difference.
#[cfg(any(target_os = "linux", target_os = "freebsd"))]
pub mod hook {
    //! The double-tap hotkey.
    pub use crate::hotkey::Hook;
}

#[cfg(any(target_os = "linux", target_os = "freebsd"))]
pub mod combo {
    //! The registered-combo hotkey.
    pub use crate::hotkey::Combo;
}

#[cfg(any(target_os = "linux", target_os = "freebsd"))]
pub use sole::{Sole, Taken};
#[cfg(any(target_os = "linux", target_os = "freebsd"))]
pub use window::{Awareness, Compositor, Event, Key, View, Window};

/// Whether this session has a display to draw on.
///
/// X11 needs `DISPLAY`; a Wayland session reaches this crate through
/// XWayland, which sets it too. A login entry on a machine with no
/// session has nothing to draw on, and the caller falls back to the
/// terminal interface.
pub fn session_available() -> bool {
    #[cfg(any(target_os = "linux", target_os = "freebsd"))]
    {
        session::available()
    }
    #[cfg(not(any(target_os = "linux", target_os = "freebsd")))]
    {
        false
    }
}

/// Give back the console the launcher was started with.
///
/// An X11 session has no console to give back: a resident launcher started
/// from a desktop entry or a login item has its streams on the null device
/// or on a journal, and there is no console window to close. The call is
/// the Windows one's twin, and answering `Ok(())` is the honest answer —
/// the guard did its job by having nothing to do.
pub fn put_the_console_away() -> Result<(), alloc::string::String> {
    Ok(())
}
