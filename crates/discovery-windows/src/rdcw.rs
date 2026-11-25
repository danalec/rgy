// SPDX-License-Identifier: Apache-2.0

//! The Windows live-change feed that needs no privileges:
//! `ReadDirectoryChangesW`.
//!
//! The USN journal is the better feed — it keeps a history, it names every
//! change on the volume, and a later run can resume where the last one
//! stopped — but it is also a privileged one: the volume handle is opened
//! with `GENERIC_READ`, and a process without the rights that needs is
//! refused with `ERROR_ACCESS_DENIED`. On a machine where the daemon runs
//! as an ordinary user the journal is there and cannot be read, and the
//! index is a frozen snapshot: a file created while the daemon runs is
//! invisible to it.
//!
//! `ReadDirectoryChangesW` is the fallback that needs no rights: a
//! directory handle is opened with `FILE_LIST_DIRECTORY`, which an ordinary
//! process has. What it does not have is a history — the feed starts when
//! it is armed and keeps nothing to replay — so the position it reports is
//! none, and a change that happens between the daemon's walk and the
//! arming is a change the feed never saw.
//!
//! The feed is **file-granular**, like the journal: a record names the file
//! that changed and carries no directory bit, so the live overlay re-lists
//! the folder that holds it. The name is the full path, in the platform's
//! spelling (`\` between the parts), which the overlay normalizes.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;
use core::mem::offset_of;

use crate::win::{
    self, FILE_FLAG_BACKUP_SEMANTICS, FILE_LIST_DIRECTORY, FILE_NOTIFY_CHANGE_CREATION,
    FILE_NOTIFY_CHANGE_DIR_NAME, FILE_NOTIFY_CHANGE_FILE_NAME, FILE_NOTIFY_CHANGE_LAST_WRITE,
    FILE_NOTIFY_CHANGE_SIZE, FILE_NOTIFY_INFORMATION, FILE_SHARE_DELETE, FILE_SHARE_READ,
    FILE_SHARE_WRITE, HANDLE, OPEN_EXISTING,
};
use rgy_discovery::{ChangeBatch, UsnRecord, Watch};

// The tests, and only the tests, need `std`: a tree to watch, a thread to
// watch it on, and a channel to answer over. The crate body is `core` plus
// `alloc`.
#[cfg(test)]
extern crate std;

/// The buffer the call fills: 64 KiB, the size the USN reader uses.
///
/// The call fails with `ERROR_INVALID_PARAMETER` when the buffer cannot
/// hold the changes that happened, so the buffer is generous; and it has to
/// be DWORD-aligned, because the records in it are DWORD-aligned
/// structures.
const BUFFER_WORDS: usize = 16 * 1024;

/// The room after the buffer the platform may fill.
///
/// The last record ends where the filled part ends, and the structure the
/// platform wrote is a whole `FILE_NOTIFY_INFORMATION` — four bytes longer
/// than its header and a one-character name. The slack is where those four
/// bytes come from: the record is read as the platform's structure, whole,
/// and a buffer without the room for it would be read past its end.
const SLACK_WORDS: usize = 4;

/// The bytes of the header of a `FILE_NOTIFY_INFORMATION`: the offset to
/// the next record, the action, and the length of the name. The name
/// follows the header, in the same buffer.
const HEADER_BYTES: usize = offset_of!(FILE_NOTIFY_INFORMATION, FileName);

/// What the feed asks for: the changes a name index cares about.
///
/// A name is a name, a size and a time, so the filter asks for the name,
/// the size and the write time — and for the creations that change which
/// paths exist.
const NOTIFY_FILTER: u32 = FILE_NOTIFY_CHANGE_FILE_NAME
    | FILE_NOTIFY_CHANGE_DIR_NAME
    | FILE_NOTIFY_CHANGE_SIZE
    | FILE_NOTIFY_CHANGE_LAST_WRITE
    | FILE_NOTIFY_CHANGE_CREATION;

