//! Independent reimplementation of memmap2-rs advice enums.

#![allow(clippy::upper_case_acronyms)]

#[cfg(unix)]
mod imp {
    // `libc` exposes these seven on Linux alone: MADV_REMOVE and the
    // fork/merge/hugepage calls are its own. Not "unix" -- Darwin has
    // MADV_FREE and not these, and the BSDs stop at MADV_PROTECT. So the
    // numbers come from Linux's `bits/mman-linux.h` everywhere else.
    //
    // The enum keeps one shape on every platform so callers need no cfg of
    // their own. What must vary is what happens when one of these is passed
    // to a kernel that has no such advice: `advise` turns it down rather
    // than handing over a number the kernel would read as something else.
    #[cfg(any(target_os = "linux", target_os = "android"))]
    use libc::{
        MADV_DOFORK, MADV_DONTFORK, MADV_HUGEPAGE, MADV_MERGEABLE, MADV_NOHUGEPAGE, MADV_REMOVE,
        MADV_UNMERGEABLE,
    };
    #[cfg(not(any(target_os = "linux", target_os = "android")))]
    mod missing {
        pub const MADV_REMOVE: i32 = 9;
        pub const MADV_DONTFORK: i32 = 10;
        pub const MADV_DOFORK: i32 = 11;
        pub const MADV_MERGEABLE: i32 = 12;
        pub const MADV_UNMERGEABLE: i32 = 13;
        pub const MADV_HUGEPAGE: i32 = 14;
        pub const MADV_NOHUGEPAGE: i32 = 15;
    }
    #[cfg(not(any(target_os = "linux", target_os = "android")))]
    use missing::{
        MADV_DOFORK, MADV_DONTFORK, MADV_HUGEPAGE, MADV_MERGEABLE, MADV_NOHUGEPAGE, MADV_REMOVE,
        MADV_UNMERGEABLE,
    };

    #[repr(i32)]
    #[derive(Copy, Clone, Debug, Eq, PartialEq)]
    pub enum Advice {
        Normal = libc::MADV_NORMAL as i32,
        Random = libc::MADV_RANDOM as i32,
        Sequential = libc::MADV_SEQUENTIAL as i32,
        WillNeed = libc::MADV_WILLNEED as i32,
        DontNeed = libc::MADV_DONTNEED as i32,
        HugePage = MADV_HUGEPAGE as i32,
        NoHugePage = MADV_NOHUGEPAGE as i32,
        DontFork = MADV_DONTFORK as i32,
        DoFork = MADV_DOFORK as i32,
        Mergeable = MADV_MERGEABLE as i32,
        Unmergeable = MADV_UNMERGEABLE as i32,
    }

    #[repr(i32)]
    #[derive(Copy, Clone, Debug, Eq, PartialEq)]
    pub enum UncheckedAdvice {
        DontNeed = libc::MADV_DONTNEED as i32,
        Free = libc::MADV_FREE as i32,
        Remove = MADV_REMOVE as i32,
    }
}

#[cfg(unix)]
pub use self::imp::{Advice, UncheckedAdvice};
