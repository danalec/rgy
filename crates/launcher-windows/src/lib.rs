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

use alloc::format;
use alloc::string::String;

pub mod actions;
pub mod combo;
pub mod hook;
pub mod paint;
pub mod session;

pub mod window;

// The notification area is the window's own business: the icon is not an
// API of this crate, and a public function that takes the platform's own
// handle is a public function that says the platform is this crate's
// business.
mod tray;

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

/// Give back the console the launcher was started with, when the console
/// is the launcher's own.
///
/// The launcher is a resident one: it sits in the notification area and
/// waits for the key, and the console Windows gave it — because it was
/// started from a folder, a shortcut or a login entry, none of which has
/// a terminal — is a window the user would have to close by hand.
///
/// Only the launcher's own console goes. The console host's list of
/// processes says whose it is: one process attached means Windows made
/// this console for this launch alone, and two or more mean a shell is
/// sharing it — a console a terminal gave the launcher is the terminal's
/// window, and giving it back would take the terminal the user is typing
/// in. No console at all — a launch that was given pipes — has nothing
/// to ask and nothing to give back.
///
/// The console is **given back** and not hidden. Measured on 2026-11-01,
/// with the Terminal as the delegated console host: the window
/// `GetConsoleWindow` names is not the window the screen has — the
/// Terminal's window carries tabs and has an owner of its own — and
/// hiding the named one leaves the real one on the screen. Giving the
/// console back closes the session's window, or its tab when the host
/// carries more of them, which is the only thing that is true of every
/// host.
///
/// The call has to come **before** anything has been said on the
/// standard streams: the library reads each standard handle once and
/// keeps it, and a write to a handle whose console has been given back
/// is a panic. The streams are pointed at the null device first, so the
/// news the launcher says once on the standard error still has somewhere
/// to go — a resident launch from a folder has nobody reading it anyway,
/// and a launch a terminal shared keeps its console and its streams.
///
/// The answer says what happened, and the reason is the platform's own
/// number read **where the call is**: a code read later is a code about
/// something else. A console that is not the launcher's own is not a
/// failure — the guard did its job — and the answer says so by being
/// the absence of one.
pub fn put_the_console_away() -> Result<(), String> {
    let console = unsafe { win::GetConsoleWindow() };
    if console.is_null() {
        // There is no console to give back: a launch that was given pipes
        // has none, and there is nothing to ask and nothing to give.
        return Ok(());
    }
    // The console host's own list, with room for more than a console is
    // ever shared by: the question is whether the launcher is alone on
    // it, and a list that had to be longer still answers that.
    let mut attached = [0; 8];
    let count = win::console_process_list(&mut attached);
    if !the_console_is_the_launcher_s_own(count) {
        // A console somebody else is typing in is not the launcher's to
        // give back.
        return Ok(());
    }
    // The null device: the streams keep a writer, and nobody has to read
    // it. The handle is never given back — it is the process's standard
    // now, and it goes when the process does.
    let nul: alloc::vec::Vec<u16> = "NUL\0".encode_utf16().collect();
    let nul = unsafe {
        win::CreateFileW(
            nul.as_ptr(),
            win::GENERIC_WRITE,
            win::FILE_SHARE_ALL,
            core::ptr::null(),
            win::OPEN_EXISTING,
            0,
            core::ptr::null_mut(),
        )
    };
    if nul == win::INVALID_HANDLE_VALUE {
        // There is nowhere for the streams to go: the console stays, and
        // a console on the screen is better than a launcher that panics
        // when it has something to say. The reason is the platform's own
        // number, read here where the call is.
        let code = unsafe { win::GetLastError() };
        return Err(format!("the null device could not be opened ({code})"));
    }
    unsafe {
        // Each stream's answer is read where its own call is: the second
        // call would change the first one's code.
        let out = win::SetStdHandle(win::STD_OUTPUT_HANDLE, nul);
        let out_code = if out == 0 { win::GetLastError() } else { 0 };
        let err = win::SetStdHandle(win::STD_ERROR_HANDLE, nul);
        let err_code = if err == 0 { win::GetLastError() } else { 0 };
        if out == 0 {
            // A stream that stayed with the console keeps a writer that
            // works; the console stays with it.
            //
            // The handle is only given back when NEITHER stream took it:
            // a stream that took it points at it, and closing it now would
            // leave the standard output on a closed handle — and the first
            // thing the launcher says would be a panic.
            if err == 0 {
                win::CloseHandle(nul);
            }
            return Err(format!("the standard output kept the console ({out_code})"));
        }
        if err == 0 {
            return Err(format!("the standard error kept the console ({err_code})"));
        }
        // The last process leaving is what closes the console's window
        // or tab, and a refusal leaves everything as it was said: the
        // streams are on the null device and the console is on the
        // screen, which is where it already was.
        if win::FreeConsole() == 0 {
            let code = win::GetLastError();
            return Err(format!("the console could not be given back ({code})"));
        }
    }
    Ok(())
}

/// Whether a console with this many processes on it is the launcher's
/// own.
///
/// One is alone — the console was made for this launch. Zero is the
/// call's refusal, and two or more is a console somebody else is typing
/// in.
fn the_console_is_the_launcher_s_own(attached: u32) -> bool {
    attached == 1
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One process on a console is alone on it.
    ///
    /// The guard is the whole rule: zero is the call's refusal, one is a
    /// console Windows made for this launch, and two is a console the
    /// user is typing in.
    #[test]
    fn one_process_on_a_console_is_alone_on_it() {
        assert!(the_console_is_the_launcher_s_own(1));
        assert!(!the_console_is_the_launcher_s_own(0));
        assert!(!the_console_is_the_launcher_s_own(2));
        assert!(!the_console_is_the_launcher_s_own(9));
    }

    /// A console that is not the launcher's own stays.
    ///
    /// The tests run in a console the harness shares — or with no console
    /// at all, when the run was given pipes, or with a console that was
    /// made without a window, which is a console that was never on the
    /// screen. Either way there is nothing of the launcher's own to give
    /// back, and a console that is there is still the process's console
    /// afterwards. A run that has a console all to itself (a lone test
    /// binary on a fresh console) is not the case the rule speaks about,
    /// and the test says so and returns.
    #[test]
    fn a_console_that_is_not_the_launcher_s_own_stays() {
        let console = unsafe { crate::win::GetConsoleWindow() };
        if console.is_null() {
            return;
        }
        let mut attached = [0; 8];
        let count = unsafe {
            crate::win::GetConsoleProcessList(attached.as_mut_ptr(), attached.len() as u32)
        };
        if the_console_is_the_launcher_s_own(count) {
            return;
        }
        // A console made without a window was never on the screen: the
        // rule is about not taking the console away, and visibility is
        // what it already was.
        let was_visible = unsafe { crate::win::IsWindowVisible(console) } != 0;
        // The guard did its job, and the answer says so: a console that
        // is not the launcher's own is not a failure.
        let answer = put_the_console_away();
        assert!(
            answer.is_ok(),
            "the console that is not ours stayed: {answer:?}"
        );
        let after = unsafe { crate::win::GetConsoleWindow() };
        assert!(
            !after.is_null(),
            "the console the harness shares is still the process's own"
        );
        if was_visible {
            assert!(
                unsafe { crate::win::IsWindowVisible(after) } != 0,
                "the console the harness shares is still on the screen"
            );
        }
    }
}
