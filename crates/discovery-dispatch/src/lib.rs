//! The `Source` selector (`docs/freebsd-support.md` §5 decision 3).
//!
//! One walk per platform, chosen by `cfg`, with the portable walk as the
//! answer everywhere else and as the fallback whenever a native backend
//! refuses: the pick is a speed decision, never a semantics one. Every
//! backend emits the same [`Listing`] block stream as
//! [`discovery::walk::scan_sequential`], which the differential test below
//! proves per platform.
//!
//! This crate exists because the platform backends depend on `discovery`
//! for the traits, so the selector that knows all of them cannot live
//! there without a dependency cycle.

#[cfg(any(target_os = "linux", target_os = "freebsd"))]
use std::path::PathBuf;

use discovery::walk::{Cancel, Cancelled, Listing, Progress};
use discovery::{PortableSource, Source};

/// The backend [`AnySource`] delegates to. Not public: the only thing a
/// caller may need is [`AnySource::backend_name`].
enum Kind {
    /// The portable walk. It is the pick where the platform has no native
    /// backend of its own, and on Windows where the MFT walk declines a root
    /// that is not a volume root.
    ///
    /// Linux, macOS and FreeBSD always have their own backend, so the
    /// variant does not exist there: their fallback to the portable walk is
    /// a run-time decision (`enumerate_cancellable`), not a pick.
    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "freebsd")))]
    Portable,
    #[cfg(target_os = "linux")]
    Linux(discovery_linux::LinuxSource),
    #[cfg(target_os = "macos")]
    Macos(MacSource),
    #[cfg(target_os = "freebsd")]
    FreeBsd(discovery_freebsd::FreeBsdSource),
    #[cfg(windows)]
    Mft(discovery_windows::MftSource<'static>),
}

/// macOS exposes its walk as a free function (`getattrlistbulk`), not as a
/// `Source` type; this is the thinnest adapter over it.
#[cfg(target_os = "macos")]
struct MacSource {
    root: Vec<u8>,
    threads: usize,
}

#[cfg(target_os = "macos")]
impl Source for MacSource {
    fn enumerate(&self) -> Result<Vec<Listing>, String> {
        Ok(discovery_macos::scan(&self.root, self.threads))
    }
}

/// The [`Source`] for `root`: the native walk on its own platform, the
/// portable walk elsewhere. `threads` is the portable walk's directory-read
/// fan-out; backends with their own parallelism ignore it.
pub struct AnySource {
    root: Vec<u8>,
    threads: usize,
    kind: Kind,
}

/// Build the platform [`Source`] for `root` (docs/freebsd-support.md P2.1).
pub fn source_for(root: Vec<u8>, threads: usize) -> AnySource {
    let kind = pick(&root, threads);
    AnySource { root, threads, kind }
}

/// The root as a path, for the backends that take one. Linux and FreeBSD
/// only: those two walks want a `PathBuf`, and a Unix path is a byte string
/// rather than a UTF-8 string, so the bytes are carried through unchanged.
/// The Windows and macOS backends take the root as it is.
#[cfg(any(target_os = "linux", target_os = "freebsd"))]
fn path_of(root: &[u8]) -> PathBuf {
    use std::os::unix::ffi::OsStringExt;
    PathBuf::from(std::ffi::OsString::from_vec(root.to_vec()))
}

fn pick(root: &[u8], threads: usize) -> Kind {
    #[cfg(target_os = "linux")]
    {
        let _ = threads;
        return Kind::Linux(discovery_linux::LinuxSource { root: path_of(root) });
    }
    #[cfg(target_os = "freebsd")]
    {
        let _ = threads;
        return Kind::FreeBsd(discovery_freebsd::FreeBsdSource { root: path_of(root) });
    }
    #[cfg(target_os = "macos")]
    {
        return Kind::Macos(MacSource {
            root: root.to_vec(),
            threads,
        });
    }
    #[cfg(windows)]
    {
        let _ = threads;
        if let Some(mft) = try_mft(root) {
            return Kind::Mft(mft);
        }
        Kind::Portable
    }
    #[cfg(not(any(windows, target_os = "linux", target_os = "macos", target_os = "freebsd")))]
    {
        let _ = (root, threads);
        Kind::Portable
    }
}

