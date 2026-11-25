// SPDX-License-Identifier: Apache-2.0

//! Parallel whole-disk enumeration with getattrlistbulk(2).
//!
//! One syscall returns hundreds of entries with name, type, size, mtime and
//! flags already attached, so there is no per-file stat. Directories fan out
//! over the workers; mount points are not crossed, firmlinks are (that is
//! how /Users etc. on the data volume appear under / exactly once).
//!
//! # The shape
//!
//! The walk is a shared stack of directories still to list. A worker takes
//! one off the stack, lists it, and puts its subdirectories on the stack;
//! the walk ends when the stack drains. The listing id comes from one
//! shared counter, and the listings are collected in one place: the index
//! follows the `child` links, not the order the listings arrive in.
//!
//! The `std` feature is what the fan-out needs: with it the workers are
//! threads (`rgy-par`), without it there is one worker, on the thread that
//! called. The walk itself is the same either way.
//!
//! Children are opened with `openat()` relative to the parent's fd, so
//! paths are never rebuilt and PATH_MAX never bites. Measured on this Mac:
//! open() + close() is ~19us per directory (two Endpoint Security clients
//! tax every open), getattrlistbulk ~14us; past ~8 threads the kernel side
//! stops scaling. 18% of directories are empty: those the parent's listing
//! already says so (rechecked with one lookup) are never opened.

use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;
use core::ffi::{c_char, c_int, c_void};
use core::sync::atomic::{AtomicU32, AtomicUsize, Ordering};

use rgy_par::sync::{Mutex, OnceCell};

// The platform vocabulary, spelled from the Darwin headers: the crate has
// no dependency to reach the platform through, so the calls and the structs
// they take are declared here, by hand — the way
// `crates/discovery/src/file.rs` declares its own.

// The attribute groups the walk asks for. `sys/attr.h`.
const ATTR_BIT_MAP_COUNT: u16 = 5;
const ATTR_CMN_RETURNED_ATTRS: u32 = 0x8000_0000;
const ATTR_CMN_NAME: u32 = 0x0000_0001;
const ATTR_CMN_OBJTYPE: u32 = 0x0000_0008;
const ATTR_CMN_MODTIME: u32 = 0x0000_0400;
const ATTR_CMN_FLAGS: u32 = 0x0004_0000;
const ATTR_CMN_ERROR: u32 = 0x2000_0000;
const ATTR_DIR_ENTRYCOUNT: u32 = 0x0000_0002;
const ATTR_DIR_MOUNTSTATUS: u32 = 0x0000_0004;
const ATTR_DIR_DATALENGTH: u32 = 0x0000_0020;
const ATTR_FILE_DATALENGTH: u32 = 0x0000_0200;

// `FSOPT_NOFOLLOW`: do not follow a symbolic link. `sys/attr.h`.
const FSOPT_NOFOLLOW: u64 = 0x0000_0001;

// The mount-status bits of a directory. `sys/attr.h`.
const DIR_MNTSTATUS_MNTPOINT: u32 = 0x0000_0001;
const DIR_MNTSTATUS_TRIGGER: u32 = 0x0000_0002;

// The file-status flags. `sys/stat.h`.
const SF_FIRMLINK: u32 = 0x0080_0000;
const UF_HIDDEN: u32 = 0x0000_8000;

// The open(2) flags the walk asks for. `sys/fcntl.h`.
const O_RDONLY: c_int = 0x0000;
const O_DIRECTORY: c_int = 0x100000;
const O_NOFOLLOW: c_int = 0x0100;
const O_CLOEXEC: c_int = 0x1000000;

// `RLIMIT_NOFILE`: how many descriptors the process may hold open.
// `sys/resource.h`.
const RLIMIT_NOFILE: c_int = 8;

// `EPERM`: the refusal that means "no Full Disk Access". `sys/errno.h`.
const EPERM: c_int = 1;

/// The Darwin attrlist, as `sys/attr.h` declares it: how many attribute
/// groups the list carries, a reserved word, and the five groups.
#[repr(C)]
struct Attrlist {
    bitmapcount: u16,
    reserved: u16,
    commonattr: u32,
    volattr: u32,
    dirattr: u32,
    fileattr: u32,
    forkattr: u32,
}

