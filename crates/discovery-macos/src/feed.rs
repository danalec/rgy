// SPDX-License-Identifier: Apache-2.0

//! The macOS live-change feed: FSEvents pushes, the daemon pulls.
//!
//! FSEvents is a callback: the stream calls back with a batch of events and
//! the caller reads the batches off a channel. The daemon speaks the other
//! dialect — [`rgy_discovery::Watch::next_batch`] is a pull — so this module is
//! the adapter between the two, and it is where the two decisions live:
//!
//! - **the granularity.** FSEvents reports the *folder* whose contents
//!   changed, not the file that changed. The live model re-lists the parent
//!   of a file-granular record, so a folder event has to say "this is a
//!   folder": [`rgy_discovery::FILE_ATTRIBUTE_DIRECTORY`] is the one bit that
//!   says it, and the overlay then re-lists the named folder. Re-listing the
//!   folder's *parent* instead would see only the folder's own mtime and
//!   miss the edit inside.
//! - **the root.** The stream watches the whole disk, so most events belong
//!   to another tree; re-listing one of those would put entries into the
//!   index that the indexed tree never had.
//!
//! The adapter owns the stream. Dropping the stream stops the feed, and a
//! stopped feed is a daemon that stopped noticing changes — which looks
//! exactly like a disk that stopped changing.
//!
//! The channel is the tree's own (`rgy_par::mpsc`), so the feed is the same
//! with and without `std`: with it the reader that finds the queue empty
//! parks, without it the reader spins.

use alloc::boxed::Box;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
// The `vec!` macro, for the path buffers below. They are macOS's, so the
// import is macOS's too.
#[cfg(target_os = "macos")]
use alloc::vec;

use rgy_par::mpsc::Receiver;

use rgy_discovery::{ChangeBatch, FILE_ATTRIBUTE_DIRECTORY, UsnRecord, Watch};

use crate::event::{Event, ITEM_IS_DIR, KERNEL_DROPPED, MUST_SCAN_SUBDIRS, USER_DROPPED};

/// A handle the feed must keep alive: dropping it stops the feed.
///
/// The macOS implementation is the FSEvents stream. The trait keeps the
/// adapter's shape independent of the platform, and lets a test stand in a
/// holder whose drop is observable.
pub trait KeepAlive: Send {}

/// What the stream reports.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Grain {
    /// The stream names the folders whose contents changed — what FSEvents
    /// does without `kFSEventStreamCreateFlagFileEvents`, the flag the
    /// daemon's stream does not set.
    Folder,
    /// The stream names the items that changed, folders included; the event
    /// then says which it is.
    Item,
}

/// The macOS live-change feed, pulled.
pub struct FseventsWatch {
    /// The batches the FSEvents callback pushes.
    rx: Receiver<Vec<Event>>,
    /// The FSEvents stream. Owned and never handed out: the stream stops
    /// when it is dropped, so the feed lives exactly as long as this does.
    _stream: Box<dyn KeepAlive>,
    /// The indexed root, absolute: events outside it are another tree's.
    root: Vec<u8>,
    /// The newest event id delivered: the feed's place in the change
    /// history.
    position: u64,
}

impl FseventsWatch {
    /// The feed over a new FSEvents stream, for the tree at `root`.
    ///
    /// The stream pushes its batches into a channel and the adapter pulls
    /// them from the other end; the adapter holds the stream, so the feed
    /// lives exactly as long as it does.
    ///
    /// `since` is the event id to start from: the framework delivers the
    /// events after it. `latency` is how long the framework may wait to
    /// coalesce events into one batch.
    #[cfg(target_os = "macos")]
    pub fn open(root: &[u8], since: u64, latency: f64) -> Self {
        let (tx, rx) = rgy_par::mpsc::channel();
        let stream = crate::fsevents::watch(since, latency, tx, 0);
        Self::new(rx, Box::new(stream), root)
    }

    /// The feed over `rx`, keeping `stream` alive, for the tree at `root`.
    ///
    /// The root is resolved to an absolute path: the stream reports
    /// absolute paths, and the daemon's root may be relative (it defaults
    /// to the working directory) or reach the tree through a symlink.
    pub fn new(rx: Receiver<Vec<Event>>, stream: Box<dyn KeepAlive>, root: &[u8]) -> Self {
        Self::with_prefix(rx, stream, &absolute_root(root))
    }

