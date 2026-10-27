// SPDX-License-Identifier: Apache-2.0

//! Recursive inotify `Watch`: every directory under the root gets a watch;
//! an `IN_Q_OVERFLOW` (or watch-limit hit) reports [`ChangeBatch::Rescan`],
//! and anything else is translated into `UsnRecord`-shaped
//! [`rgy_discovery::UsnRecord`]s the name layer already knows (create/delete/
//! rename reason bits) — one change vocabulary across backends.
//!
//! The crate is `no_std`: the kernel is asked through `libc`, and the paths
//! the watch holds are bytes, the shape the tree speaks everywhere else.

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;

use rgy_discovery::path;
use rgy_discovery::{ChangeBatch, UsnRecord, Watch};

/// fsearch/Zen path-reason equivalents, folded into the shared
/// `USN_REASON_*` bits so `Index::apply_usn` filters them the same way.
const REASON_CREATE: u32 = rgy_discovery::USN_REASON_FILE_CREATE;
const REASON_DELETE: u32 = rgy_discovery::USN_REASON_FILE_DELETE;
const REASON_RENAME_OLD: u32 = rgy_discovery::USN_REASON_RENAME_OLD_NAME;
const REASON_RENAME_NEW: u32 = rgy_discovery::USN_REASON_RENAME_NEW_NAME;

pub struct InotifyWatch {
    fd: i32,
    /// watch descriptor -> directory path.
    watches: BTreeMap<i32, Vec<u8>>,
    /// Pending rename_old record awaiting its rename_new partner.
    pending_old: Option<UsnRecord>,
}

/// The names in `dir`, with `.` and `..` left out.
///
/// A directory read skips the two directory entries, and so does this: a
/// tree walk that followed `.` would never end. A directory that cannot
/// be read answers `None`, the way a failed read did.
fn dir_names(dir: &[u8]) -> Option<Vec<Vec<u8>>> {
    let c = path::cstring(dir)?;
    let dp = unsafe { libc::opendir(path::as_c_ptr(&c)) };
    if dp.is_null() {
        return None;
    }
    let mut names: Vec<Vec<u8>> = Vec::new();
    loop {
        let ent = unsafe { libc::readdir(dp) };
        if ent.is_null() {
            break;
        }
        let name = unsafe { core::ffi::CStr::from_ptr((*ent).d_name.as_ptr()) }.to_bytes();
        if name == b"." || name == b".." {
            continue;
        }
        names.push(name.to_vec());
    }
    unsafe {
        libc::closedir(dp);
    }
    Some(names)
}

/// The mode of `full`, without following a symlink.
///
/// This is the `symlink_metadata` the std walk asked for, asked through
/// `lstat`: a symlink to a directory is a symlink, not a directory, and the
/// watch must not descend through it.
fn lstat_mode(full: &[u8]) -> Option<libc::mode_t> {
    let c = path::cstring(full)?;
    let mut st: libc::stat = unsafe { core::mem::zeroed() };
    if unsafe { libc::lstat(path::as_c_ptr(&c), &mut st) } != 0 {
        return None;
    }
    Some(st.st_mode)
}

/// Whether `full` is a directory (and not a symlink to one).
fn is_dir(full: &[u8]) -> bool {
    matches!(lstat_mode(full), Some(m) if m & libc::S_IFMT == libc::S_IFDIR)
}

impl InotifyWatch {
    /// Start watching `root` recursively. Directory watches are added on
    /// the fly for created subdirectories; a create event for a dir is
    /// enough to extend coverage.
    pub fn new(root: &[u8]) -> Result<InotifyWatch, String> {
        let fd = unsafe { libc::inotify_init1(libc::IN_NONBLOCK | libc::IN_CLOEXEC) };
        if fd < 0 {
            return Err("inotify_init failed".into());
        }
        let mut w = InotifyWatch {
            fd,
            watches: BTreeMap::new(),
            pending_old: None,
        };
        w.add_tree(root)
            .map_err(|e| alloc::format!("inotify_add_watch failed: {e}"))?;
        Ok(w)
    }

    fn add_tree(&mut self, dir: &[u8]) -> Result<(), i32> {
        self.add_one(dir)?;
        let Some(names) = dir_names(dir) else {
            return Ok(());
        };
        for name in names {
            let p = path::join(dir, &name);
            if is_dir(&p) {
                self.add_tree(&p)?;
            }
        }
        Ok(())
    }

    fn add_one(&mut self, dir: &[u8]) -> Result<(), i32> {
        let c = path::cstring(dir).ok_or(-1)?;
        let wd = unsafe {
            libc::inotify_add_watch(
                self.fd,
                path::as_c_ptr(&c),
                libc::IN_CREATE
                    | libc::IN_DELETE
                    | libc::IN_MOVED_FROM
                    | libc::IN_MOVED_TO
                    | libc::IN_CLOSE_WRITE,
            )
        };
        if wd < 0 {
            return Err(wd);
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
            let n = unsafe { libc::read(self.fd, buf.as_mut_ptr().cast(), buf.len()) };
            if n < 0 {
                // The read is non-blocking, so "nothing yet" is `EAGAIN`
                // and everything else is a real failure.
                let errno = unsafe { *libc::__errno_location() };
                if errno == libc::EAGAIN || errno == libc::EWOULDBLOCK {
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

                if mask & libc::IN_Q_OVERFLOW != 0 {
                    return Ok(ChangeBatch::Rescan);
                }
                if mask & libc::IN_CREATE != 0 {
                    if mask & libc::IN_ISDIR != 0 {
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
                        let gone = lstat_mode(&dir).is_none();
                        if !gone && self.add_tree(&dir).is_err() {
                            return Ok(ChangeBatch::Rescan);
                        }
                    }
                    events.push(self.record(wd, &name, REASON_CREATE));
                }
                if mask & libc::IN_DELETE != 0 {
                    events.push(self.record(wd, &name, REASON_DELETE));
                }
                if mask & libc::IN_MOVED_FROM != 0 {
                    self.pending_old = Some(self.record(wd, &name, REASON_RENAME_OLD));
                }
                if mask & libc::IN_MOVED_TO != 0 {
                    if mask & libc::IN_ISDIR != 0 {
                        // A directory moved into the tree brings its subtree
                        // with it, and the subtree has to be watched: the
                        // watches are on the paths, so a tree that was moved
                        // from elsewhere has none.
                        let dir =
                            path::join(&self.watches.get(&wd).cloned().unwrap_or_default(), &name);
                        let gone = lstat_mode(&dir).is_none();
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
            libc::close(self.fd);
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
