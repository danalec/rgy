// SPDX-License-Identifier: Apache-2.0

//! The filesystem, asked directly: the directory reader.
//!
//! The portable walk has to read a folder and say what is in it, on every
//! platform, without `std`. `std::fs` is a library over the very calls this
//! module makes by hand, and it brings the whole standard library with it.
//! So the reader is here, and the walk is written on top of it.
//!
//! The calls are declared by hand, the way `crates/discovery/src/file.rs`
//! declares the file layer and the way `crates/name-core/src/clock.rs`
//! declares the clock -- and so are the two structs the platform writes,
//! one platform at a time. The seam stays a crate of bytes: no
//! dependency, and nothing here that a target without an operating system
//! cannot build.
//!
//! # What the reader promises
//!
//! The walk is written against this list, and the block stream it produces
//! is the reference every other backend is compared with:
//!
//! * **The names are the platform's own bytes.** On unix they are the
//!   bytes the filesystem gave, which are not necessarily text -- a unix
//!   path is not UTF-8 and the tree does not pretend otherwise. On
//!   Windows they are UTF-16 turned into UTF-8, because the tree speaks
//!   UTF-8 everywhere.
//! * **`.` and `..` are never in the list.** They are the folder itself
//!   and its parent, not children of it, and descending into them is a
//!   loop.
//! * **A link is a link, not the thing it points at.** The metadata is
//!   read without following it, so a link's length is the link's own
//!   length and its time is the link's own. A folder that is a link is
//!   not a folder to descend into: that is what
//!   [`KIND_LINK`](crate::walk::KIND_LINK) against
//!   [`KIND_DIR`](crate::walk::KIND_DIR) decides.
//! * **An entry whose metadata cannot be read is left out.** A listing
//!   says what is there; an entry the platform cannot vouch for is not
//!   there.
//! * **The order is the platform's order.** Nothing here sorts: the index
//!   sorts every listing itself, and the block stream's order is part of
//!   the contract only in that it is the platform's.
//! * **A refused read is `None`, not an empty listing.** A folder that
//!   is not there, or that cannot be opened, is left out of the tree
//!   rather than recorded as a folder with nothing in it.
//!
//! The modification time is in seconds since the epoch, clamped to what
//! the block stream carries, and 0 when the platform does not say one.
//!
//! # The reparse point on Windows
//!
//! On Windows a reparse point is a link, whatever it points at: a
//! symlink, a junction, a cloud placeholder. It is [`KIND_LINK`], and it
//! is not descended into. That is the tree's rule and the MFT backend's
//! rule (`crates/discovery-windows/src/indexer.rs`): a reparse point is
//! its own kind, not a file and not a folder. The portable walk has to
//! agree with the backend, because the backend is what the differential
//! test compares the portable walk with.

use alloc::vec::Vec;

/// What one directory holds: the names, and what is known about each.
pub struct DirList {
    /// The names, as the platform gave them (bytes, not text).
    pub names: Vec<Vec<u8>>,
    /// What is known about each name, in the same order.
    pub stats: Vec<Stat>,
}

/// What the platform says about one entry.
pub struct Stat {
    /// The kind: [`KIND_DIR`](crate::walk::KIND_DIR),
    /// [`KIND_FILE`](crate::walk::KIND_FILE),
    /// [`KIND_LINK`](crate::walk::KIND_LINK) or
    /// [`KIND_OTHER`](crate::walk::KIND_OTHER).
    pub kind: u8,
    /// The platform's flags: [`FLAG_HIDDEN`](crate::walk::FLAG_HIDDEN) on
    /// Windows, 0 elsewhere.
    pub flags: u8,
    /// The length in bytes. For a link it is the link's own length, not
    /// the target's -- the metadata is read without following it.
    pub size: u64,
    /// The modification time, in seconds since the epoch, clamped to
    /// `u32::MAX`, and 0 when the platform does not say one.
    pub mtime: u32,
}

/// The entries of the directory at `path`, without following symlinks.
///
/// `None` when the folder could not be read: it is not there, it is not a
/// folder, or the platform refused it. The walk leaves such a folder out
/// of the tree; it does not record it as empty.
pub fn list_dir(path: &[u8]) -> Option<DirList> {
    imp::list(path)
}

/// What is known about `path`, without following symlinks.
///
/// `None` when the platform refused to say.
pub fn stat(path: &[u8]) -> Option<Stat> {
    imp::stat(path)
}

/// The modification time in seconds since the epoch, clamped to what the
/// block stream carries.
///
/// A time before the epoch, or none at all, is 0: the block stream
/// carries a time it can compare, and a file that was never written has
/// no time of its own.
#[cfg(any(unix, windows))]
fn seconds(secs: i64) -> u32 {
    if secs <= 0 {
        return 0;
    }
    (secs as u64).min(u32::MAX as u64) as u32
}

/// The unix side: `openat(2)`, `fdopendir(3)`, `readdir(3)` and
/// `fstatat(2)`.
///
/// The calls are the platform's, and so are the two structs the platform
/// writes: the `dirent` a directory read hands out and the `stat` a
/// metadata read fills in. They differ from one unix to the next -- Linux,
/// macOS and FreeBSD each lay them out differently, and their open flags
/// and their `AT_*` numbers differ too -- so the declarations are written
/// out here by hand, one platform at a time, the way
/// `crates/discovery/src/file.rs` declares the file layer and the way
/// `crates/name-core/src/clock.rs` declares the clock. A struct the
/// platform writes is the platform's struct, whole: a field left out is
/// not a shorter struct, it is a buffer the platform writes past.
///
/// The sizes and the offsets the reader reads at are asserted where the
/// structs are declared, so a layout that drifts fails the build and not
/// the walk.
#[cfg(unix)]
mod imp {
    use alloc::vec::Vec;
    use core::ffi::c_int;

