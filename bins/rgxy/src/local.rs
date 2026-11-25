// SPDX-License-Identifier: Apache-2.0

//! The names, searched in this process.
//!
//! The launcher is a client, and a client with no daemon at all has nothing
//! to ask. This is the stand-in for the **name layer only**: one in-memory
//! index of one folder, searched with the same `Alternatives` / `Searcher`
//! the daemon and `searchctl` use, so the ranking is the same ranking.
//!
//! # Where
//!
//! The user's home — `USERPROFILE` first on Windows, `HOME` first elsewhere —
//! and the current folder only when there is no home to be had. The folder is
//! never a filesystem root: a launcher that scans `/` by accident is a
//! launcher nobody starts twice. The scope is named to the user, because a
//! search over the wrong tree answers the wrong question without saying so.
//!
//! # How
//!
//! The index is opened **once** per source — by the background [`Prewarm`]
//! task when the window's worker started one, otherwise on the first real
//! question — in one of two ways:
//!
//! 1. the per-root cache (`persist::load_for`, read only — the daemon's
//!    snapshot is never overwritten from here), when it is current;
//! 2. otherwise one walk through the platform's source selector
//!    (`source_for`), kept in memory and not written anywhere.
//!
//! Every keystroke after that is a search of the index in memory. A walk
//! that failed is not tried again on the next keystroke but after
//! [`RETRY_AFTER`].
//!
//! # What it is not
//!
//! The content layer. A name result that is local is not a scope the daemon
//! chose, and the caller does not hand it to the `rgx` fallback.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, channel};
use std::time::{Duration, Instant};

use rgy_discovery::walk::KIND_DIR;
use rgy_discovery_dispatch::source_for;
use rgy_launcher::Answer;
use rgy_launcher::text::{Lang, Text, fill};
use rgy_name_core::live::Live;
use rgy_name_core::name_index::Index;
use rgy_name_core::name_query::{Alternatives, Searcher, matched_chars};
use rgy_name_core::persist;
use rgy_name_core::rules::Rules;

/// The workers the one walk uses: the portable source's own default.
const SCAN_THREADS: usize = 4;

/// How long a walk that failed is left alone before the next question may
/// try it again.
const RETRY_AFTER: Duration = Duration::from_secs(60);

/// What the launcher does when the daemon is not there.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Fallback {
    /// Nothing: the daemon is the only answer.
    ///
    /// An explicit `--endpoint` is this. The user named a daemon, and an
    /// answer from another tree under that name would be a silent lie.
    Strict,
    /// Search the names of this folder, in this process.
    Local(PathBuf),
}

/// What the index is doing while a question waits for it.
///
/// Said before the blocking work starts, so the window can show it while the
/// worker is still inside the scan. It is said only when there is work to do:
/// an index that is already open says nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    /// Looking for the per-root cache. A cache hit ends here and never says
    /// [`Phase::Building`].
    Preparing,
    /// The cache did not serve: the filesystem is being walked.
    Building,
    /// The index work is over and the query is about to run: the line goes
    /// back to "Searching…", and the build time is not the query's.
    Searching,
}

/// What the line under the field says about the local index.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalStatus {
    /// The folder that is searched, as the answers spell it.
    pub scope: String,
    /// What the index is: from the disk, scanned, not read yet, or failed.
    pub snapshot: String,
    /// How many entries the index holds, once there is one.
    pub entries: Option<usize>,
    /// How long opening the index took (the cache read or the walk). It is
    /// the build, not the query: the query's time is the answer's own.
    pub opened_in: Option<Duration>,
    /// The short, localized fragment for the window's line ("built in 42 ms"
    /// or "cache read in 3 ms"); the long sentence does not repeat it.
    pub took_label: Option<String>,
    /// Whether the index has settled: it is there, or it failed. An answer
    /// from a settled index — an empty one included — is the answer.
    pub done: bool,
}

