// SPDX-License-Identifier: Apache-2.0

//! Independent reimplementation of memmap2-rs platform dispatch.

use std::fs::File;
use std::io;
use std::ops::{Deref, DerefMut};
use std::slice;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
pub(crate) use self::linux::MmapInner;

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
pub(crate) use self::macos::MmapInner;

#[cfg(target_os = "freebsd")]
mod freebsd;
#[cfg(target_os = "freebsd")]
pub(crate) use self::freebsd::MmapInner;

#[cfg(target_os = "windows")]
mod windows;
#[cfg(target_os = "windows")]
pub(crate) use self::windows::MmapInner;

#[cfg(not(any(
    target_os = "linux",
    target_os = "macos",
    target_os = "freebsd",
    target_os = "windows"
)))]
mod fallback;
#[cfg(not(any(
    target_os = "linux",
    target_os = "macos",
    target_os = "freebsd",
    target_os = "windows"
)))]
pub(crate) use self::fallback::MmapInner;

/// The unix calls the memory map is made of.
///
/// The `libc` crate used to carry them: the calls themselves, the
/// protection and mapping flags, and the number that asks the kernel for
/// the page size. They are declared here, by hand, the way
/// `crates/name-core/src/clock.rs` declares the clock and the way
/// `crates/discovery/src/file.rs` declares the file calls — the tree
/// carries no crate to make a syscall for it, and the constants are the
/// platform's, spelled with the platform's numbers.
///
/// The numbers are not the same everywhere, and a number the platform does
/// not use is an instruction it does use: Linux's `MS_SYNC` is 4, macOS's
/// is 16 and FreeBSD's is 0, and the anonymous-map flag is 0x20 on Linux
/// and 0x1000 on the BSDs. Each one is declared with the value the
/// platform gives it.
#[cfg(any(target_os = "linux", target_os = "macos", target_os = "freebsd"))]
mod unix {
    use core::ffi::{c_int, c_long, c_void};

    /// The protection of the pages of a mapping: what they may be.
    pub const PROT_NONE: c_int = 0;
    pub const PROT_READ: c_int = 1;
    pub const PROT_WRITE: c_int = 2;
    pub const PROT_EXEC: c_int = 4;

    /// The mapping is shared with the file behind it.
    pub const MAP_SHARED: c_int = 0x0001;

    /// The mapping is private to the process.
    pub const MAP_PRIVATE: c_int = 0x0002;