    /// The feed over `rx`, keeping `stream` alive, for the tree at `prefix`.
    ///
    /// `prefix` is the indexed root as an absolute path.
    pub fn with_prefix(
        rx: Receiver<Vec<Event>>,
        stream: Box<dyn KeepAlive>,
        prefix: &[u8],
    ) -> Self {
        Self {
            rx,
            _stream: stream,
            root: prefix.to_vec(),
            position: 0,
        }
    }
}

impl Watch for FseventsWatch {
    fn next_batch(&mut self) -> Result<ChangeBatch, String> {
        let events = self
            .rx
            .recv()
            .map_err(|_| "the fsevents stream stopped".to_string())?;
        // The feed says it lost history: what is in this batch is not the
        // whole truth, and the only complete answer is a rebuild.
        if events.iter().any(|e| lost_history(e.flags)) {
            return Ok(ChangeBatch::Rescan);
        }
        self.position = events.iter().map(|e| e.id).max().unwrap_or(self.position);
        Ok(ChangeBatch::Events(records_for(
            &events,
            &self.root,
            Grain::Folder,
        )))
    }

    /// The newest event id delivered: FSEvents keeps a history, and the id
    /// is where in it the feed stands.
    fn position(&self) -> Option<u64> {
        Some(self.position)
    }
}

/// The change records a batch of events means.
///
/// A folder event re-lists the folder itself — the directory bit says so —
/// while an item event re-lists the folder that holds the item, which is
/// what the live overlay does with a record that carries no directory bit.
///
/// Events outside the indexed root are dropped: they are another tree's.
pub fn records_for(events: &[Event], root: &[u8], grain: Grain) -> Vec<UsnRecord> {
    events
        .iter()
        .filter(|e| is_under(&e.path, root))
        .map(|e| record_for(e, grain))
        .collect()
}

/// Is the path inside the indexed root?
///
/// The stream watches the whole disk, so most events are another tree's:
/// re-listing one of those would put entries into the index that the
/// indexed tree never had.
///
/// The comparison is on separators: the feed's paths and the daemon's root
/// are two spellings of the same tree, and a prefix test has to compare
/// them the same way.
pub fn is_under(path: &[u8], root: &[u8]) -> bool {
    let path = slashes(path);
    let root = slashes(root);
    let root = root.strip_suffix(b"/").unwrap_or(&root);
    if root.is_empty() {
        // The whole disk.
        return true;
    }
    path == root || path.starts_with(&[root, b"/"].concat())
}

/// The path with `/` as the separator.
fn slashes(path: &[u8]) -> Vec<u8> {
    path.iter()
        .map(|&b| if b == b'\\' { b'/' } else { b })
        .collect()
}

/// Is the path this event names a folder?
///
/// A folder-granular stream only ever names folders. An item-granular one
/// says so per event: [`ITEM_IS_DIR`] is set on the events whose item is a
/// directory.
pub fn is_folder(ev: &Event, grain: Grain) -> bool {
    match grain {
        Grain::Folder => true,
        Grain::Item => ev.flags & ITEM_IS_DIR != 0,
    }
}

/// Did the feed lose history?
///
/// [`KERNEL_DROPPED`] and [`USER_DROPPED`] say the batch is missing events.
/// [`MUST_SCAN_SUBDIRS`] says a folder's subtree changed in a way the
/// framework coalesced: the events for that subtree are not there, and the
/// live model cannot say "re-list this subtree" — it re-lists one folder at
/// a time — so the honest answer is a rebuild.
fn lost_history(flags: u32) -> bool {
    flags & (KERNEL_DROPPED | USER_DROPPED | MUST_SCAN_SUBDIRS) != 0
}

/// The change record one event means.
fn record_for(ev: &Event, grain: Grain) -> UsnRecord {
    UsnRecord {
        // The live overlay keys by name, not by file reference: the path is
        // what it consumes, and zero marks an overlay row (as the inotify
        // feed does).
        file_reference: 0,
        parent_reference: 0,
        usn: 0,
        // FSEvents says that a path changed, not why; the live overlay
        // filters on the path, not on the reason.
        reason: 0,
        file_attributes: if is_folder(ev, grain) {
            FILE_ATTRIBUTE_DIRECTORY
        } else {
            0
        },
        name: String::from_utf8_lossy(&ev.path).into_owned(),
    }
}