/// `ERROR_INVALID_PARAMETER`. The call answers this when the buffer could
/// not hold the changes that happened.
const ERROR_INVALID_PARAMETER: u32 = 87;

/// A live-change feed over a directory tree, driven by
/// `ReadDirectoryChangesW`.
///
/// The watch owns the directory handle and closes it on drop. The feed
/// stops when the watch does, and a stopped feed is a daemon that stopped
/// noticing changes — which looks exactly like a disk that stopped
/// changing.
pub struct RdcwWatch {
    /// The directory handle the call reads from. Owned exclusively: the
    /// feed is the only holder, and the handle is closed on drop.
    handle: HANDLE,
    /// The root as the records name it — the tree's own spelling, with the
    /// platform's separator between the parts.
    root: String,
    /// The buffer the call fills.
    ///
    /// A `Vec<u32>` and not a `Vec<u8>`: the call wants a DWORD-aligned
    /// buffer, and the records in it are DWORD-aligned structures. A byte
    /// vector promises one byte of alignment and no more.
    buffer: Vec<u32>,
}

// The handle is owned exclusively by this struct and closed on drop, and
// the daemon moves the watch into the thread that runs the feed.
unsafe impl Send for RdcwWatch {}

impl RdcwWatch {
    /// Open the watch on the tree at `root`.
    ///
    /// `root` is the tree's root as the daemon spells it — the bytes the
    /// index was built from. The records name the paths under it in the
    /// same spelling, so the overlay can match them against the tree.
    ///
    /// The handle is opened on the long-path form of the root, because a
    /// deep tree is longer than `MAX_PATH` and the long form is the only
    /// spelling that says so.
    pub fn new(root: &[u8]) -> Result<RdcwWatch, String> {
        let text = rgy_discovery::path::win_text(root)
            .ok_or_else(|| "the root is not a path: it carries a NUL".to_string())?;
        let wide = handle_path(&text);

        let handle = win::create_file_w(
            wide.as_ptr(),
            // `FILE_LIST_DIRECTORY` and nothing else: the call reads
            // the directory's names, and an ordinary process has that
            // right. This is the whole difference from the volume
            // handle the journal needs, which wants `GENERIC_READ` and
            // is refused without the rights for it.
            FILE_LIST_DIRECTORY,
            // The whole world shares it: the daemon watches a tree other
            // programs are writing to, and a handle that locked it
            // would keep them out.
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            OPEN_EXISTING,
            // A directory handle needs the backup semantics: without
            // them the call opens the path as a file and fails. On a
            // volume device the same flag is what makes `\\.\X:`
            // resolve as a directory instead of the volume, which is
            // why the volume reader does not pass it.
            FILE_FLAG_BACKUP_SEMANTICS,
        )
        .map_err(|code| format!("cannot watch {text} (win32 error {code})"))?;

        Ok(RdcwWatch {
            handle,
            root: text,
            buffer: vec![0u32; BUFFER_WORDS + SLACK_WORDS],
        })
    }

    /// The records in the filled part of the buffer.
    fn records(&self, filled: usize) -> Vec<UsnRecord> {
        records_in(self.storage(), filled, &self.root)
    }

    /// The buffer as bytes.
    ///
    /// The call wants a DWORD-aligned buffer and the records in it are
    /// DWORD-aligned structures, so the buffer is a `Vec<u32>` and the
    /// bytes are a view of it.
    fn storage(&self) -> &[u8] {
        let bytes = self.buffer.len() * core::mem::size_of::<u32>();
        // SAFETY: the buffer is a live allocation of whole `u32`s, and the
        // bytes are read through a shared borrow of it.
        unsafe { core::slice::from_raw_parts(self.buffer.as_ptr().cast::<u8>(), bytes) }
    }