    use crate::walk::{KIND_DIR, KIND_FILE, KIND_LINK, KIND_OTHER};

    use super::{DirList, Stat};

    /// The Linux declarations: the calls the reader makes, the flags they
    /// take, and the two structs the kernel writes.
    ///
    /// The layout is the one glibc and musl agree on for a 64-bit Linux:
    /// `struct stat` is 144 bytes and `struct dirent` is 280, with the
    /// name at 19 (`bits/struct_stat.h` and `bits/dirent.h`).
    #[cfg(target_os = "linux")]
    mod sys {
        use core::ffi::{c_char, c_int, c_void};

        /// Read only.
        pub(super) const O_RDONLY: c_int = 0;
        /// `O_DIRECTORY`: the open has to be of a folder.
        ///
        /// The number is the architecture's, and the architectures
        /// disagree: x86_64 takes it from `asm-generic/fcntl.h` (00200000),
        /// arm64 overrides it in its own `asm/fcntl.h` (0040000). The
        /// kernel does not ignore a bit it does not know -- the open
        /// fails with EINVAL and the folder stays shut -- so the number
        /// that is right on the host is the number that is wrong on the
        /// other one, and the whole tree reads nothing.
        #[cfg(target_arch = "x86_64")]
        pub(super) const O_DIRECTORY: c_int = 0o200_000;
        #[cfg(target_arch = "aarch64")]
        pub(super) const O_DIRECTORY: c_int = 0o40_000;
        /// `O_CLOEXEC`: the descriptor does not outlive the process.
        ///
        /// The same number on both architectures (`asm-generic/fcntl.h`).
        pub(super) const O_CLOEXEC: c_int = 0o2_000_000;
        /// The architectures the reader is declared for.
        ///
        /// The open flags and the metadata layout are the
        /// architecture's, and they were measured on the two the tree
        /// builds for. Another architecture gets a build error and not a
        /// wrong answer: what it needs is the `struct stat` its own
        /// headers describe, read the same way.
        #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
        compile_error!("the Linux reader is declared for x86_64 and aarch64");

        /// `AT_FDCWD`: the folder the call is made against when the
        /// caller has none of its own.
        pub(super) const AT_FDCWD: c_int = -100;
        /// `AT_SYMLINK_NOFOLLOW`: a link is a link, and its own length
        /// and its own time are its own.
        pub(super) const AT_SYMLINK_NOFOLLOW: c_int = 0x100;

        /// The mode's type: the file kind bits live in it.
        pub type Mode = u32;
        /// The file kind mask.
        pub const S_IFMT: Mode = 0o17_0000;
        /// A link.
        pub const S_IFLNK: Mode = 0o12_0000;
        /// A folder.
        pub const S_IFDIR: Mode = 0o4_0000;
        /// A regular file.
        pub const S_IFREG: Mode = 0o10_0000;

        /// What the platform writes for one directory entry.
        ///
        /// The name is the last field and it is 256 bytes long, so the
        /// reader takes the name's length from the type and never reads
        /// past the row.
        #[repr(C)]
        #[allow(dead_code)]
        pub struct PlatformDirent {
            pub d_ino: u64,
            pub d_off: i64,
            pub d_reclen: u16,
            pub d_type: u8,
            pub d_name: [u8; 256],
        }

        /// What the platform writes for one file's metadata, on x86_64.
        ///
        /// The whole struct, because the platform writes the whole
        /// struct: a field left out is not a shorter struct, it is a
        /// buffer the platform writes past.
        ///
        /// x86_64 puts the link count before the mode (`bits/stat.h`), so
        /// the mode lands at 24 and the struct is 144 bytes long.
        #[cfg(target_arch = "x86_64")]
        #[repr(C)]
        #[allow(dead_code)]
        pub struct PlatformStat {
            pub st_dev: u64,
            pub st_ino: u64,
            pub st_nlink: u64,
            pub st_mode: u32,
            pub st_uid: u32,
            pub st_gid: u32,
            pub st_pad0: u32,
            pub st_rdev: u64,
            pub st_size: i64,
            pub st_blksize: i64,
            pub st_blocks: i64,
            pub st_atime: i64,
            pub st_atime_nsec: i64,
            pub st_mtime: i64,
            pub st_mtime_nsec: i64,
            pub st_ctime: i64,
            pub st_ctime_nsec: i64,
            pub st_unused: [i64; 3],
        }

        /// What the platform writes for one file's metadata, on arm64.
        ///
        /// The arm64 layout is not x86_64's: the mode comes **before**
        /// the link count, the link count is 32 bits wide, and the whole
        /// struct is 128 bytes rather than 144. The size and the time
        /// land on the same offsets on both architectures; the mode does
        /// not -- read at x86_64's offset it is the link count, and every
        /// entry comes back as "other", which is a walk that sees
        /// folders and files as neither.
        #[cfg(target_arch = "aarch64")]
        #[repr(C)]
        #[allow(dead_code)]
        pub struct PlatformStat {
            pub st_dev: u64,
            pub st_ino: u64,
            pub st_mode: u32,
            pub st_nlink: u32,
            pub st_uid: u32,
            pub st_gid: u32,
            pub st_rdev: u64,
            pub st_pad1: u64,
            pub st_size: i64,
            pub st_blksize: i64,
            pub st_blocks: i64,
            pub st_atime: i64,
            pub st_atime_nsec: i64,
            pub st_mtime: i64,
            pub st_mtime_nsec: i64,
            pub st_ctime: i64,
            pub st_ctime_nsec: i64,
            pub st_unused: u64,
        }

