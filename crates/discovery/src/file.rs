// SPDX-License-Identifier: Apache-2.0

//! Files: the bytes of a file, and the path a file really has.
//!
//! Three things the tree needs from a filesystem, and nothing else:
//! reading a file, writing one, and resolving a path (the index holds
//! real paths — on macOS `/etc` is `/private/etc` — and a query that
//! compares a scope against the index has to compare real paths on both
//! sides).
//!
//! The crate has no `std` to reach for, so the calls are declared here, by
//! hand, the way `crates/name-core/src/clock.rs` declares the clock and the
//! way `crates/discovery-macos/src/feed.rs` declares `realpath(3)`. There
//! are no new dependencies: the seam stays a crate of bytes.
//!
//! The shape is the tree's shape everywhere else: the path is bytes, and
//! the bytes are what the filesystem gave. Nothing here assumes UTF-8 —
//! the bytes go to the OS as they are, and the OS is the one that knows
//! what they mean.
//!
//! # What the platform does not have
//!
//! A bare-metal target has no filesystem to ask. The functions there answer
//! "no": a file that cannot be read is a file that is not there, and a path
//! that cannot be resolved is a path that stays as it was. A caller on such
//! a target owns the bytes already.

use alloc::vec::Vec;

/// The bytes of a file, or `None` when it cannot be read.
///
/// The whole file, or nothing: a file that fails halfway through is a file
/// this does not answer for. A partial read would be a file that looks
/// complete and is not — the caller would index half a document and never
/// know.
pub fn read_file(path: &[u8]) -> Option<Vec<u8>> {
    imp::read_file(path)
}

/// Write `bytes` to `path`, replacing what was there.
///
/// `true` when the bytes are on the disk. The write is not atomic: it
/// goes to the file, in place, and a crash in the middle leaves a half
/// written file. A caller that needs the old one to survive has to write
/// beside it and rename over it (which is what the persisted index does).
pub fn write_file(path: &[u8], bytes: &[u8]) -> bool {
    imp::write_file(path, bytes)
}

/// The path with its symlinks followed, as bytes.
///
/// `None` when there is no such path. The answer is absolute: a relative
/// path is resolved against the working directory, because a path that
/// depends on where the process stands is not a path the index can hold.
///
/// On Windows the long-path prefix (`\\?\`) is not part of the answer: it
/// is how the platform spells the path, not what the user wrote, and an
/// index that holds one spelling cannot be compared with another. A UNC
/// path keeps its `\\` — that is the only thing that says it is a UNC
/// path.
pub fn canonicalize(path: &[u8]) -> Option<Vec<u8>> {
    imp::canonicalize(path)
}

/// The unix side: `open(2)`, `read(2)`, `write(2)`, `close(2)` and
/// `realpath(3)`.
#[cfg(unix)]
mod imp {
    use super::Vec;
    use core::ffi::{c_char, c_int};

    const O_RDONLY: c_int = 0;
    const O_WRONLY: c_int = 1;

    // `O_CREAT` and `O_TRUNC` are not the same numbers everywhere: Linux
    // keeps the traditional octal values, and the BSDs (macOS and
    // FreeBSD) have their own. The tree builds for those three, and each
    // of them is spelled here.
    #[cfg(target_os = "linux")]
    const O_CREAT: c_int = 0o100;
    #[cfg(target_os = "linux")]
    const O_TRUNC: c_int = 0o1000;
    #[cfg(not(target_os = "linux"))]
    const O_CREAT: c_int = 0x0200;
    #[cfg(not(target_os = "linux"))]
    const O_TRUNC: c_int = 0x0400;

    /// The mode a new file gets: what `std::fs::write` asks for, before
    /// the umask takes its part.
    const FILE_MODE: c_int = 0o666;

    /// The buffer the path resolution uses.
    ///
    /// PATH_MAX is 4096 on Linux and 1024 on macOS, and the Darwin
    /// extension of `realpath` answers paths longer than either, so the
    /// buffer is wider than both. `std::fs::canonicalize` has the same
    /// limit as this on Linux: a path that does not fit is a path that
    /// does not resolve.
    const PATH_BUF: usize = 64 * 1024;