/// The Darwin rlimit, as `sys/resource.h` declares it: two `rlim_t`, and
/// `rlim_t` is 64 bits wide on every Darwin target.
#[repr(C)]
struct Rlimit {
    rlim_cur: u64,
    rlim_max: u64,
}

// The platform calls, declared here: the crate has no dependency to reach
// the platform through, so they are written by hand — the way
// `crates/discovery/src/file.rs` writes its own.
unsafe extern "C" {
    fn open(path: *const c_char, oflag: c_int, ...) -> c_int;
    fn openat(dirfd: c_int, pathname: *const c_char, flags: c_int, ...) -> c_int;
    fn close(fd: c_int) -> c_int;
    // The BSD spelling of the errno accessor; the GNU one is
    // `__errno_location`.
    fn __error() -> *mut c_int;
    fn getrlimit(resource: c_int, rlim: *mut Rlimit) -> c_int;
    fn setrlimit(resource: c_int, rlim: *const Rlimit) -> c_int;
    fn getattrlistat(
        fd: c_int,
        path: *const c_char,
        attr_list: *mut Attrlist,
        attr_buf: *mut c_void,
        attr_buf_size: usize,
        options: u64,
    ) -> c_int;
    fn getattrlistbulk(
        dirfd: c_int,
        attr_list: *mut Attrlist,
        attr_buf: *mut c_void,
        attr_buf_size: usize,
        options: u64,
    ) -> c_int;
}

/// Folders never to open: set when the daemon runs without Full Disk
/// Access, where opening a consent-gated folder (Downloads, Desktop, ...)
/// pops a privacy prompt and blocks the call until someone answers it.
pub static SKIP: OnceCell<Vec<Vec<u8>>> = OnceCell::new();

/// Folders a scan was refused (EPERM): without Full Disk Access macOS also
/// silently denies some, like ~/Library/Mail. Recorded only while paths are
/// tracked, i.e. while SKIP is set.
pub static DENIED: Mutex<Vec<Vec<u8>>> = Mutex::new(Vec::new());

/// Entries listed by scans so far: the first scan's progress.
pub static LISTED: AtomicUsize = AtomicUsize::new(0);

pub fn blocked(path: &[u8]) -> bool {
    SKIP.get().is_some_and(|v| {
        v.iter()
            .any(|s| path.starts_with(s) && (path.len() == s.len() || path[s.len()] == b'/'))
    })
}

// One vocabulary across backends: the block type the name index consumes
// lives in the seam (rgy_discovery::walk) — this walk produces it.
pub use rgy_discovery::walk::{
    FLAG_HIDDEN, FLAG_MOUNT, KIND_DIR, KIND_FILE, KIND_LINK, KIND_OTHER, Listing, NONE, RawEnt,
};

/// Only while a listing is parsed: a directory its parent reports empty.
/// Firmlinks report 0 too (their entries live on the data volume), so they
/// never get it; grafts (MobileAsset images) do, so `finish_dir` checks.
const MAYBE_EMPTY: u8 = 1 << 7;

/// The buffer `getattrlistbulk` writes into: 256 KiB, one per worker.
///
/// `thread_local!` has no `no_std` equivalent, so the buffer is part of
/// the state the worker factory hands out.
const BUF_SIZE: usize = 256 * 1024;

/// What the workers share: the directories still to list, the id counter,
/// and the listings they produce.
struct Ctx {
    /// The directories still to list.
    stack: Mutex<Vec<Work>>,
    /// How many work items are outstanding: on the stack, or being listed.
    ///
    /// The walk is over when the stack is empty and this is zero. A worker
    /// that finds the stack empty while another one is still listing a
    /// directory waits for it: that worker is about to put more on the
    /// stack, and leaving early would leave those directories unlisted.
    pending: AtomicUsize,
    /// The next listing id.
    next_id: AtomicU32,
    /// The listings, as they come.
    out: Mutex<Vec<Listing>>,
}

