// SPDX-License-Identifier: Apache-2.0

//! Persisting a built name index across processes.
//!
//! [`Index::save`] writes the blob and [`Index::load`] maps it back, but the
//! blob header records only counts and sync stamps — **not which tree the
//! index was built from**. Loading such a file for a different root produces
//! a well-formed index whose paths are all wrong, and nothing complains.
//!
//! So every saved index gets a `<index>.meta` sidecar naming the root it was
//! built from. A load whose sidecar is missing, unreadable, or names another
//! root is reported as a plain miss and the caller rebuilds; it is never
//! trusted. The index file is also never silently promoted past its age
//! limit, because the portable walk has no change feed to tell a fresh tree
//! from a stale one: the limit defaults to [`DEFAULT_MAX_AGE`], and a caller
//! that passes its own limit overrides it.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::name_index::Index;

/// How old a saved index may be before it is rebuilt instead of reused.
///
/// Thirty minutes. Long enough that a daemon restart does not walk the tree
/// again, short enough that the snapshot it answers from is not "yesterday".
/// The CLI's help text already advertised this window ("rebuild an index
/// older than 30m/2h/7d"), so this is the value the text implies -- and the
/// text is not the reason: a saved index is a snapshot, and a snapshot served
/// without saying how old it is is how a search ends up answering "no such
/// file" for a file created yesterday.
pub const DEFAULT_MAX_AGE: Duration = Duration::from_secs(30 * 60);

fn unix(d: Duration) -> u64 {
    d.as_secs()
}

/// Is the index built at `built_at` (unix seconds) too old to reuse at `now`?
///
/// The whole staleness policy as one pure function of (built, now, limit),
/// so it can be tested without a filesystem and so the two programs that
/// share it cannot disagree:
///
/// * `None` -- the default limit, [`DEFAULT_MAX_AGE`]. This case used to mean
///   "no limit at all", which is why an index of any age was served silently:
///   a limit only existed if the caller passed one, and neither did.
/// * `Some(ZERO)` -- never reuse. This is what `--max-age 0` means.
/// * `Some(d)` -- reuse while the index is younger than `d`.
///
/// `>=`, not `>`: `--max-age 0` reads as "never reuse", and a stamp written
/// this same second is otherwise younger than zero seconds.
pub fn is_stale(built_at: u64, now: u64, max_age: Option<Duration>) -> bool {
    let limit = max_age.unwrap_or(DEFAULT_MAX_AGE);
    now.saturating_sub(built_at) >= unix(limit)
}

/// An age in the words a person would use. "86400s old" is a number; "1d
/// old" is an answer, and the answer is what a reader of the daemon's
/// startup log is after.
pub fn age_text(age: u64) -> String {
    match age {
        0..=59 => format!("{age}s"),
        60..=3_599 => format!("{}m", age / 60),
        3_600..=86_399 => format!("{}h", age / 3_600),
        _ => format!("{}d", age / 86_400),
    }
}

fn now_unix() -> u64 {
    unix(SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default())
}

/// Canonicalise so `.`, `..`, trailing separators and (on Windows) verbatim
/// prefixes all compare equal. Falls back to the literal path when the root
/// does not exist — a later successful build overwrites the stamp anyway.
fn canon(p: &Path) -> PathBuf {
    std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf())
}

fn stamp_path(index: &Path) -> PathBuf {
    let mut s = index.as_os_str().to_os_string();
    s.push(".meta");
    PathBuf::from(s)
}

/// What a save wrote, for reporting.
#[derive(Debug)]
pub struct Saved {
    /// The blob.
    pub index: PathBuf,
    /// The sidecar that pins the root.
    pub meta: PathBuf,
}

/// Write the index and its sidecar.
///
/// The blob lands first: it is the thing that has to be all-or-nothing
/// (`Index::save` is tmp+rename). A sidecar without its blob would be worse
/// than nothing, so a failure between the two writes leaves the previous
/// sidecar in place, whose root check then rejects the new blob.
pub fn save(idx: &Index, root: &Path, index: &Path) -> std::io::Result<Saved> {
    if let Some(dir) = index.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir)?;
    }
    idx.save(index)?;
    let meta = stamp_path(index);
    std::fs::write(&meta, stamp(root, idx.n, now_unix()))?;
    Ok(Saved { index: index.to_path_buf(), meta })
}

