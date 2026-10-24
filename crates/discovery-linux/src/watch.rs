// SPDX-License-Identifier: Apache-2.0

//! Recursive inotify `Watch`: every directory under the root gets a watch;
//! an `IN_Q_OVERFLOW` (or watch-limit hit) reports [`ChangeBatch::Rescan`],
//! and anything else is translated into `UsnRecord`-shaped
//! [`rgy_discovery::UsnRecord`]s the name layer already knows (create/delete/
//! rename reason bits) — one change vocabulary across backends.

use std::ffi::CString;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::PathBuf;

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
    watches: std::collections::HashMap<i32, PathBuf>,
    /// Pending rename_old record awaiting its rename_new partner.
    pending_old: Option<UsnRecord>,
}

impl InotifyWatch {
    /// Start watching `root` recursively. Directory watches are added on
    /// the fly for created subdirectories; a create event for a dir is
    /// enough to extend coverage.
    pub fn new(root: &PathBuf) -> Result<InotifyWatch, String> {
        let fd = unsafe { libc::inotify_init1(libc::IN_NONBLOCK | libc::IN_CLOEXEC) };
        if fd < 0 {
            return Err("inotify_init failed".into());
        }
        let mut w = InotifyWatch {
            fd,
            watches: std::collections::HashMap::new(),
            pending_old: None,
        };
        w.add_tree(root)
            .map_err(|e| format!("inotify_add_watch failed: {e}"))?;
        Ok(w)
    }

    fn add_tree(&mut self, dir: &PathBuf) -> Result<(), i32> {
        self.add_one(dir)?;
        let Ok(rd) = std::fs::read_dir(dir) else {
            return Ok(());
        };
        for e in rd.flatten() {
            let p = e.path();
            if p.symlink_metadata().map(|m| m.is_dir()).unwrap_or(false) {
                self.add_tree(&p)?;
            }
        }
        Ok(())
    }

    fn add_one(&mut self, dir: &PathBuf) -> Result<(), i32> {
        let c = CString::new(dir.as_os_str().as_bytes()).map_err(|_| -1)?;
        let wd = unsafe {
            libc::inotify_add_watch(
                self.fd,
                c.as_ptr(),
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
        self.watches.insert(wd, dir.clone());
        Ok(())
    }

    fn record(&self, wd: i32, name: &[u8], reason: u32) -> UsnRecord {
        let dir = self.watches.get(&wd).cloned().unwrap_or_default();
        let path = if name.is_empty() {
            dir
        } else {
            dir.join(std::ffi::OsString::from_vec(name.to_vec()))
        };
        UsnRecord {
            // The index keys by name rows, not FRNs, in the live overlay;
            // the path is what `Live` consumes. FRN 0 marks "overlay row".
            file_reference: 0,
            parent_reference: 0,
            usn: 0,
            reason,
            file_attributes: 0,
            name: path.to_string_lossy().into_owned(),
        }
    }
}

impl Watch for InotifyWatch {
    fn next_batch(&mut self) -> Result<ChangeBatch, String> {
        let mut buf = vec![0u8; 64 * 1024];
        let mut events: Vec<UsnRecord> = Vec::new();
        loop {
            let n = unsafe { libc::read(self.fd, buf.as_mut_ptr().cast(), buf.len()) };
            if n < 0 {
                let err = std::io::Error::last_os_error();
                if err.kind() == std::io::ErrorKind::WouldBlock {
                    break;
                }
                return Err(format!("inotify read: {err}"));
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
                        let dir = self
                            .watches
                            .get(&wd)
                            .cloned()
                            .unwrap_or_default()
                            .join(std::ffi::OsString::from_vec(name.clone()));
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
                        let gone = dir.symlink_metadata().is_err();
                        if !gone && self.add_one(&dir).is_err() {
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