        /// The struct is the platform's, and the platform writes all of
        /// it. The sizes and the offsets the reader reads at are pinned
        /// here: a layout that drifts fails the build and not the walk.
        #[cfg(target_arch = "x86_64")]
        const _: () = {
            assert!(core::mem::size_of::<PlatformStat>() == 144);
            assert!(core::mem::offset_of!(PlatformStat, st_mode) == 24);
            assert!(core::mem::offset_of!(PlatformStat, st_size) == 48);
            assert!(core::mem::offset_of!(PlatformStat, st_mtime) == 88);
        };
        #[cfg(target_arch = "aarch64")]
        const _: () = {
            assert!(core::mem::size_of::<PlatformStat>() == 128);
            assert!(core::mem::offset_of!(PlatformStat, st_mode) == 16);
            assert!(core::mem::offset_of!(PlatformStat, st_size) == 48);
            assert!(core::mem::offset_of!(PlatformStat, st_mtime) == 88);
        };
        const _: () = assert!(core::mem::size_of::<PlatformDirent>() == 280);
        const _: () = assert!(core::mem::offset_of!(PlatformDirent, d_name) == 19);

        unsafe extern "C" {
            pub(super) fn openat(dirfd: c_int, pathname: *const c_char, flags: c_int, ...)
            -> c_int;
            pub(super) fn fdopendir(fd: c_int) -> *mut c_void;
            pub(super) fn readdir(dirp: *mut c_void) -> *mut PlatformDirent;
            pub(super) fn closedir(dirp: *mut c_void) -> c_int;
            pub(super) fn close(fd: c_int) -> c_int;
            pub(super) fn fstatat(
                dirfd: c_int,
                pathname: *const c_char,
                buf: *mut PlatformStat,
                flags: c_int,
            ) -> c_int;
        }
    }

    /// The macOS declarations: the same calls, and the two structs
    /// Darwin writes.
    ///
    /// `struct stat` is 144 bytes and `struct dirent` is 1048, with the
    /// name at 21 (`bsd/sys/stat.h` and `bsd/sys/dirent.h`).
    #[cfg(target_os = "macos")]
    mod sys {
        use core::ffi::{c_char, c_int, c_void};

        /// Read only.
        pub(super) const O_RDONLY: c_int = 0;
        /// `O_DIRECTORY`: Darwin's own value.
        pub(super) const O_DIRECTORY: c_int = 0x0010_0000;
        /// `O_CLOEXEC`: Darwin's own value.
        pub(super) const O_CLOEXEC: c_int = 0x0100_0000;

        /// `AT_FDCWD`: Darwin's own value, and it is not Linux's.
        pub(super) const AT_FDCWD: c_int = -2;
        /// `AT_SYMLINK_NOFOLLOW`: Darwin's own value.
        pub(super) const AT_SYMLINK_NOFOLLOW: c_int = 0x0020;

        /// The mode's type: the file kind bits live in it.
        pub type Mode = u16;
        /// The file kind mask.
        pub const S_IFMT: Mode = 0o17_0000;
        /// A link.
        pub const S_IFLNK: Mode = 0o12_0000;
        /// A folder.
        pub const S_IFDIR: Mode = 0o4_0000;
        /// A regular file.
        pub const S_IFREG: Mode = 0o10_0000;

        /// What the platform writes for one directory entry.
        ///
        /// The name is the last field and it is 1024 bytes long, so the
        /// reader takes the name's length from the type and never reads
        /// past the row.
        #[repr(C)]
        #[allow(dead_code)]
        pub struct PlatformDirent {
            pub d_ino: u64,
            pub d_seekoff: u64,
            pub d_reclen: u16,
            pub d_namlen: u16,
            pub d_type: u8,
            pub d_name: [u8; 1024],
        }

        /// What the platform writes for one file's metadata.
        ///
        /// The whole struct, because the platform writes the whole
        /// struct: a field left out is not a shorter struct, it is a
        /// buffer the platform writes past.
        #[repr(C)]
        #[allow(dead_code)]
        pub struct PlatformStat {
            pub st_dev: i32,
            pub st_mode: u16,
            pub st_nlink: u16,
            pub st_ino: u64,
            pub st_uid: u32,
            pub st_gid: u32,
            pub st_rdev: i32,
            pub st_atime: i64,
            pub st_atime_nsec: i64,
            pub st_mtime: i64,
            pub st_mtime_nsec: i64,
            pub st_ctime: i64,
            pub st_ctime_nsec: i64,
            pub st_birthtime: i64,
            pub st_birthtime_nsec: i64,
            pub st_size: i64,
            pub st_blocks: i64,
            pub st_blksize: i32,
            pub st_flags: u32,
            pub st_gen: u32,
            pub st_lspare: i32,
            pub st_qspare: [i64; 2],
        }

