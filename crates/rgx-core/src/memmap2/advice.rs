//! Independent reimplementation of memmap2-rs advice enums.

#![allow(clippy::upper_case_acronyms)]

#[cfg(unix)]
mod imp {
    #[repr(i32)]
    #[derive(Copy, Clone, Debug, Eq, PartialEq)]
    pub enum Advice {
        Normal = libc::MADV_NORMAL as i32,
        Random = libc::MADV_RANDOM as i32,
        Sequential = libc::MADV_SEQUENTIAL as i32,
        WillNeed = libc::MADV_WILLNEED as i32,
        DontNeed = libc::MADV_DONTNEED as i32,
        HugePage = libc::MADV_HUGEPAGE as i32,
        NoHugePage = libc::MADV_NOHUGEPAGE as i32,
        DontFork = libc::MADV_DONTFORK as i32,
        DoFork = libc::MADV_DOFORK as i32,
        Mergeable = libc::MADV_MERGEABLE as i32,
        Unmergeable = libc::MADV_UNMERGEABLE as i32,
    }

    #[repr(i32)]
    #[derive(Copy, Clone, Debug, Eq, PartialEq)]
    pub enum UncheckedAdvice {
        DontNeed = libc::MADV_DONTNEED as i32,
        Free = libc::MADV_FREE as i32,
        Remove = libc::MADV_REMOVE as i32,
    }
}

#[cfg(unix)]
pub use self::imp::{Advice, UncheckedAdvice};