/// A directory still to list.
struct Work {
    /// The id its listing will carry.
    id: u32,
    /// The directory it is opened against: the child is opened with
    /// `openat()` relative to the parent's fd, so no path is ever rebuilt
    /// and PATH_MAX never bites. `None` for the root, which is opened by
    /// path.
    parent: Option<Arc<Fd>>,
    /// The name, NUL-terminated: what `openat()` takes.
    name: Vec<u8>,
    /// The path, when paths are tracked (SKIP is set).
    path: Option<Vec<u8>>,
}

/// A directory fd, closed when the last holder lets it go.
struct Fd(i32);

impl Drop for Fd {
    fn drop(&mut self) {
        unsafe { close(self.0) };
    }
}

/// What a worker owns: the buffer `getattrlistbulk` writes into.
struct Worker {
    buf: Vec<u8>,
}

/// Scan `root` recursively. Listing id 0 is `root` itself.
pub fn scan(root: &[u8], threads: usize) -> Vec<Listing> {
    raise_fd_limit();
    let workers = threads.max(1);
    let ctx = Arc::new(Ctx {
        stack: Mutex::new(Vec::new()),
        pending: AtomicUsize::new(0),
        next_id: AtomicU32::new(1),
        out: Mutex::new(Vec::new()),
    });
    // The root: the one work item that is not opened relative to a parent.
    ctx.stack.lock().unwrap().push(Work {
        id: 0,
        parent: None,
        name: rgy_discovery::path::cstring(root).unwrap_or_default(),
        path: tracked_path(root),
    });
    ctx.pending.store(1, Ordering::Release);
    // The workers: one buffer each, and the shared stack between them.
    // The closure runs until the stack drains, so the walk ends when the
    // last directory has been listed.
    rgy_par::each_index_at(
        workers,
        workers,
        || Worker {
            buf: vec![0u8; BUF_SIZE],
        },
        |worker, _| {
            loop {
                let work = {
                    let mut stack = ctx.stack.lock().unwrap();
                    match stack.pop() {
                        Some(w) => Some(w),
                        None => {
                            if ctx.pending.load(Ordering::Acquire) == 0 {
                                // The stack is empty and nothing is
                                // outstanding: the walk is over.
                                break;
                            }
                            None
                        }
                    }
                };
                let Some(work) = work else {
                    // Another worker is listing a directory, and it is
                    // about to put more on the stack. Nothing to do but
                    // wait for it.
                    core::hint::spin_loop();
                    continue;
                };
                list_dir(work, &ctx, &mut worker.buf);
            }
        },
    );
    let mut out = ctx.out.lock().unwrap();
    core::mem::take(&mut *out)
}

fn raise_fd_limit() {
    let mut r: Rlimit = unsafe { core::mem::zeroed() };
    unsafe { getrlimit(RLIMIT_NOFILE, &mut r) };
    r.rlim_cur = r.rlim_max.min(65536);
    unsafe { setrlimit(RLIMIT_NOFILE, &r) };
}

/// List a single directory (no recursion). Subdirectories come back with
/// `child == NONE`. Used by the live updater.
pub fn list_one(path: &[u8]) -> Option<Listing> {
    if blocked(path) {
        return None;
    }
    let mut l = Listing {
        id: 0,
        names: Vec::new(),
        ents: Vec::new(),
    };
    if !list_into(path, &mut l) {
        return None;
    }
    for e in &mut l.ents {
        e.kind &= !MAYBE_EMPTY;
    }
    Some(l)
}

const OPEN_DIR: c_int = O_RDONLY | O_DIRECTORY | O_NOFOLLOW | O_CLOEXEC;

/// The ROOT is opened without `O_NOFOLLOW`.
///
/// The root is the folder the caller named, and a root that reaches the tree
/// through a symbolic link is resolved: the portable walk resolves it, the
/// two are the same answer, and a root that answers "nothing" on one
/// platform and "everything" on another is not an answer at all. Every other
/// folder is opened with `O_NOFOLLOW`: the walk does not descend into a
/// link, and a link that points back up the tree is a loop.
const OPEN_ROOT: c_int = O_RDONLY | O_DIRECTORY | O_CLOEXEC;

/// The path of the root, when paths are tracked: they are only tracked
/// when there is something to skip.
fn tracked_path(root: &[u8]) -> Option<Vec<u8>> {
    SKIP.get()
        .is_some_and(|v| !v.is_empty())
        .then(|| root.to_vec())
}

