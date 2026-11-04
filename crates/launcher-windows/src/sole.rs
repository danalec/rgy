// SPDX-License-Identifier: Apache-2.0

//! One launcher per session.
//!
//! The launcher is resident, and everything it holds is a **global**: a
//! keyboard hook that sees every key on the machine, an icon in the
//! notification area, a topmost window, a clock. A second `rgxy` is a
//! second one of each — two hooks competing for the same double tap, two
//! icons the user has to find, two windows over the same desktop — and
//! the second one is exactly what a user who forgot the hotkey or the
//! icon gets when they start the launcher again.
//!
//! Measured on 2026-11-04: two starts were two residents, and the second
//! one's hotkey never fired because the first one's hook answered first.
//!
//! So the launcher takes a **named mutex** for its session before it
//! builds anything. The first one gets the name; the second finds it
//! taken, asks the first one's window to show itself and leaves — a
//! launcher that is already there is a launcher to *show*, not a second
//! launcher to start.
//!
//! Two rules in that:
//!
//! - The name is `Local\` — the session's own namespace. Two users
//!   logged in at once have two launchers, and a launcher in another
//!   session is not this session's launcher. A window cannot be shown
//!   across sessions anyway.
//! - The name is claimed **before the window exists**, so a second start
//!   that arrives while the first is still building has to wait for the
//!   window rather than conclude there is none. The wait is bounded: a
//!   launcher that is wedged is a launcher the user will kill, and the
//!   second start must not pile up behind it.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use crate::win::*;
use crate::window;

/// The name of the session's one-launcher mutex.
///
/// The prefix is the session's namespace, the rest is the program's. A
/// name that says nothing about the program is a name another program
/// can take, and a launcher that refuses to start because something else
/// holds a name is a launcher nobody can explain.
const NAME: &str = "Local\\rgy-rgxy-launcher";

/// How long the second launcher waits for the first one's window.
///
/// The first launcher has the name before it has a window, and the gap
/// is the window's creation and the first frame — milliseconds on this
/// host. Two seconds is the same budget the window gives the daemon's
/// answer, and it is long enough for a cold start on a busy machine
/// without being long enough to look like a hang.
const PATIENCE_MS: u32 = 2000;

/// How long the second launcher looks before it looks again.
const STEP_MS: u32 = 50;

/// What the launcher found when it asked for the session.
#[derive(Debug)]
pub enum Taken {
    /// This process is the session's launcher, and holds the claim.
    First(Sole),
    /// Another launcher holds the claim: this process is the second.
    Other,
    /// The platform would not answer the question, with the reason to
    /// say. The launcher runs anyway: refusing to start because the
    /// question could not be asked would make a broken mutex the only
    /// thing that can stop the launcher.
    Unknown(String),
}

/// The claim a launcher takes on its session.
///
/// The handle is the claim: it goes when the process does, and a process
/// that ends gives the name back — a launcher that was killed is a
/// launcher that can be started again.
#[derive(Debug)]
pub struct Sole {
    /// The mutex's handle. Owned by this struct, closed when it drops.
    handle: HANDLE,
}

impl Sole {
    /// Ask for the session, and learn what this process is.
    pub fn take() -> Taken {
        Sole::take_named(NAME)
    }

    /// The same question, for a name the caller gives.
    ///
    /// The name is the launcher's constant in the product, and the tests
    /// take a name of their own: a suite that ran beside a resident
    /// launcher would be a suite that found the launcher's name taken and
    /// reported the wrong answer for the test.
    pub(crate) fn take_named(name: &str) -> Taken {
        let name = wide(name);
        // SAFETY: the name is this module's own UTF-16 string, the
        // attributes are the platform's default ones, and the answer is
        // checked before it is used.
        let handle = unsafe { CreateMutexW(core::ptr::null(), 1, name.as_ptr()) };
        if handle.is_null() {
            let code = unsafe { GetLastError() };
            return Taken::Unknown(format!(
                "the launcher's own name could not be taken ({code})"
            ));
        }
        // The call's own answer to "was it there already" is the last
        // error, and it is read here, where the call is: a code read
        // after another call is a code about something else.
        let code = unsafe { GetLastError() };
        if code == ERROR_ALREADY_EXISTS {
            // The name is the other launcher's, and this handle does not
            // own it: it is closed, not released.
            unsafe {
                CloseHandle(handle);
            }
            return Taken::Other;
        }
        Taken::First(Sole { handle })
    }

    /// Give the name back, when the launcher is done before the process is.
    pub fn release(self) {
        // The drop closes the handle, which releases the name: a mutex
        // is released by the handle that owns it going away.
        drop(self);
    }
}

impl Drop for Sole {
    fn drop(&mut self) {
        // SAFETY: the handle came from `CreateMutexW` and was never
        // given to anyone else.
        unsafe {
            CloseHandle(self.handle);
        }
    }
}

/// Ask the launcher that is already running to show itself.
///
/// True when the window was found and asked. The window may not exist
/// yet — the first launcher holds the name before it builds anything —
/// so the search is patient, and it ends: a second start that waits for
/// ever behind a wedged launcher is worse than a second start that says
/// it could not find the one that is there.
pub fn ask_the_running_one_to_show() -> bool {
    let mut waited = 0;
    loop {
        if let Some(hwnd) = window::find_window() {
            window::ask_to_show(hwnd);
            return true;
        }
        if waited >= PATIENCE_MS {
            return false;
        }
        // SAFETY: the wait is the platform's own, and it does not hold
        // anything the platform needs.
        unsafe {
            Sleep(STEP_MS);
        }
        waited += STEP_MS;
    }
}

/// A UTF-16 string with a NUL, for the calls that take one.
fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(core::iter::once(0)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The name is one launcher's at a time.
    ///
    /// The test's own name, not the product's: a suite that ran beside a
    /// resident launcher would find the product's name taken and call
    /// the launcher a second one. The three answers are the three the
    /// product meets: the first start, the start that arrives while the
    /// launcher is there, and the start that arrives after it is gone.
    #[test]
    fn the_name_is_one_launchers_at_a_time() {
        let name = "Local\\rgy-rgxy-launcher-sole-test";

        let first = Sole::take_named(name);
        assert!(
            matches!(first, Taken::First(_)),
            "the first start takes the name"
        );

        let second = Sole::take_named(name);
        assert!(
            matches!(second, Taken::Other),
            "a start beside the launcher is the second one"
        );

        // The launcher goes: its handle closes and the name goes with it.
        drop(first);

        let third = Sole::take_named(name);
        assert!(
            matches!(third, Taken::First(_)),
            "a launcher that is gone is a launcher that can be started"
        );
    }
}