        /// The struct is the platform's, and the platform writes all of
        /// it. The sizes and the offsets the reader reads at are pinned
        /// here: a layout that drifts fails the build and not the walk.
        const _: () = assert!(core::mem::size_of::<PlatformStat>() == 144);
        const _: () = assert!(core::mem::size_of::<PlatformDirent>() == 1048);
        const _: () = assert!(core::mem::offset_of!(PlatformStat, st_mode) == 4);
        const _: () = assert!(core::mem::offset_of!(PlatformStat, st_size) == 96);
        const _: () = assert!(core::mem::offset_of!(PlatformStat, st_mtime) == 48);
        const _: () = assert!(core::mem::offset_of!(PlatformDirent, d_name) == 21);

        unsafe extern "C" {
            pub(super) fn openat(dirfd: c_int, pathname: *const c_char, flags: c_int, ...)
            -> c_int;
            pub(super) fn fdopendir(fd: c_int) -> *mut c_void;
            pub(super) fn readdir(dirp: *mut c_void) -> *mut PlatformDirent;
            pub(super) fn closedir(dirp: *mut c_void) -> c_int;
            pub(super) fn close(fd: c_int) -> c_int;
            pub(super) fn fstatat(
                dirfd: c_int,
                pathname: *const c_char,
                buf: *mut PlatformStat,
                flags: c_int,
            ) -> c_int;
        }
    }

    /// The FreeBSD declarations: the same calls, and the two structs
    /// FreeBSD writes.
    ///
    /// `struct stat` is 224 bytes and `struct dirent` is 280, with the
    /// name at 24 (`sys/sys/stat.h` and `sys/sys/dirent.h`).
    #[cfg(target_os = "freebsd")]
    mod sys {
        use core::ffi::{c_char, c_int, c_void};

        /// Read only.
        pub(super) const O_RDONLY: c_int = 0;
        /// `O_DIRECTORY`: FreeBSD's own value.
        pub(super) const O_DIRECTORY: c_int = 0x0002_0000;
        /// `O_CLOEXEC`: FreeBSD's own value.
        pub(super) const O_CLOEXEC: c_int = 0x0010_0000;

        /// `AT_FDCWD`: the folder the call is made against when the
        /// caller has none of its own.
        pub(super) const AT_FDCWD: c_int = -100;
        /// `AT_SYMLINK_NOFOLLOW`: FreeBSD's own value.
        pub(super) const AT_SYMLINK_NOFOLLOW: c_int = 0x0200;

        /// The mode's type: the file kind bits live in it.
        pub type Mode = u16;
        /// The file kind mask.
        pub const S_IFMT: Mode = 0o17_0000;
        /// A link.
        pub const S_IFLNK: Mode = 0o12_0000;
        /// A folder.
        pub const S_IFDIR: Mode = 0o4_0000;
        /// A regular file.
        pub const S_IFREG: Mode = 0o10_0000;

        /// What the platform writes for one directory entry.
        ///
        /// The name is the last field and it is 256 bytes long, so the
        /// reader takes the name's length from the type and never reads
        /// past the row.
        #[repr(C)]
        #[allow(dead_code)]
        pub struct PlatformDirent {
            pub d_fileno: u64,
            pub d_off: i64,
            pub d_reclen: u16,
            pub d_type: u8,
            pub d_pad0: u8,
            pub d_namlen: u16,
            pub d_pad1: u16,
            pub d_name: [u8; 256],
        }

        /// What the platform writes for one file's metadata.
        ///
        /// The whole struct, because the platform writes the whole
        /// struct: a field left out is not a shorter struct, it is a
        /// buffer the platform writes past.
        ///
        /// The widths are the header's (`sys/sys/stat.h`), and two of them
        /// are not the widths a reader expects: `nlink_t` is 64 bits and
        /// `dev_t` is 32. The link count therefore takes eight bytes
        /// between the inode number and the mode, and `st_mode` sits at
        /// offset 24 -- not at 18, which is where it sits when the link
        /// count is read as the 16 bits an old header had. Reading the
        /// mode from the wrong offset does not fail: it reads the low
        /// half of the link count, the file-type bits come out as
        /// something else, and every file in the tree arrives with the
        /// kind "other" and is not indexed.
        ///
        /// The tail is FreeBSD 14's (`st_filerev` and nine spare words);
        /// FreeBSD 12 and 13 have ten spare words there instead. Both
        /// are 224 bytes and both put the fields the reader reads at the
        /// same offsets, so one layout answers for the three of them.
        #[repr(C)]
        #[allow(dead_code)]
        pub struct PlatformStat {
            pub st_dev: u64,
            pub st_ino: u64,
            pub st_nlink: u64,
            pub st_mode: u16,
            pub st_bsdflags: i16,
            pub st_uid: u32,
            pub st_gid: u32,
            pub st_padding1: i32,
            pub st_rdev: u64,
            pub st_atime: i64,
            pub st_atime_nsec: i64,
            pub st_mtime: i64,
            pub st_mtime_nsec: i64,
            pub st_ctime: i64,
            pub st_ctime_nsec: i64,
            pub st_birthtime: i64,
            pub st_birthtime_nsec: i64,
            pub st_size: i64,
            pub st_blocks: i64,
            pub st_blksize: i32,
            pub st_flags: u32,
            pub st_gen: u64,
            pub st_filerev: u64,
            pub st_spare: [u64; 9],
        }