fn stamp(root: &Path, entries: usize, built: u64) -> String {
    format!("rgy-index-meta 1\nroot {}\nentries {entries}\nbuilt {built}\n", canon(root).to_string_lossy())
}

/// What [`load_for`] decided, and why — so a caller can say so out loud
/// instead of silently rebuilding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Miss {
    /// No blob and no sidecar.
    Absent,
    /// The sidecar could not be parsed or read.
    NoStamp,
    /// The sidecar names a different tree than the one being searched.
    WrongRoot { saved: String, want: String },
    /// Older than the caller's `max_age`.
    TooOld { built: u64 },
    /// The sidecar checked out but the blob is missing or will not load.
    BadIndex,
}

impl std::fmt::Display for Miss {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Miss::Absent => write!(f, "no index on disk"),
            Miss::NoStamp => write!(f, "index has no root stamp"),
            Miss::WrongRoot { saved, want } => {
                write!(f, "index was built for {saved}, not {want}")
            }
            Miss::TooOld { built } => {
                let age = now_unix().saturating_sub(*built);
                write!(f, "index is {} old", age_text(age))
            }
            Miss::BadIndex => write!(f, "index blob would not load"),
        }
    }
}

/// Load an index previously saved for `root`.
///
/// Either the index, or the reason there is none — the caller is expected to
/// rebuild on `Err`, and can say why. `max_age` is the caller's staleness
/// policy: the portable walk has no change feed, so freshness is a decision,
/// not something this can infer. `None` means the default limit
/// ([`DEFAULT_MAX_AGE`]); a zero `max_age` always rebuilds.
pub fn load_for(root: &Path, index: &Path, max_age: Option<Duration>) -> Result<Index, Miss> {
    let want = canon(root).to_string_lossy().into_owned();
    let meta = match std::fs::read_to_string(stamp_path(index)) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            // Distinguish "never saved" from "saved without a stamp".
            return Err(if index.exists() { Miss::NoStamp } else { Miss::Absent });
        }
        Err(_) => return Err(Miss::NoStamp),
    };

    let mut saved_root = None;
    let mut built = None;
    for line in meta.lines() {
        let Some((key, value)) = line.split_once(' ') else {
            continue;
        };
        match key {
            "root" => saved_root = Some(value.to_string()),
            "built" => built = value.parse::<u64>().ok(),
            _ => {}
        }
    }

    let Some(saved) = saved_root else {
        return Err(Miss::NoStamp);
    };
    if saved != want {
        return Err(Miss::WrongRoot { saved, want });
    }
    // An index with no build stamp cannot be dated, and an index that cannot
    // be dated cannot be judged fresh: it is refused rather than served on
    // trust. (A stamp written by this program always carries one.)
    let Some(built) = built else {
        return Err(Miss::NoStamp);
    };
    if is_stale(built, now_unix(), max_age) {
        return Err(Miss::TooOld { built });
    }

    // The sidecar checked out, so a blob that will not map is corruption,
    // not an absence — say so instead of looking like "never saved".
    Index::load(index).ok_or(Miss::BadIndex)
}

/// Load a saved index trusting the root its sidecar names -- for tools that
/// open a file the user pointed at (`zen-rs load <file>`), where there is no
/// "want" to check the stamp against. The stamped root comes back so the
/// caller can serve the paths the index was built with.
pub fn load_saved(index: &Path) -> Result<(Index, String), Miss> {
    let meta = match std::fs::read_to_string(stamp_path(index)) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(if index.exists() { Miss::NoStamp } else { Miss::Absent });
        }
        Err(_) => return Err(Miss::NoStamp),
    };
    let mut saved_root = None;
    for line in meta.lines() {
        if let Some(("root", value)) = line.split_once(' ') {
            saved_root = Some(value.to_string());
        }
    }
    let saved = saved_root.ok_or(Miss::NoStamp)?;
    let idx = Index::load(index).ok_or(Miss::BadIndex)?;
    Ok((idx, saved))
}

