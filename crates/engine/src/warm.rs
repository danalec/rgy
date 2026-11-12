// SPDX-License-Identifier: Apache-2.0

//! The warm window: the span the index is kept warm for.
//!
//! The window has two ends -- when it opens and when it closes -- and the
//! arithmetic that decides them is pure: a moment in time and a duration
//! in, a window out. That half is here, and it is `no_std`.
//!
//! The other half is the spinner: a thread that waits for the window to
//! open and then spins inside it, so the first query after idle answers
//! at typing latency rather than at walk latency. A thread needs `std`,
//! so that half is behind the feature -- and the window it waits on is
//! the one above, unchanged.

#[cfg(feature = "std")]
use core::sync::atomic::Ordering;

/// The warm window, in nanoseconds on the clock the spinner reads.
///
/// The window is closed when `until` is zero: that is what a closed
/// window is, and it is why a search can close it with one store.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Window {
    from: u64,
    until: u64,
}
/// What the window says at a given moment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    /// The window is closed: there is nothing to keep warm.
    Closed,
    /// The window has not started yet.
    Waiting,
    /// The window is open: the index is warm.
    Open,
}

impl Window {
    /// The window that opens at `from` and closes `dur` nanoseconds later.
    pub fn open(from: u64, dur_ns: u64) -> Window {
        Window {
            from,
            until: from.saturating_add(dur_ns),
        }
    }

    /// The closed window.
    pub fn closed() -> Window {
        Window { from: 0, until: 0 }
    }

    /// When the window opens.
    pub fn from(&self) -> u64 {
        self.from
    }

    /// When the window closes.
    pub fn until(&self) -> u64 {
        self.until
    }

    /// What the window says at `now`.
    ///
    /// A closed window (`until` at zero) is closed at every moment, which
    /// is what makes "the search closed the window" one store.
    pub fn state(&self, now: u64) -> State {
        if now >= self.until {
            State::Closed
        } else if now < self.from {
            State::Waiting
        } else {
            State::Open
        }
    }
}

#[cfg(feature = "std")]
mod spinner {
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    use std::time::{Duration, Instant};

    use super::{State, Window};
    pub(super) static WARM_FROM: AtomicU64 = AtomicU64::new(0);
    pub(super) static WARM_UNTIL: AtomicU64 = AtomicU64::new(0);
    pub(super) static WARM_IDLE: AtomicBool = AtomicBool::new(false);

    /// The clock the window is measured on: nanoseconds since the first
    /// call. Monotonic, and the only clock a window can be measured on.
    pub(super) fn now_ns() -> u64 {
        static EPOCH: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
        EPOCH.get_or_init(Instant::now).elapsed().as_nanos() as u64
    }

    /// The window the process is in, as the spinner sees it.
    pub(super) fn window() -> Window {
        Window {
            from: WARM_FROM.load(Ordering::SeqCst),
            until: WARM_UNTIL.load(Ordering::SeqCst),
        }
    }

    /// The thread that waits for the window and spins inside it.
    ///
    /// The window's own arithmetic decides what the thread does: the
    /// `no_std` half is not a description of the `std` half, it is the
    /// half that decides.
    pub(super) fn warmer() -> &'static std::thread::Thread {
        static T: std::sync::OnceLock<std::thread::Thread> = std::sync::OnceLock::new();
        T.get_or_init(|| {
            let t = std::thread::Builder::new()
                .name("rgy-warm".into())
                .spawn(|| {
                    loop {
                        let w = window();
                        let now = now_ns();
                        match w.state(now) {
                            State::Closed => {
                                WARM_IDLE.store(true, Ordering::SeqCst);
                                if now_ns() >= window().until() {
                                    std::thread::park();
                                }
                                WARM_IDLE.store(false, Ordering::SeqCst);
                            }
                            State::Waiting => {
                                std::thread::park_timeout(Duration::from_nanos(w.from() - now));
                            }
                            State::Open => std::hint::spin_loop(),
                        }
                    }
                })
                .expect("spawn keep-warm thread");
            t.thread().clone()
        })
    }
}

/// Keep the index warm for `d`: the warmer spins until then, or until the
/// next search closes the window.
#[cfg(feature = "std")]
pub fn keep_warm(d: std::time::Duration) {
    let now = spinner::now_ns();
    spinner::WARM_FROM.store(now, Ordering::SeqCst);
    spinner::WARM_UNTIL.store(now + d.as_nanos() as u64, Ordering::SeqCst);
    spinner::warmer().unpark();
}

/// A search is answering: close the warm window (`until == 0`).
#[cfg(feature = "std")]
pub fn warm_stop() {
    spinner::WARM_UNTIL.store(0, Ordering::SeqCst);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_window_opens_and_closes() {
        let w = Window::open(100, 50);
        assert_eq!(w.from(), 100);
        assert_eq!(w.until(), 150);
        assert_eq!(w.state(99), State::Waiting);
        assert_eq!(w.state(100), State::Open);
        assert_eq!(w.state(149), State::Open);
        assert_eq!(w.state(150), State::Closed);
    }

    #[test]
    fn a_closed_window_is_closed_at_every_moment() {
        let w = Window::closed();
        assert_eq!(w.state(0), State::Closed);
        assert_eq!(w.state(1_000_000), State::Closed);
    }

    /// A window that would overflow the clock is a window that closes at
    /// the end of the clock, not one that wraps around to the beginning.
    #[test]
    fn a_window_that_overflows_saturates() {
        let w = Window::open(u64::MAX - 1, 100);
        assert_eq!(w.until(), u64::MAX);
        assert_eq!(w.state(u64::MAX - 1), State::Open);
    }
}