    /// The buffer as bytes, to write into.
    ///
    /// The same view as `storage`, borrowed mutably: the platform fills
    /// the buffer, and the fill is a write.
    fn storage_mut(&mut self) -> &mut [u8] {
        let bytes = self.buffer.len() * core::mem::size_of::<u32>();
        // SAFETY: the buffer is a live allocation of whole `u32`s, and the
        // bytes are written through a mutable borrow of it.
        unsafe { core::slice::from_raw_parts_mut(self.buffer.as_mut_ptr().cast::<u8>(), bytes) }
    }
}

impl Watch for RdcwWatch {
    /// The next batch of changes under the watched tree.
    ///
    /// The call blocks until a change arrives: the feed is a pull, and the
    /// daemon runs it on a thread of its own.
    fn next_batch(&mut self) -> Result<ChangeBatch, String> {
        let mut filled: u32 = 0;
        let ok = win::read_directory_changes_w(
            self.handle,
            self.storage_mut(),
            // The whole tree: a change in a subdirectory is reported
            // with its path relative to the root.
            true,
            NOTIFY_FILTER,
            // The call is synchronous, so the number of bytes it wrote
            // is the only way to know how much of the buffer holds
            // records.
            Some(&mut filled),
            None,
        );

        if let Err(code) = ok {
            // The buffer could not hold the changes that happened: the feed
            // lost history, and the only complete answer is a rebuild. A
            // partial batch would be a lie — the records that did fit are
            // not the records that happened.
            if code == ERROR_INVALID_PARAMETER {
                return Ok(ChangeBatch::Rescan);
            }
            return Err(format!("ReadDirectoryChangesW failed (win32 error {code})"));
        }

        Ok(ChangeBatch::Events(self.records(filled as usize)))
    }

    /// The feed keeps no history: there is no place to resume from, and
    /// nothing to replay.
    fn position(&self) -> Option<u64> {
        None
    }
}

impl Drop for RdcwWatch {
    fn drop(&mut self) {
        if !win::is_invalid(self.handle) {
            let _ = win::close_handle(self.handle);
        }
    }
}

/// The path the handle is opened on: the long-path form of the root.
///
/// The long form (`\\?\`) is what lets a path be longer than `MAX_PATH`,
/// and the root of a deep tree is. A root that already carries the prefix
/// keeps it: the prefix is not something to be added twice, and the seam's
/// own spelling knows that now — it hands a prefixed root back as it is.
fn handle_path(root: &str) -> Vec<u16> {
    rgy_discovery::path::win_wide(&rgy_discovery::path::win_verbatim_text(root))
}

/// The records in the filled part of the buffer.
///
/// The buffer holds a chain of `FILE_NOTIFY_INFORMATION` records: each one
/// is variable-length, and each one says where the next one starts. The
/// chain is walked with the offsets the platform wrote.
fn records_in(storage: &[u8], filled: usize, root: &str) -> Vec<UsnRecord> {
    let mut out = Vec::new();
    let mut offset = 0usize;
    while offset + HEADER_BYTES <= filled {
        // The structures are DWORD-aligned inside the buffer, and the
        // offsets the platform writes are multiples of four. A record that
        // says otherwise is not one this can read, and the chain stops
        // there.
        if !offset.is_multiple_of(4) {
            break;
        }
        // SAFETY: the record is the platform's own structure, it lies
        // inside the buffer — which has the slack its tail needs — and the
        // offset is DWORD-aligned, as the structure is.
        let info = unsafe { &*(storage.as_ptr().add(offset) as *const FILE_NOTIFY_INFORMATION) };
        let next = info.NextEntryOffset as usize;
        let name_bytes = info.FileNameLength as usize;
        let name_at = offset + HEADER_BYTES;
        // The name is the rest of the record, and it has to be inside the
        // part the platform filled.
        if name_at + name_bytes > filled {
            break;
        }
        let name = utf16_at(storage, name_at, name_bytes / 2);
        out.push(record(root, &name));
        if next == 0 {
            break;
        }
        offset += next;
    }
    out
}