        /// The struct is the platform's, and the platform writes all of
        /// it. The sizes and the offsets the reader reads at are pinned
        /// here: a layout that drifts fails the build and not the walk.
        const _: () = assert!(core::mem::size_of::<PlatformStat>() == 224);
        const _: () = assert!(core::mem::size_of::<PlatformDirent>() == 280);
        const _: () = assert!(core::mem::offset_of!(PlatformStat, st_mode) == 24);
        const _: () = assert!(core::mem::offset_of!(PlatformStat, st_size) == 112);
        const _: () = assert!(core::mem::offset_of!(PlatformStat, st_mtime) == 64);
        const _: () = assert!(core::mem::offset_of!(PlatformDirent, d_name) == 24);

        unsafe extern "C" {
            pub(super) fn openat(dirfd: c_int, pathname: *const c_char, flags: c_int, ...)
            -> c_int;
            pub(super) fn fdopendir(fd: c_int) -> *mut c_void;
            pub(super) fn readdir(dirp: *mut c_void) -> *mut PlatformDirent;
            pub(super) fn closedir(dirp: *mut c_void) -> c_int;
            pub(super) fn close(fd: c_int) -> c_int;
            pub(super) fn fstatat(
                dirfd: c_int,
                pathname: *const c_char,
                buf: *mut PlatformStat,
                flags: c_int,
            ) -> c_int;
        }
    }

    /// The reader is declared for the three unixes the tree builds for.
    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "freebsd")))]
    compile_error!("the unix reader is declared for Linux, macOS and FreeBSD");

    /// The structs above are the 64-bit unix ones. On a 32-bit unix
    /// `off_t` and `time_t` are 32 bits wide, the same fields land
    /// somewhere else, and the layout is a different one.
    #[cfg(not(target_pointer_width = "64"))]
    compile_error!("the unix reader is declared for a 64-bit unix");

    use sys::*;

    /// The entries of the directory at `path`.
    pub fn list(path: &[u8]) -> Option<DirList> {
        let c = crate::path::cstring(path)?;
        // The folder is opened by the path the caller gave, and every
        // entry is then read against that descriptor: a name is a name
        // *in that folder*, and the metadata of an entry is read
        // without following a link.
        let fd = unsafe {
            openat(
                AT_FDCWD,
                crate::path::as_c_ptr(&c),
                O_RDONLY | O_DIRECTORY | O_CLOEXEC,
            )
        };
        if fd < 0 {
            return None;
        }
        // `fdopendir` takes the descriptor over, and `closedir` closes
        // it. The descriptor number does not change, so it is still the
        // folder the entries are read from.
        let dir = unsafe { fdopendir(fd) };
        if dir.is_null() {
            unsafe { close(fd) };
            return None;
        }
        let mut names = Vec::new();
        let mut stats = Vec::new();
        loop {
            let entry = unsafe { readdir(dir) };
            if entry.is_null() {
                break;
            }
            let Some(name) = name_of(entry) else {
                continue;
            };
            // `.` is the folder itself and `..` its parent: neither is a
            // child of it, and descending into them is a loop.
            if name == b"." || name == b".." {
                continue;
            }
            // An entry whose metadata cannot be read is not an entry.
            let Some(stat) = stat_in(fd, &name) else {
                continue;
            };
            names.push(name);
            stats.push(stat);
        }
        unsafe { closedir(dir) };
        Some(DirList { names, stats })
    }

    /// What the platform knows about `path`, without following a link.
    pub fn stat(path: &[u8]) -> Option<Stat> {
        let c = crate::path::cstring(path)?;
        let mut st: PlatformStat = unsafe { core::mem::zeroed() };
        let rc = unsafe {
            fstatat(
                AT_FDCWD,
                crate::path::as_c_ptr(&c),
                &mut st,
                AT_SYMLINK_NOFOLLOW,
            )
        };
        if rc != 0 {
            return None;
        }
        Some(stat_of(&st))
    }

    /// What the platform knows about `name` inside the folder `dirfd`,
    /// without following a link.
    fn stat_in(dirfd: c_int, name: &[u8]) -> Option<Stat> {
        let c = crate::path::cstring(name)?;
        let mut st: PlatformStat = unsafe { core::mem::zeroed() };
        // `AT_SYMLINK_NOFOLLOW`: a link is a link, and its own length and
        // its own time are its own.
        let rc = unsafe {
            fstatat(
                dirfd,
                crate::path::as_c_ptr(&c),
                &mut st,
                AT_SYMLINK_NOFOLLOW,
            )
        };
        if rc != 0 {
            return None;
        }
        Some(stat_of(&st))
    }

    /// The kind, the length and the time of what the platform described.
    fn stat_of(st: &PlatformStat) -> Stat {
        let mode = st.st_mode & S_IFMT;
        let kind = if mode == S_IFLNK {
            KIND_LINK
        } else if mode == S_IFDIR {
            KIND_DIR
        } else if mode == S_IFREG {
            KIND_FILE
        } else {
            KIND_OTHER
        };
        Stat {
            kind,
            // The hidden flag is a Windows thing: on unix the platform
            // has no such bit to give.
            flags: 0,
            size: st.st_size as u64,
            // The `time_t` is 64 bits on every unix the reader is
            // declared for, so the seconds are the seconds.
            mtime: super::seconds(st.st_mtime),
        }
    }

    /// The name in a `dirent`, as bytes.
    ///
    /// The platform's `dirent` carries the name in an array whose length
    /// differs from one platform to the next, so the array's own size is
    /// read from the type and the name is the bytes before the NUL.
    fn name_of(entry: *mut PlatformDirent) -> Option<Vec<u8>> {
        let name = unsafe { core::ptr::addr_of!((*entry).d_name) }.cast::<u8>();
        let max = core::mem::size_of_val(unsafe { &(*entry).d_name });
        let mut len = 0;
        while len < max && unsafe { *name.add(len) } != 0 {
            len += 1;
        }
        if len == max {
            // No NUL: the row does not carry a name.
            return None;
        }
        Some(unsafe { core::slice::from_raw_parts(name, len) }.to_vec())
    }
}

