// SPDX-License-Identifier: Apache-2.0

//! `getdents64` bulk walk: one syscall returns a whole directory block with
//! name and type already attached (DT_DIR/DT_REG/DT_LNK), so there is no
//! per-entry directory open — the Linux half of fsearch's getattrlistbulk
//! trick. The walk is sequential DFS with a single id counter (the
//! parallel fanout refactor keeps its own allocator; until then the
//! portable `rgy_discovery::walk::scan` and this must agree on id semantics).
//!
//! Paths are bytes: the walk joins them the way the tree joins them
//! (`rgy_discovery::path::join`) and hands them to the kernel unchanged.

use alloc::string::String;
use alloc::vec::Vec;

use rgy_discovery::path;
use rgy_discovery::walk::{
    KIND_DIR, KIND_FILE, KIND_LINK, KIND_OTHER, Listing, NONE, RawEnt, blocked,
};

pub struct LinuxSource {
    pub root: Vec<u8>,
}

impl rgy_discovery::Source for LinuxSource {
    fn enumerate(&self) -> Result<Vec<Listing>, String> {
        Ok(scan(&self.root))
    }
}

/// One directory via `getdents64`. `child` is left `NONE`; the walk fills
/// it once the subdirectory's listing exists. Entries whose d_type is
/// `DT_UNKNOWN` (some filesystems) take one `lstat64` fallback; every
/// entry takes one `lstat64` for size/mtime — the plan's `statx` batching
/// replaces that.
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
    loop {
        // getdents64 is a raw syscall (the libc crate does not wrap it);
        // the buffer of linux_dirent64 rows is parsed below.
        let n = unsafe { libc::syscall(libc::SYS_getdents64, fd, buf.as_mut_ptr(), buf.len()) };
        if n <= 0 {
            break;
        }
        let mut off = 0usize;
        while off + 19 <= n as usize {
            // linux_dirent64: ino@0, off@8, reclen@16 (u16), type@18, name@19.
            let reclen = u16::from_ne_bytes([buf[off + 16], buf[off + 17]]) as usize;
            if reclen < 20 || off + reclen > n as usize {
                break;
            }
            let kind_byte = buf[off + 18];
            let raw = &buf[off + 19..off + reclen];
            let name: Vec<u8> = match raw.iter().position(|&b| b == 0) {
                Some(z) => raw[..z].to_vec(),
                None => raw.to_vec(),
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
            let mut st: libc::stat64 = unsafe { core::mem::zeroed() };
            if unsafe { libc::lstat64(path::as_c_ptr(&fc), &mut st) } != 0 {
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