/// The UTF-16 name at `at`, as text.
///
/// The name is UTF-16 and carries no terminator. The tree speaks UTF-8,
/// and the conversion is the boundary's job.
fn utf16_at(storage: &[u8], at: usize, units: usize) -> String {
    // SAFETY: the name lies inside the buffer, at a DWORD-aligned offset,
    // and the units are the ones the platform wrote.
    let wide =
        unsafe { core::slice::from_raw_parts(storage.as_ptr().add(at).cast::<u16>(), units) };
    String::from_utf16_lossy(wide)
}

/// The change record one notification means.
///
/// The feed is file-granular: the record names the file that changed and
/// carries no directory bit, so the live overlay re-lists the folder that
/// holds it. A notification about a folder names the folder, and the
/// overlay re-lists the folder's parent — which is where a new folder
/// shows up.
fn record(root: &str, name: &str) -> UsnRecord {
    UsnRecord {
        // The overlay keys by name, not by file reference: this feed has no
        // file references, and zero marks an overlay row.
        file_reference: 0,
        parent_reference: 0,
        // There is no USN here: this feed keeps no journal and no history.
        usn: 0,
        // The filter already asked only for the changes that matter, and
        // the overlay reads the path, not the reason.
        reason: 0,
        // The directory bit is clear: this feed names files.
        file_attributes: 0,
        name: full_path(root, name),
    }
}

