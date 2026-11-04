// SPDX-License-Identifier: Apache-2.0

//! `getdirentries` bulk walk: one syscall
//! returns a whole directory block with name and type already attached
//! (`d_type` in the dirent), so there is no per-entry directory open — the
//! FreeBSD shape of the Linux `getdents64` walk, and closer to it than to
//! the macOS backend: there is no `getattrlistbulk` here, so every entry
//! still takes one `lstat` for size/mtime (a `stat` batching pass replaces
//! that later, same as on Linux).
//!
//! The dirent row is parsed at offsets taken from the `struct dirent` this
//! file declares via `offset_of!` — never hand-remembered: a layout mistake
//! is a build failure or a differential-test failure, not a wrong kind byte.
//! Rows are stride-walked by `d_reclen`, which is what the plan mandates for
//! the version-dependent parts of the layout.
//!
//! `getdirentries` requires `nbytes` >= the filesystem block size or some
//! filesystems answer `EINVAL`; the buffer is 64 KiB, above any block size
//! in use.
//!
//! Paths are bytes: the walk joins them the way the tree joins them
//! (`rgy_discovery::path::join`) and hands them to the kernel unchanged.

use alloc::string::String;
use alloc::vec::Vec;
use core::ffi::{c_char, c_int, c_long};
use core::mem::offset_of;

use rgy_discovery::path;
use rgy_discovery::walk::{
    KIND_DIR, KIND_FILE, KIND_LINK, KIND_OTHER, Listing, NONE, RawEnt, blocked,
};

pub struct FreeBsdSource {
    pub root: Vec<u8>,
}

impl rgy_discovery::Source for FreeBsdSource {
    fn enumerate(&self) -> Result<Vec<Listing>, String> {
        Ok(scan(&self.root))
    }
}

/// The FreeBSD dirent, as `sys/sys/dirent.h` declares it.
///
/// The kernel writes the whole row into the buffer `getdirentries` fills, so
/// the struct is declared whole: every field the header has, and the padding
/// the header puts between them. FreeBSD 12 gave the row a `d_off` and a
/// 16-bit `d_namlen`, and every release since keeps that shape — the
/// offsets below are the current header's, not the ones of FreeBSD 11 and
/// older, whose `d_name` sat at offset 8. `d_name` stays the last field:
/// the header says there is code depending on it.
#[repr(C)]
struct Dirent {
    d_fileno: u64,
    d_off: i64,
    d_reclen: u16,
    d_type: u8,
    d_pad0: u8,
    d_namlen: u16,
    d_pad1: u16,
    d_name: [c_char; 256],
}

/// The FreeBSD timespec, as `sys/_timespec.h` declares it.
#[repr(C)]
struct Timespec {
    tv_sec: i64,
    tv_nsec: c_long,
}

/// The FreeBSD stat, as `sys/sys/stat.h` declares it.
///
/// The kernel fills this one in the caller's buffer, so it is declared
/// whole: the header's fields, in its order, with the padding it spells
/// out. The widths are the header's, not the obvious ones: `nlink_t` is 64
/// bits (`st_mode` sits at offset 24, not 18) and `blksize_t` is 32, so
/// the struct is 224 bytes — the size the kernel writes into.
///
/// FreeBSD 12 and FreeBSD 15 spell the same slots differently — the word at
/// offset 26 is `st_padding0` there and `st_bsdflags` here, and the tail is
/// `st_spare` of ten or `st_filerev` with `st_spare` of nine — and both are
/// 224 bytes with every field the walk reads at the same offset, so one
/// declaration serves both. `st_mtime` is a macro in C (`st_mtim.tv_sec`);
/// here the seconds of `st_mtim` are read the same way.
#[repr(C)]
struct Stat {
    st_dev: u64,
    st_ino: u64,
    st_nlink: u64,
    st_mode: u16,
    st_bsdflags: i16,
    st_uid: u32,
    st_gid: u32,
    st_padding1: i32,
    st_rdev: u64,
    st_atim: Timespec,
    st_mtim: Timespec,
    st_ctim: Timespec,
    st_birthtim: Timespec,
    st_size: i64,
    st_blocks: i64,
    st_blksize: i32,
    st_flags: u32,
    st_gen: u64,
    st_filerev: u64,
    st_spare: [u64; 9],
}

