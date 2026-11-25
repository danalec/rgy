// SPDX-License-Identifier: Apache-2.0

//! Ctrl-C as a cooperative stop, without a signal-handling dependency.
//!
//! The workspace takes no dependency on `ctrlc`/`signal-hook`: the same
//! pattern `name-core`'s Darwin/unix bits use in `content.rs` — declare the
//! two `extern` declarations the platform needs, and nothing else. That keeps
//! the portable backend's dependency list at `rgy-par` alone.
//!
//! What Ctrl-C means here is narrow on purpose. A scan that is cut short is a
//! tree missing whole subtrees, so it must never look like a smaller tree:
//! the walk returns [`walk::Cancelled`] and the caller writes no index. Ctrl-C
//! that lands outside a scan keeps its usual meaning — the default handler,
//! since nothing is installed until [`cancel_on_interrupt`] is called.

use alloc::format;
use alloc::string::String;
use core::sync::atomic::AtomicBool;

use crate::walk::{Cancel, Progress};

/// The flag Ctrl-C writes to. One per process: handlers are process-global,
/// so there is no point letting each caller have its own.
static TRIPPED: AtomicBool = AtomicBool::new(false);

/// The token Ctrl-C trips.
///
/// A Ctrl-C landing before [`install`] still reaches the platform's default
/// handler and kills the process — the pre-existing behaviour. Not a hang,
/// and not a partial index.
pub fn cancel_on_interrupt() -> Cancel {
    install();
    Cancel::on_global(&TRIPPED)
}

/// Install a Ctrl-C handler that trips [`cancel_on_interrupt`] and returns,
/// letting the walk notice at its next poll.
///
/// Nothing happens on platforms with neither `signal(2)` nor
/// `SetConsoleCtrlHandler`: the token is returned untripped, so a scan there
/// behaves exactly as it did before this existed.
#[cfg(unix)]
fn install() {
    unsafe extern "C" {
        fn signal(signum: i32, handler: usize) -> usize;
    }
    const SIGINT: i32 = 2;

    // Async-signal-safe: one relaxed store on a static, no lock, no lazy
    // init, nothing to re-enter.
    extern "C" fn on_sigint(_sig: i32) {
        TRIPPED.store(true, core::sync::atomic::Ordering::Relaxed);
    }

    // The cell is `rgy-par`'s: the installation happens once per process,
    // and there is no `std` here to keep a `Once` in.
    static ONCE: rgy_par::sync::OnceCell<()> = rgy_par::sync::OnceCell::new();
    ONCE.get_or_init(|| unsafe {
        // Through a pointer: casting a function straight to an integer is a
        // lint (`function_casts_as_integer`) on its way to being an error.
        signal(SIGINT, on_sigint as *const () as usize);
    });
}

#[cfg(windows)]
fn install() {
    unsafe extern "system" {
        fn SetConsoleCtrlHandler(
            routine: Option<unsafe extern "system" fn(u32) -> i32>,
            add: i32,
        ) -> i32;
    }
    const CTRL_C_EVENT: u32 = 0;
    const CTRL_BREAK_EVENT: u32 = 1;

    // Returning nonzero claims the event: the process survives Ctrl-C and the
    // walk stops at its next poll instead. Async-signal-safe in the same
    // sense as the unix handler.
    unsafe extern "system" fn on_console(u32_event: u32) -> i32 {
        match u32_event {
            CTRL_C_EVENT | CTRL_BREAK_EVENT => {
                TRIPPED.store(true, core::sync::atomic::Ordering::Relaxed);
                1
            }
            _ => 0,
        }
    }

    static ONCE: rgy_par::sync::OnceCell<()> = rgy_par::sync::OnceCell::new();
    ONCE.get_or_init(|| unsafe {
        SetConsoleCtrlHandler(Some(on_console), 1);
    });
}

#[cfg(not(any(unix, windows)))]
fn install() {}

/// Format a progress line: what the walk has covered so far.
pub fn line(p: Progress) -> String {
    format!("{} dirs, {} entries", p.dirs, p.entries)
}

#[cfg(all(test, feature = "std"))]
mod tests {
    extern crate std;

    use super::*;

    /// Two tokens are equal only when a trip on one is a trip on the other —
    /// which is the property that lets a test assert the handler and the walk
    /// are pointing at the same flag.
    #[test]
    fn tokens_compare_by_the_flag_they_share() {
        let a = Cancel::new();
        let b = a.clone();
        assert_eq!(a, b, "a clone must share its flag");
        a.trip();
        assert!(b.is_cancelled(), "a trip on one clone tripped the other");

        let other = Cancel::new();
        assert_ne!(a, other, "separate tokens must not compare equal");
        assert!(
            !other.is_cancelled(),
            "a new scan must not inherit a cancel"
        );
    }

    /// Every caller gets the same global flag, so whichever token a scan
    /// holds is the one Ctrl-C sets.
    #[test]
    fn every_call_hands_out_the_same_interrupt_flag() {
        assert_eq!(cancel_on_interrupt(), cancel_on_interrupt());
    }
}
