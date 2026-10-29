// SPDX-License-Identifier: Apache-2.0

//! The launcher's window on Windows.
//!
//! A compact overlay that appears over whatever is on screen, draws a
//! field and a list, and gives the answers back. It is plain Win32 — no
//! toolkit, no window manager, no runtime — because the launcher has to
//! be *there* a few milliseconds after the key, and every layer between
//! the key and the pixels is a layer that can be slow.
//!
//! # `no_std`
//!
//! The crate is `no_std` + `alloc`, and it is empty off Windows: a
//! crate-level `cfg` that empties a crate is a crate with no `no_std` to
//! declare, and this one declares `no_std` first and the `cfg` second.
//!
//! What the window needs is the allocator and nothing else. The Win32
//! calls are hand-written — there is no `windows` crate in this tree, and
//! the seam owns the calls the tree needs from the operating system — the
//! strings are `alloc`'s, and the one lock the hotkey needs is a lock the
//! crate carries itself, because a hook that runs on the message-loop
//! thread never contends with anything.

#![no_std]
#![cfg(windows)]

extern crate alloc;

pub mod actions;
pub mod hook;
pub mod paint;
pub mod session;

pub mod window;

mod win;

pub use window::{Awareness, Compositor, Event, Key, View, Window};

/// Whether this session has a desktop to draw on.
///
/// The question is asked before the window is built: a launcher that
/// cannot show a window has to do something else, and the something else
/// is the caller's decision.
pub fn session_available() -> bool {
    session::available()
}