// The rows the walk reads, at the offsets the struct above pins: `d_reclen`
// is the stride to the next row (8-aligned on every supported release); the
// name is NUL-scanned within the row rather than taken from `d_namlen`,
// whose width moved between releases.
const OFF_RECLEN: usize = offset_of!(Dirent, d_reclen);
const OFF_TYPE: usize = offset_of!(Dirent, d_type);
const OFF_NAME: usize = offset_of!(Dirent, d_name);

// The open(2) flags the walk asks for: a directory, and not a symbolic link
// to one. `sys/fcntl.h`.
const O_RDONLY: c_int = 0x0000;
const O_DIRECTORY: c_int = 0x0002_0000;
const O_NOFOLLOW: c_int = 0x0000_0100;

// The file-type bits of `st_mode` (`sys/sys/stat.h`) and the entry types of
// `d_type` (`sys/sys/dirent.h`).
const S_IFMT: u16 = 0o170000;
const S_IFDIR: u16 = 0o040000;
const S_IFREG: u16 = 0o100000;
const S_IFLNK: u16 = 0o120000;
const DT_DIR: u8 = 4;
const DT_REG: u8 = 8;
const DT_LNK: u8 = 10;

// The platform calls, declared here: the crate has no `std` and no
// dependency to reach the kernel through, so the calls are written by hand
// the way `crates/discovery/src/file.rs` writes its own.
//
// getdirentries(2): `ssize_t getdirentries(int fd, char *buf, int nbytes,
// long *basep)`. No binding in the tree wraps it, so the ABI is declared
// here, values from the header. `nbytes` must be at least the filesystem
// block size or some filesystems answer `EINVAL` — the buffer below is
// 64 KiB, above any block size in use.
unsafe extern "C" {
    fn open(path: *const c_char, flags: c_int, ...) -> c_int;
    fn close(fd: c_int) -> c_int;
    fn lstat(path: *const c_char, buf: *mut Stat) -> c_int;
    fn getdirentries(fd: c_int, buf: *mut c_char, nbytes: c_int, basep: *mut c_long) -> isize;
}

