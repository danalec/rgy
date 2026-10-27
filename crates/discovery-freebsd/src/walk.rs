// SPDX-License-Identifier: Apache-2.0

//! `getdirentries` bulk walk (docs/design.md §2.2): one syscall
//! returns a whole directory block with name and type already attached
//! (`d_type` in the dirent), so there is no per-entry directory open — the
//! FreeBSD shape of the Linux `getdents64` walk, and closer to it than to
//! the macOS backend: there is no `getattrlistbulk` here, so every entry
//! still takes one `lstat` for size/mtime (a `stat` batching pass replaces
//! that later, same as on Linux).
//!
//! The dirent row is parsed at offsets taken from `libc::dirent` via
//! `offset_of!` — never hand-remembered: a layout mistake is a build
//! failure or a differential-test failure, not a wrong kind byte. Rows are
//! stride-walked by `d_reclen`, which is what the plan mandates for the
//! version-dependent parts of the layout.
//!
//! `getdirentries` requires `nbytes` >= the filesystem block size or some
//! filesystems answer `EINVAL`; the buffer is 64 KiB, above any block size
//! in use.
//!
//! Paths are bytes: the walk joins them the way the tree joins them
//! (`rgy_discovery::path::join`) and hands them to the kernel unchanged.

use alloc::string::String;
use alloc::vec::Vec;
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

// The rows the walk reads, at the offsets the libc binding pins for the
// target ABI (its build script selects the FreeBSD ABI — 12 by default,
// the exact release when built on one). `d_reclen` is the stride to the
// next row (8-aligned on every supported release); the name is NUL-scanned
// within the row rather than taken from `d_namlen`, whose width moved
// between releases.
const OFF_RECLEN: usize = offset_of!(libc::dirent, d_reclen);
const OFF_TYPE: usize = offset_of!(libc::dirent, d_type);
const OFF_NAME: usize = offset_of!(libc::dirent, d_name);

// getdirentries(2): `ssize_t getdirentries(int fd, char *buf, int nbytes,
// long *basep)`. The libc crate wraps neither it nor getdents for FreeBSD,
// so the ABI is declared here, values from the header. `nbytes` must be at
// least the filesystem block size or some filesystems answer `EINVAL` —
// the buffer below is 64 KiB, above any block size in use.
unsafe extern "C" {
    fn getdirentries(
        fd: libc::c_int,
        buf: *mut libc::c_char,
        nbytes: libc::c_int,
        basep: *mut libc::c_long,
    ) -> isize;
}

/// One directory via `getdirentries`. `child` is left `NONE`; the walk
/// fills it once the subdirectory's listing exists. Entries whose `d_type`
/// is `DT_UNKNOWN` (some filesystems) fall back to the `lstat` kind, which
/// every entry takes anyway for size/mtime.
fn list_dir(dir: &[u8]) -> Option<Listing> {
    let c = path::cstring(dir)?;
    let fd = unsafe {
        libc::open(
            path::as_c_ptr(&c),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW,
        )
    };
    if fd < 0 {
        return None;
    }
    let mut names: Vec<u8> = Vec::new();
    let mut ents: Vec<RawEnt> = Vec::new();
    let mut buf = alloc::vec![0u8; 64 * 1024];
    // The directory offset cookie getdirentries keeps across calls. Its
    // type is `long *` in the header; on every target we support that is
    // the same 8-byte slot as `off_t`.
    let mut base: libc::c_long = 0;
    loop {
        let n = unsafe {
            getdirentries(
                fd,
                buf.as_mut_ptr() as *mut libc::c_char,
                buf.len() as libc::c_int,
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
            // offset comes from `libc::dirent` above.
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
            let mut st: libc::stat = unsafe { core::mem::zeroed() };
            if unsafe { libc::lstat(path::as_c_ptr(&fc), &mut st) } != 0 {
                continue;
            }
            let m = st.st_mode & libc::S_IFMT;
            let kind = if m == libc::S_IFLNK {
                KIND_LINK
            } else if m == libc::S_IFDIR {
                KIND_DIR
            } else if m == libc::S_IFREG {
                KIND_FILE
            } else {
                // Prefer the stat over d_type when they could disagree.
                match kind_byte {
                    libc::DT_LNK => KIND_LINK,
                    libc::DT_DIR => KIND_DIR,
                    libc::DT_REG => KIND_FILE,
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
                mtime: st.st_mtime.clamp(0, u32::MAX as i64) as u32,
                child: NONE,
            });
        }
    }
    unsafe {
        libc::close(fd);
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
        let Some(mut listing) = list_dir(&dir) else {
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
