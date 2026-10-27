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
use core::sync::atomic::{AtomicU32, AtomicUsize, Ordering};

use rgy_par::sync::{Mutex, OnceCell};

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

const ATTR_CMN_ERROR: u32 = 0x2000_0000;
const DIR_MNTSTATUS_TRIGGER: u32 = 0x2;
const SF_FIRMLINK: u32 = 0x0080_0000;

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
        unsafe { libc::close(self.0) };
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
    let mut r: libc::rlimit = unsafe { core::mem::zeroed() };
    unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut r) };
    r.rlim_cur = r.rlim_max.min(65536);
    unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &r) };
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

const OPEN_DIR: i32 = libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC;

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
            libc::openat(
                parent.0,
                rgy_discovery::path::as_c_ptr(&work.name),
                OPEN_DIR,
            )
        },
        None => unsafe { libc::open(rgy_discovery::path::as_c_ptr(&work.name), OPEN_DIR) },
    };
    if fd < 0 {
        // (With paths tracked, opening is how refusals get recorded.)
        if work.parent.is_some()
            && unsafe { *libc::__error() } == libc::EPERM
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
    let mut al: libc::attrlist = unsafe { core::mem::zeroed() };
    al.bitmapcount = libc::ATTR_BIT_MAP_COUNT;
    al.dirattr = libc::ATTR_DIR_ENTRYCOUNT;
    let mut b = [0u32; 2];
    let r = unsafe {
        libc::getattrlistat(
            dir,
            rgy_discovery::path::as_c_ptr(name),
            (&raw mut al).cast(),
            b.as_mut_ptr().cast(),
            8,
            libc::FSOPT_NOFOLLOW as _,
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
    let fd = unsafe { libc::open(rgy_discovery::path::as_c_ptr(&cpath), OPEN_DIR) };
    if fd < 0 {
        return false;
    }
    let mut buf = vec![0u8; BUF_SIZE];
    list_fd(fd, l, &mut buf);
    unsafe { libc::close(fd) };
    true
}

fn list_fd(fd: i32, l: &mut Listing, buf: &mut [u8]) {
    let mut al: libc::attrlist = unsafe { core::mem::zeroed() };
    al.bitmapcount = libc::ATTR_BIT_MAP_COUNT;
    al.commonattr = libc::ATTR_CMN_RETURNED_ATTRS
        | libc::ATTR_CMN_NAME
        | ATTR_CMN_ERROR
        | libc::ATTR_CMN_OBJTYPE
        | libc::ATTR_CMN_MODTIME
        | libc::ATTR_CMN_FLAGS;
    al.dirattr = libc::ATTR_DIR_ENTRYCOUNT | libc::ATTR_DIR_MOUNTSTATUS;
    al.fileattr = libc::ATTR_FILE_DATALENGTH;
    loop {
        let n = unsafe {
            libc::getattrlistbulk(
                fd,
                &mut al as *mut _ as *mut libc::c_void,
                buf.as_mut_ptr() as *mut libc::c_void,
                buf.len(),
                0,
            )
        };
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
            if common & libc::ATTR_CMN_NAME != 0 {
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
    if common & ATTR_CMN_ERROR != 0 {
        f += 4;
    }
    if common & libc::ATTR_CMN_NAME == 0 {
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
    if common & libc::ATTR_CMN_OBJTYPE != 0 {
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
    if common & libc::ATTR_CMN_MODTIME != 0 {
        let Some(t) = rd64_at(b, f) else {
            return;
        };
        mtime = (t as i64).clamp(0, u32::MAX as i64) as u32;
        f += 16;
    }
    let mut flags = 0;
    if common & libc::ATTR_CMN_FLAGS != 0 {
        let Some(fl) = rd32_at(b, f) else {
            return;
        };
        flags = fl;
        if flags & libc::UF_HIDDEN != 0 {
            kind |= FLAG_HIDDEN;
        }
        f += 4;
    }
    if dirattr & libc::ATTR_DIR_ENTRYCOUNT != 0 {
        let Some(count) = rd32_at(b, f) else {
            return;
        };
        if count == 0 && flags & SF_FIRMLINK == 0 {
            kind |= MAYBE_EMPTY;
        }
        f += 4;
    }
    if dirattr & libc::ATTR_DIR_MOUNTSTATUS != 0 {
        let Some(mnt) = rd32_at(b, f) else {
            return;
        };
        if mnt & (libc::DIR_MNTSTATUS_MNTPOINT | DIR_MNTSTATUS_TRIGGER) != 0 {
            kind |= FLAG_MOUNT;
        }
        f += 4;
    }
    let mut size = 0u64;
    if fileattr & libc::ATTR_FILE_DATALENGTH != 0 {
        let Some(s) = rd64_at(b, f) else {
            return;
        };
        size = s;
    }
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