/// Default index location for a root: `<cache_dir>/rgy/<digest>.idx`, so two
/// roots never collide and no root has to be spelled into the filename.
///
/// The cache directory is the platform's own: `%LOCALAPPDATA%` on Windows,
/// `$XDG_CACHE_HOME` (or `~/.cache`) on Linux, `~/Library/Caches` on macOS.
/// `RGY_INDEX_DIR` overrides all of them.
///
/// This used to fall back to `std::env::temp_dir()` whenever none of those
/// was set, and `XDG_CACHE_HOME` is unset by default on most Linux systems
/// and on every macOS one -- so the index landed in `/tmp/rgy`, a directory
/// every user on the host shares and every user can write to. Two users
/// searching the same root landed on the same digest file, and the file
/// itself sat where somebody else could have planted a symlink first. The
/// temp dir is still the last resort, for a machine with no home at all,
/// but it is no longer the first one reached.
pub fn default_index_path(root: &Path) -> PathBuf {
    let digest = fnv1a(canon(root).to_string_lossy().as_bytes());
    let base = std::env::var_os("RGY_INDEX_DIR").map(PathBuf::from).or_else(cache_dir).unwrap_or_else(std::env::temp_dir);
    base.join("rgy").join(format!("{digest:016x}.idx"))
}

/// The platform's cache directory, read from the environment.
///
/// Split from the path logic so the test does not have to mutate the
/// process environment: a test that sets `HOME` races every other test in
/// the process.
fn cache_dir() -> Option<PathBuf> {
    // Windows has no `HOME`; the user's profile directory is the same
    // thing under another name.
    let home = if cfg!(windows) { std::env::var_os("USERPROFILE") } else { std::env::var_os("HOME") };
    cache_dir_from(std::env::var_os("XDG_CACHE_HOME").as_deref(), home.as_deref(), std::env::var_os("LOCALAPPDATA").as_deref())
}

/// The cache directory, given the environment.
///
/// `$XDG_CACHE_HOME` when it is set, else `~/.cache` on Linux and
/// `~/Library/Caches` on macOS -- the two the manual names. Windows keeps
/// `%LOCALAPPDATA%`, and `%USERPROFILE%\AppData\Local` when that is unset.
fn cache_dir_from(xdg: Option<&std::ffi::OsStr>, home: Option<&std::ffi::OsStr>, local: Option<&std::ffi::OsStr>) -> Option<PathBuf> {
    let some = |v: Option<&std::ffi::OsStr>| v.filter(|s| !s.is_empty()).map(PathBuf::from);
    if cfg!(windows) {
        return some(local).or_else(|| some(home).map(|h| h.join("AppData").join("Local")));
    }
    if cfg!(target_os = "macos") {
        return some(xdg).or_else(|| some(home).map(|h| h.join("Library").join("Caches")));
    }
    some(xdg).or_else(|| some(home).map(|h| h.join(".cache")))
}