    /// The anonymous mapping: a mapping with no file behind it.
    ///
    /// Linux gives the flag 0x20 and the BSDs give it 0x1000, and each
    /// platform spells it twice: `MAP_ANON` and `MAP_ANONYMOUS` are the
    /// same number on all of them.
    #[cfg(all(
        target_os = "linux",
        not(any(target_arch = "mips", target_arch = "mips64"))
    ))]
    pub const MAP_ANON: c_int = 0x0020;
    #[cfg(all(target_os = "linux", any(target_arch = "mips", target_arch = "mips64")))]
    pub const MAP_ANON: c_int = 0x0800;
    #[cfg(not(target_os = "linux"))]
    pub const MAP_ANON: c_int = 0x1000;

    /// The same flag under its other name.
    pub const MAP_ANONYMOUS: c_int = MAP_ANON;

    /// The pages are read in as the mapping is made: `MAP_POPULATE`.
    ///
    /// Linux has the flag and the BSDs do not, so there it is 0 — which
    /// is no flag at all, and the mapping is made the same way.
    #[cfg(all(
        target_os = "linux",
        not(any(target_arch = "mips", target_arch = "mips64"))
    ))]
    pub const MAP_POPULATE: c_int = 0x8000;
    #[cfg(all(target_os = "linux", any(target_arch = "mips", target_arch = "mips64")))]
    pub const MAP_POPULATE: c_int = 0x10000;
    #[cfg(not(target_os = "linux"))]
    pub const MAP_POPULATE: c_int = 0;

    /// The mapping is a stack: `MAP_STACK`.
    ///
    /// Linux gives it 0x20000, FreeBSD gives it 0x400 and macOS has no
    /// such flag at all — there it is 0.
    #[cfg(all(
        target_os = "linux",
        not(any(target_arch = "mips", target_arch = "mips64"))
    ))]
    pub const MAP_STACK: c_int = 0x20000;
    #[cfg(all(target_os = "linux", any(target_arch = "mips", target_arch = "mips64")))]
    pub const MAP_STACK: c_int = 0x40000;
    #[cfg(target_os = "freebsd")]
    pub const MAP_STACK: c_int = 0x0400;
    #[cfg(not(any(target_os = "linux", target_os = "freebsd")))]
    pub const MAP_STACK: c_int = 0;

    /// The mapping failed: `MAP_FAILED`, which is the address no map
    /// ever has.
    pub const MAP_FAILED: *mut c_void = -1isize as *mut c_void;

    /// The write-back is immediate: `MS_SYNC`.
    ///
    /// Linux's is 4, macOS's is 16 and FreeBSD's is 0 — where 0 is a flag
    /// of its own and not "no flag".
    #[cfg(target_os = "linux")]
    pub const MS_SYNC: c_int = 0x0004;
    #[cfg(target_os = "macos")]
    pub const MS_SYNC: c_int = 0x0010;
    #[cfg(target_os = "freebsd")]
    pub const MS_SYNC: c_int = 0x0000;

    /// The write-back is in the background: `MS_ASYNC`. It is 1 on every
    /// unix.
    pub const MS_ASYNC: c_int = 0x0001;

    /// The question `sysconf(3)` answers with the page size. The number
    /// is the platform's: 30 on Linux, 29 on macOS and 47 on FreeBSD.
    #[cfg(target_os = "linux")]
    pub const SC_PAGESIZE: c_int = 30;
    #[cfg(target_os = "macos")]
    pub const SC_PAGESIZE: c_int = 29;
    #[cfg(target_os = "freebsd")]
    pub const SC_PAGESIZE: c_int = 47;

    /// Where a seek is measured from: the start of the file, the current
    /// position, or its end. The three are 0, 1 and 2 on every unix.
    pub const SEEK_SET: c_int = 0;
    pub const SEEK_CUR: c_int = 1;
    pub const SEEK_END: c_int = 2;

    unsafe extern "C" {
        /// The mapping is made: `mmap(2)`.
        ///
        /// The offset is 64 bits on every platform the tree builds for. A
        /// 32-bit glibc links the large-file symbol `mmap64`: `mmap`
        /// there reads 32 bits, and a file over 4 GiB would be mapped at
        /// the wrong place.
        #[cfg_attr(
            all(target_os = "linux", target_env = "gnu", target_pointer_width = "32"),
            link_name = "mmap64"
        )]
        pub fn mmap(
            addr: *mut c_void,
            len: usize,
            prot: c_int,
            flags: c_int,
            fd: c_int,
            offset: i64,
        ) -> *mut c_void;

        /// The mapping is given back: `munmap(2)`.
        pub fn munmap(addr: *mut c_void, len: usize) -> c_int;

        /// The pages are written back: `msync(2)`.
        pub fn msync(addr: *mut c_void, len: usize, flags: c_int) -> c_int;

        /// The protection of the pages is changed: `mprotect(2)`.
        pub fn mprotect(addr: *mut c_void, len: usize, prot: c_int) -> c_int;

        /// The kernel is told how the pages will be used: `madvise(2)`.
        pub fn madvise(addr: *mut c_void, len: usize, advice: c_int) -> c_int;

        /// The pages are pinned in memory: `mlock(2)`.
        pub fn mlock(addr: *const c_void, len: usize) -> c_int;

        /// The pages are let go: `munlock(2)`.
        pub fn munlock(addr: *const c_void, len: usize) -> c_int;

        /// The platform is asked a question: `sysconf(3)`.
        pub fn sysconf(name: c_int) -> c_long;

        /// The position in the file is moved, and the new one is
        /// answered: `lseek(2)`.
        ///
        /// The offset is 64 bits, as it is for `mmap(2)`, and a 32-bit
        /// glibc links the large-file symbol `lseek64` for the same
        /// reason.
        #[cfg_attr(
            all(target_os = "linux", target_env = "gnu", target_pointer_width = "32"),
            link_name = "lseek64"
        )]
        pub fn lseek(fd: c_int, offset: i64, whence: c_int) -> i64;
    }
}

