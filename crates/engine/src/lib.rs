//! Orchestration: index lifecycle, sync, pools, keep-warm.
//!
//! The keep-warm spinner (plan §4.4, from fsearch `engine.rs`): a parked
//! thread spins from `WARM_FROM` to `WARM_UNTIL` at user-interactive
//! priority so the first query after idle still answers at typing latency;
//! the search itself sets the window and the spin is cancelled when it
//! closes.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

static WARM_FROM: AtomicU64 = AtomicU64::new(0);
static WARM_UNTIL: AtomicU64 = AtomicU64::new(0);
static WARM_IDLE: AtomicBool = AtomicBool::new(false);

fn now_ns() -> u64 {
    static EPOCH: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    EPOCH.get_or_init(Instant::now).elapsed().as_nanos() as u64
}

fn warmer() -> &'static std::thread::Thread {
    static T: std::sync::OnceLock<std::thread::Thread> = std::sync::OnceLock::new();
    T.get_or_init(|| {
        let t = std::thread::Builder::new()
            .name("rgy-warm".into())
            .spawn(|| {
                loop {
                    let (from, until, now) = (
                        WARM_FROM.load(Ordering::SeqCst),
                        WARM_UNTIL.load(Ordering::SeqCst),
                        now_ns(),
                    );
                    if now >= until {
                        WARM_IDLE.store(true, Ordering::SeqCst);
                        if now_ns() >= WARM_UNTIL.load(Ordering::SeqCst) {
                            std::thread::park();
                        }
                        WARM_IDLE.store(false, Ordering::SeqCst);
                    } else if now < from {
                        std::thread::park_timeout(Duration::from_nanos(from - now));
                    } else {
                        std::hint::spin_loop();
                    }
                }
            })
            .expect("spawn keep-warm thread");
        t.thread().clone()
    })
}

/// Keep the index warm for `d`: the warmer spins until then, or until the
/// next search closes the window.
pub fn keep_warm(d: Duration) {
    let now = now_ns();
    WARM_FROM.store(now, Ordering::SeqCst);
    WARM_UNTIL.store(now + d.as_nanos() as u64, Ordering::SeqCst);
    warmer().unpark();
}

/// A search is answering: close the warm window (fsearch: `WARM_UNTIL = 0`).
pub fn warm_stop() {
    WARM_UNTIL.store(0, Ordering::SeqCst);
}

/// TV pack store glue (the daemon corpus store).
pub mod tv;