/// List one directory, and put its subdirectories on the stack.
///
/// The directory is opened here: `openat()` against the parent's fd, or
/// `open()` for the root. A directory that cannot be opened still gets a
/// listing — an empty one, under the id it was given — because the index
/// follows the `child` links and a missing link is a broken tree.
fn list_dir(work: Work, ctx: &Ctx, buf: &mut [u8]) {
    list_dir_inner(work, ctx, buf);
    // The directory is listed: its children are on the stack now, so the
    // count of the outstanding work can drop. This is the last thing the
    // worker does with the item, and it happens on every path -- the one
    // where the directory could not be opened included. An item that was
    // never counted down would keep the other workers waiting for a
    // directory that is not coming.
    ctx.pending.fetch_sub(1, Ordering::Release);
}

/// The work of one directory: open it, list it, and queue its children.
fn list_dir_inner(work: Work, ctx: &Ctx, buf: &mut [u8]) {
    let fd = match &work.parent {
        Some(parent) => unsafe {
            openat(
                parent.0,
                rgy_discovery::path::as_c_ptr(&work.name),
                OPEN_DIR,
            )
        },
        None => unsafe { open(rgy_discovery::path::as_c_ptr(&work.name), OPEN_ROOT) },
    };
    if fd < 0 {
        // (With paths tracked, opening is how refusals get recorded.)
        if work.parent.is_some()
            && unsafe { *__error() } == EPERM
            && let Some(p) = &work.path
        {
            DENIED.lock().unwrap().push(p.clone());
        }
        push(
            Listing {
                id: work.id,
                names: Vec::new(),
                ents: Vec::new(),
            },
            ctx,
        );
        return;
    }
    let me = Arc::new(Fd(fd));
    let mut l = Listing {
        id: work.id,
        names: Vec::new(),
        ents: Vec::new(),
    };
    list_fd(fd, &mut l, buf);
    let mut kids = Vec::new();
    let mut empty = Vec::new();
    for e in l.ents.iter_mut() {
        let maybe_empty = e.kind & MAYBE_EMPTY != 0;
        e.kind &= !MAYBE_EMPTY;
        if e.kind & 3 == KIND_DIR && e.kind & FLAG_MOUNT == 0 {
            let name = &l.names[e.name_off as usize..e.name_off as usize + e.name_len as usize];
            let child_path = work
                .path
                .as_ref()
                .map(|p| rgy_discovery::path::join(p, name));
            if child_path.as_deref().is_some_and(blocked) {
                continue;
            }
            e.child = ctx.next_id.fetch_add(1, Ordering::Relaxed);
            let cname = rgy_discovery::path::cstring(name).unwrap_or_default();
            if maybe_empty && child_path.is_none() && entry_count(fd, &cname) == Some(0) {
                empty.push(e.child);
            } else {
                kids.push((cname, e.child, child_path));
            }
        }
    }
    push(l, ctx);
    for id in empty {
        push(
            Listing {
                id,
                names: Vec::new(),
                ents: Vec::new(),
            },
            ctx,
        );
    }
    // The children go on the stack, each holding the parent's fd: it is
    // opened relative to it, so no path is ever rebuilt. The count of the
    // outstanding work goes up with them, under the same lock, so a worker
    // that finds the stack empty sees both.
    let mut stack = ctx.stack.lock().unwrap();
    for (name, id, path) in kids {
        stack.push(Work {
            id,
            parent: Some(me.clone()),
            name,
            path,
        });
        ctx.pending.fetch_add(1, Ordering::Relaxed);
    }
}

/// Entries in the directory `name` (in `dir`) resolves to.
fn entry_count(dir: i32, name: &[u8]) -> Option<u32> {
    let mut al: Attrlist = unsafe { core::mem::zeroed() };
    al.bitmapcount = ATTR_BIT_MAP_COUNT;
    al.dirattr = ATTR_DIR_ENTRYCOUNT;
    let mut b = [0u32; 2];
    let r = unsafe {
        getattrlistat(
            dir,
            rgy_discovery::path::as_c_ptr(name),
            &raw mut al,
            b.as_mut_ptr().cast(),
            8,
            FSOPT_NOFOLLOW,
        )
    };
    (r == 0).then_some(b[1])
}