pub(crate) fn page_size() -> usize {
    #[cfg(any(target_os = "linux", target_os = "macos", target_os = "freebsd"))]
    {
        let p = unsafe { unix::sysconf(unix::SC_PAGESIZE) };
        if p > 0 {
            p as usize
        } else {
            4096
        }
    }
    #[cfg(target_os = "windows")]
    {
        use std::sync::OnceLock;
        static CACHE: OnceLock<usize> = OnceLock::new();
        *CACHE.get_or_init(|| unsafe {
            #[repr(C)]
            #[derive(Default)]
            struct SystemInfo {
                _arch: u16,
                _reserved: u16,
                dw_page_size: u32,
                _min_addr: *mut std::ffi::c_void,
                _max_addr: *mut std::ffi::c_void,
                _active_mask: *mut u32,
                _procs: u32,
                _proc_type: u32,
                _alloc_granularity: u32,
                _proc_level: u16,
                _proc_revision: u16,
            }
            extern "system" {
                fn GetSystemInfo(lp_system_info: *mut SystemInfo);
            }
            let mut info = SystemInfo::default();
            GetSystemInfo(&mut info);
            info.dw_page_size as usize
        })
    }
    #[cfg(not(any(
        target_os = "linux",
        target_os = "macos",
        target_os = "freebsd",
        target_os = "windows"
    )))]
    {
        4096
    }
}

pub(crate) fn file_len(file: &File) -> io::Result<u64> {
    Ok(file.metadata()?.len())
}

/// The refusal of an advice this platform does not have.
///
/// The seven advices that only Linux has are numbers Linux owns, and a
/// number a kernel does not know is not an error but an instruction it
/// does know: FreeBSD's `MADV_PROTECT` is 10 and Linux's `MADV_DONTFORK`
/// is 10 too, so handing the number over would change the protection of
/// the mapping instead of turning the advice down.
///
/// The rule is one rule for the three unix platforms: Linux has the seven
/// and refuses nothing, and the two BSDs have the five that every unix
/// kernel has and refuse the other seven.
#[cfg(any(target_os = "linux", target_os = "macos", target_os = "freebsd"))]
pub(crate) fn refuse_unavailable(advice: core::ffi::c_int) -> io::Result<()> {
    if crate::memmap2::advice::is_available(advice) {
        return Ok(());
    }
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "this advice is a Linux one and this kernel does not have it",
    ))
}

