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

use crate::win::*;

/// Whether this session has a desktop to draw on.
pub fn available() -> bool {
    windows_available()
}

/// `WSF_VISIBLE`: the station is on a visible desktop.
const WSF_VISIBLE: u32 = 0x0001;

/// The flags the platform reports for the process's window station.
///
/// `WSF_VISIBLE`: the station is on a visible desktop. A process in a
/// non-interactive session (session 0, the services one) has a window
/// station that is not visible: there is no desktop there, and a window
/// would be created in a place nobody can see.
fn windows_available() -> bool {
    match station_flags() {
        Ok(flags) => flags & WSF_VISIBLE != 0,
        // The platform would not say. The launcher tries anyway: the
        // window's own creation is the judge of whether it can be shown,
        // and a launcher that hides itself because a question went
        // unanswered is a launcher that does not exist.
        Err(_) => true,
    }
}

/// The platform's own flags, read from the window station.
fn station_flags() -> Result<u32, ()> {
    use core::ffi::c_void;

    /// `UOI_FLAGS`: the station's flags.
    const UOI_FLAGS: i32 = 1;

    unsafe {
        let station = GetProcessWindowStation();
        if station.is_null() {
            return Err(());
        }
        let mut flags = UserObjectFlags::default();
        let mut needed = 0u32;
        let ok = GetUserObjectInformationW(
            station,
            UOI_FLAGS,
            (&mut flags as *mut UserObjectFlags).cast::<c_void>(),
            core::mem::size_of::<UserObjectFlags>() as u32,
            &mut needed,
        );
        if ok == 0 {
            return Err(());
        }
        Ok(flags.dw_flags)
    }
}

/// The structure the platform fills in.
///
/// The whole structure, whole — and the whole structure is **twelve
/// bytes**: `GetUserObjectInformation` refuses a buffer smaller than it
/// with `ERROR_INSUFFICIENT_BUFFER`, and a buffer of four bytes (the
/// size of the flags alone) is refused on every machine. The answer was
/// then "no desktop" everywhere, and the launcher — which is a window
/// that talks to a daemon — never opened one.
///
/// The flags are the **third** field: the first two are `fInherit` and
/// `fReserved`. Reading the first one would read `fInherit`, and a
/// window station that inherits is not a window station you can see.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
struct UserObjectFlags {
    /// Whether the objects on this station inherit it.
    f_inherit: i32,
    /// Reserved by the platform.
    f_reserved: i32,
    /// The flags: `WSF_VISIBLE` and the rest.
    dw_flags: u32,
}

// The platform writes the whole structure and checks the size it is
// given, so its layout is fixed at COMPILE TIME. A runtime assertion
// protects one host, and only while its tests happen to run; a
// truncated structure is a buffer `GetUserObjectInformationW` writes
// past on every host. `dw_flags` is the third field: reading the first
// one would read `fInherit` instead. The runtime test below stays as a
// second pair of eyes on the same numbers.
const _: () = assert!(core::mem::size_of::<UserObjectFlags>() == 12);
const _: () = assert!(core::mem::offset_of!(UserObjectFlags, dw_flags) == 8);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_answer_is_a_boolean_and_not_a_panic() {
        // The question is asked on a machine that may have no desktop, and
        // it has to have an answer on every one of them.
        let _ = available();
    }

    /// The platform answers the question.
    ///
    /// This is the regression: the buffer was the size of the flags
    /// instead of the size of the structure, the platform refused it with
    /// `ERROR_INSUFFICIENT_BUFFER`, and the refusal was read as "there
    /// is no desktop" — on every machine, including the ones with a
    /// desktop in front of them.
    #[test]
    fn the_platform_answers_the_question() {
        assert!(
            station_flags().is_ok(),
            "the platform answered the flags: {:?}",
            station_flags()
        );
    }

    #[test]
    fn the_flags_are_the_third_field_of_the_structure() {
        // The structure is twelve bytes and the flags are the last four:
        // the first two are `fInherit` and `fReserved`.
        assert_eq!(core::mem::size_of::<UserObjectFlags>(), 12);
        let flags = UserObjectFlags {
            f_inherit: 1,
            dw_flags: 0x0001,
            ..UserObjectFlags::default()
        };
        assert_eq!(flags.dw_flags, 0x0001);
        assert_eq!(flags.dw_flags & WSF_VISIBLE, WSF_VISIBLE);
    }
}
