// SPDX-License-Identifier: Apache-2.0

//! Whether this session can show a window at all.
//!
//! The launcher is registered to start at login, and a login entry can
//! happen where there is no desktop: a service session on Windows, an
//! SSH shell, a machine whose window manager never came up. A window that
//! cannot be shown is not a window that shows nothing — it is a launcher
//! that eats the keystroke and answers nothing.
//!
//! So the question is asked before the window is built, and the answer
//! decides what happens: the window, the terminal UI, or nothing at all.

/// Whether this session has a desktop to draw on.
pub fn available() -> bool {
    windows_available()
}

/// Windows: the session's window station, and whether it is visible.
///
/// A process in a non-interactive session (session 0, the services one)
/// has a window station that is not visible: there is no desktop there,
/// and a window would be created in a place nobody can see.
fn windows_available() -> bool {
    use core::ffi::c_void;

    #[link(name = "user32")]
    unsafe extern "system" {
        fn GetProcessWindowStation() -> *mut c_void;
        fn GetUserObjectInformationW(
            hObj: *mut c_void,
            nIndex: i32,
            pvInfo: *mut c_void,
            nLength: u32,
            lpnLengthNeeded: *mut u32,
        ) -> i32;
    }

    /// `UOI_FLAGS`: the station's flags.
    const UOI_FLAGS: i32 = 1;
    /// `WSF_VISIBLE`: the station is on a visible desktop.
    const WSF_VISIBLE: u32 = 0x0001;

    unsafe {
        let station = GetProcessWindowStation();
        if station.is_null() {
            return false;
        }
        let mut flags: u32 = 0;
        let mut needed: u32 = 0;
        let ok = GetUserObjectInformationW(
            station,
            UOI_FLAGS,
            (&mut flags as *mut u32).cast::<c_void>(),
            core::mem::size_of::<u32>() as u32,
            &mut needed,
        );
        ok != 0 && flags & WSF_VISIBLE != 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_answer_is_a_boolean_and_not_a_panic() {
        // The question is asked on a machine that may have no desktop, and
        // it has to have an answer on every one of them.
        let _ = available();
    }
}