/// The Windows side: `FindFirstFileW` and `FindNextFileW`.
///
/// The directory read gives the name, the attributes and the times in one
/// call: there is no stat per entry, and the long-path form
/// (`\\?\`) is what lets a path past `MAX_PATH` be listed at all.
#[cfg(windows)]
mod imp {
    use alloc::string::String;
    use alloc::vec::Vec;
    use core::ffi::c_void;

    use crate::walk::{FLAG_HIDDEN, KIND_DIR, KIND_FILE, KIND_LINK};

    use super::{DirList, Stat};

    /// `MAX_PATH`: the longest a name can be, in UTF-16 units.
    const MAX_PATH: usize = 260;
    /// The short name (`8.3`) the platform writes beside the long one:
    /// fourteen units, and the tail of the struct.
    const ALTERNATE_NAME: usize = 14;

    /// The hidden bit.
    const FILE_ATTRIBUTE_HIDDEN: u32 = 0x0000_0002;
    /// The folder bit.
    const FILE_ATTRIBUTE_DIRECTORY: u32 = 0x0000_0010;
    /// The link bit: a symlink, a junction, a cloud placeholder.
    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0000_0400;

    /// The share mode an open takes: everything may hold the file.
    const FILE_SHARE_ALL: u32 = 0x0000_0007;

    /// The access a metadata read takes.
    const FILE_READ_ATTRIBUTES: u32 = 0x0000_0080;

    const OPEN_EXISTING: u32 = 3;

    /// A folder has to be opened with this to be opened at all.
    const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
    /// The handle is opened on the link itself, not on what it points at.
    const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;

    /// The unix epoch in 100-nanosecond intervals since 1601-01-01.
    const EPOCH_100NS: u64 = 116_444_736_000_000_000;

    /// The Windows clock: 100-nanosecond ticks since 1601-01-01.
    ///
    /// The fields the walk does not read are still in the layout: the
    /// struct is the platform's, and a struct that is not the platform's
    /// is a struct the platform reads wrongly.
    #[allow(dead_code)]
    #[derive(Clone, Copy)]
    #[repr(C)]
    struct FileTime {
        dw_low_date_time: u32,
        dw_high_date_time: u32,
    }

    /// What one directory entry carries.
    ///
    /// The last array is the short name (`PROPAGATE`/8.3), which the
    /// walk does not read and the platform writes all the same: the
    /// struct is the platform's, and a struct that is not the
    /// platform's is a struct the platform writes past.
    #[allow(dead_code)]
    #[repr(C)]
    struct Win32FindDataW {
        dw_file_attributes: u32,
        ft_creation_time: FileTime,
        ft_last_access_time: FileTime,
        ft_last_write_time: FileTime,
        n_file_size_high: u32,
        n_file_size_low: u32,
        dw_reserved0: u32,
        dw_reserved1: u32,
        c_file_name: [u16; MAX_PATH],
        c_alternate_file_name: [u16; ALTERNATE_NAME],
    }

    /// What an open handle carries.
    #[allow(dead_code)]
    #[repr(C)]
    struct ByHandleFileInformation {
        dw_file_attributes: u32,
        ft_creation_time: FileTime,
        ft_last_access_time: FileTime,
        ft_last_write_time: FileTime,
        dw_volume_serial_number: u32,
        n_file_size_high: u32,
        n_file_size_low: u32,
        n_number_of_links: u32,
        n_file_index_high: u32,
        n_file_index_low: u32,
    }

    /// The struct is the platform's, and the platform writes all of it.
    ///
    /// A field left out is not a shorter struct: it is a buffer the
    /// platform writes past, and the stack that holds it. The sizes are
    /// asserted where the struct is declared, so a field left out fails
    /// the build instead of the walk.
    const _: () = assert!(core::mem::size_of::<Win32FindDataW>() == 592);
    const _: () = assert!(core::mem::size_of::<ByHandleFileInformation>() == 52);

    unsafe extern "system" {
        fn FindFirstFileW(
            lpFileName: *const u16,
            lpFindFileData: *mut Win32FindDataW,
        ) -> *mut c_void;
        fn FindNextFileW(hFindFile: *mut c_void, lpFindFileData: *mut Win32FindDataW) -> i32;
        fn FindClose(hFindFile: *mut c_void) -> i32;
        fn CreateFileW(
            lpFileName: *const u16,
            dwDesiredAccess: u32,
            dwShareMode: u32,
            lpSecurityAttributes: *const c_void,
            dwCreationDisposition: u32,
            dwFlagsAndAttributes: u32,
            hTemplateFile: *const c_void,
        ) -> *mut c_void;
        fn GetFileInformationByHandle(
            hFile: *mut c_void,
            lpFileInformation: *mut ByHandleFileInformation,
        ) -> i32;
        fn CloseHandle(hObject: *mut c_void) -> i32;
    }