fn push(l: Listing, ctx: &Ctx) {
    LISTED.fetch_add(l.ents.len(), Ordering::Relaxed);
    ctx.out.lock().unwrap().push(l);
}

/// The 32-bit word at `at`, if the buffer holds it.
///
/// The records come from the kernel, and a record shorter than the fields
/// it claims to hold is one this reader cannot trust. Reading it anyway
/// panics on a value the kernel wrote, which is the worst place for a
/// panic: the walk of a volume the user is looking at.
fn rd32_at(b: &[u8], at: usize) -> Option<u32> {
    crate::record::word32(b, at)
}

/// The 64-bit word at `at`, if the buffer holds it.
fn rd64_at(b: &[u8], at: usize) -> Option<u64> {
    crate::record::word64(b, at)
}

/// Returns false if the directory could not be opened.
fn list_into(path: &[u8], l: &mut Listing) -> bool {
    let Some(cpath) = rgy_discovery::path::cstring(path) else {
        return false;
    };
    let fd = unsafe { open(rgy_discovery::path::as_c_ptr(&cpath), OPEN_DIR) };
    if fd < 0 {
        return false;
    }
    let mut buf = vec![0u8; BUF_SIZE];
    list_fd(fd, l, &mut buf);
    unsafe { close(fd) };
    true
}

fn list_fd(fd: i32, l: &mut Listing, buf: &mut [u8]) {
    let mut al: Attrlist = unsafe { core::mem::zeroed() };
    al.bitmapcount = ATTR_BIT_MAP_COUNT;
    al.commonattr = ATTR_CMN_RETURNED_ATTRS
        | ATTR_CMN_NAME
        | ATTR_CMN_ERROR
        | ATTR_CMN_OBJTYPE
        | ATTR_CMN_MODTIME
        | ATTR_CMN_FLAGS;
    al.dirattr = ATTR_DIR_ENTRYCOUNT | ATTR_DIR_MOUNTSTATUS | ATTR_DIR_DATALENGTH;
    al.fileattr = ATTR_FILE_DATALENGTH;
    loop {
        let n = unsafe { getattrlistbulk(fd, &mut al, buf.as_mut_ptr().cast(), buf.len(), 0) };
        if n <= 0 {
            break;
        }
        // Size the listing exactly: 8M entries' worth of growth slack
        // would be ~150 MB at the scan's peak.
        // The records the kernel wrote, as byte ranges: a record that
        // does not fit in the buffer ends the listing, and the walk
        // reads the ones it can trust.
        let recs = crate::record::records(buf, n as usize);
        let mut bytes = 0usize;
        for &(start, end) in &recs {
            let (b, common) = (&buf[start..end], rd32_at(buf, start + 4).unwrap_or(0));
            if common & ATTR_CMN_NAME != 0 {
                let f = if common & ATTR_CMN_ERROR != 0 { 28 } else { 24 };
                if let Some(nlen) = rd32_at(b, f + 4) {
                    bytes += (nlen as usize).saturating_sub(1);
                }
            }
        }
        l.ents.reserve_exact(recs.len());
        l.names.reserve_exact(bytes);
        for &(start, end) in &recs {
            parse_entry(&buf[start..end], l);
        }
    }
}