/// The full path of a changed file: the root, then the path the
/// notification carried.
///
/// The notification's path is relative to the watched folder and uses the
/// platform's separator; the root is the tree's own spelling. The overlay
/// normalizes the two into one.
fn full_path(root: &str, relative: &str) -> String {
    let mut out = String::with_capacity(root.len() + 1 + relative.len());
    out.push_str(root);
    if !root.is_empty() && !root.ends_with('\\') {
        out.push('\\');
    }
    out.push_str(relative);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::win::FILE_ACTION_ADDED;
    use rgy_discovery::FILE_ATTRIBUTE_DIRECTORY;
    use std::sync::mpsc;
    use std::time::Duration;

    /// A tree in the temporary directory that takes itself away.
    ///
    /// A test that leaves a folder behind is a test that fills the
    /// temporary directory, and that directory is the one place every test
    /// on the machine shares. The tree is removed when it is dropped, so a
    /// test that panics takes its folder with it.
    struct TempTree(std::path::PathBuf);

    impl TempTree {
        /// A tree of its own, named after what it is for.
        fn new(tag: &str) -> TempTree {
            let dir = std::env::temp_dir().join(format!(
                "{tag}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            std::fs::create_dir_all(&dir).unwrap();
            TempTree(dir)
        }

        /// The root as the tree speaks it: the bytes of the path.
        fn root(&self) -> Vec<u8> {
            self.0.as_os_str().as_encoded_bytes().to_vec()
        }

        /// The root as the records name it.
        fn text(&self) -> String {
            rgy_discovery::path::win_text(&self.root()).expect("a path")
        }
    }

    impl std::ops::Deref for TempTree {
        type Target = std::path::Path;

        fn deref(&self) -> &std::path::Path {
            &self.0
        }
    }

    impl Drop for TempTree {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// The batch that carries the change the test is waiting for, with the
    /// feed running on a thread of its own.
    ///
    /// The call blocks until a change arrives, so the watch has to run
    /// somewhere else and the test has to make the change from here.
    ///
    /// The watch is armed inside the call that waits for a change, so a
    /// change made before the arming is not one it sees. The test
    /// therefore keeps touching the file until the feed answers: which of
    /// the touches was the one that was reported does not matter, and a
    /// test that waited for the *first* change would be a test that waited
    /// for a race.
    ///
    /// **The first batch is not the batch the test is about.** The feed
    /// can report the watched folder's own change before the file's — the
    /// folder's timestamp moved when the file was created inside it — and
    /// the file's own notification then comes in a later batch. (Measured
    /// on the CI runner: the first batch named the folder, not the file.)
    /// So the batch is looked for in what the feed delivers, and the feed
    /// is kept fed until it is found: `wanted` says which batch the test
    /// is about.
    fn wait_for(
        mut watch: RdcwWatch,
        touch: impl Fn() + Send + 'static,
        wanted: impl Fn(&[UsnRecord]) -> bool,
    ) -> Vec<UsnRecord> {
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            // The feed keeps delivering: the test reads the batches one
            // by one, and the thread stops when the test stops reading.
            loop {
                let batch = watch.next_batch();
                let failed = batch.is_err();
                if tx.send(batch).is_err() || failed {
                    break;
                }
            }
        });
        for _ in 0..50 {
            touch();
            match rx.recv_timeout(Duration::from_millis(200)) {
                Ok(Ok(ChangeBatch::Events(records))) => {
                    if wanted(&records) {
                        return records;
                    }
                }
                Ok(Ok(ChangeBatch::Rescan)) => {
                    panic!("the feed lost its history on a tree that did not overflow")
                }
                Ok(Err(e)) => panic!("the watch failed: {e}"),
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => panic!("the watch thread died"),
            }
        }
        panic!("the watch delivered no change the test asked for in ten seconds");
    }

    /// A file created under the watched root is reported by its full path.
    #[test]
    fn a_created_file_is_reported_by_its_full_path() {
        let tree = TempTree::new("rgy-rdcw-file");
        let root = tree.root();
        let text = tree.text();
        let file = tree.join("alpha.txt");

        let watch = RdcwWatch::new(&root).expect("the watch opens");
        let wanted = format!("{text}\\alpha.txt");
        let named = wanted.clone();
        let records = wait_for(
            watch,
            move || {
                let _ = std::fs::write(&file, b"alpha");
            },
            move |records| records.iter().any(|r| r.name == named),
        );

        let names: Vec<&str> = records.iter().map(|r| r.name.as_str()).collect();
        assert!(
            names.contains(&wanted.as_str()),
            "the batch names the file that changed: {names:?}"
        );
    }

    /// The feed is file-granular: the record names the file and carries no
    /// directory bit, so the overlay re-lists the folder that holds it
    /// instead of the file itself.
    #[test]
    fn a_record_is_file_granular() {
        let tree = TempTree::new("rgy-rdcw-grain");
        let root = tree.root();
        let text = tree.text();
        let file = tree.join("alpha.txt");

        let watch = RdcwWatch::new(&root).expect("the watch opens");
        let records = wait_for(
            watch,
            move || {
                let _ = std::fs::write(&file, b"alpha");
            },
            |records| !records.is_empty(),
        );

        assert!(!records.is_empty(), "the feed said something");
        for r in &records {
            assert_eq!(
                r.file_attributes & FILE_ATTRIBUTE_DIRECTORY,
                0,
                "a file-granular feed must not carry the directory bit: {}",
                r.name
            );
            assert!(
                r.name.starts_with(&text),
                "the name is the full path under the root: {}",
                r.name
            );
            // The feed has no USN and no file references: the overlay keys
            // by name.
            assert_eq!(r.file_reference, 0);
            assert_eq!(r.parent_reference, 0);
            assert_eq!(r.usn, 0);
        }
    }

    /// A change in a subdirectory is reported with its path relative to
    /// the root: the watch is on the whole tree.
    ///
    /// The folder's own change can be the feed's first word about the
    /// tree — and then the file's notification is in a later batch. The
    /// test therefore waits for the batch that names the file: what it
    /// proves is that the change in the subdirectory is reported
    /// *relative to the root*, and that is what the batch that names the
    /// file says.
    #[test]
    fn a_change_in_a_subdirectory_is_reported_relative_to_the_root() {
        let tree = TempTree::new("rgy-rdcw-sub");
        std::fs::create_dir_all(tree.join("docs")).unwrap();
        let root = tree.root();
        let text = tree.text();
        let file = tree.join("docs/deep.txt");

        let watch = RdcwWatch::new(&root).expect("the watch opens");
        let wanted = format!("{text}\\docs\\deep.txt");
        let named = wanted.clone();
        let records = wait_for(
            watch,
            move || {
                let _ = std::fs::write(&file, b"deep");
            },
            move |records| records.iter().any(|r| r.name == named),
        );

        let names: Vec<&str> = records.iter().map(|r| r.name.as_str()).collect();
        assert!(
            names.contains(&wanted.as_str()),
            "the batch names the file under the subdirectory: {names:?}"
        );
    }

    /// The feed keeps no history: there is no position to resume from, and
    /// nothing to replay.
    #[test]
    fn the_feed_has_no_position_to_resume_from() {
        let tree = TempTree::new("rgy-rdcw-pos");
        let watch = RdcwWatch::new(&tree.root()).expect("the watch opens");
        assert_eq!(watch.position(), None);
    }

    /// One `FILE_NOTIFY_INFORMATION` record, as the platform writes it.
    ///
    /// The header is written through the platform's own structure, so the
    /// record is laid out the way the platform lays it out — and the name
    /// is written through that structure's own field.
    fn notify_record(name: &str, next_offset: u32) -> Vec<u32> {
        let wide: Vec<u16> = name.encode_utf16().collect();
        let mut record = vec![0u32; 3 + wide.len().div_ceil(2)];
        unsafe {
            let info = record.as_mut_ptr() as *mut FILE_NOTIFY_INFORMATION;
            (*info).NextEntryOffset = next_offset;
            (*info).Action = FILE_ACTION_ADDED;
            (*info).FileNameLength = (wide.len() * 2) as u32;
            let units = core::ptr::addr_of_mut!((*info).FileName).cast::<u16>();
            for (i, unit) in wide.iter().enumerate() {
                units.add(i).write(*unit);
            }
        }
        record
    }

    /// The buffer holds a chain of variable-length records, and the chain
    /// is walked with the offsets the platform wrote.
    #[test]
    fn a_chain_of_records_is_walked() {
        let mut first = notify_record("alpha.txt", 0);
        let second = notify_record("docs\\deep.txt", 0);
        let first_bytes = first.len() * 4;
        // The first record points at the second.
        unsafe {
            let info = first.as_mut_ptr() as *mut FILE_NOTIFY_INFORMATION;
            (*info).NextEntryOffset = first_bytes as u32;
        }

        let mut buffer: Vec<u8> = first.iter().flat_map(|w| w.to_le_bytes()).collect();
        buffer.extend(second.iter().flat_map(|w| w.to_le_bytes()));
        let filled = buffer.len();
        // The room after the filled part that the reader needs for the
        // platform's structure.
        buffer.extend(std::iter::repeat_n(0u8, 16));

        let records = records_in(&buffer, filled, r"C:\root");
        assert_eq!(records.len(), 2, "both records are read: {records:?}");
        assert_eq!(records[0].name, r"C:\root\alpha.txt");
        assert_eq!(records[1].name, r"C:\root\docs\deep.txt");
        // File-granular: the directory bit is clear.
        assert_eq!(records[0].file_attributes & FILE_ATTRIBUTE_DIRECTORY, 0);
        assert_eq!(records[1].file_attributes & FILE_ATTRIBUTE_DIRECTORY, 0);
    }

    /// A record that runs past the part the platform filled is not read:
    /// the chain stops where the data stops.
    #[test]
    fn a_record_past_the_filled_part_is_not_read() {
        let record = notify_record("alpha.txt", 0);
        let mut buffer: Vec<u8> = record.iter().flat_map(|w| w.to_le_bytes()).collect();
        buffer.extend(std::iter::repeat_n(0u8, 16));

        // The name is not inside the filled part.
        let records = records_in(&buffer, 12, r"C:\root");
        assert!(records.is_empty(), "no record is read: {records:?}");
    }
}