    /// The entries of the directory at `path`.
    pub fn list(path: &[u8]) -> Option<DirList> {
        // The pattern is the folder with a `*` at the end, spelled in the
        // long form when the path can take it.
        let pattern = crate::path::win_pattern(path)?;
        let mut data: Win32FindDataW = unsafe { core::mem::zeroed() };
        let handle = unsafe { FindFirstFileW(pattern.as_ptr(), &mut data) };
        if handle.is_null() || handle == invalid() {
            return None;
        }
        let mut names = Vec::new();
        let mut stats = Vec::new();
        loop {
            let name = name_of(&data);
            // `.` is the folder itself and `..` its parent: neither is a
            // child of it.
            if name != b"." && name != b".." {
                names.push(name);
                stats.push(stat_of(&data));
            }
            // The listing ends when the platform says there is nothing
            // left (`ERROR_NO_MORE_FILES`) or refuses; either way the
            // entries read so far are what the folder holds.
            if unsafe { FindNextFileW(handle, &mut data) } == 0 {
                break;
            }
        }
        unsafe { FindClose(handle) };
        Some(DirList { names, stats })
    }

    /// What the platform knows about `path`, without following a link.
    pub fn stat(path: &[u8]) -> Option<Stat> {
        let wide = crate::path::win_verbatim(path)?;
        // The handle is opened on the link itself, not on what it points
        // at: that is what `FILE_FLAG_OPEN_REPARSE_POINT` is for.
        let handle = unsafe {
            CreateFileW(
                wide.as_ptr(),
                FILE_READ_ATTRIBUTES,
                FILE_SHARE_ALL,
                core::ptr::null(),
                OPEN_EXISTING,
                FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
                core::ptr::null(),
            )
        };
        if handle.is_null() || handle == invalid() {
            return None;
        }
        let mut info: ByHandleFileInformation = unsafe { core::mem::zeroed() };
        let ok = unsafe { GetFileInformationByHandle(handle, &mut info) };
        unsafe { CloseHandle(handle) };
        if ok == 0 {
            return None;
        }
        Some(Stat {
            kind: kind_of(info.dw_file_attributes),
            flags: if info.dw_file_attributes & FILE_ATTRIBUTE_HIDDEN != 0 {
                FLAG_HIDDEN
            } else {
                0
            },
            size: ((info.n_file_size_high as u64) << 32) | info.n_file_size_low as u64,
            mtime: super::seconds(unix_seconds(info.ft_last_write_time)),
        })
    }

    /// The kind an attribute set stands for.
    ///
    /// A reparse point is a link, whatever it points at: a symlink, a
    /// junction, a cloud placeholder. The MFT backend says the same, and
    /// the two have to agree -- the differential test compares the
    /// portable walk with the backend's.
    fn kind_of(attributes: u32) -> u8 {
        if attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            KIND_LINK
        } else if attributes & FILE_ATTRIBUTE_DIRECTORY != 0 {
            KIND_DIR
        } else {
            KIND_FILE
        }
    }

    /// The kind, the length and the time of what the platform described.
    fn stat_of(data: &Win32FindDataW) -> Stat {
        Stat {
            kind: kind_of(data.dw_file_attributes),
            flags: if data.dw_file_attributes & FILE_ATTRIBUTE_HIDDEN != 0 {
                FLAG_HIDDEN
            } else {
                0
            },
            size: ((data.n_file_size_high as u64) << 32) | data.n_file_size_low as u64,
            mtime: super::seconds(unix_seconds(data.ft_last_write_time)),
        }
    }

    /// The name the platform gave, as UTF-8 bytes.
    ///
    /// Windows hands the name over in UTF-16 and the tree speaks UTF-8.
    /// A name that is not valid UTF-8 cannot be spelled here, so the
    /// unpaired surrogates are replaced -- the same thing the tree did
    /// when it read the name as text.
    fn name_of(data: &Win32FindDataW) -> Vec<u8> {
        let wide: Vec<u16> = data
            .c_file_name
            .iter()
            .copied()
            .take_while(|&c| c != 0)
            .collect();
        String::from_utf16_lossy(&wide).into_bytes()
    }

    /// The time in seconds since the epoch.
    ///
    /// The Windows clock counts 100-nanosecond ticks from 1601-01-01,
    /// which is 11644473600 seconds before the unix epoch.
    fn unix_seconds(ft: FileTime) -> i64 {
        let ticks = ((ft.dw_high_date_time as u64) << 32) | ft.dw_low_date_time as u64;
        if ticks <= EPOCH_100NS {
            // Before the epoch, or a time the platform does not have.
            return 0;
        }
        ((ticks - EPOCH_100NS) / 10_000_000) as i64
    }

    /// The handle a failed call leaves behind: `INVALID_HANDLE_VALUE`.
    fn invalid() -> *mut c_void {
        usize::MAX as *mut c_void
    }
}

/// A target with no filesystem to ask: no folder, no entry.
#[cfg(not(any(unix, windows)))]
mod imp {
    use super::{DirList, Stat};

    pub fn list(_path: &[u8]) -> Option<DirList> {
        None
    }

    pub fn stat(_path: &[u8]) -> Option<Stat> {
        None
    }
}

#[cfg(all(test, feature = "std"))]
mod tests {
    extern crate std;

    use super::*;
    use alloc::format;
    use alloc::vec::Vec;
    use std::collections::BTreeMap;

