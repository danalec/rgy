// SPDX-License-Identifier: Apache-2.0

//! The one launcher's machine state, shared with the threads it starts.
//!
//! There is exactly one resident launcher per session — [`crate::sole`]
//! takes that name before anything is built — so the machine state is one
//! set of numbers, not a table. The window's thread owns the X connection;
//! the daemon's worker thread and the second launcher only ever need to
//! wake the loop, and they do it through an fd that is safe to write from
//! another thread. No Xlib call is ever made from a thread that did not
//! open the display, which is the whole reason there is no `XInitThreads`
//! here.
//!
//! The numbers are set once, before the first worker starts, and are read
//! afterwards. An atomic is the honest word for that: a `Mutex` would need
//! `std`, and this crate is `no_std`.

use core::ffi::{c_int, c_void};
use core::sync::atomic::{AtomicI32, AtomicPtr, AtomicU64, Ordering};

use crate::x;

/// The X connection. Set once by [`Window::new`]; read by the tray, the
/// hotkey and the actions, never written through by them.
static DISPLAY: AtomicPtr<c_void> = AtomicPtr::new(core::ptr::null_mut());
/// The root window the passive key grabs live on.
static ROOT: AtomicU64 = AtomicU64::new(0);
/// The launcher's own window, the one a second launcher asks to show.
static WINDOW: AtomicU64 = AtomicU64::new(0);
/// The read end of the self-pipe the daemon's worker writes to.
static WAKE_READ: AtomicI32 = AtomicI32::new(-1);
/// The write end of the self-pipe.
static WAKE_WRITE: AtomicI32 = AtomicI32::new(-1);
/// The abstract socket a second launcher connects to, when one exists.
static SOLE_LISTEN: AtomicI32 = AtomicI32::new(-1);

/// Install the machine state of the session's one launcher.
pub(crate) fn install(
    display: *mut c_void,
    root: u64,
    window: u64,
    wake_read: c_int,
    wake_write: c_int,
) {
    DISPLAY.store(display, Ordering::Release);
    ROOT.store(root, Ordering::Release);
    WINDOW.store(window, Ordering::Release);
    WAKE_READ.store(wake_read, Ordering::Release);
    WAKE_WRITE.store(wake_write, Ordering::Release);
}

/// The X connection, or null when the window has not been made.
pub(crate) fn display() -> *mut c_void {
    DISPLAY.load(Ordering::Acquire)
}

/// The root window.
pub(crate) fn root() -> u64 {
    ROOT.load(Ordering::Acquire)
}

/// The launcher's own window.
pub(crate) fn window() -> u64 {
    WINDOW.load(Ordering::Acquire)
}

/// The read end of the wake pipe, for the event loop's `poll`.
pub(crate) fn wake_read_fd() -> c_int {
    WAKE_READ.load(Ordering::Acquire)
}

/// Remember the single-instance socket's listening fd.
pub(crate) fn set_sole_listen(fd: c_int) {
    SOLE_LISTEN.store(fd, Ordering::Release);
}

/// The single-instance socket's listening fd, or -1.
///
/// Kept beside the other numbers of the running launcher: the pump asks
/// `sole::listen_fd()` today because the socket owns the fd, but the
/// machine's view of its own descriptors is this module's business.
#[allow(dead_code)]
pub(crate) fn sole_listen_fd() -> c_int {
    SOLE_LISTEN.load(Ordering::Acquire)
}

/// Forget the machine state. The window gives its handles back itself.
pub(crate) fn clear() {
    DISPLAY.store(core::ptr::null_mut(), Ordering::Release);
    ROOT.store(0, Ordering::Release);
    WINDOW.store(0, Ordering::Release);
    WAKE_READ.store(-1, Ordering::Release);
    WAKE_WRITE.store(-1, Ordering::Release);
    SOLE_LISTEN.store(-1, Ordering::Release);
}

/// Wake the window's loop from another thread.
///
/// One byte, non-blocking: a byte that does not fit is a byte the reader
/// has not drained yet, and a reader that has not drained a byte is a
/// reader that is already awake. The window's `poll` sees the fd and
/// drains it before it goes back to sleep.
pub(crate) fn wake() -> bool {
    let fd = WAKE_WRITE.load(Ordering::Acquire);
    if fd < 0 {
        return false;
    }
    let byte = 1u8;
    // SAFETY: `fd` is a pipe write end set by `install` and still owned by
    // the process; the byte is one byte of our own, and `write` to a
    // non-blocking pipe is thread-safe.
    unsafe { x::write(fd, (&raw const byte).cast::<c_void>(), 1) >= 0 }
}