/// The MFT walk answers only when `root` *is* the volume root: it
/// enumerates the whole volume from its root FRN (5), so pointing it at a
/// subtree would answer "everything on the volume", not "everything under
/// root". Anything else — a subtree, a volume that will not open without
/// privileges, a non-NTFS root — keeps the portable walk.
#[cfg(windows)]
fn try_mft(root: &[u8]) -> Option<discovery_windows::MftSource<'static>> {
    let path = std::str::from_utf8(root).ok()?;
    let p = std::path::Path::new(path);
    if p.parent().is_some() {
        return None;
    }
    let vol_root = p.to_str()?;
    // The volume lives for the process; leak it so the MftSource owns a
    // 'static borrow (the same trade `searchd`'s `open_watch` makes).
    let volume: &'static discovery_windows::volume::Volume =
        Box::leak(Box::new(discovery_windows::volume::Volume::open(vol_root).ok()?));
    let indexer = discovery_windows::indexer::Indexer::new(volume);
    Some(discovery_windows::MftSource::new(indexer))
}

impl AnySource {
    /// Which backend answers this source. The differential test asserts it
    /// before comparing, so a silent fallback cannot make a broken backend
    /// look right.
    pub fn backend_name(&self) -> &'static str {
        match &self.kind {
            #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "freebsd")))]
            Kind::Portable => "portable",
            #[cfg(target_os = "linux")]
            Kind::Linux(_) => "linux",
            #[cfg(target_os = "macos")]
            Kind::Macos(_) => "macos",
            #[cfg(target_os = "freebsd")]
            Kind::FreeBsd(_) => "freebsd",
            #[cfg(windows)]
            Kind::Mft(_) => "mft",
        }
    }

    /// The native answer, errors included — no fallback. This is what the
    /// differential test compares; a failing backend fails the test instead
    /// of quietly handing the walk to the portable path.
    pub fn enumerate(&self) -> Result<Vec<Listing>, String> {
        match &self.kind {
            #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "freebsd")))]
            Kind::Portable => Ok(discovery::walk::scan(&self.root, self.threads)),
            #[cfg(target_os = "linux")]
            Kind::Linux(s) => s.enumerate(),
            #[cfg(target_os = "macos")]
            Kind::Macos(s) => s.enumerate(),
            #[cfg(target_os = "freebsd")]
            Kind::FreeBsd(s) => s.enumerate(),
            #[cfg(windows)]
            Kind::Mft(s) => s.enumerate(),
        }
    }

    /// The production path. The token is honoured at the boundaries of a
    /// one-shot native walk (they enumerate in a single call); the portable
    /// walk checks it per directory. A native backend that refuses falls
    /// back to the portable walk rather than taking the daemon down — the
    /// same contract `searchd`'s `open_watch` holds.
    pub fn enumerate_cancellable(
        &self,
        cancel: &Cancel,
        on_progress: &(dyn Fn(Progress) + Sync),
    ) -> Result<Vec<Listing>, Cancelled> {
        match &self.kind {
            #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "freebsd")))]
            Kind::Portable => self.portable().enumerate_cancellable(cancel, on_progress),
            _ => {
                if cancel.is_cancelled() {
                    return Err(Cancelled);
                }
                match self.enumerate() {
                    Ok(ls) => {
                        if cancel.is_cancelled() {
                            Err(Cancelled)
                        } else {
                            Ok(ls)
                        }
                    }
                    Err(_) => self.portable().enumerate_cancellable(cancel, on_progress),
                }
            }
        }
    }

    fn portable(&self) -> PortableSource {
        PortableSource::new(self.root.clone(), self.threads)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    use discovery::walk::{NONE, RawEnt, scan_sequential};

    type Rows = BTreeMap<String, Vec<(String, u8, u64, u32, Option<String>)>>;

    /// The backend this platform's selector must pick for a fixture
    /// subtree. Windows is `portable` on purpose: the MFT walk enumerates
    /// whole volumes, and a fixture root is not one (see `try_mft`).
    #[cfg(target_os = "linux")]
    const EXPECTED: &str = "linux";
    #[cfg(target_os = "macos")]
    const EXPECTED: &str = "macos";
    #[cfg(target_os = "freebsd")]
    const EXPECTED: &str = "freebsd";
    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "freebsd")))]
    const EXPECTED: &str = "portable";

    fn fixture(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("rgy-disp-{tag}-{}", std::process::id()));
        let _ = std::fs::create_dir_all(dir.join("a/aa"));
        let _ = std::fs::create_dir_all(dir.join("b"));
        std::fs::write(dir.join("top.txt"), b"t").unwrap();
        std::fs::write(dir.join("a/one.txt"), b"11").unwrap();
        std::fs::write(dir.join("a/aa/deep.txt"), b"ddd").unwrap();
        std::fs::write(dir.join("b/two.txt"), b"22").unwrap();
        dir
    }

    fn name_of(l: &Listing, e: &RawEnt) -> String {
        let s = &l.names[e.name_off as usize..(e.name_off + e.name_len as u32) as usize];
        String::from_utf8_lossy(s).into_owned()
    }

    /// The same normalization `parallel_walk_matches_sequential_oracle`
    /// uses: directory path -> rows, so the comparison does not depend on
    /// the order a directory happened to be read in.
    fn normalize(ls: &[Listing]) -> Rows {
        let mut paths: Vec<Option<String>> = vec![None; ls.len()];
        if ls.is_empty() {
            return BTreeMap::new();
        }
        paths[0] = Some(String::new());
        for id in 0..ls.len() {
            let Some(base) = paths[id].clone() else {
                continue;
            };
            for ent in &ls[id].ents {
                let child = ent.child as usize;
                if ent.child == NONE || child >= ls.len() {
                    continue;
                }
                paths[child] = Some(format!("{base}/{}", name_of(&ls[id], ent)));
            }
        }
        ls.iter()
            .enumerate()
            .map(|(id, l)| {
                let base = paths[id].clone().unwrap_or_default();
                let rows = l
                    .ents
                    .iter()
                    .map(|e| {
                        (
                            name_of(l, e),
                            e.kind,
                            e.size,
                            e.mtime,
                            if e.child == NONE {
                                None
                            } else {
                                paths.get(e.child as usize).cloned().flatten()
                            },
                        )
                    })
                    .collect();
                (base, rows)
            })
            .collect()
    }

    /// P2.1 acceptance: the selected backend matches the sequential oracle
    /// — names, kinds, sizes, mtimes and child links — and the premise is
    /// checked first, so this cannot pass by silently falling back to the
    /// portable walk.
    #[test]
    fn the_selected_backend_matches_the_sequential_oracle() {
        let dir = fixture("diff");
        let root = dir.to_string_lossy().as_bytes().to_vec();
        let src = source_for(root.clone(), 4);
        assert_eq!(src.backend_name(), EXPECTED, "the selector picked wrong");
        let want = normalize(&scan_sequential(&root));
        assert!(!want.is_empty(), "fixture produced no blocks");
        let got = src.enumerate().expect("the selected backend must answer");
        assert_eq!(want, normalize(&got), "{EXPECTED} diverged from the oracle");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The production path answers the same tree, cancellation token and
    /// all, and the two paths agree.
    #[test]
    fn the_cancellable_path_agrees_with_the_oracle() {
        let dir = fixture("cancellable");
        let root = dir.to_string_lossy().as_bytes().to_vec();
        let src = source_for(root.clone(), 4);
        let want = normalize(&scan_sequential(&root));
        let got = src
            .enumerate_cancellable(&Cancel::new(), &|_| {})
            .expect("an uncancelled walk must answer");
        assert_eq!(want, normalize(&got));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