/// How the index came to be.
enum Origin {
    /// Nothing has asked yet.
    Unread,
    /// The per-root cache, built at this unix second, read in `took`.
    Cached { synced_at: u64, took: Duration },
    /// One walk, made by this process, that lasted `took`.
    Scanned { at: Instant, took: Duration },
    /// The walk failed.
    Failed { at: Instant, why: String },
}

/// One in-memory index of one folder.
pub struct Local {
    /// The folder, spelled as the answers spell it.
    shown: String,
    /// Where the per-root cache lives.
    index_path: PathBuf,
    /// The user's home, for the `~` of a path filter.
    home: String,
    /// The index, once it is open.
    live: Option<Live>,
    origin: Origin,
}

impl Local {
    /// The index of `root`, not opened yet.
    pub fn new(root: &Path) -> Local {
        let index_path = persist::default_index_path(root);
        Local::with_cache(root, index_path)
    }

    /// The same, with the cache at `index_path`.
    pub fn with_cache(root: &Path, index_path: PathBuf) -> Local {
        let home = std::env::var("USERPROFILE")
            .or_else(|_| std::env::var("HOME"))
            .unwrap_or_default();
        Local {
            shown: spelled(root),
            index_path,
            home,
            live: None,
            origin: Origin::Unread,
        }
    }

    /// Whether the index is open.
    #[cfg(test)]
    pub fn is_open(&self) -> bool {
        self.live.is_some()
    }

    /// Open the index if nothing has, and say whether there is one.
    ///
    /// Blocking: the first call reads the cache or walks the folder. Every
    /// call after it is free, which is what keeps the keystroke cheap.
    #[cfg(test)]
    pub fn ensure(&mut self) -> bool {
        self.ensure_with(&mut |_| {})
    }

    /// The same, saying each [`Phase`] before the blocking work of it.
    ///
    /// `Preparing` precedes the cache lookup and `Building` precedes the
    /// walk, so a cache hit never claims a scan. Nothing is said when the
    /// index is already open or a failed walk is still inside its retry
    /// pause: there is no work to announce.
    pub fn ensure_with(&mut self, said: &mut dyn FnMut(Phase)) -> bool {
        let retry = match &self.origin {
            Origin::Unread => true,
            Origin::Failed { at, .. } => at.elapsed() >= RETRY_AFTER,
            Origin::Cached { .. } | Origin::Scanned { .. } => false,
        };
        if retry {
            self.open(said);
        }
        self.live.is_some()
    }

    /// The cache when it is current, one walk when it is not.
    fn open(&mut self, said: &mut dyn FnMut(Phase)) {
        let began = Instant::now();
        said(Phase::Preparing);
        let root = Path::new(&self.shown);
        if let Ok(idx) = persist::load_for(root, &self.index_path, None) {
            let synced_at = u64::from(idx.synced_at);
            // On Windows a mapped file cannot be replaced, and the daemon
            // replaces its snapshot: keep an owned copy and drop the map.
            #[cfg(windows)]
            let idx = match Index::from_bytes(&idx.to_bytes()) {
                Some(owned) => {
                    drop(idx);
                    owned
                }
                None => idx,
            };
            self.live = Some(Live::with_root(idx, self.shown.as_bytes()));
            self.origin = Origin::Cached {
                synced_at,
                took: began.elapsed(),
            };
            said(Phase::Searching);
            return;
        }
        // The cache did not serve: from here on it is a real walk, and only
        // now does the line say so.
        let scan_began = Instant::now();
        said(Phase::Building);
        let source = source_for(self.shown.as_bytes().to_vec(), SCAN_THREADS);
        // `enumerate()` skips the portable fallback when the native walk
        // fails; the cancellable form keeps it.
        let cancel = rgy_discovery::walk::Cancel::new();
        match source.enumerate_cancellable(&cancel, &|_| {}) {
            Ok(listings) => {
                // The rules beside the cache are read, never written; a
                // file that does not parse is the defaults.
                let (rules, _) = Rules::beside(&self.index_path);
                let idx =
                    Index::build_with_rules(listings, 0, now_secs(), self.shown.as_bytes(), &rules);
                self.live = Some(Live::with_root(idx, self.shown.as_bytes()));
                self.origin = Origin::Scanned {
                    at: Instant::now(),
                    took: scan_began.elapsed(),
                };
                said(Phase::Searching);
            }
            Err(why) => {
                self.live = None;
                self.origin = Origin::Failed {
                    at: Instant::now(),
                    why: why.to_string(),
                };
            }
        }
    }