fn fnv1a(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(0x100_0000_01b3);
    }
    h
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::name_index::Index;
    use rgy_discovery::PortableSource;
    use rgy_discovery::Source;

    fn tree(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("rgy-persist-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(dir.join("sub/a.txt"), b"a").unwrap();
        std::fs::write(dir.join("b.txt"), b"b").unwrap();
        dir
    }

    fn build(root: &Path) -> Index {
        let src = PortableSource::new(root.to_string_lossy().as_bytes().to_vec(), 2);
        Index::build(src.enumerate().unwrap(), 0, 1, b"/")
    }

    /// The *decision* rather than the index itself: `Index` is deliberately
    /// not `Debug`, and a test here is about hit/refusal, not contents.
    fn outcome(root: &Path, index: &Path, max_age: Option<Duration>) -> Result<usize, Miss> {
        load_for(root, index, max_age).map(|i| i.n)
    }

    #[test]
    fn absent_index_is_a_miss() {
        let dir = tree("absent");
        let idx_path = dir.join("cache.idx");
        assert_eq!(outcome(&dir, &idx_path, None).unwrap_err(), Miss::Absent);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The cache directory is the platform's, and not the temp dir.
    ///
    /// The temp dir is the last resort, for a machine with no home at all:
    /// it is shared by every user on the host, and a file there is a file
    /// somebody else can replace.
    #[test]
    fn the_cache_dir_is_the_platforms_own() {
        let home = std::ffi::OsStr::new("/home/tester");
        let local = std::ffi::OsStr::new("C:/Users/tester/AppData/Local");
        let got = cache_dir_from(None, Some(home), Some(local)).expect("a cache directory");
        if cfg!(windows) {
            assert_eq!(got, PathBuf::from("C:/Users/tester/AppData/Local"));
        } else if cfg!(target_os = "macos") {
            assert_eq!(got, PathBuf::from("/home/tester/Library/Caches"));
        } else {
            assert_eq!(got, PathBuf::from("/home/tester/.cache"));
        }
    }

    /// An explicit `XDG_CACHE_HOME` wins where the platform has one: the
    /// user said where the cache is. Windows has its own convention
    /// (`%LOCALAPPDATA%`), and there the platform's answer stands.
    #[test]
    fn xdg_cache_home_wins_wherever_it_is_set() {
        let xdg = std::ffi::OsStr::new("/xdg/cache");
        let local = std::ffi::OsStr::new("C:/Users/tester/AppData/Local");
        let got = cache_dir_from(Some(xdg), None, Some(local)).expect("a cache directory");
        if cfg!(windows) {
            assert_eq!(got, PathBuf::from("C:/Users/tester/AppData/Local"));
        } else {
            assert_eq!(got, PathBuf::from("/xdg/cache"));
        }
    }

    /// An empty variable is not a directory. `XDG_CACHE_HOME=` in a shell
    /// profile is a way of saying "unset", not a way of pointing the cache
    /// at the current folder.
    #[test]
    fn an_empty_environment_variable_is_not_a_directory() {
        let empty = std::ffi::OsStr::new("");
        assert!(cache_dir_from(Some(empty), Some(empty), Some(empty)).is_none());
    }

    /// A machine with no home has no cache directory, and the temp dir is
    /// then the last resort rather than the first one reached.
    #[test]
    fn a_machine_with_no_home_has_no_cache_dir() {
        assert!(cache_dir_from(None, None, None).is_none());
    }

    /// The index file sits in the cache dir, under `rgy`, named by the
    /// digest of the root -- so two roots never collide and no root has to
    /// be spelled into the filename.
    #[test]
    fn the_index_path_is_the_cache_dirs_rgy_digest() {
        let path = default_index_path(Path::new("/some/root"));
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        assert!(name.ends_with(".idx"), "{name}");
        assert_eq!(name.len(), 16 + 4, "a 16-digit digest and .idx: {name}");
        assert_eq!(path.parent().unwrap().file_name().unwrap(), "rgy");
    }

    #[test]
    fn round_trips_and_survives_a_reload() {
        let dir = tree("rt");
        let idx_path = dir.join("cache.idx");
        let built = build(&dir);
        save(&built, &dir, &idx_path).expect("save");

        assert_eq!(outcome(&dir, &idx_path, None).expect("no miss"), built.n);

        // Same tree reached by a different spelling must still hit.
        let dotted = dir.join("sub").join("..");
        assert!(outcome(&dotted, &idx_path, None).is_ok());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_index_from_another_root_never_loads() {
        let a = tree("root-a");
        let b = tree("root-b");
        let idx_path = a.join("cache.idx");
        save(&build(&a), &a, &idx_path).expect("save");

        // Point B's query at A's index: must be a refusal, not a wrong answer.
        let err = outcome(&b, &idx_path, None).unwrap_err();
        assert!(matches!(err, Miss::WrongRoot { .. }), "got {err}");
        // And the refusal must never come back as an index on any channel.
        assert!(matches!(outcome(&b, &idx_path, None), Err(Miss::WrongRoot { .. })), "an index built for {a:?} was served for {b:?}");

        let _ = std::fs::remove_dir_all(&a);
        let _ = std::fs::remove_dir_all(&b);
    }

    #[test]
    fn max_age_expires_the_index() {
        let dir = tree("age");
        let idx_path = dir.join("cache.idx");
        save(&build(&dir), &dir, &idx_path).expect("save");

        assert!(outcome(&dir, &idx_path, Some(Duration::from_secs(3600))).is_ok());
        // Zero tolerance means "never reuse", even for a same-second stamp.
        assert!(matches!(outcome(&dir, &idx_path, Some(Duration::from_secs(0))), Err(Miss::TooOld { .. })));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The staleness policy, as the one pure function it is written as.
    ///
    /// The three cases are the three the programs can ask for: the default
    /// limit when the caller passes none, "never reuse" for `--max-age 0`,
    /// and the caller's own limit when it passes one.
    #[test]
    fn the_age_policy_is_one_function_of_built_now_and_limit() {
        let now = 1_000_000;
        let fresh = now - 60;
        let old = now - DEFAULT_MAX_AGE.as_secs() - 60;

        // No limit from the caller means the default limit, not "no limit":
        // this is the case that used to serve an index of any age.
        assert!(!is_stale(fresh, now, None), "a fresh index is not stale");
        assert!(is_stale(old, now, None), "the default limit must expire an old index");

        // `--max-age 0` never reuses, not even a same-second index.
        assert!(is_stale(now, now, Some(Duration::ZERO)), "zero means never reuse");
        assert!(is_stale(now - 1, now, Some(Duration::ZERO)), "zero means never reuse");

        // An explicit limit wins over the default, in both directions.
        assert!(!is_stale(old, now, Some(Duration::from_secs(7 * 86_400))), "an explicit limit longer than the default must win");
        assert!(is_stale(fresh, now, Some(Duration::from_secs(30))), "an explicit limit shorter than the default must win");
        // And the boundary is the limit itself: an index exactly at the
        // limit is rebuilt.
        assert!(is_stale(now - 3600, now, Some(Duration::from_secs(3600))));
        assert!(!is_stale(now - 3599, now, Some(Duration::from_secs(3600))));
    }

    /// The default limit, at the level the programs see it: an index older
    /// than it is refused, a younger one is served.
    ///
    /// The stamp is written by hand rather than by `save` so the age is
    /// exact -- a saved index is a second old, and the default limit is
    /// half an hour.
    #[test]
    fn an_index_older_than_the_default_limit_is_not_reused() {
        let dir = tree("default-age");
        let idx_path = dir.join("cache.idx");
        save(&build(&dir), &dir, &idx_path).expect("save");

        // Age the stamp past the default limit.
        let mut meta_name = idx_path.as_os_str().to_os_string();
        meta_name.push(".meta");
        let meta = PathBuf::from(meta_name);
        let text = std::fs::read_to_string(&meta).expect("sidecar");
        let old = now_unix() - DEFAULT_MAX_AGE.as_secs() - 60;
        let aged = text.lines().map(|l| if l.starts_with("built ") { format!("built {old}") } else { l.to_string() }).collect::<Vec<_>>().join("\n");
        std::fs::write(&meta, aged).expect("age the stamp");

        // The default limit refuses it...
        assert!(matches!(outcome(&dir, &idx_path, None), Err(Miss::TooOld { .. })));
        // ...an explicit longer limit accepts it...
        assert!(outcome(&dir, &idx_path, Some(Duration::from_secs(7 * 86_400))).is_ok());
        // ...and `--max-age 0` refuses it whatever its age.
        assert!(matches!(outcome(&dir, &idx_path, Some(Duration::ZERO)), Err(Miss::TooOld { .. })));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The refusal says how old the index is, in words a person reads: the
    /// reason a rebuild happened has to reach the user, and "index is 86400s
    /// old" is a number, not an answer.
    #[test]
    fn the_refusal_says_how_old_the_index_is() {
        let built = now_unix() - 3 * 3_600;
        assert_eq!(Miss::TooOld { built }.to_string(), format!("index is {} old", age_text(3 * 3_600)));
        assert_eq!(age_text(45), "45s");
        assert_eq!(age_text(12 * 60), "12m");
        assert_eq!(age_text(3 * 3_600), "3h");
        assert_eq!(age_text(2 * 86_400), "2d");
    }

    #[test]
    fn a_blob_without_a_stamp_is_refused() {
        let dir = tree("nostamp");
        let idx_path = dir.join("cache.idx");
        build(&dir).save(&idx_path).expect("raw save");
        assert_eq!(outcome(&dir, &idx_path, None).unwrap_err(), Miss::NoStamp);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_saved_index_returns_its_stamped_root() {
        let dir = tree("saved-root");
        let idx_path = dir.join("cache.idx");
        let built = build(&dir);
        save(&built, &dir, &idx_path).expect("save");

        let (loaded, root) = load_saved(&idx_path).expect("load");
        assert_eq!(loaded.n, built.n);
        assert_eq!(root, canon(&dir).to_string_lossy().into_owned());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_stamp_without_a_blob_is_refused() {
        let dir = tree("noblob");
        let idx_path = dir.join("cache.idx");
        save(&build(&dir), &dir, &idx_path).expect("save");
        std::fs::remove_file(&idx_path).expect("drop the blob");

        assert_eq!(outcome(&dir, &idx_path, None).unwrap_err(), Miss::BadIndex);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
