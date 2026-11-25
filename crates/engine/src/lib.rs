// SPDX-License-Identifier: Apache-2.0

//! Orchestration: index lifecycle, sync, pools, keep-warm.
//!
//! The crate is `no_std` with the `std` feature off: what is left is the
//! decisions -- when the warm window opens, which policy the environment
//! asks for, how a console is written -- over bytes the caller owns. The
//! `std` feature adds the effects: the spinner thread, the clock, the
//! environment, the console handles and the pack store.
//!
//! The keep-warm spinner: a parked thread spins from
//! `WARM_FROM` to `WARM_UNTIL` at user-interactive priority so the first
//! query after idle still answers at typing latency; the search itself
//! sets the window and the spin is cancelled when it closes.

#![no_std]

extern crate alloc;
/// The threads, the clock, the environment and the filesystem -- only with
/// the `std` feature, and only for the parts that need them.
#[cfg(feature = "std")]
extern crate std;

/// The warm window: when the index is kept warm, and for how long.
///
/// The arithmetic is here and it is `no_std`; the thread that waits on it
/// is not (see [`keep_warm`]).
pub mod warm;

/// Console output encoding: the Windows code page.
///
/// One place for every binary to put the console into UTF-8, and
/// for the streams to fall back to `WriteConsoleW` when the console
/// refuses. See the module docs for what it does and does not change.
pub mod console;

/// Which view of an adapter-claimed file a search reads.
///
/// rgy answers to `RGY_ADAPTER_VIEW` and falls back to rgx's own
/// `RGX_ADAPTER_VIEW`, so a shell configured for rgx is configured for
/// rgy too, and a shell that wants them to differ has a name of its own.
/// See the module docs for why the namespace is rgy's to begin with.
pub mod adapter_view;

/// The paths the user typed, and the home directory a `~` names.
///
/// A `~` at the front of a path is the home, on every platform and in every
/// shell — the shell expands it on a POSIX one and does not on Windows, and
/// the tree expands it itself so the two agree. See the module docs for why
/// the disagreement it ended was silent, and therefore the worst kind.
pub mod paths;

/// TV pack store glue (the daemon corpus store).
#[cfg(feature = "std")]
pub mod tv;

#[cfg(feature = "std")]
pub use warm::{keep_warm, warm_stop};
