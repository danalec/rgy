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
//! trusted. The index file is also never silently promoted past its
//! [`max_age`], because the portable walk has no change feed to tell a fresh
//! tree from a stale one.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::name_index::Index;

fn unix(d: Duration) -> u64 {
    d.as_secs()
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
                write!(f, "index is {age}s old")
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
/// not something this can infer. A zero `max_age` always rebuilds.
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
    if let (Some(built), Some(max_age)) = (built, max_age) {
        // `>=`, not `>`: `--max-age 0` reads as "never reuse", and a stamp
        // written this same second is otherwise younger than zero seconds.
        if now_unix().saturating_sub(built) >= unix(max_age) {
            return Err(Miss::TooOld { built });
        }
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
pub fn default_index_path(root: &Path) -> PathBuf {
    let digest = fnv1a(canon(root).to_string_lossy().as_bytes());
    let base = std::env::var_os("RGY_INDEX_DIR")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("LOCALAPPDATA").or_else(|| std::env::var_os("XDG_CACHE_HOME")).map(PathBuf::from))
        .unwrap_or_else(std::env::temp_dir);
    base.join("rgy").join(format!("{digest:016x}.idx"))
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
    use discovery::PortableSource;
    use discovery::Source;

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