    /// The chunk a read asks for.
    const CHUNK: usize = 64 * 1024;

    unsafe extern "C" {
        fn open(path: *const c_char, flags: c_int, ...) -> c_int;
        // The buffers are `c_void`, not `u8`: `char` is signed on some
        // platforms (macOS) and unsigned on others (Linux), and a
        // `*const u8` means different things on each. `c_void` means the
        // same thing on both.
        fn read(fd: c_int, buf: *mut core::ffi::c_void, count: usize) -> isize;
        fn write(fd: c_int, buf: *const core::ffi::c_void, count: usize) -> isize;
        fn close(fd: c_int) -> c_int;
        // The Darwin extension of `realpath`: the plain symbol assumes a
        // PATH_MAX buffer, and this is the one the C library itself
        // links.
        #[cfg_attr(target_os = "macos", link_name = "realpath$DARWIN_EXTSN")]
        fn realpath(path: *const c_char, resolved: *mut c_char) -> *mut c_char;
    }

    pub fn read_file(path: &[u8]) -> Option<Vec<u8>> {
        let c = crate::path::cstring(path)?;
        let fd = unsafe { open(crate::path::as_c_ptr(&c), O_RDONLY) };
        if fd < 0 {
            return None;
        }
        let mut out = Vec::new();
        let mut buf = [0u8; CHUNK];
        loop {
            let n = unsafe { read(fd, buf.as_mut_ptr().cast(), buf.len()) };
            if n < 0 {
                unsafe { close(fd) };
                return None;
            }
            if n == 0 {
                break;
            }
            out.extend_from_slice(&buf[..n as usize]);
        }
        unsafe { close(fd) };
        Some(out)
    }

    pub fn write_file(path: &[u8], bytes: &[u8]) -> bool {
        let Some(c) = crate::path::cstring(path) else {
            return false;
        };
        let fd = unsafe {
            open(
                crate::path::as_c_ptr(&c),
                O_WRONLY | O_CREAT | O_TRUNC,
                FILE_MODE,
            )
        };
        if fd < 0 {
            return false;
        }
        let mut off = 0;
        while off < bytes.len() {
            let n = unsafe { write(fd, bytes.as_ptr().add(off).cast(), bytes.len() - off) };
            if n <= 0 {
                unsafe { close(fd) };
                return false;
            }
            off += n as usize;
        }
        unsafe { close(fd) };
        true
    }

    pub fn canonicalize(path: &[u8]) -> Option<Vec<u8>> {
        let c = crate::path::cstring(path)?;
        let mut buf = alloc::vec![0u8; PATH_BUF];
        let p = unsafe { realpath(crate::path::as_c_ptr(&c), buf.as_mut_ptr().cast()) };
        if p.is_null() {
            return None;
        }
        let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
        buf.truncate(end);
        Some(buf)
    }
}

/// The Windows side: `CreateFileW`, `ReadFile`, `WriteFile`,
/// `CloseHandle` and `GetFinalPathNameByHandleW`.
#[cfg(windows)]
mod imp {
    use super::Vec;
    use alloc::string::{String, ToString};
    use core::ffi::c_void;

    const GENERIC_READ: u32 = 0x8000_0000;
    const GENERIC_WRITE: u32 = 0x4000_0000;
    const FILE_READ_ATTRIBUTES: u32 = 0x0080;

    const FILE_SHARE_READ: u32 = 0x0000_0001;
    const FILE_SHARE_WRITE: u32 = 0x0000_0002;
    const FILE_SHARE_DELETE: u32 = 0x0000_0004;
    const SHARE_ALL: u32 = FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE;

    const CREATE_ALWAYS: u32 = 2;
    const OPEN_EXISTING: u32 = 3;

    const FILE_ATTRIBUTE_NORMAL: u32 = 0x0080;
    /// A directory has to be opened with this to be opened at all.
    const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;

    /// The answer of `GetFinalPathNameByHandleW` in the spelling of the
    /// volume: `C:\...`, not the volume GUID.
    const VOLUME_NAME_DOS: u32 = 0;