/// The indexed root as an absolute path.
///
/// The stream reports absolute paths, so a relative root (the daemon's
/// default is the working directory) has to be resolved against it. A root
/// that reaches the tree through a symlink is resolved too: the kernel
/// reports the resolved path, and an unresolved root would never match the
/// events under it.
///
/// The two calls behind it are `getcwd(3)` and the path resolution: the
/// ones `std::path::absolute` and `std::fs::canonicalize` are made of, and
/// the crate has no `std` to make them with. The resolution is the seam's
/// (`rgy_discovery::file::canonicalize`); the working directory is here.
#[cfg(target_os = "macos")]
fn absolute_root(root: &[u8]) -> Vec<u8> {
    // The absolute path: a relative one is resolved against the working
    // directory.
    let absolute = if root.starts_with(b"/") {
        root.to_vec()
    } else {
        match working_directory() {
            Some(cwd) => rgy_discovery::path::join(&cwd, root),
            None => root.to_vec(),
        }
    };
    // The resolved path: the symlinks followed. A path that does not
    // resolve is kept as it is — the absolute path is still the tree.
    match rgy_discovery::file::canonicalize(&absolute) {
        Some(p) => p,
        None => absolute,
    }
}

/// The indexed root as an absolute path.
///
/// Off macOS there is no FSEvents stream, and no absolute path for the
/// events to be under: the root is taken as it comes.
#[cfg(not(target_os = "macos"))]
fn absolute_root(root: &[u8]) -> Vec<u8> {
    root.to_vec()
}

/// The buffer the two path calls use.
///
/// PATH_MAX is 1024 on macOS, and the Darwin extension of `realpath`
/// answers paths longer than that, so the buffer is wider than either.
#[cfg(target_os = "macos")]
const PATH_BUF: usize = 64 * 1024;

// The call the root resolution is made of. `getcwd(3)` is the working
// directory; the path with its symlinks followed is the seam's
// (`rgy_discovery::file::canonicalize`), because that is where the
// resolution of a path lives. The crate declares `getcwd` itself because
// it has no `std::env` to reach for.
#[cfg(target_os = "macos")]
unsafe extern "C" {
    fn getcwd(buf: *mut core::ffi::c_char, size: usize) -> *mut core::ffi::c_char;
}

/// The working directory, as bytes.
#[cfg(target_os = "macos")]
fn working_directory() -> Option<Vec<u8>> {
    let mut buf = vec![0u8; PATH_BUF];
    let p = unsafe { getcwd(buf.as_mut_ptr().cast(), buf.len()) };
    if p.is_null() {
        return None;
    }
    let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    buf.truncate(end);
    Some(buf)
}

#[cfg(test)]
mod tests {

    /// A tree in the temporary directory that takes itself away.
    ///
    /// A test that leaves a folder behind is a test that fills the
    /// temporary directory, and that directory is the one place every
    /// test on the machine shares. The tree is removed when it is
    /// dropped, so a test that panics takes its folder with it.
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
    use super::*;
    use alloc::sync::Arc;
    use core::sync::atomic::{AtomicBool, Ordering};
    use rgy_discovery::Source;

    /// A stand-in for the FSEvents stream: its drop is observable, which is
    /// the property the test is about.
    struct FakeStream(Arc<AtomicBool>);

    impl KeepAlive for FakeStream {}

    impl Drop for FakeStream {
        fn drop(&mut self) {
            self.0.store(false, Ordering::SeqCst);
        }
    }

    fn event(path: &str, flags: u32) -> Event {
        Event {
            path: path.as_bytes().to_vec(),
            flags,
            id: 1,
            stream: 0,
        }
    }

    /// A stream holder that reports whether it is still there.
    fn fake() -> (Arc<AtomicBool>, Box<dyn KeepAlive>) {
        let alive = Arc::new(AtomicBool::new(true));
        (alive.clone(), Box::new(FakeStream(alive)))
    }

    // --- the granularity ----------------------------------------------------