    /// A folder with the shapes that tell a reader apart: a file, a
    /// folder, a name that is not ASCII, and -- where the platform lets
    /// one be made -- a link.
    fn fixture(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("rgy-dir-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("sub")).expect("fixture folder");
        std::fs::write(dir.join("hello.txt"), b"the bytes of a file").expect("fixture file");
        std::fs::write(dir.join("acentuacao.txt"), b"y").expect("fixture file");
        // A link is a link: the reader must say so, and must not read
        // the file it points at. Where the platform refuses to make one
        // (Windows needs a privilege for it), there is nothing to pin.
        #[cfg(unix)]
        std::os::unix::fs::symlink("hello.txt", dir.join("link.txt")).ok();
        dir
    }

    /// What the standard library says about one entry.
    fn std_entry(md: &std::fs::Metadata) -> (u8, u64, u32) {
        let ft = md.file_type();
        let kind = if ft.is_symlink() {
            crate::walk::KIND_LINK
        } else if ft.is_dir() {
            crate::walk::KIND_DIR
        } else if ft.is_file() {
            crate::walk::KIND_FILE
        } else {
            crate::walk::KIND_OTHER
        };
        let mtime = md
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs().min(u32::MAX as u64) as u32)
            .unwrap_or(0);
        (kind, md.len(), mtime)
    }

    /// The reader and the standard library must agree about a folder: the
    /// same names, the same kinds, the same lengths, the same times.
    ///
    /// This is the oracle for the port: `std::fs` is the implementation
    /// the reader replaced, and a difference here is a difference in what
    /// the walk sees.
    #[test]
    fn the_reader_agrees_with_the_standard_library() {
        let root = fixture("agree");
        let bytes = root.as_os_str().as_encoded_bytes().to_vec();
        let list = list_dir(&bytes).expect("the fixture folder is readable");

        let mut mine: BTreeMap<Vec<u8>, &Stat> = BTreeMap::new();
        for (name, stat) in list.names.iter().zip(list.stats.iter()) {
            mine.insert(name.clone(), stat);
        }

        let mut theirs: BTreeMap<Vec<u8>, (u8, u64, u32)> = BTreeMap::new();
        for entry in std::fs::read_dir(&root).expect("the fixture folder is readable") {
            let entry = entry.expect("the entry is readable");
            let name = entry.file_name();
            let name = name.to_string_lossy().as_bytes().to_vec();
            let md = entry.metadata().expect("the entry's metadata is readable");
            theirs.insert(name, std_entry(&md));
        }

        assert_eq!(
            mine.len(),
            theirs.len(),
            "the reader and the standard library found a different number of entries"
        );
        for (name, stat) in &mine {
            let Some((kind, size, mtime)) = theirs.get(name) else {
                panic!("the standard library did not see {name:?}");
            };
            assert_eq!(stat.kind, *kind, "{name:?} is a different kind");
            assert_eq!(stat.size, *size, "{name:?} is a different length");
            // The two reads are two moments in time: a file written
            // between them is a second apart, and a second is not a
            // disagreement.
            let diff = (stat.mtime as i64 - *mtime as i64).abs();
            assert!(diff <= 1, "{name:?} is {diff} seconds apart");
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The folder itself and its parent are not children of it.
    #[test]
    fn a_folder_does_not_list_itself() {
        let root = fixture("self");
        let bytes = root.as_os_str().as_encoded_bytes().to_vec();
        let list = list_dir(&bytes).expect("the fixture folder is readable");
        for name in &list.names {
            assert_ne!(name.as_slice(), b".", "`.` is not a child");
            assert_ne!(name.as_slice(), b"..", "`..` is not a child");
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The reader reads a real folder: the names it hands back are the
    /// names the folder holds, and the kinds are the kinds they are.
    ///
    /// This is what proves the platform's layouts. A name read at the
    /// wrong offset is garbage, and a mode read at the wrong offset is
    /// not a kind -- so the folder below is read and its entries are
    /// named, one at a time, instead of being compared with another
    /// reader's answer.
    #[test]
    fn the_reader_names_and_kinds_a_real_folder() {
        let root = fixture("names");
        let bytes = root.as_os_str().as_encoded_bytes().to_vec();
        let list = list_dir(&bytes).expect("the fixture folder is readable");

        let kinds: BTreeMap<Vec<u8>, u8> = list
            .names
            .iter()
            .cloned()
            .zip(list.stats.iter().map(|s| s.kind))
            .collect();

        assert_eq!(
            kinds.get(b"hello.txt".as_slice()),
            Some(&crate::walk::KIND_FILE),
            "hello.txt is a file: {kinds:?}"
        );
        assert_eq!(
            kinds.get(b"sub".as_slice()),
            Some(&crate::walk::KIND_DIR),
            "sub is a folder: {kinds:?}"
        );
        // A link is a link: the reader must not read the file it points
        // at. Where the platform refuses to make one (Windows needs a
        // privilege for it), there is nothing to pin.
        #[cfg(unix)]
        assert_eq!(
            kinds.get(b"link.txt".as_slice()),
            Some(&crate::walk::KIND_LINK),
            "link.txt is a link: {kinds:?}"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A folder that is not there is not an empty folder.
    #[test]
    fn a_folder_that_is_not_there_is_not_listed() {
        let root = fixture("absent");
        let mut missing = root.as_os_str().as_encoded_bytes().to_vec();
        missing.extend_from_slice(b"/rgy-dir-not-here");
        assert!(
            list_dir(&missing).is_none(),
            "a folder that is not there was listed"
        );
        assert!(
            stat(&missing).is_none(),
            "a folder that is not there was stat-ed"
        );
        let _ = std::fs::remove_dir_all(&root);
    }
}