pub(crate) fn file_len_for_desc(desc: &MmapRawDescriptor) -> io::Result<u64> {
    #[cfg(any(target_os = "linux", target_os = "macos", target_os = "freebsd"))]
    {
        // The size of a file is where its end is, and `lseek(2)` says
        // where that is without the kernel writing a struct into our
        // stack. The `stat` layout is the platform's, and the BSDs have
        // five of them -- FreeBSD 11 to 15 -- so a size read through one
        // would be a layout written out here and kept right by hand. The
        // tree's own reader says the same about its `stat`, in
        // `crates/discovery/src/dir.rs`: the offsets come from the
        // platform's layout, "never hand-remembered".
        //
        // The position the caller had is the caller's: asking a file how
        // long it is must not move it, so it is put back where it was.
        let here = unsafe { unix::lseek(desc.0, 0, unix::SEEK_CUR) };
        if here < 0 {
            return Err(io::Error::last_os_error());
        }
        let end = unsafe { unix::lseek(desc.0, 0, unix::SEEK_END) };
        if end < 0 {
            return Err(io::Error::last_os_error());
        }
        unsafe { unix::lseek(desc.0, here, unix::SEEK_SET) };
        Ok(end as u64)
    }
    #[cfg(target_os = "windows")]
    {
        let handle = desc.0;
        let mut size: i64 = 0;
        let r = unsafe { GetFileSizeEx(handle, &mut size) };
        if r == 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(size as u64)
        }
    }
    #[cfg(not(any(
        target_os = "linux",
        target_os = "macos",
        target_os = "freebsd",
        target_os = "windows"
    )))]
    {
        let _ = desc;
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "file_len_for_desc is unavailable on this platform",
        ))
    }
}

#[cfg(target_os = "windows")]
unsafe extern "system" {
    fn GetFileSizeEx(file: *mut std::ffi::c_void, file_size: *mut i64) -> i32;
}

pub(crate) fn map(
    desc: &MmapRawDescriptor,
    len: usize,
    offset: u64,
    populate: bool,
) -> io::Result<MmapInner> {
    #[cfg(any(target_os = "linux", target_os = "macos", target_os = "freebsd"))]
    {
        MmapInner::map(len, desc.0, offset, populate)
    }
    #[cfg(target_os = "windows")]
    {
        MmapInner::map(len, desc.0, offset, populate)
    }
    #[cfg(not(any(
        target_os = "linux",
        target_os = "macos",
        target_os = "freebsd",
        target_os = "windows"
    )))]
    {
        let _ = (desc, len, offset, populate);
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "memory mapping is not supported on this platform",
        ))
    }
}

pub(crate) fn map_exec(
    desc: &MmapRawDescriptor,
    len: usize,
    offset: u64,
    populate: bool,
) -> io::Result<MmapInner> {
    #[cfg(any(target_os = "linux", target_os = "macos", target_os = "freebsd"))]
    {
        MmapInner::map_exec(len, desc.0, offset, populate)
    }
    #[cfg(target_os = "windows")]
    {
        MmapInner::map_exec(len, desc.0, offset, populate)
    }
    #[cfg(not(any(
        target_os = "linux",
        target_os = "macos",
        target_os = "freebsd",
        target_os = "windows"
    )))]
    {
        let _ = (desc, len, offset, populate);
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "memory mapping is not supported on this platform",
        ))
    }
}

pub(crate) fn map_mut(
    desc: &MmapRawDescriptor,
    len: usize,
    offset: u64,
    populate: bool,
) -> io::Result<MmapInner> {
    #[cfg(any(target_os = "linux", target_os = "macos", target_os = "freebsd"))]
    {
        MmapInner::map_mut(len, desc.0, offset, populate)
    }
    #[cfg(target_os = "windows")]
    {
        MmapInner::map_mut(len, desc.0, offset, populate)
    }
    #[cfg(not(any(
        target_os = "linux",
        target_os = "macos",
        target_os = "freebsd",
        target_os = "windows"
    )))]
    {
        let _ = (desc, len, offset, populate);
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "memory mapping is not supported on this platform",
        ))
    }
}

pub(crate) fn map_copy(
    desc: &MmapRawDescriptor,
    len: usize,
    offset: u64,
    populate: bool,
) -> io::Result<MmapInner> {
    #[cfg(any(target_os = "linux", target_os = "macos", target_os = "freebsd"))]
    {
        MmapInner::map_copy(len, desc.0, offset, populate)
    }
    #[cfg(target_os = "windows")]
    {
        MmapInner::map_copy(len, desc.0, offset, populate)
    }
    #[cfg(not(any(
        target_os = "linux",
        target_os = "macos",
        target_os = "freebsd",
        target_os = "windows"
    )))]
    {
        let _ = (desc, len, offset, populate);
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "memory mapping is not supported on this platform",
        ))
    }
}