    /// The answer to a name question, and the warnings about it.
    ///
    /// Fail closed: a query that does not parse answers nothing and says why.
    pub fn answer(&self, query: &str, limit: usize) -> (Answer, Vec<String>) {
        let Some(live) = self.live.as_ref() else {
            return (Answer::empty(), Vec::new());
        };
        let alts = match Alternatives::parse(query, &self.home) {
            Ok(alts) => alts,
            Err(why) => return (Answer::empty(), vec![why]),
        };
        let mut notes = alts.warnings.clone();
        let searcher = Searcher { live };
        let hits = searcher.search_alternatives(&alts.alts, limit);
        if !rgy_proto::name_answer_is_complete(hits.len(), limit) {
            notes.push(rgy_proto::client::ANSWER_STOPS_AT_THE_LIMIT.to_string());
        }
        let root = self.shown.trim_end_matches(['/', '\\']);
        let mut answer = Answer::empty();
        let mut buf = Vec::new();
        for hit in &hits {
            let (path, folder, name_start) = match &hit.over {
                Some(path) => {
                    let spelled = rgy_engine::console::spell(path);
                    let folder = spelled.ends_with(['/', '\\']);
                    (spelled, folder, 0)
                }
                None => {
                    live.base.path(hit.idx as usize, &mut buf);
                    let rel = rgy_engine::console::spell(&buf);
                    let rel = rel.trim_start_matches(['/', '\\']);
                    let start = buf.iter().rposition(|b| *b == b'/').map_or(0, |i| i + 1);
                    let full = if rel.is_empty() {
                        root.to_string()
                    } else {
                        format!(
                            "{root}{}{}",
                            std::path::MAIN_SEPARATOR,
                            rel.replace('/', std::path::MAIN_SEPARATOR_STR)
                        )
                    };
                    let folder = live.base.kind()[hit.idx as usize] & 3 == KIND_DIR;
                    (full, folder, start)
                }
            };
            // The span belongs to the one alternative that made the hit; with
            // several the hit does not say which, and a guess is a lie.
            let span = match (alts.alts.as_slice(), hit.over.is_none()) {
                ([one], true) => matched_chars(&buf[name_start..], &one.tokens),
                _ => Vec::new(),
            };
            answer.paths.push(path);
            answer.spans.push(span);
            answer.folders.push(folder);
        }
        (answer, notes)
    }

    /// What the line says about the index.
    pub fn status(&self, lang: Lang) -> LocalStatus {
        let text = Text::for_lang(lang);
        let entries = self.live.as_ref().map(|l| l.base.n.saturating_sub(1));
        let count = entries.unwrap_or(0).to_string();
        let (snapshot, done, opened_in, took_label) = match &self.origin {
            Origin::Unread => (text.local_unread.to_string(), false, None, None),
            Origin::Cached { synced_at, took } => {
                let age = now_secs_wide().saturating_sub(*synced_at);
                (
                    fill(text.local_cached, &[&coarse(age), &count]),
                    true,
                    Some(*took),
                    Some(fill(text.local_read_short, &[&duration_text(*took)])),
                )
            }
            Origin::Scanned { at, took } => (
                fill(
                    text.local_scanned,
                    &[&coarse(at.elapsed().as_secs()), &count],
                ),
                true,
                Some(*took),
                Some(fill(text.local_built_short, &[&duration_text(*took)])),
            ),
            Origin::Failed { why, .. } => (
                fill(text.local_scan_failed, &[why.as_str()]),
                true,
                None,
                None,
            ),
        };
        LocalStatus {
            scope: self.shown.clone(),
            snapshot,
            entries,
            opened_in,
            took_label,
            done,
        }
    }
}

