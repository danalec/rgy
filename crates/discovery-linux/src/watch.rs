// SPDX-License-Identifier: Apache-2.0

//! Recursive inotify `Watch`: every directory under the root gets a watch;
//! an `IN_Q_OVERFLOW` (or watch-limit hit) reports [`ChangeBatch::Rescan`],
//! and anything else is translated into `UsnRecord`-shaped
//! [`rgy_discovery::UsnRecord`]s the name layer already knows (create/delete/
//! rename reason bits) — one change vocabulary across backends.
//!
//! The crate is `no_std`: the kernel is asked through the calls declared in
//! [`sys`], and the paths the watch holds are bytes, the shape the tree
//! speaks everywhere else. The folders under the root are read by the seam
//! (`rgy_discovery::dir::list_dir`), which is where the platform's `dirent`
//! and `stat` layouts live.

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;

use rgy_discovery::path;
use rgy_discovery::walk::KIND_DIR;
use rgy_discovery::{ChangeBatch, UsnRecord, Watch};

/// The inotify calls this backend makes, declared by hand.
///
/// The crate has no `std` and no `libc`: the kernel is asked through the
/// symbols spelled here, the way `crates/discovery/src/file.rs` declares
/// the file layer.
mod sys {
    use core::ffi::{c_char, c_int, c_void};

    /// `IN_NONBLOCK`: the read does not wait for an event to arrive.
    pub(super) const IN_NONBLOCK: c_int = 0o4000;
    /// `IN_CLOEXEC`: the descriptor does not outlive the process.
    pub(super) const IN_CLOEXEC: c_int = 0o2_000_000;

    /// The events the watch asks for, and the two it reads back.
    ///
    /// `IN_CLOSE_WRITE` is a file that was written and closed, which is
    /// the moment its name and its length are worth having.
    pub(super) const IN_CLOSE_WRITE: u32 = 0x0000_0008;
    pub(super) const IN_MOVED_FROM: u32 = 0x0000_0040;
    pub(super) const IN_MOVED_TO: u32 = 0x0000_0080;
    pub(super) const IN_CREATE: u32 = 0x0000_0100;
    pub(super) const IN_DELETE: u32 = 0x0000_0200;
    /// The queue overflowed: the watch missed something, and the only
    /// honest answer is a rescan.
    pub(super) const IN_Q_OVERFLOW: u32 = 0x0000_4000;
    /// The event is about a folder, not about a file.
    pub(super) const IN_ISDIR: u32 = 0x4000_0000;

    /// `EAGAIN`: the non-blocking read had nothing to say.
    pub(super) const EAGAIN: c_int = 11;
    /// `EWOULDBLOCK`: the same number under the other name.
    pub(super) const EWOULDBLOCK: c_int = EAGAIN;
    /// `EACCES`: the folder cannot be watched.
    pub(super) const EACCES: c_int = 13;
    /// `ENOENT`: the folder is gone.
    pub(super) const ENOENT: c_int = 2;
    /// `ENOSPC`: the watch limit is reached.
    pub(super) const ENOSPC: c_int = 28;

    unsafe extern "C" {
        pub(super) fn inotify_init1(flags: c_int) -> c_int;
        pub(super) fn inotify_add_watch(fd: c_int, pathname: *const c_char, mask: u32) -> c_int;
        pub(super) fn read(fd: c_int, buf: *mut c_void, count: usize) -> isize;
        pub(super) fn close(fd: c_int) -> c_int;
        /// The GNU spelling of the errno slot. The BSDs and macOS call
        /// it `__error()`; this crate is Linux only, so this is the
        /// name that is there.
        pub(super) fn __errno_location() -> *mut c_int;
    }
}

/// fsearch/Zen path-reason equivalents, folded into the shared
/// `USN_REASON_*` bits so `Index::apply_usn` filters them the same way.
const REASON_CREATE: u32 = rgy_discovery::USN_REASON_FILE_CREATE;
const REASON_DELETE: u32 = rgy_discovery::USN_REASON_FILE_DELETE;
const REASON_RENAME_OLD: u32 = rgy_discovery::USN_REASON_RENAME_OLD_NAME;
const REASON_RENAME_NEW: u32 = rgy_discovery::USN_REASON_RENAME_NEW_NAME;

/// The name of an errno the watch can be refused with.
///
/// The crate has no `libc` to ask, and the three that matter are the three
/// a user can do something about: the watch limit
/// (`/proc/sys/fs/inotify/max_user_watches`), the permission, and the
/// folder that is gone. An errno with no name is still the number, and the
/// number is the fact.
fn errno_name(errno: i32) -> &'static str {
    match errno {
        sys::EACCES => "EACCES: the folder cannot be watched",
        sys::ENOENT => "ENOENT: the folder is gone",
        sys::ENOSPC => "ENOSPC: the watch limit is reached",
        _ => "the kernel refused the watch",
    }
}

pub struct InotifyWatch {
    fd: i32,
    /// watch descriptor -> directory path.
    watches: BTreeMap<i32, Vec<u8>>,
    /// Pending rename_old record awaiting its rename_new partner.
    pending_old: Option<UsnRecord>,
}

