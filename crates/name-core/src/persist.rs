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
//! trusted.
//!
//! The sidecar is not the only word: the blob carries the root's digest in
//! its header, and a load checks the blob against the tree it is asked
//! about. Two files can disagree — they are written one after the other,
//! and a crash between the two writes leaves the sidecar naming the tree
//! the blob was built for *before* — and a loader that trusted the sidecar
//! alone would serve the wrong tree's paths for the right tree.
//!
//! The index file is also never silently promoted past its age
//! limit, because the portable walk has no change feed to tell a fresh tree
//! from a stale one: the limit defaults to [`DEFAULT_MAX_AGE`], and a caller
//! that passes its own limit overrides it.

use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::name_index::Index;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

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
    // The crate's own clock (crate::clock): the same wall clock in every
    // build, declared rather than borrowed from std::time.
    crate::clock::unix_millis() / 1000
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
///
/// The blob says which tree it is for as well, and that is not belt and
/// braces: the sidecar and the blob are two files, and a crash between
/// the two writes leaves them disagreeing -- the sidecar naming the tree
/// the blob was built for *before*, the blob holding the tree built
/// *after*. A loader that trusted the sidecar alone would then serve the
/// wrong tree's paths for the right tree, which is the one failure a
/// cache must not have. The digest in the blob's header is what closes
/// it: the blob is checked against the tree it is asked about, and not
/// only against a sidecar a crash may have left behind.
pub fn save(idx: &mut Index, root: &Path, index: &Path) -> std::io::Result<Saved> {
    if let Some(dir) = index.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir)?;
    }
    // The root's bytes are read once: the digest in the blob and the root
    // in the sidecar are two words about the same tree, and two reads
    // could be two trees if the path changed under the first one.
    let bytes = root_bytes(root);
    idx.set_root_digest(fnv1a(&bytes));
    idx.save(index)?;
    let meta = stamp_path(index);
    std::fs::write(&meta, stamp(&bytes, idx.n, now_unix()))?;
    Ok(Saved { index: index.to_path_buf(), meta })
}

/// The stamp's version.
///
/// Version 1 wrote the root as plain text on a line of its own. A root
/// is bytes, and a root can carry a line break -- and a line break in a
/// line-delimited file is the end of the line, so the root came back
/// cut in half: the index never matched its own tree, the daemon walked
/// the tree again on every start, and a root whose own name carried the
/// words `root /etc` chose the tree the index was served for.
///
/// Version 2 writes the root as hex. Hex is ASCII, so it cannot carry a
/// line break, and it carries the bytes exactly.
const STAMP_VERSION: u32 = 2;

fn stamp(root: &[u8], entries: usize, built: u64) -> String {
    format!("rgy-index-meta {STAMP_VERSION}\nroot-hex {}\nentries {entries}\nbuilt {built}\n", hex(root))
}

/// The root's bytes, exactly as the filesystem spells them.
///
/// The bytes are the root's own. `to_string_lossy` replaces the bytes
/// that are not text with a question mark, and two roots that differ
/// only in those bytes are then the same text -- and the same text is
/// the same digest, and the same digest is the same index file, and one
/// index file for two trees is an index that answers for the wrong tree.
fn root_bytes(root: &Path) -> Vec<u8> {
    canon(root).as_os_str().as_encoded_bytes().to_vec()
}

/// The hex of some bytes.
fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        out.push(DIGITS[(b >> 4) as usize] as char);
        out.push(DIGITS[(b & 0x0f) as usize] as char);
    }
    out
}

/// The bytes of some hex, or nothing when it is not hex.
fn unhex(text: &str) -> Option<Vec<u8>> {
    let bytes = text.as_bytes();
    if !bytes.len().is_multiple_of(2) {
        return None;
    }
    let mut out = Vec::with_capacity(bytes.len() / 2);
    for pair in bytes.chunks(2) {
        let hi = (pair[0] as char).to_digit(16)?;
        let lo = (pair[1] as char).to_digit(16)?;
        out.push((hi * 16 + lo) as u8);
    }
    Some(out)
}