    /// The chunk a read asks for.
    const CHUNK: usize = 64 * 1024;

    unsafe extern "system" {
        fn CreateFileW(
            lpFileName: *const u16,
            dwDesiredAccess: u32,
            dwShareMode: u32,
            lpSecurityAttributes: *const c_void,
            dwCreationDisposition: u32,
            dwFlagsAndAttributes: u32,
            hTemplateFile: *const c_void,
        ) -> *mut c_void;
        fn ReadFile(
            hFile: *mut c_void,
            lpBuffer: *mut u8,
            nNumberOfBytesToRead: u32,
            lpNumberOfBytesRead: *mut u32,
            lpOverlapped: *const c_void,
        ) -> i32;
        fn WriteFile(
            hFile: *mut c_void,
            lpBuffer: *const u8,
            nNumberOfBytesToWrite: u32,
            lpNumberOfBytesWritten: *mut u32,
            lpOverlapped: *const c_void,
        ) -> i32;
        fn CloseHandle(hObject: *mut c_void) -> i32;
        fn GetFinalPathNameByHandleW(
            hFile: *mut c_void,
            lpszFilePath: *mut u16,
            cchFilePath: u32,
            dwFlags: u32,
        ) -> u32;
    }

    /// The handle a failed `CreateFileW` leaves behind.
    fn invalid() -> *mut c_void {
        usize::MAX as *mut c_void
    }

    /// Open a path for reading (`access`, `disposition`).
    fn open(path: &[u8], access: u32, disposition: u32, flags: u32) -> Option<*mut c_void> {
        let wide = crate::path::win_verbatim(path)?;
        let h = unsafe {
            CreateFileW(
                wide.as_ptr(),
                access,
                SHARE_ALL,
                core::ptr::null(),
                disposition,
                FILE_ATTRIBUTE_NORMAL | flags,
                core::ptr::null(),
            )
        };
        if h.is_null() || h == invalid() {
            return None;
        }
        Some(h)
    }

    pub fn read_file(path: &[u8]) -> Option<Vec<u8>> {
        let h = open(path, GENERIC_READ, OPEN_EXISTING, 0)?;
        let mut out = Vec::new();
        let mut buf = [0u8; CHUNK];
        loop {
            let mut got: u32 = 0;
            let ok = unsafe {
                ReadFile(
                    h,
                    buf.as_mut_ptr(),
                    buf.len() as u32,
                    &mut got,
                    core::ptr::null(),
                )
            };
            if ok == 0 {
                unsafe { CloseHandle(h) };
                return None;
            }
            if got == 0 {
                break;
            }
            out.extend_from_slice(&buf[..got as usize]);
        }
        unsafe { CloseHandle(h) };
        Some(out)
    }

    pub fn write_file(path: &[u8], bytes: &[u8]) -> bool {
        let Some(h) = open(path, GENERIC_WRITE, CREATE_ALWAYS, 0) else {
            return false;
        };
        let mut off = 0;
        while off < bytes.len() {
            // The length of a write is the platform's own (a `DWORD`), so a
            // payload bigger than 4 GiB is written in pieces. The remaining
            // length is CAPPED, never cast: a cast would make the second
            // piece zero bytes long, and the file would be left half
            // written with the function saying that it failed.
            let want = (bytes.len() - off).min(u32::MAX as usize) as u32;
            let mut put: u32 = 0;
            let ok = unsafe {
                WriteFile(
                    h,
                    bytes.as_ptr().add(off),
                    want,
                    &mut put,
                    core::ptr::null(),
                )
            };
            if ok == 0 {
                unsafe { CloseHandle(h) };
                return false;
            }
            if put == 0 {
                unsafe { CloseHandle(h) };
                return false;
            }
            off += put as usize;
        }
        unsafe { CloseHandle(h) };
        true
    }

