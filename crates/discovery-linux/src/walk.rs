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
//!
//! The kernel is asked through the calls declared in [`sys`], by hand: the
//! crate is `no_std` and has no `libc`, so the symbols are spelled out the
//! way `crates/discovery/src/file.rs` spells out the file layer. The
//! metadata of an entry is asked of the seam (`rgy_discovery::dir::stat`),
//! which is where the platform's `stat` layout lives.

use alloc::string::String;
use alloc::vec::Vec;

use core::ffi::c_long;

use rgy_discovery::path;
use rgy_discovery::walk::{
    KIND_DIR, KIND_FILE, KIND_LINK, KIND_OTHER, Listing, NONE, RawEnt, blocked,
};

/// The Linux calls this backend makes, declared by hand.
///
/// The crate has no `std` and no `libc`: the kernel is asked through the
/// symbols spelled here, the way `crates/discovery/src/file.rs` declares
/// the file layer.
mod sys {
    use core::ffi::{c_char, c_int, c_long};

    /// Read only.
    pub(super) const O_RDONLY: c_int = 0;
    /// `O_DIRECTORY`: the open has to be of a folder.
    ///
    /// The number is the architecture's: x86_64 takes it from
    /// `asm-generic/fcntl.h` (00200000) and arm64 overrides it in its own
    /// `asm/fcntl.h` (0040000). A bit the kernel does not know is not
    /// ignored -- the open fails with EINVAL and the folder stays shut,
    /// so the walk finds nothing at all.
    #[cfg(target_arch = "x86_64")]
    pub(super) const O_DIRECTORY: c_int = 0o200_000;
    #[cfg(target_arch = "aarch64")]
    pub(super) const O_DIRECTORY: c_int = 0o40_000;
    /// `O_NOFOLLOW`: the open is of the folder itself, not of what it
    /// points at.
    ///
    /// The architecture's, for the same reason: `asm-generic/fcntl.h`
    /// gives 00400000 on x86_64 and arm64 overrides it with 0100000.
    #[cfg(target_arch = "x86_64")]
    pub(super) const O_NOFOLLOW: c_int = 0o400_000;
    #[cfg(target_arch = "aarch64")]
    pub(super) const O_NOFOLLOW: c_int = 0o100_000;

    /// `getdents64`'s syscall number, which is the architecture's.
    ///
    /// `x86_64` is 217; `aarch64`, `riscv64` and `loongarch64` are 61.
    #[cfg(target_arch = "x86_64")]
    pub(super) const SYS_GETDENTS64: c_long = 217;
    #[cfg(any(
        target_arch = "aarch64",
        target_arch = "riscv64",
        target_arch = "loongarch64"
    ))]
    pub(super) const SYS_GETDENTS64: c_long = 61;
    #[cfg(target_arch = "s390x")]
    pub(super) const SYS_GETDENTS64: c_long = 220;
    #[cfg(target_arch = "powerpc64")]
    pub(super) const SYS_GETDENTS64: c_long = 202;
    #[cfg(target_arch = "mips64")]
    pub(super) const SYS_GETDENTS64: c_long = 5308;
    #[cfg(target_arch = "sparc64")]
    pub(super) const SYS_GETDENTS64: c_long = 154;
    /// The syscall number is the architecture's, and the tree builds for
    /// the ones above.
    #[cfg(not(any(
        target_arch = "x86_64",
        target_arch = "aarch64",
        target_arch = "riscv64",
        target_arch = "loongarch64",
        target_arch = "s390x",
        target_arch = "powerpc64",
        target_arch = "mips64",
        target_arch = "sparc64"
    )))]
    compile_error!("getdents64's syscall number is declared for the 64-bit Linux architectures");

    /// The kinds a directory's own rows carry (`DT_*`).
    pub(super) const DT_DIR: u8 = 4;
    pub(super) const DT_REG: u8 = 8;
    pub(super) const DT_LNK: u8 = 10;

    unsafe extern "C" {
        pub(super) fn open(pathname: *const c_char, flags: c_int, ...) -> c_int;
        pub(super) fn close(fd: c_int) -> c_int;
        /// `getdents64` is a raw syscall: the C library does not wrap it,
        /// and the rows it fills in are the kernel's own.
        pub(super) fn syscall(number: c_long, ...) -> c_long;
    }
}

pub struct LinuxSource {
    pub root: Vec<u8>,
}

impl rgy_discovery::Source for LinuxSource {
    fn enumerate(&self) -> Result<Vec<Listing>, String> {
        Ok(scan(&self.root))
    }
}

/// One directory via `getdents64`. `child` is left `NONE`; the walk fills
/// it once the subdirectory's listing exists.
///
/// The kind of an entry and its length and its time come from the seam,
/// which reads the metadata without following a link. Where the metadata
/// says nothing about the kind (a socket, a fifo, a filesystem that does
/// not fill the mode in), the row's own kind is what there is.
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
        sys::O_RDONLY | sys::O_DIRECTORY
    } else {
        sys::O_RDONLY | sys::O_DIRECTORY | sys::O_NOFOLLOW
    };
    let fd = unsafe { sys::open(path::as_c_ptr(&c), flags) };
    if fd < 0 {
        return None;
    }
    let mut names: Vec<u8> = Vec::new();
    let mut ents: Vec<RawEnt> = Vec::new();
    let mut buf = alloc::vec![0u8; 64 * 1024];
    loop {
        // getdents64 is a raw syscall (the C library does not wrap it);
        // the buffer of linux_dirent64 rows is parsed below.
        let n = unsafe {
            // The descriptor is passed as the width the call reads it at.
            // `syscall` is variadic and it reads every argument as a
            // `long`; a 32-bit `int` in a variadic slot leaves the upper
            // half of the register unspecified (AAPCS64, C.14), and the
            // call would then be made with a descriptor nobody can name.
            // The call is declared here by hand, so the width is stated
            // here too.
            sys::syscall(
                sys::SYS_GETDENTS64,
                fd as c_long,
                buf.as_mut_ptr(),
                buf.len(),
            )
        };
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

            // The metadata of the entry, read by the seam: a link is a
            // link, and its own length and its own time are its own.
            let full = path::join(dir, &name);
            let Some(st) = rgy_discovery::dir::stat(&full) else {
                continue;
            };
            let kind = if st.kind == KIND_OTHER {
                // The mode said nothing: the row's own kind is what
                // there is.
                match kind_byte {
                    sys::DT_LNK => KIND_LINK,
                    sys::DT_DIR => KIND_DIR,
                    sys::DT_REG => KIND_FILE,
                    _ => KIND_OTHER,
                }
            } else {
                st.kind
            };

            let name_off = names.len() as u32;
            names.extend_from_slice(&name);
            ents.push(RawEnt {
                name_off,
                name_len: name.len().min(u16::MAX as usize) as u16,
                kind,
                size: st.size,
                mtime: st.mtime,
                child: NONE,
            });
        }
    }
    unsafe {
        sys::close(fd);
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