pub(crate) fn map_copy_read_only(
    desc: &MmapRawDescriptor,
    len: usize,
    offset: u64,
    populate: bool,
) -> io::Result<MmapInner> {
    #[cfg(any(target_os = "linux", target_os = "macos", target_os = "freebsd"))]
    {
        MmapInner::map_copy_read_only(len, desc.0, offset, populate)
    }
    #[cfg(target_os = "windows")]
    {
        MmapInner::map_copy_read_only(len, desc.0, offset, populate)
    }
    #[cfg(not(any(
        target_os = "linux",
        target_os = "macos",
        target_os = "freebsd",
        target_os = "windows"
    )))]
    {
        let _ = (desc, len, offset, populate);
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "memory mapping is not supported on this platform",
        ))
    }
}

pub(crate) fn map_anon(len: usize, stack: bool, populate: bool) -> io::Result<MmapInner> {
    MmapInner::map_anon(len, stack, populate)
}

#[cfg(any(target_os = "linux", target_os = "macos", target_os = "freebsd"))]
pub struct MmapRawDescriptor(pub std::os::unix::io::RawFd);
#[cfg(target_os = "windows")]
pub struct MmapRawDescriptor(pub std::os::windows::io::RawHandle);
#[cfg(not(any(
    target_os = "linux",
    target_os = "macos",
    target_os = "freebsd",
    target_os = "windows"
)))]
pub struct MmapRawDescriptor<'a>(pub &'a File);

pub trait MmapAsRawDesc {
    fn as_raw_desc(&self) -> MmapRawDescriptor;
}

#[cfg(any(target_os = "linux", target_os = "macos", target_os = "freebsd"))]
impl MmapAsRawDesc for std::os::unix::io::RawFd {
    fn as_raw_desc(&self) -> MmapRawDescriptor {
        MmapRawDescriptor(*self)
    }
}

#[cfg(any(target_os = "linux", target_os = "macos", target_os = "freebsd"))]
impl<'a, T> MmapAsRawDesc for &'a T
where
    T: std::os::unix::io::AsRawFd,
{
    fn as_raw_desc(&self) -> MmapRawDescriptor {
        MmapRawDescriptor(self.as_raw_fd())
    }
}

#[cfg(target_os = "windows")]
impl MmapAsRawDesc for std::os::windows::io::RawHandle {
    fn as_raw_desc(&self) -> MmapRawDescriptor {
        MmapRawDescriptor(*self)
    }
}

#[cfg(target_os = "windows")]
impl<'a, T> MmapAsRawDesc for &'a T
where
    T: std::os::windows::io::AsRawHandle,
{
    fn as_raw_desc(&self) -> MmapRawDescriptor {
        MmapRawDescriptor(self.as_raw_handle())
    }
}

#[cfg(not(any(
    target_os = "linux",
    target_os = "macos",
    target_os = "freebsd",
    target_os = "windows"
)))]
impl<'a> MmapAsRawDesc for &'a File {
    fn as_raw_desc(&self) -> MmapRawDescriptor {
        MmapRawDescriptor(self)
    }
}

pub(crate) fn slice_from_inner(inner: &MmapInner) -> &[u8] {
    unsafe { slice::from_raw_parts(inner.ptr(), inner.len()) }
}

pub(crate) fn slice_from_inner_mut(inner: &mut MmapInner) -> &mut [u8] {
    unsafe { slice::from_raw_parts_mut(inner.mut_ptr(), inner.len()) }
}

impl Deref for MmapInner {
    type Target = [u8];
    #[inline]
    fn deref(&self) -> &[u8] {
        slice_from_inner(self)
    }
}

impl DerefMut for MmapInner {
    #[inline]
    fn deref_mut(&mut self) -> &mut [u8] {
        slice_from_inner_mut(self)
    }
}