    pub fn canonicalize(path: &[u8]) -> Option<Vec<u8>> {
        // The handle has to be opened on the path itself, so the answer is
        // the path the platform resolved it to — the symlinks followed and
        // the short names spelled out.
        let h = open(
            path,
            FILE_READ_ATTRIBUTES,
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS,
        )?;
        let mut buf = alloc::vec![0u16; CHUNK];
        let n = unsafe {
            GetFinalPathNameByHandleW(h, buf.as_mut_ptr(), buf.len() as u32, VOLUME_NAME_DOS)
        };
        unsafe { CloseHandle(h) };
        if n == 0 {
            return None;
        }
        let len = (n as usize).min(buf.len() - 1);
        let text = String::from_utf16_lossy(&buf[..len]);
        // The long-path prefix is how the platform spells the path, not
        // what the user wrote: it comes off, and a UNC path keeps the
        // `\\` that says it is one.
        let text = match text.strip_prefix(r"\\?\UNC\") {
            Some(rest) => alloc::format!(r"\\{rest}"),
            None => text.strip_prefix(r"\\?\").unwrap_or(&text).to_string(),
        };
        Some(text.into_bytes())
    }
}

/// A target with no filesystem to ask: no file, no path.
#[cfg(not(any(unix, windows)))]
mod imp {
    use super::Vec;

    pub fn read_file(_path: &[u8]) -> Option<Vec<u8>> {
        None
    }

    pub fn write_file(_path: &[u8], _bytes: &[u8]) -> bool {
        false
    }

    pub fn canonicalize(_path: &[u8]) -> Option<Vec<u8>> {
        None
    }
}

#[cfg(all(test, feature = "std"))]
mod tests {
    use super::*;
    use alloc::format;
    use alloc::vec::Vec;

    /// A path in the temporary folder, with a name nobody else is using.
    ///
    /// The temporary folder of the host, spelled the way the host spells
    /// it: the tests write real files, and a folder that does not exist
    /// is a test that fails for the wrong reason.
    fn temp_path(tag: &str) -> Vec<u8> {
        let mut p = std::env::temp_dir().as_os_str().as_encoded_bytes().to_vec();
        p.push(b'/');
        p.extend_from_slice(b"rgy-file-");
        p.extend_from_slice(tag.as_bytes());
        // The process id, like the folder fixture in `dir.rs`: two runs of
        // the same test binary write the same name, and the read-back can
        // read the other run's bytes.
        p.push(b'-');
        p.extend_from_slice(format!("{}", std::process::id()).as_bytes());
        p.extend_from_slice(b".bin");
        p
    }

    #[test]
    fn a_file_that_was_written_can_be_read_back() {
        let path = temp_path("roundtrip");
        let bytes = b"the bytes of a file\n";
        assert!(write_file(&path, bytes), "the write failed");
        assert_eq!(read_file(&path).as_deref(), Some(bytes.as_slice()));
        let _ = remove(&path);
    }

    #[test]
    fn a_file_that_is_not_there_is_not_read() {
        assert!(read_file(&temp_path("missing")).is_none());
    }

    #[test]
    fn writing_replaces_what_was_there() {
        let path = temp_path("replace");
        assert!(write_file(&path, b"the first version\n"));
        assert!(write_file(&path, b"the second\n"));
        assert_eq!(read_file(&path).as_deref(), Some(&b"the second\n"[..]));
        let _ = remove(&path);
    }

    #[test]
    fn a_path_with_a_nul_in_it_is_refused() {
        let mut path = temp_path("nul");
        path.push(0);
        assert!(read_file(&path).is_none());
        assert!(!write_file(&path, b"x"));
    }

    /// Take the file away again, so the test folder stays as it was.
    fn remove(path: &[u8]) -> Option<()> {
        #[cfg(unix)]
        {
            let c = crate::path::cstring(path)?;
            unsafe extern "C" {
                fn unlink(path: *const core::ffi::c_char) -> core::ffi::c_int;
            }
            unsafe { unlink(crate::path::as_c_ptr(&c)) };
            Some(())
        }
        #[cfg(windows)]
        {
            let wide = crate::path::win_verbatim(path)?;
            unsafe extern "system" {
                fn DeleteFileW(lpFileName: *const u16) -> i32;
            }
            unsafe { DeleteFileW(wide.as_ptr()) };
            Some(())
        }
        #[cfg(not(any(unix, windows)))]
        {
            let _ = path;
            None
        }
    }
}