/// One message of the prewarm task, in the order it sends them.
#[cfg_attr(
    not(any(windows, target_os = "linux", target_os = "freebsd", test)),
    allow(dead_code)
)]
pub enum Warm {
    /// The task is about to do the blocking work of this phase.
    Phase(Phase),
    /// The index is open (or failed, and says so in its status): it is the
    /// one index of the launcher, handed over whole.
    Ready(Box<Local>),
    /// The daemon was not proven absent, or the task is gone: nothing comes.
    Skipped,
    /// Nothing yet.
    Pending,
}

/// The one background task that opens the fallback index at launcher start.
///
/// It owns the [`Local`] while it is built and hands it over once, through a
/// channel; whoever receives it owns the only copy, so there is one scan and
/// no snapshot is shared. The task starts with a probe the caller supplies
/// and warms only when that probe says the daemon is proven absent.
#[cfg_attr(
    not(any(windows, target_os = "linux", target_os = "freebsd", test)),
    allow(dead_code)
)]
pub struct Prewarm {
    messages: Receiver<Warm>,
}

// The index crosses to the task and back: a `Local` that stopped being `Send`
// should fail here, not in a distant closure.
const _: fn() = || {
    fn is_send<T: Send>() {}
    is_send::<Local>();
};

#[cfg_attr(
    not(any(windows, target_os = "linux", target_os = "freebsd", test)),
    allow(dead_code)
)]
impl Prewarm {
    /// Start the task: `daemon_is_gone` first, then `make` and one open.
    ///
    /// One thread, once. `None` when the platform refuses it: the caller then
    /// opens the index on the first question, as before.
    pub fn start<M, P>(make: M, daemon_is_gone: P) -> Option<Prewarm>
    where
        M: FnOnce() -> Local + Send + 'static,
        P: FnOnce() -> bool + Send + 'static,
    {
        let (tx, messages) = channel::<Warm>();
        let body = move || {
            if !daemon_is_gone() {
                let _ = tx.send(Warm::Skipped);
                return;
            }
            let mut local = make();
            local.ensure_with(&mut |phase| {
                let _ = tx.send(Warm::Phase(phase));
            });
            let _ = tx.send(Warm::Ready(Box::new(local)));
        };
        std::thread::Builder::new()
            .name("rgxy-prewarm".to_string())
            .spawn(body)
            .ok()?;
        Some(Prewarm { messages })
    }

    /// The next message, waiting at most `wait` for it.
    pub fn next(&self, wait: Duration) -> Warm {
        match self.messages.recv_timeout(wait) {
            Ok(message) => message,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => Warm::Pending,
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => Warm::Skipped,
        }
    }
}

/// The sentence the user reads: where the daemon was expected, and what is
/// searched in its place.
pub fn sentence(endpoint: &str, status: &LocalStatus, lang: Lang) -> String {
    let text = Text::for_lang(lang);
    // The state of the index leads and the folder trails (see the table).
    let local = fill(text.local_names_in, &[&status.snapshot, &status.scope]);
    fill(text.no_daemon_in, &[endpoint, &local])
}

/// The folder the fallback searches by default.
pub fn default_root() -> Option<PathBuf> {
    let names = if cfg!(windows) {
        ["USERPROFILE", "HOME"]
    } else {
        ["HOME", "USERPROFILE"]
    };
    let homes: Vec<Option<std::ffi::OsString>> = names.iter().map(std::env::var_os).collect();
    choose_root(&homes, std::env::current_dir().ok())
}