impl InotifyWatch {
    /// Start watching `root` recursively. Directory watches are added on
    /// the fly for created subdirectories; a create event for a dir is
    /// enough to extend coverage.
    pub fn new(root: &[u8]) -> Result<InotifyWatch, String> {
        let fd = unsafe { sys::inotify_init1(sys::IN_NONBLOCK | sys::IN_CLOEXEC) };
        if fd < 0 {
            return Err("inotify_init failed".into());
        }
        let mut w = InotifyWatch {
            fd,
            watches: BTreeMap::new(),
            pending_old: None,
        };
        w.add_tree(root)?;
        Ok(w)
    }

    fn add_tree(&mut self, dir: &[u8]) -> Result<(), String> {
        self.add_one(dir)?;
        // The seam reads the folder and the metadata of every entry in
        // it, so the names and the kinds come out of one pass. A folder
        // that cannot be read is a folder with nothing in it, and the
        // two directory entries are left out: a tree walk that followed
        // `.` would never end.
        let Some(list) = rgy_discovery::dir::list_dir(dir) else {
            return Ok(());
        };
        for (name, stat) in list.names.iter().zip(list.stats.iter()) {
            // A link to a folder is a link, not a folder: the watch must
            // not descend through it.
            if stat.kind != KIND_DIR {
                continue;
            }
            let p = path::join(dir, name);
            self.add_tree(&p)?;
        }
        Ok(())
    }

    fn add_one(&mut self, dir: &[u8]) -> Result<(), String> {
        let Some(c) = path::cstring(dir) else {
            return Err("inotify_add_watch: the path has a NUL in it".into());
        };
        let wd = unsafe {
            sys::inotify_add_watch(
                self.fd,
                path::as_c_ptr(&c),
                sys::IN_CREATE
                    | sys::IN_DELETE
                    | sys::IN_MOVED_FROM
                    | sys::IN_MOVED_TO
                    | sys::IN_CLOSE_WRITE,
            )
        };
        if wd < 0 {
            // The errno is read HERE, where the call is: the number that
            // says why the watch was refused is the number the call set,
            // and a later read of it would be a later fact. The number is
            // the answer the user needs -- the watch limit and the
            // permission are the two reasons a live feed does not start,
            // and both of them have something the user can do about them.
            let errno = unsafe { *sys::__errno_location() };
            return Err(alloc::format!(
                "inotify_add_watch: errno {errno} ({}) on {}",
                errno_name(errno),
                String::from_utf8_lossy(dir),
            ));
        }
        self.watches.insert(wd, dir.to_vec());
        Ok(())
    }

    fn record(&self, wd: i32, name: &[u8], reason: u32) -> UsnRecord {
        let dir = self.watches.get(&wd).cloned().unwrap_or_default();
        let full = if name.is_empty() {
            dir
        } else {
            path::join(&dir, name)
        };
        UsnRecord {
            // The index keys by name rows, not FRNs, in the live overlay;
            // the path is what `Live` consumes. FRN 0 marks "overlay row".
            file_reference: 0,
            parent_reference: 0,
            usn: 0,
            reason,
            file_attributes: 0,
            name: String::from_utf8_lossy(&full).into_owned(),
        }
    }
}

