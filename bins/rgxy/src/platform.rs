// SPDX-License-Identifier: Apache-2.0

//! The one name the window driver knows the platform by.
//!
//! `gui.rs` is one driver: the model, the source and the window's event
//! loop are the same on Windows and on Linux. Only the crate that owns the
//! window differs, and it differs behind this alias. Both backends export
//! the same `Window`, `Event`, `View`, `Hook`, `Combo`, `Sole` and
//! `actions` — a port that made the driver ask `cfg` for itself would be
//! two drivers, and two drivers are two places for the same bug.
//!
//! Linux and FreeBSD share the X11 backend; a host with neither backend
//! (macOS, and anything else) still compiles this module — it is empty —
//! and the binary takes the terminal interface.

#[cfg(windows)]
pub use rgy_launcher_windows as backend;

#[cfg(any(target_os = "linux", target_os = "freebsd"))]
pub use rgy_launcher_x11 as backend;