/// The home that exists, and the current folder only when none does.
///
/// A current folder that is a filesystem root is not a scope: it has no
/// parent, and a walk of it is a walk of the machine.
pub fn choose_root(homes: &[Option<std::ffi::OsString>], cwd: Option<PathBuf>) -> Option<PathBuf> {
    for home in homes.iter().flatten().filter(|h| !h.is_empty()) {
        if let Some(scope) = scope_of(Path::new(home)) {
            return Some(scope);
        }
    }
    cwd.and_then(|dir| scope_of(&dir))
}

/// The folder as a scope: it exists, it is valid UTF-8 once resolved, and it
/// is not a filesystem root — however it is spelled, a symlink to one
/// included.
fn scope_of(dir: &Path) -> Option<PathBuf> {
    if !dir.is_dir() {
        return None;
    }
    let absolute = std::path::absolute(dir).unwrap_or_else(|_| dir.to_path_buf());
    let resolved = std::fs::canonicalize(&absolute).unwrap_or(absolute);
    resolved.to_str()?;
    let shown = PathBuf::from(spelled(dir));
    shown.parent().is_some().then_some(shown)
}

/// The folder as one absolute spelling, without the Windows canonical prefix
/// (it is not what the user wrote and not what an answer should carry).
fn spelled(root: &Path) -> String {
    let absolute = std::path::absolute(root).unwrap_or_else(|_| root.to_path_buf());
    let resolved = std::fs::canonicalize(&absolute).unwrap_or(absolute);
    let text = resolved.as_os_str().to_string_lossy().into_owned();
    if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
        return format!(r"\\{rest}");
    }
    text.strip_prefix(r"\\?\").unwrap_or(&text).to_string()
}

/// An age the user can read, and one that does not change every second: the
/// line is said again whenever it changes.
fn coarse(secs: u64) -> String {
    if secs < 60 {
        String::from("<1m")
    } else {
        persist::age_text(secs)
    }
}

/// A build time the user can read: milliseconds under a second, then
/// seconds with one decimal, then minutes. Not an age — the age is `coarse`.
fn duration_text(took: Duration) -> String {
    let ms = took.as_millis();
    if ms < 1000 {
        format!("{ms} ms")
    } else if ms < 60_000 {
        format!("{}.{} s", ms / 1000, (ms % 1000) / 100)
    } else {
        format!("{} min {} s", ms / 60_000, (ms % 60_000) / 1000)
    }
}

fn now_secs_wide() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