/// The sidecar's words: the root it names, in bytes, and when it was built.
///
/// The version decides how the root is spelled. Version 1 wrote it in
/// plain text and version 2 writes it in hex, and both are read: the
/// index a user already has is still the index the program uses. A
/// version 1 stamp whose root carried a line break cannot be read back
/// whole, and that one is refused -- the tree is walked again and the
/// stamp is written again, in hex.
fn read_stamp(meta: &str) -> Option<(Vec<u8>, Option<u64>)> {
    let mut lines = meta.lines();
    let header = lines.next()?;
    let version = header.strip_prefix("rgy-index-meta ")?.trim().parse::<u32>().ok()?;
    let mut root = None;
    let mut built = None;
    for line in lines {
        let Some((key, value)) = line.split_once(' ') else {
            continue;
        };
        match (version, key) {
            (1, "root") => root = Some(value.as_bytes().to_vec()),
            (2, "root-hex") => root = unhex(value),
            (_, "built") => built = value.parse::<u64>().ok(),
            _ => {}
        }
    }
    Some((root?, built))
}

/// The bytes of a path, as one line a person can read.
///
/// The message is one line, and a path that carries a line break cannot
/// be one: the line break is shown as what it is.
fn show(bytes: &[u8]) -> String {
    let mut out = String::new();
    for ch in String::from_utf8_lossy(bytes).chars() {
        match ch {
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\x{:02x}", c as u32)),
            c => out.push(c),
        }
    }
    out
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
    ///
    /// The two roots are as one line each: a root that carries a line
    /// break cannot be one, and the message is one line.
    WrongRoot { saved: String, want: String },
    /// The blob and the sidecar are about different trees.
    ///
    /// The sidecar checked out and the blob did not: the blob was built
    /// for another tree. This is what a crash between the two writes
    /// leaves behind -- the sidecar naming the tree built before, the blob
    /// holding the tree built after -- and the tree is walked again.
    WrongBlob,
    /// Older than the caller's `max_age`.
    TooOld { built: u64 },
    /// The sidecar checked out but the blob is missing or will not load.
    BadIndex,
    /// The blob loaded, but its parent chain is not a forest: a broken
    /// chain, a cycle or a parent that is not a folder.
    BadForest { fault: crate::name_index::ForestFault },
}