/// One directory via `getdirentries`. `child` is left `NONE`; the walk
/// fills it once the subdirectory's listing exists. Entries whose `d_type`
/// is `DT_UNKNOWN` (some filesystems) fall back to the `lstat` kind, which
/// every entry takes anyway for size/mtime.
fn list_dir(dir: &[u8], follow: bool) -> Option<Listing> {
    let c = path::cstring(dir)?;
    // The ROOT is the folder the caller named, and a root that reaches the
    // tree through a symbolic link is resolved: the portable walk resolves
    // it, the two are the same answer, and a root that answers "nothing"
    // on one platform and "everything" on another is not an answer at all.
    //
    // A CHILD is opened without following a link: the walk does not descend
    // into a link, and a link that points back up the tree is a loop.
    let flags = if follow {
        O_RDONLY | O_DIRECTORY
    } else {
        O_RDONLY | O_DIRECTORY | O_NOFOLLOW
    };
    let fd = unsafe { open(path::as_c_ptr(&c), flags) };
    if fd < 0 {
        return None;
    }
    let mut names: Vec<u8> = Vec::new();
    let mut ents: Vec<RawEnt> = Vec::new();
    let mut buf = alloc::vec![0u8; 64 * 1024];
    // The directory offset cookie getdirentries keeps across calls. Its
    // type is `long *` in the header; on every target we support that is
    // the same 8-byte slot as `off_t`.
    let mut base: c_long = 0;
    loop {
        let n = unsafe {
            getdirentries(
                fd,
                buf.as_mut_ptr() as *mut c_char,
                buf.len() as c_int,
                &mut base,
            )
        };
        if n <= 0 {
            break;
        }
        let mut off = 0usize;
        while off + OFF_NAME < n as usize {
            // linux_dirent64 was a fixed layout; a FreeBSD dirent is not,
            // so the stride comes from the row itself and every field
            // offset comes from the `struct dirent` above.
            let reclen =
                u16::from_ne_bytes([buf[off + OFF_RECLEN], buf[off + OFF_RECLEN + 1]]) as usize;
            if reclen <= OFF_NAME || off + reclen > n as usize {
                break;
            }
            let kind_byte = buf[off + OFF_TYPE];
            // The name is NUL-terminated inside the row; a row without one
            // is corrupt and skipped rather than read past its stride.
            let raw = &buf[off + OFF_NAME..off + reclen];
            let name: Vec<u8> = match raw.iter().position(|&b| b == 0) {
                Some(z) if z > 0 => raw[..z].to_vec(),
                _ => {
                    off += reclen;
                    continue;
                }
            };
            off += reclen;
            if name == b"." || name == b".." {
                continue;
            }

            let full = path::join(dir, &name);
            let fc = match path::cstring(&full) {
                Some(c) => c,
                None => continue,
            };
            let mut st: Stat = unsafe { core::mem::zeroed() };
            if unsafe { lstat(path::as_c_ptr(&fc), &mut st) } != 0 {
                continue;
            }
            let m = st.st_mode & S_IFMT;
            let kind = if m == S_IFLNK {
                KIND_LINK
            } else if m == S_IFDIR {
                KIND_DIR
            } else if m == S_IFREG {
                KIND_FILE
            } else {
                // Prefer the stat over d_type when they could disagree.
                match kind_byte {
                    DT_LNK => KIND_LINK,
                    DT_DIR => KIND_DIR,
                    DT_REG => KIND_FILE,
                    _ => KIND_OTHER,
                }
            };

            let name_off = names.len() as u32;
            names.extend_from_slice(&name);
            ents.push(RawEnt {
                name_off,
                name_len: name.len().min(u16::MAX as usize) as u16,
                kind,
                size: st.st_size as u64,
                mtime: st.st_mtim.tv_sec.clamp(0, u32::MAX as i64) as u32,
                child: NONE,
            });
        }
    }
    unsafe {
        close(fd);
    }
    Some(Listing { id: 0, names, ents })
}

/// The walk's stack entry: the directory's bytes, and the parent slot
/// (listing index, entry index) whose `child` takes this listing's id.
type Frame = (Vec<u8>, Option<(usize, usize)>);

/// DFS walk, listing 0 = `root`'s children, ids in pre-order — the same
/// block contract as [`rgy_discovery::walk::scan`].
pub fn scan(root: &[u8]) -> Vec<Listing> {
    let mut out: Vec<Listing> = Vec::new();
    let mut next_id = 1u32;
    // (dir, parent slot whose `child` takes this listing's id)
    let mut stack: Vec<Frame> = alloc::vec![(root.to_vec(), None)];
    while let Some((dir, fix)) = stack.pop() {
        if blocked(&dir) {
            continue;
        }
        // The first frame is the root: it has no parent slot, and it is
        // the one folder that is resolved rather than refused.
        let Some(mut listing) = list_dir(&dir, fix.is_none()) else {
            continue;
        };
        let id = if fix.is_none() { 0 } else { next_id };
        if fix.is_some() {
            next_id += 1;
        }
        listing.id = id;
        if let Some((li, ei)) = fix {
            out[li].ents[ei].child = id;
        }
        let mut kids: Vec<(Vec<u8>, usize)> = Vec::new();
        for (ei, ent) in listing.ents.iter().enumerate() {
            if ent.kind == KIND_DIR {
                let name = &listing.names
                    [ent.name_off as usize..(ent.name_off + ent.name_len as u32) as usize];
                kids.push((path::join(&dir, name), ei));
            }
        }
        let li = out.len();
        out.push(listing);
        for (d, ei) in kids.into_iter().rev() {
            stack.push((d, Some((li, ei))));
        }
    }
    out
}