fn now_secs() -> u32 {
    now_secs_wide() as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A folder with a few files in it, and a cache path that is its own.
    fn tree(tag: &str) -> (PathBuf, PathBuf) {
        let base = std::env::temp_dir().join(format!("rgxy-local-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let root = base.join("home");
        std::fs::create_dir_all(root.join("docs")).expect("the folder is made");
        std::fs::write(root.join("docs").join("invoice-march.txt"), b"x").expect("a file");
        std::fs::write(root.join("notes.txt"), b"x").expect("a file");
        (root, base.join("cache").join("one.idx"))
    }

    #[test]
    fn a_walk_answers_a_name_in_the_folder() {
        let (root, cache) = tree("walk");
        let mut local = Local::with_cache(&root, cache);
        assert!(
            !local.is_open(),
            "nothing is read before the first question"
        );
        assert!(local.ensure());
        let (answer, _) = local.answer("invoice", 10);
        assert_eq!(answer.paths.len(), 1, "{:?}", answer.paths);
        assert!(
            answer.paths[0].ends_with("invoice-march.txt"),
            "{:?}",
            answer.paths
        );
        assert!(
            Path::new(&answer.paths[0]).is_file(),
            "the path opens: {:?}",
            answer.paths
        );
        assert_eq!(answer.folders, vec![false]);
        let status = local.status(Lang::English);
        assert!(status.done);
        assert!(status.snapshot.contains("index built"), "{status:?}");
        assert!(
            !status.snapshot.contains(" ms"),
            "no duration in the sentence: {status:?}"
        );
        assert!(
            status.opened_in.is_some(),
            "the build time is kept: {status:?}"
        );
        let label = status.took_label.as_deref().expect("a short label");
        assert!(label.starts_with("built in "), "{label}");
        let pt = local.status(Lang::Portuguese).took_label.expect("a label");
        assert!(pt.starts_with("construído em "), "{pt}");
        // docs, invoice-march.txt and notes.txt: the root is not an entry.
        assert_eq!(status.entries, Some(3), "{status:?}");
        let _ = std::fs::remove_dir_all(root.parent().unwrap());
    }

    #[test]
    fn a_walk_says_preparing_and_then_building_before_it_blocks() {
        let (root, cache) = tree("phases");
        let mut local = Local::with_cache(&root, cache);
        let mut said = Vec::new();
        assert!(local.ensure_with(&mut |p| said.push(p)));
        assert_eq!(
            said,
            vec![Phase::Preparing, Phase::Building, Phase::Searching]
        );
        // An open index has no work to announce.
        said.clear();
        assert!(local.ensure_with(&mut |p| said.push(p)));
        assert!(said.is_empty(), "{said:?}");
        let _ = std::fs::remove_dir_all(root.parent().unwrap());
    }

    #[test]
    fn a_cache_hit_never_says_building() {
        let (root, cache) = tree("phases-cache");
        let canonical = PathBuf::from(spelled(&root));
        let listings = source_for(spelled(&root).into_bytes(), 1)
            .enumerate()
            .expect("the walk");
        let mut idx = Index::build(listings, 0, now_secs(), spelled(&root).as_bytes());
        persist::save(&mut idx, &canonical, &cache).expect("the cache is saved");
        let mut local = Local::with_cache(&root, cache);
        let mut said = Vec::new();
        assert!(local.ensure_with(&mut |p| said.push(p)));
        assert_eq!(
            said,
            vec![Phase::Preparing, Phase::Searching],
            "no scan was made"
        );
        let label = local.status(Lang::English).took_label.expect("a label");
        assert!(label.starts_with("cache read in "), "{label}");
        let _ = std::fs::remove_dir_all(root.parent().unwrap());
    }

    /// Everything the task says, up to the end of its messages.
    fn drain(warm: &Prewarm) -> (Vec<Phase>, Option<Box<Local>>) {
        let mut phases = Vec::new();
        for _ in 0..200 {
            match warm.next(Duration::from_millis(100)) {
                Warm::Phase(p) => phases.push(p),
                Warm::Ready(local) => return (phases, Some(local)),
                Warm::Skipped => return (phases, None),
                Warm::Pending => {}
            }
        }
        panic!("the prewarm task never finished");
    }

    #[test]
    fn a_prewarm_hands_over_one_index_that_is_not_scanned_again() {
        let (root, cache) = tree("prewarm");
        let made = root.clone();
        let warm = Prewarm::start(move || Local::with_cache(&made, cache), || true)
            .expect("the thread starts");
        let (phases, local) = drain(&warm);
        assert_eq!(
            phases,
            vec![Phase::Preparing, Phase::Building, Phase::Searching]
        );
        let mut local = local.expect("the index is handed over");
        // A file made after the warm is not in the snapshot, and the first
        // question neither scans again nor announces anything.
        std::fs::write(root.join("later-file.txt"), b"x").expect("a file");
        let mut said = Vec::new();
        assert!(local.ensure_with(&mut |p| said.push(p)));
        assert!(said.is_empty(), "{said:?}");
        assert_eq!(local.answer("invoice", 10).0.paths.len(), 1);
        assert!(local.answer("later-file", 10).0.is_empty());
        let status = local.status(Lang::English);
        assert!(status.done && status.opened_in.is_some(), "{status:?}");
        // The task is over: the handle says nothing more is coming.
        assert!(matches!(warm.next(Duration::ZERO), Warm::Skipped));
        let _ = std::fs::remove_dir_all(root.parent().unwrap());
    }

    #[test]
    fn a_prewarm_without_a_proven_absence_scans_nothing() {
        let (root, cache) = tree("prewarm-skip");
        let built = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = std::sync::Arc::clone(&built);
        let base = root.parent().expect("the fixture has a base").to_path_buf();
        let warm = Prewarm::start(
            move || {
                flag.store(true, std::sync::atomic::Ordering::SeqCst);
                Local::with_cache(&root, cache)
            },
            || false,
        )
        .expect("the thread starts");
        let (phases, local) = drain(&warm);
        assert!(phases.is_empty() && local.is_none());
        assert!(
            !built.load(std::sync::atomic::Ordering::SeqCst),
            "no index was made"
        );
        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn a_build_time_reads_in_milliseconds_seconds_and_minutes() {
        assert_eq!(duration_text(Duration::from_millis(42)), "42 ms");
        assert_eq!(duration_text(Duration::from_millis(41_250)), "41.2 s");
        assert_eq!(duration_text(Duration::from_secs(135)), "2 min 15 s");
    }

    #[test]
    fn a_name_that_is_not_there_is_an_empty_answer_of_a_settled_index() {
        let (root, cache) = tree("miss");
        let mut local = Local::with_cache(&root, cache);
        local.ensure();
        let (answer, notes) = local.answer("zzzqqqxxx", 10);
        assert!(answer.is_empty(), "{:?}", answer.paths);
        assert!(notes.is_empty(), "{notes:?}");
        assert!(local.status(Lang::English).done, "no match is not building");
        let _ = std::fs::remove_dir_all(root.parent().unwrap());
    }

    #[test]
    fn a_folder_hit_says_it_is_a_folder() {
        let (root, cache) = tree("folder");
        let mut local = Local::with_cache(&root, cache);
        local.ensure();
        let (answer, _) = local.answer("docs", 10);
        let at = answer
            .paths
            .iter()
            .position(|p| p.ends_with("docs"))
            .expect("the folder is a hit");
        assert!(answer.folders[at]);
        let _ = std::fs::remove_dir_all(root.parent().unwrap());
    }

    #[test]
    fn the_index_is_opened_once() {
        let (root, cache) = tree("once");
        let mut local = Local::with_cache(&root, cache);
        local.ensure();
        // A file made after the walk is not in the snapshot, and a second
        // question does not walk again to find it.
        std::fs::write(root.join("later-file.txt"), b"x").expect("a file");
        assert!(local.ensure());
        let (answer, _) = local.answer("later-file", 10);
        assert!(
            answer.is_empty(),
            "the index is a snapshot: {:?}",
            answer.paths
        );
        let _ = std::fs::remove_dir_all(root.parent().unwrap());
    }

    #[test]
    fn a_current_cache_is_read_and_not_overwritten() {
        let (root, cache) = tree("cache");
        let canonical = PathBuf::from(spelled(&root));
        let listings = source_for(spelled(&root).into_bytes(), 1)
            .enumerate()
            .expect("the walk");
        let mut idx = Index::build(listings, 0, now_secs(), spelled(&root).as_bytes());
        persist::save(&mut idx, &canonical, &cache).expect("the cache is saved");
        let before = std::fs::metadata(&cache)
            .expect("the cache is there")
            .modified();
        // A file made after the cache was saved: only a walk would find it.
        std::fs::write(root.join("after-the-cache.txt"), b"x").expect("a file");

        let mut local = Local::with_cache(&root, cache.clone());
        assert!(local.ensure());
        let status = local.status(Lang::English);
        assert!(status.snapshot.contains("from disk"), "{status:?}");
        assert!(
            !status.snapshot.contains("built"),
            "a cache hit does not pretend to have scanned: {status:?}"
        );
        let (answer, _) = local.answer("notes", 10);
        assert_eq!(answer.paths.len(), 1, "{:?}", answer.paths);
        let (stale, _) = local.answer("after-the-cache", 10);
        assert!(
            stale.is_empty(),
            "the snapshot is the cache: {:?}",
            stale.paths
        );
        assert_eq!(
            std::fs::metadata(&cache)
                .expect("still there")
                .modified()
                .ok(),
            before.ok(),
            "the daemon's snapshot is not written from here"
        );
        // The daemon replaces its snapshot while this source is loaded; on
        // Windows that fails if the cache is still mapped.
        idx.save(&cache)
            .expect("the cache is replaced while the source is loaded");
        assert!(local.ensure());
        let _ = std::fs::remove_dir_all(root.parent().unwrap());
    }

    #[test]
    fn a_filter_that_does_not_parse_says_so_and_answers_nothing() {
        let (root, cache) = tree("parse");
        let mut local = Local::with_cache(&root, cache);
        local.ensure();
        let (answer, notes) = local.answer("re:(", 10);
        assert!(answer.is_empty());
        assert!(!notes.is_empty(), "the refusal is said");
        let _ = std::fs::remove_dir_all(root.parent().unwrap());
    }

    #[test]
    fn the_home_is_the_scope_and_the_current_folder_only_when_there_is_none() {
        let here = std::env::current_dir().expect("a current folder");
        let home = std::env::temp_dir();
        let chosen = choose_root(&[Some(home.clone().into_os_string())], Some(here.clone()))
            .expect("a scope");
        assert_eq!(chosen, PathBuf::from(spelled(&home)));

        // A home that is not a folder is no home.
        let missing = std::ffi::OsString::from("/no/such/rgxy/home");
        let chosen = choose_root(&[Some(missing), None], Some(here.clone())).expect("a scope");
        assert_eq!(chosen, PathBuf::from(spelled(&here)));

        // Empty variables are no home either.
        let chosen = choose_root(&[Some(std::ffi::OsString::new())], Some(here.clone()));
        assert_eq!(chosen, Some(PathBuf::from(spelled(&here))));
    }

    #[test]
    fn a_filesystem_root_is_never_a_scope() {
        let root = std::env::temp_dir()
            .ancestors()
            .last()
            .expect("a root")
            .to_path_buf();
        assert_eq!(choose_root(&[None], Some(root.clone())), None);
        assert_eq!(choose_root(&[None], None), None);
        // A home that is a root is no home either.
        assert_eq!(
            choose_root(&[Some(root.clone().into_os_string())], None),
            None
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_home_that_is_a_symlink_to_a_filesystem_root_is_never_a_scope() {
        let base = std::env::temp_dir().join(format!("rgxy-rootlink-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).expect("the folder is made");
        let link = base.join("home");
        std::os::unix::fs::symlink("/", &link).expect("the link is made");
        assert_eq!(choose_root(&[Some(link.into_os_string())], None), None);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn the_sentence_names_the_daemon_and_the_scope_in_both_languages() {
        let status = LocalStatus {
            scope: "C:\\Users\\dana".to_string(),
            snapshot: "x".to_string(),
            entries: None,
            opened_in: None,
            took_label: None,
            done: false,
        };
        let en = sentence("pipe:rgy", &status, Lang::English);
        assert!(en.contains("no daemon in pipe:rgy"), "{en}");
        assert!(en.contains("C:\\Users\\dana"), "{en}");
        assert!(en.contains("content search needs the daemon"), "{en}");
        // The state of the index and the note about content come before the
        // folder, so a long path is what a cut line loses.
        let path = en.find("C:\\Users\\dana").expect("the scope is said");
        assert!(en.find("(x)").expect("the snapshot") < path, "{en}");
        assert!(en.find("content search").expect("the note") < path, "{en}");
        let pt = sentence("pipe:rgy", &status, Lang::Portuguese);
        assert!(pt.contains("sem daemon em pipe:rgy"), "{pt}");
        assert!(pt.contains("precisa do daemon"), "{pt}");
    }
}