impl Watch for InotifyWatch {
    fn next_batch(&mut self) -> Result<ChangeBatch, String> {
        let mut buf = alloc::vec![0u8; 64 * 1024];
        let mut events: Vec<UsnRecord> = Vec::new();
        loop {
            let n = unsafe { sys::read(self.fd, buf.as_mut_ptr().cast(), buf.len()) };
            if n < 0 {
                // The read is non-blocking, so "nothing yet" is `EAGAIN`
                // and everything else is a real failure.
                let errno = unsafe { *sys::__errno_location() };
                if errno == sys::EAGAIN || errno == sys::EWOULDBLOCK {
                    break;
                }
                return Err(alloc::format!("inotify read: errno {errno}"));
            }
            if n == 0 {
                break;
            }
            let mut off = 0usize;
            while off + 16 <= n as usize {
                // inotify_event: wd@0(i32), mask@4, cookie@8, len@12, name@16.
                let wd = i32::from_ne_bytes([buf[off], buf[off + 1], buf[off + 2], buf[off + 3]]);
                let mask =
                    u32::from_ne_bytes([buf[off + 4], buf[off + 5], buf[off + 6], buf[off + 7]]);
                let cookie =
                    u32::from_ne_bytes([buf[off + 8], buf[off + 9], buf[off + 10], buf[off + 11]]);
                let len = u32::from_ne_bytes([
                    buf[off + 12],
                    buf[off + 13],
                    buf[off + 14],
                    buf[off + 15],
                ]) as usize;
                let name = &buf[off + 16..(off + 16 + len).min(n as usize)];
                let name: Vec<u8> = name.iter().take_while(|&&b| b != 0).cloned().collect();
                off += 16 + len;

                if mask & sys::IN_Q_OVERFLOW != 0 {
                    return Ok(ChangeBatch::Rescan);
                }
                if mask & sys::IN_CREATE != 0 {
                    if mask & sys::IN_ISDIR != 0 {
                        let dir =
                            path::join(&self.watches.get(&wd).cloned().unwrap_or_default(), &name);
                        // A directory we cannot watch is a directory we will
                        // never hear about: its whole subtree goes unobserved
                        // and the live index silently misses it. The batch
                        // therefore ends in a rescan, which is what the
                        // watch-limit hit already does (IN_Q_OVERFLOW).
                        // The error is not dropped: the create is reported
                        // below, and the rescan is what makes it true.
                        //
                        // A directory that is already gone is the other
                        // case: there is no subtree to miss, and the delete
                        // that follows covers it. A rescan there would be a
                        // loop -- a folder created and removed in the same
                        // breath would ask for one every time.
                        //
                        // The whole tree is watched, not just the folder:
                        // a directory created with content (`mkdir -p a/b/c`,
                        // a clone, an unpack) has folders under it that were
                        // made before the watch on it existed, and a watch
                        // on the folder alone would miss all of them.
                        let gone = rgy_discovery::dir::stat(&dir).is_none();
                        if !gone && self.add_tree(&dir).is_err() {
                            return Ok(ChangeBatch::Rescan);
                        }
                    }
                    events.push(self.record(wd, &name, REASON_CREATE));
                }
                if mask & sys::IN_DELETE != 0 {
                    events.push(self.record(wd, &name, REASON_DELETE));
                }
                if mask & sys::IN_MOVED_FROM != 0 {
                    self.pending_old = Some(self.record(wd, &name, REASON_RENAME_OLD));
                }
                if mask & sys::IN_MOVED_TO != 0 {
                    if mask & sys::IN_ISDIR != 0 {
                        // A directory moved into the tree brings its subtree
                        // with it, and the subtree has to be watched: the
                        // watches are on the paths, so a tree that was moved
                        // from elsewhere has none.
                        let dir =
                            path::join(&self.watches.get(&wd).cloned().unwrap_or_default(), &name);
                        let gone = rgy_discovery::dir::stat(&dir).is_none();
                        if !gone && self.add_tree(&dir).is_err() {
                            return Ok(ChangeBatch::Rescan);
                        }
                    }
                    if let Some(mut old) = self.pending_old.take() {
                        let _ = cookie;
                        old.reason = REASON_RENAME_OLD;
                        events.push(old);
                    }
                    events.push(self.record(wd, &name, REASON_RENAME_NEW));
                }
            }
        }
        Ok(ChangeBatch::Events(events))
    }
}

impl Drop for InotifyWatch {
    fn drop(&mut self) {
        unsafe {
            sys::close(self.fd);
        }
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use super::*;

    /// A temporary directory of the test's own.
    fn temp(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(alloc::format!(
            "rgy-inotify-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Wait for the watch to report something.
    fn wait_for(w: &mut InotifyWatch) -> Vec<UsnRecord> {
        for _ in 0..100 {
            match w.next_batch() {
                Ok(ChangeBatch::Events(evs)) if !evs.is_empty() => return evs,
                _ => std::thread::sleep(std::time::Duration::from_millis(20)),
            }
        }
        Vec::new()
    }

    /// The paths the watch is on.
    fn watched(w: &InotifyWatch) -> Vec<String> {
        w.watches
            .values()
            .map(|p| String::from_utf8_lossy(p).into_owned())
            .collect()
    }

    /// A directory created with content under it is watched all the way
    /// down.
    ///
    /// The folders under it were made before the watch on it existed --
    /// `mkdir -p a/b/c` makes all three at once -- and a watch on the
    /// folder alone would miss all of them.
    #[test]
    fn a_directory_created_with_content_is_watched_all_the_way_down() {
        let root = temp("created");
        let mut w = InotifyWatch::new(root.to_string_lossy().as_bytes()).expect("the watch opens");
        std::fs::create_dir_all(root.join("a/b/c")).unwrap();
        let events = wait_for(&mut w);
        assert!(!events.is_empty(), "the create arrives");
        let watched = watched(&w);
        for want in ["a", "a/b", "a/b/c"] {
            assert!(
                watched.iter().any(|p| p.ends_with(want)),
                "the watch covers {want}; watched: {watched:?}"
            );
        }
    }

    /// A directory moved into the tree is watched all the way down.
    ///
    /// The watches are on the paths, and a tree moved in from elsewhere has
    /// none.
    #[test]
    fn a_directory_moved_into_the_tree_is_watched_all_the_way_down() {
        let root = temp("moved-into");
        let outside = temp("moved-from");
        std::fs::create_dir_all(outside.join("x/y")).unwrap();
        let mut w = InotifyWatch::new(root.to_string_lossy().as_bytes()).expect("the watch opens");
        std::fs::rename(outside.join("x"), root.join("x")).unwrap();
        let events = wait_for(&mut w);
        assert!(!events.is_empty(), "the move arrives");
        let watched = watched(&w);
        for want in ["x", "x/y"] {
            assert!(
                watched.iter().any(|p| p.ends_with(want)),
                "the watch covers {want}; watched: {watched:?}"
            );
        }
    }
}