    /// A folder event names the folder whose contents changed, so the
    /// folder is re-listed. The directory bit is what says so: without it
    /// the overlay would re-list the folder's *parent*, which sees only the
    /// folder's own mtime and misses the edit inside.
    #[test]
    fn a_folder_event_becomes_a_folder_relist() {
        let records = records_for(&[event("/w/docs", 0)], b"/w", Grain::Folder);
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].name, "/w/docs");
        assert_ne!(
            records[0].file_attributes & FILE_ATTRIBUTE_DIRECTORY,
            0,
            "a folder event must carry the directory bit"
        );
    }

    /// An item event names the item, so the folder that holds it is
    /// re-listed: no directory bit, and the overlay takes the parent.
    #[test]
    fn a_file_event_becomes_a_parent_relist() {
        let records = records_for(&[event("/w/docs/a.txt", 0)], b"/w", Grain::Item);
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].name, "/w/docs/a.txt");
        assert_eq!(
            records[0].file_attributes & FILE_ATTRIBUTE_DIRECTORY,
            0,
            "an item event must not carry the directory bit"
        );
    }

    /// An item-granular stream says per event whether the item is a folder.
    #[test]
    fn an_item_event_says_itself_whether_it_is_a_folder() {
        let dir = event("/w/docs", ITEM_IS_DIR);
        let file = event("/w/docs/a.txt", 0);
        assert!(is_folder(&dir, Grain::Item));
        assert!(!is_folder(&file, Grain::Item));
        // A folder-granular stream only ever names folders.
        assert!(is_folder(&file, Grain::Folder));
    }

    // --- the root -----------------------------------------------------------

    /// The stream watches the whole disk. An event outside the indexed tree
    /// belongs to another index, and re-listing it would put entries into
    /// this one that it never had.
    #[test]
    fn events_outside_the_indexed_root_are_dropped() {
        let events = [
            event("/w/docs", 0),
            event("/other/place", 0),
            event("/w", 0),
        ];
        let records = records_for(&events, b"/w", Grain::Folder);
        let names: Vec<&str> = records.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, ["/w/docs", "/w"]);
    }

    /// A root is a path, not a prefix: `/w/docs` is not inside `/w/doc`.
    #[test]
    fn the_root_is_matched_as_a_path_and_not_as_a_prefix() {
        assert!(is_under(b"/w/doc/a", b"/w/doc"));
        assert!(!is_under(b"/w/docs/a", b"/w/doc"));
        assert!(is_under(b"/w/doc", b"/w/doc"));
        assert!(!is_under(b"/w/docs", b"/w/doc"));
        // The whole disk holds everything.
        assert!(is_under(b"/anything", b"/"));
        assert!(is_under(b"/anything", b""));
    }

    // --- the pull -----------------------------------------------------------

    /// The callback pushes, the daemon pulls: one batch in, one batch out.
    #[test]
    fn a_batch_becomes_one_change_batch() {
        let (tx, rx) = rgy_par::mpsc::channel();
        let (_alive, stream) = fake();
        let mut watch = FseventsWatch::with_prefix(rx, stream, b"/w");
        tx.send(vec![event("/w/docs", 0)]).unwrap();
        match watch.next_batch().expect("a batch") {
            ChangeBatch::Events(records) => {
                assert_eq!(records.len(), 1);
                assert_eq!(records[0].name, "/w/docs");
            }
            ChangeBatch::Rescan => panic!("a batch with no lost history is not a rescan"),
        }
    }

    /// A feed that lost history says so, and the caller rebuilds: a batch
    /// that is missing events is not a batch of changes.
    #[test]
    fn a_feed_that_lost_history_asks_for_a_rebuild() {
        let (tx, rx) = rgy_par::mpsc::channel();
        let (_alive, stream) = fake();
        let mut watch = FseventsWatch::with_prefix(rx, stream, b"/w");
        tx.send(vec![event("/w/docs", KERNEL_DROPPED)]).unwrap();
        assert!(matches!(
            watch.next_batch().expect("a batch"),
            ChangeBatch::Rescan
        ));

        tx.send(vec![event("/w/docs", MUST_SCAN_SUBDIRS)]).unwrap();
        assert!(matches!(
            watch.next_batch().expect("a batch"),
            ChangeBatch::Rescan
        ));
    }

    /// The feed's position is where it stands in the change history.
    #[test]
    fn the_position_is_the_newest_event_id_delivered() {
        let (tx, rx) = rgy_par::mpsc::channel();
        let (_alive, stream) = fake();
        let mut watch = FseventsWatch::with_prefix(rx, stream, b"/w");
        assert_eq!(watch.position(), Some(0));
        tx.send(vec![
            Event {
                path: b"/w/docs".to_vec(),
                flags: 0,
                id: 41,
                stream: 0,
            },
            Event {
                path: b"/w/other".to_vec(),
                flags: 0,
                id: 42,
                stream: 0,
            },
        ])
        .unwrap();
        let _ = watch.next_batch().expect("a batch");
        assert_eq!(watch.position(), Some(42));
    }

    /// The stream is what keeps the feed running: dropping it stops the
    /// feed, so the adapter has to hold it for as long as it lives. A
    /// dropped stream is the failure that looks like a disk that stopped
    /// changing.
    #[test]
    fn the_stream_is_kept_alive_for_as_long_as_the_adapter() {
        let (tx, rx) = rgy_par::mpsc::channel();
        let (alive, stream) = fake();
        let mut watch = FseventsWatch::with_prefix(rx, stream, b"/w");
        // The feed works while the adapter holds the stream.
        tx.send(vec![event("/w/docs", 0)]).unwrap();
        assert!(matches!(
            watch.next_batch().expect("a batch"),
            ChangeBatch::Events(_)
        ));
        assert!(alive.load(Ordering::SeqCst), "the stream is still there");
        // And the stream goes with the adapter, and not before.
        drop(watch);
        assert!(
            !alive.load(Ordering::SeqCst),
            "dropping the adapter must drop the stream it kept alive"
        );
    }

    // --- the events reach the index -----------------------------------------

    /// A temp tree to index, which takes itself away when it is dropped.
    ///
    /// A test that leaves a folder behind is a test that fills the
    /// temporary directory, and that directory is the one place every
    /// test on the machine shares.
    fn fixture_dir(tag: &str) -> TempTree {
        TempTree::new(&format!("rgy-macos-feed-{tag}"))
    }

    /// A path as the feed spells it: `/` separated, the way macOS spells
    /// every path, and the way the live overlay stores them.
    fn feed_path(path: &std::path::Path) -> Vec<u8> {
        String::from_utf8_lossy(path.as_os_str().as_encoded_bytes())
            .replace('\\', "/")
            .into_bytes()
    }

    /// The whole point of the adapter: a file that appears after the index
    /// was built has to be findable, and the only thing that puts it there
    /// is the folder being re-listed.
    ///
    /// A folder event names the folder whose contents changed, and the live
    /// overlay re-lists that folder — the file inside it is found.
    #[test]
    fn a_folder_event_puts_a_new_file_into_the_index() {
        let dir = fixture_dir("folder");
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(dir.join("sub/keep.txt"), b"keep\n").unwrap();
        let root = feed_path(&dir);

        // The index as it was before the file appeared.
        let src = rgy_discovery::PortableSource::new(root.clone(), 2);
        let mut live = rgy_name_core::live::Live::with_root(
            rgy_name_core::name_index::Index::build(src.enumerate().expect("the walk"), 0, 1, b"/"),
            &root,
        );

        // A file appears inside the folder.
        std::fs::write(dir.join("sub/new.txt"), b"new\n").unwrap();

        // FSEvents says: the folder whose contents changed.
        let folder = feed_path(&dir.join("sub"));
        let records = records_for(
            &[event(&String::from_utf8_lossy(&folder), 0)],
            &root,
            Grain::Folder,
        );
        live.apply_events(&records);

        // And the index knows about it.
        let wanted = feed_path(&dir.join("sub/new.txt"));
        let hits = rgy_name_core::name_query::Searcher { live: &live }
            .search(&rgy_name_core::name_query::Query::parse("new", "").expect("a query"));
        assert!(
            hits.iter()
                .any(|h| h.over.as_deref() == Some(wanted.as_slice())),
            "the new file must be in the index; the overlay holds {:?}",
            live.over.keys().collect::<Vec<_>>()
        );
    }

    /// The other granularity: an item event names the item, and the folder
    /// that holds it is re-listed — the file is found the same way.
    #[test]
    fn a_file_event_puts_a_new_file_into_the_index() {
        let dir = fixture_dir("item");
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(dir.join("sub/keep.txt"), b"keep\n").unwrap();
        let root = feed_path(&dir);

        let src = rgy_discovery::PortableSource::new(root.clone(), 2);
        let mut live = rgy_name_core::live::Live::with_root(
            rgy_name_core::name_index::Index::build(src.enumerate().expect("the walk"), 0, 1, b"/"),
            &root,
        );

        std::fs::write(dir.join("sub/new.txt"), b"new\n").unwrap();

        // An item-granular feed names the file itself.
        let item = feed_path(&dir.join("sub/new.txt"));
        let records = records_for(
            &[event(&String::from_utf8_lossy(&item), 0)],
            &root,
            Grain::Item,
        );
        live.apply_events(&records);

        let wanted = feed_path(&dir.join("sub/new.txt"));
        let hits = rgy_name_core::name_query::Searcher { live: &live }
            .search(&rgy_name_core::name_query::Query::parse("new", "").expect("a query"));
        assert!(
            hits.iter()
                .any(|h| h.over.as_deref() == Some(wanted.as_slice())),
            "the new file must be in the index; the overlay holds {:?}",
            live.over.keys().collect::<Vec<_>>()
        );
    }
}
