// SPDX-License-Identifier: Apache-2.0

//! The clock, without `std`.
//!
//! Two things need to know what time it is: the session's idle trim (a
//! cache that is never trimmed is a leak) and the persisted index's age (an
//! index that never looks stale is a stale index).
//!
//! `std::time` is not available in a `no_std` build, and a clock that
//! always reads zero is worse than no clock at all: with zero, the cache
//! would never be trimmed and an index would never expire. So the clock is
//! declared here, the way the rest of this crate declares what it needs
//! from the platform — `clock_gettime(2)` on unix,
//! `GetSystemTimeAsFileTime` on Windows, and nothing in between.
//!
//! It is the *wall* clock, not a monotonic one, exactly as `SystemTime`
//! was: a clock that steps backwards reads as "not idle", which keeps a
//! cache a little longer than it should rather than dropping it while it
//! is wanted.

/// The wall clock, in milliseconds since the epoch.
///
/// 0 where the platform has no clock to ask (a bare-metal target): the
/// callers treat 0 as "no idea", and the trim keeps the cache rather than
/// dropping it.
pub fn unix_millis() -> u64 {
    #[cfg(unix)]
    {
        let mut ts = Timespec { tv_sec: 0, tv_nsec: 0 };
        // CLOCK_REALTIME = 0 on every unix that has a clock_gettime.
        let ok = unsafe { clock_gettime(0, &mut ts) } == 0;
        if ok {
            let (secs, nanos) = ts.parts();
            if secs >= 0 {
                return (secs as u64) * 1000 + (nanos as u64) / 1_000_000;
            }
        }
        0
    }
    #[cfg(windows)]
    {
        let mut ft: u64 = 0;
        // The FILETIME counts 100-nanosecond intervals since 1601-01-01;
        // the unix epoch is 11644473600 seconds later.
        let ok = unsafe { GetSystemTimeAsFileTime(&mut ft) } != 0;
        if ok && ft > EPOCH_100NS { (ft - EPOCH_100NS) / 10_000 } else { 0 }
    }
    #[cfg(not(any(unix, windows)))]
    {
        0
    }
}

/// The wall clock, in seconds since the epoch, clamped to `u32`.
///
/// The same clock as [`unix_millis`], at the resolution the callers that
/// ask for seconds want.
pub fn unix_secs() -> u32 {
    let ms = unix_millis();
    (ms / 1000).min(u32::MAX as u64) as u32
}

/// The unix timespec, as the platform lays it out.
///
/// `time_t` and `c_long` are 64 bits on the unix the tree builds for, and
/// the tree is a 64-bit unix tree: the unix reader of `rgy-discovery`
/// refuses a 32-bit one with a `compile_error!`, because the structs it
/// writes are the 64-bit ones and the same fields land somewhere else on
/// a 32-bit target. So the shape here has one shape, and a second one for
/// a platform the tree does not build for would be a shape no compiler
/// ever reads -- and a mistake in it would never be caught.
#[cfg(all(unix, target_pointer_width = "64"))]
#[repr(C)]
struct Timespec {
    tv_sec: i64,
    tv_nsec: i64,
}

/// The seconds and the nanoseconds, widened to what the callers read.
///
/// The shape of the timespec is declared above, and there is one shape, so
/// there is nothing to widen: the fields are already what the callers
/// read.
#[cfg(all(unix, target_pointer_width = "64"))]
impl Timespec {
    fn parts(&self) -> (i64, i64) {
        (self.tv_sec, self.tv_nsec)
    }
}

#[cfg(unix)]
unsafe extern "C" {
    fn clock_gettime(clk_id: i32, tp: *mut Timespec) -> i32;
}

#[cfg(windows)]
unsafe extern "system" {
    fn GetSystemTimeAsFileTime(lp_system_time_as_file_time: *mut u64) -> i32;
}

/// The unix epoch in 100-nanosecond intervals since 1601-01-01.
#[cfg(windows)]
const EPOCH_100NS: u64 = 116_444_736_000_000_000;

#[cfg(test)]
mod tests {
    use super::*;

    /// The clock reads a time, and a time after 2020: a clock stuck at the
    /// epoch would pass every comparison against another broken clock.
    #[test]
    fn the_clock_reads_the_wall_clock() {
        let ms = unix_millis();
        assert!(ms > 1_600_000_000_000, "the clock is not reading the wall clock: {ms}");
        // And the two resolutions agree.
        let secs = unix_secs() as u64;
        assert!(secs >= 1_600_000_000, "the seconds clock is behind the millisecond one: {secs}");
        assert!(secs <= ms / 1000 + 1, "the seconds clock is ahead of the millisecond one: {secs}");
    }
}