impl std::fmt::Display for Miss {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Miss::Absent => write!(f, "no index on disk"),
            Miss::NoStamp => write!(f, "index has no root stamp"),
            Miss::WrongRoot { saved, want } => {
                write!(f, "index was built for {saved}, not {want}")
            }
            Miss::WrongBlob => write!(f, "the index file is about another tree"),
            Miss::TooOld { built } => {
                let age = now_unix().saturating_sub(*built);
                write!(f, "index is {} old", age_text(age))
            }
            Miss::BadIndex => write!(f, "index blob would not load"),
            Miss::BadForest { fault } => write!(f, "index is not a forest: {fault}"),
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
    let want = root_bytes(root);
    let meta = match std::fs::read_to_string(stamp_path(index)) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            // Distinguish "never saved" from "saved without a stamp".
            return Err(if index.exists() { Miss::NoStamp } else { Miss::Absent });
        }
        Err(_) => return Err(Miss::NoStamp),
    };

    let Some((saved, built)) = read_stamp(&meta) else {
        return Err(Miss::NoStamp);
    };
    if saved != want {
        return Err(Miss::WrongRoot { saved: show(&saved), want: show(&want) });
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
    let idx = Index::load(index).ok_or(Miss::BadIndex)?;
    // The blob says which tree it was built for, and the sidecar says
    // which tree it is for. The two have to be the same tree: they are
    // two files, and a crash between the two writes leaves them
    // disagreeing. The sidecar alone is not enough -- it is the file that
    // was not written -- so the blob's own word is what is checked.
    if idx.root_digest != fnv1a(&want) {
        return Err(Miss::WrongBlob);
    }
    // A blob whose parent chain is not a forest would answer with paths
    // that do not exist. It is corruption too, and it is caught here
    // rather than in the middle of a search.
    idx.validate_forest().map_err(|fault| Miss::BadForest { fault })?;
    Ok(idx)
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
    let Some((saved, _)) = read_stamp(&meta) else {
        return Err(Miss::NoStamp);
    };
    let idx = Index::load(index).ok_or(Miss::BadIndex)?;
    // The blob has to be the tree the sidecar names: the caller pointed
    // at a file and the stamp is what says what is in it, and the blob
    // says so too. Two files that disagree are not read.
    if idx.root_digest != fnv1a(&saved) {
        return Err(Miss::WrongBlob);
    }
    // The root is text here because the caller prints it: the paths it
    // serves are the root's bytes joined to the names, and the root comes
    // back whole.
    Ok((idx, String::from_utf8_lossy(&saved).into_owned()))
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
    let digest = fnv1a(&root_bytes(root));
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
    use alloc::format;
    use alloc::string::ToString;
    use alloc::vec::Vec;
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
        let mut built = build(&dir);
        save(&mut built, &dir, &idx_path).expect("save");

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
        save(&mut build(&a), &a, &idx_path).expect("save");

        // Point B's query at A's index: must be a refusal, not a wrong answer.
        let err = outcome(&b, &idx_path, None).unwrap_err();
        assert!(matches!(err, Miss::WrongRoot { .. }), "got {err}");
        // And the refusal must never come back as an index on any channel.
        assert!(matches!(outcome(&b, &idx_path, None), Err(Miss::WrongRoot { .. })), "an index built for {a:?} was served for {b:?}");

        let _ = std::fs::remove_dir_all(&a);
        let _ = std::fs::remove_dir_all(&b);
    }

    /// The blob and the sidecar have to be about the same tree.
    ///
    /// They are two files, and they are written one after the other. A
    /// crash between the two writes leaves the sidecar naming the tree the
    /// blob was built for *before* and the blob holding the tree built
    /// *after* — and a loader that trusted the sidecar alone would serve
    /// the wrong tree's paths for the right tree. The blob says which tree
    /// it is for, and that is what is checked.
    #[test]
    fn a_blob_of_another_tree_is_never_served() {
        let a = tree("blob-a");
        let b = tree("blob-b");
        let idx_path = a.join("cache.idx");
        let mut built_a = build(&a);
        save(&mut built_a, &a, &idx_path).expect("save");

        // The crash: the blob is written for B and the sidecar is not, so
        // what is left on the disk is A's sidecar beside B's blob.
        let mut built_b = build(&b);
        built_b.set_root_digest(fnv1a(&root_bytes(&b)));
        built_b.save(&idx_path).expect("the blob alone");

        // A's query must be refused, not answered with B's tree.
        let err = outcome(&a, &idx_path, None).unwrap_err();
        assert!(matches!(err, Miss::WrongBlob), "got {err}");
        // And B's query is refused by the sidecar, which is A's.
        assert!(matches!(outcome(&b, &idx_path, None), Err(Miss::WrongRoot { .. })), "B was served A's sidecar");

        let _ = std::fs::remove_dir_all(&a);
        let _ = std::fs::remove_dir_all(&b);
    }

    #[test]
    fn max_age_expires_the_index() {
        let dir = tree("age");
        let idx_path = dir.join("cache.idx");
        save(&mut build(&dir), &dir, &idx_path).expect("save");

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
        save(&mut build(&dir), &dir, &idx_path).expect("save");

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
        let mut built = build(&dir);
        save(&mut built, &dir, &idx_path).expect("save");

        let (loaded, root) = load_saved(&idx_path).expect("load");
        assert_eq!(loaded.n, built.n);
        assert_eq!(root, canon(&dir).to_string_lossy().into_owned());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A root whose own name carries a line break comes back whole.
    ///
    /// The stamp is a line-delimited text file, and the root used to be
    /// written into it as plain text on a line of its own. A root is
    /// bytes, and a root can carry a line break -- and a line break in a
    /// line-delimited file is the end of the line. So the root came back
    /// cut in half, the index never matched its own tree, and the daemon
    /// walked the tree again on every start.
    #[test]
    fn a_root_with_a_line_break_comes_back_whole() {
        let dir = tree("linebreak");
        let idx_path = dir.join("cache.idx");
        let mut built = build(&dir);
        // The root's own name carries a line break. The directory need
        // not exist: the canonical form of a path that is not there is
        // the path itself, and the stamp is what carries the root.
        let root = dir.join("ro\not");
        save(&mut built, &root, &idx_path).expect("save");

        // The stamp is one line per field, and the root's line break is
        // not one of them.
        let stamp = std::fs::read_to_string(stamp_path(&idx_path)).expect("read the stamp");
        assert_eq!(stamp.lines().count(), 4, "the stamp is four lines:\n{stamp}");

        // And the root comes back whole: this index is the one this tree
        // was built for, and it is served.
        assert!(outcome(&root, &idx_path, None).is_ok(), "the index was not reused for its own tree");
        let (_, saved) = load_saved(&idx_path).expect("load");
        assert_eq!(saved, canon(&root).to_string_lossy().into_owned());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A root cannot name another tree.
    ///
    /// The root used to be written into the stamp as plain text, so a
    /// root whose own name carried the words `root /etc` put a second
    /// `root` line in the stamp -- and the loader, which believed the
    /// last one, served the index for a tree nobody asked about.
    #[test]
    fn a_root_cannot_name_another_tree() {
        let dir = tree("spoof");
        let idx_path = dir.join("cache.idx");
        let mut built = build(&dir);
        let root = dir.join("x\nroot /etc");
        save(&mut built, &root, &idx_path).expect("save");

        let (_, saved) = load_saved(&idx_path).expect("load");
        assert_eq!(saved, canon(&root).to_string_lossy().into_owned());
        // The tree it names is this one -- not the one the root's own name
        // spells out.
        assert_ne!(saved, "/etc", "the stamp chose the tree: {saved}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A stamp written by the previous program is still read.
    ///
    /// The index a user already has on disk is the index the program
    /// uses: the version says how the root is spelled, and both
    /// spellings are read.
    #[test]
    fn a_stamp_from_the_previous_program_is_still_read() {
        let dir = tree("v1");
        let idx_path = dir.join("cache.idx");
        let mut built = build(&dir);
        save(&mut built, &dir, &idx_path).expect("save");

        // The stamp as version 1 wrote it: the root in plain text.
        let v1 = format!("rgy-index-meta 1\nroot {}\nentries {}\nbuilt {}\n", canon(&dir).to_string_lossy(), built.n, now_unix());
        std::fs::write(stamp_path(&idx_path), v1).expect("write the old stamp");

        assert_eq!(outcome(&dir, &idx_path, None).expect("no miss"), built.n);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The digest is over the root's bytes, not over its text.
    ///
    /// Two roots whose text is the same and whose bytes are not are two
    /// roots. The text of both is the lossy text, and one index file for
    /// two trees is an index that answers for the wrong tree.
    #[cfg(unix)]
    #[test]
    fn two_roots_that_differ_only_in_bytes_that_are_not_text_are_two_roots() {
        use std::os::unix::ffi::OsStrExt;
        let a = Path::new(std::ffi::OsStr::from_bytes(b"/tmp/a\xff"));
        let b = Path::new(std::ffi::OsStr::from_bytes(b"/tmp/a\xfe"));
        assert_ne!(root_bytes(a), root_bytes(b));
        assert_ne!(fnv1a(&root_bytes(a)), fnv1a(&root_bytes(b)));
    }

    /// The same, on Windows: a path there is UTF-16, and a surrogate
    /// with no partner is a byte the lossy text cannot carry.
    #[cfg(windows)]
    #[test]
    fn two_roots_that_differ_only_in_bytes_that_are_not_text_are_two_roots() {
        use std::os::windows::ffi::OsStringExt;
        let a = PathBuf::from(std::ffi::OsString::from_wide(&[b'a' as u16, 0xD800]));
        let b = PathBuf::from(std::ffi::OsString::from_wide(&[b'a' as u16, 0xDC00]));
        assert_ne!(root_bytes(&a), root_bytes(&b));
        assert_ne!(fnv1a(&root_bytes(&a)), fnv1a(&root_bytes(&b)));
    }

    #[test]
    fn a_stamp_without_a_blob_is_refused() {
        let dir = tree("noblob");
        let idx_path = dir.join("cache.idx");
        save(&mut build(&dir), &dir, &idx_path).expect("save");
        std::fs::remove_file(&idx_path).expect("drop the blob");

        assert_eq!(outcome(&dir, &idx_path, None).unwrap_err(), Miss::BadIndex);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