fn parse_entry(b: &[u8], l: &mut Listing) {
    // Every word is checked: the record comes from the kernel, and one
    // shorter than the fields it claims to hold is one this reader cannot
    // trust. The entry is dropped, and the walk carries on.
    let Some(common) = rd32_at(b, 4) else {
        return;
    };
    let dirattr = rd32_at(b, 12).unwrap_or(0);
    let fileattr = rd32_at(b, 16).unwrap_or(0);
    let mut f = 24;
    // The error is the first attribute of the record: the kernel writes it
    // before the name, and it writes it on every record it was asked for --
    // a zero on the records it could describe, the code on the ones it
    // could not. The name is the one attribute an entry that errored still
    // carries. `bsd/vfs/vfs_attrlist.c` (`getattrlist_internal`,
    // `get_error_attributes`).
    let mut error = 0u32;
    if common & ATTR_CMN_ERROR != 0 {
        let Some(e) = rd32_at(b, f) else {
            return;
        };
        error = e;
        f += 4;
    }
    // An entry the platform could not describe is not an entry: the
    // portable walk drops the ones whose metadata it cannot read, and this
    // walk has to agree with it. The name it still carries is the name of
    // something the platform would not tell us about.
    if error != 0 {
        return;
    }
    if common & ATTR_CMN_NAME == 0 {
        return;
    }
    // The name offset is signed: the kernel writes it relative to the
    // attribute, and it may be negative. A negative offset (or one that
    // runs past the entry) is a record this reader cannot trust, and
    // taking it as an unsigned one wraps the start past the end of the
    // buffer -- a slice that panics on a value that came from the
    // kernel.
    let Some(off) = rd32_at(b, f).map(|v| v as i32 as isize) else {
        return;
    };
    let Some(nlen) = rd32_at(b, f + 4).map(|v| v as usize) else {
        return;
    };
    let Some(start) = (f as isize).checked_add(off).filter(|&s| s >= 0) else {
        return;
    };
    let start = start as usize;
    let Some(end) = start.checked_add(nlen.saturating_sub(1)) else {
        return;
    };
    let Some(name) = b.get(start..end) else {
        return;
    };
    f += 8;
    let mut kind = KIND_OTHER;
    if common & ATTR_CMN_OBJTYPE != 0 {
        let Some(t) = rd32_at(b, f) else {
            return;
        };
        kind = match t {
            1 => KIND_FILE,
            2 => KIND_DIR,
            5 => KIND_LINK,
            _ => KIND_OTHER,
        };
        f += 4;
    }
    let mut mtime = 0u32;
    if common & ATTR_CMN_MODTIME != 0 {
        let Some(t) = rd64_at(b, f) else {
            return;
        };
        mtime = (t as i64).clamp(0, u32::MAX as i64) as u32;
        f += 16;
    }
    let mut flags = 0;
    if common & ATTR_CMN_FLAGS != 0 {
        let Some(fl) = rd32_at(b, f) else {
            return;
        };
        flags = fl;
        if flags & UF_HIDDEN != 0 {
            kind |= FLAG_HIDDEN;
        }
        f += 4;
    }
    if dirattr & ATTR_DIR_ENTRYCOUNT != 0 {
        let Some(count) = rd32_at(b, f) else {
            return;
        };
        if count == 0 && flags & SF_FIRMLINK == 0 {
            kind |= MAYBE_EMPTY;
        }
        f += 4;
    }
    if dirattr & ATTR_DIR_MOUNTSTATUS != 0 {
        let Some(mnt) = rd32_at(b, f) else {
            return;
        };
        if mnt & (DIR_MNTSTATUS_MNTPOINT | DIR_MNTSTATUS_TRIGGER) != 0 {
            kind |= FLAG_MOUNT;
        }
        f += 4;
    }
    // The length of a directory. The kernel writes the attribute groups
    // in a fixed order -- the common one, then the directory one, then
    // the file one -- and it writes the directory group only for a
    // directory and the file group only for everything else.
    // `bsd/vfs/vfs_attrlist.c` (`attr_pack_common`, `attr_pack_dir`,
    // `attr_pack_file`).
    let mut dir_size = None;
    if dirattr & ATTR_DIR_DATALENGTH != 0 {
        let Some(s) = rd64_at(b, f) else {
            return;
        };
        dir_size = Some(s);
        f += 8;
    }
    let mut file_size = None;
    if fileattr & ATTR_FILE_DATALENGTH != 0 {
        let Some(s) = rd64_at(b, f) else {
            return;
        };
        file_size = Some(s);
    }
    // A directory's length is the one the directory group carries and a
    // file's is the one the file group carries: the number `stat(2)`
    // reports, which is what the portable walk lists.
    let size = dir_size.or(file_size).unwrap_or(0);
    if name.is_empty() || name.len() > u16::MAX as usize {
        return;
    }
    l.ents.push(RawEnt {
        name_off: l.names.len() as u32,
        name_len: name.len() as u16,
        kind,
        size,
        mtime,
        child: NONE,
    });
    l.names.extend_from_slice(name);
}
