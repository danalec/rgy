// SPDX-License-Identifier: Apache-2.0

//! The `Source` selector (`docs/design.md` §4).
//!
//! One walk per platform, chosen by `cfg`, with the portable walk as the
//! answer everywhere else and as the fallback whenever a native backend
//! refuses: the pick is a speed decision, never a semantics one. Every
//! backend emits the same [`Listing`] block stream as
//! [`rgy_discovery::walk::scan_sequential`], which the differential test below
//! proves per platform.
//!
//! This crate exists because the platform backends depend on `discovery`
//! for the traits, so the selector that knows all of them cannot live
//! there without a dependency cycle.
//!
//! It also holds the one place the catch-up rule lives: what a restart can
//! do on each OS ([`catch_up_for`]). The Windows, macOS, Linux and FreeBSD
//! paths cannot disagree about "replayed" vs "relisted", because there is
//! only one function that answers it.
//!
//! # `no_std`
//!
//! The crate is `no_std`: the pick is a `cfg` and a decision, and both
//! are portable. The portable walk is `no_std` too — it asks the operating
//! system directly — so the fallback exists in every build, on every
//! target. What the `std` feature buys is the environment (the
//! `ERS_USE_RECORDS` switch on Windows) and the threads the parallel walk
//! fans out over.

#![no_std]

extern crate alloc;
#[cfg(feature = "std")]
extern crate std;

#[cfg(windows)]
use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec::Vec;

/// The trait the backends implement, where there is one.
#[cfg(any(
    windows,
    target_os = "linux",
    target_os = "macos",
    target_os = "freebsd"
))]
use rgy_discovery::Source;
use rgy_discovery::walk::{Cancel, Cancelled, Listing, Progress};
use rgy_proto::CatchUp;

/// The platform whose change feed decides how a restart catches up.
///
/// The selector already picks a backend per platform ([`pick`]); this is
/// the same choice named as a value, so the rule that follows it can be
/// stated once and tested on any host rather than only on the host the
/// test happens to run on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Platform {
    /// NTFS and the USN journal.
    Windows,
    /// HFS+/APFS and FSEvents.
    MacOs,
    /// ext4 and friends, and inotify.
    Linux,
    /// ZFS/UFS and kqueue.
    FreeBsd,
    /// Every other target: the portable walk, and no change feed at all.
    Other,
}

/// The platform this build runs on.
pub fn current_platform() -> Platform {
    #[cfg(windows)]
    {
        Platform::Windows
    }
    #[cfg(target_os = "macos")]
    {
        Platform::MacOs
    }
    #[cfg(target_os = "linux")]
    {
        Platform::Linux
    }
    #[cfg(target_os = "freebsd")]
    {
        Platform::FreeBsd
    }
    #[cfg(not(any(
        windows,
        target_os = "macos",
        target_os = "linux",
        target_os = "freebsd"
    )))]
    {
        Platform::Other
    }
}

/// How the index catches up after a restart, per platform.
///
/// This is the whole of `docs/design.md` §3, rule 5 ("live-feed honesty"),
/// in one place:
///
/// * **Windows** (the USN journal) and **macOS** (the FSEvents event id)
///   keep the OS's history, so a restart can resume that journal at the
///   saved watermark and apply what changed while the daemon was off — the
///   catch-up can be **replayed**;
/// * **Linux** (inotify) and **FreeBSD** (kqueue) keep no such history:
///   there is no journal to resume, so the only way to catch up is to walk
///   the tree again and diff it — the catch-up is **relisted**.
///
/// The answer is what the *platform* can do, not what one particular
/// restart did. A journal can wrap (the feed then reports a rescan and the
/// index is re-listed), and a daemon that starts without a saved index walks
/// the tree whatever the OS could have replayed. It is therefore the
/// capability, and the daemon answers with an observation of its own: it
/// reports "replayed" only when it actually resumed a journal at a saved
/// watermark.
pub fn catch_up_for(platform: Platform) -> CatchUp {
    match platform {
        Platform::Windows | Platform::MacOs => CatchUp::Replayed,
        Platform::Linux | Platform::FreeBsd | Platform::Other => CatchUp::Relisted,
    }
}

/// How the index catches up after a restart on this platform.
pub fn catch_up() -> CatchUp {
    catch_up_for(current_platform())
}

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
    ///
    /// The walk itself needs no `std` — it asks the operating system
    /// directly (`rgy_discovery::dir`) — so this variant exists in every
    /// build, on every target.
    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "freebsd")))]
    Portable,
    #[cfg(target_os = "linux")]
    Linux(rgy_discovery_linux::LinuxSource),
    #[cfg(target_os = "macos")]
    Macos(MacSource),
    #[cfg(target_os = "freebsd")]
    FreeBsd(rgy_discovery_freebsd::FreeBsdSource),
    #[cfg(windows)]
    Mft(rgy_discovery_windows::MftSource<'static>),
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
        Ok(rgy_discovery_macos::scan(&self.root, self.threads))
    }
}

/// The [`Source`] for `root`: the native walk on its own platform, the
/// portable walk elsewhere. `threads` is the portable walk's directory-read
/// fan-out; backends with their own parallelism ignore it.
#[cfg_attr(
    not(any(
        windows,
        target_os = "linux",
        target_os = "macos",
        target_os = "freebsd"
    )),
    allow(dead_code)
)]
pub struct AnySource {
    root: Vec<u8>,
    threads: usize,
    kind: Kind,
}

/// Build the platform [`Source`] for `root` (docs/design.md §2.2).
pub fn source_for(root: Vec<u8>, threads: usize) -> AnySource {
    let kind = pick(&root, threads);
    AnySource {
        root,
        threads,
        kind,
    }
}

fn pick(root: &[u8], threads: usize) -> Kind {
    #[cfg(target_os = "linux")]
    {
        let _ = threads;
        Kind::Linux(rgy_discovery_linux::LinuxSource {
            root: root.to_vec(),
        })
    }
    #[cfg(target_os = "freebsd")]
    {
        let _ = threads;
        Kind::FreeBsd(rgy_discovery_freebsd::FreeBsdSource {
            root: root.to_vec(),
        })
    }
    #[cfg(target_os = "macos")]
    {
        Kind::Macos(MacSource {
            root: root.to_vec(),
            threads,
        })
    }
    #[cfg(windows)]
    {
        let _ = threads;
        if let Some(mft) = try_mft(root) {
            return Kind::Mft(mft);
        }
        Kind::Portable
    }
    #[cfg(not(any(
        windows,
        target_os = "linux",
        target_os = "macos",
        target_os = "freebsd"
    )))]
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
fn try_mft(root: &[u8]) -> Option<rgy_discovery_windows::MftSource<'static>> {
    let path = core::str::from_utf8(root).ok()?;
    if !is_volume_root(path) {
        return None;
    }
    // The volume lives for the process; leak it so the MftSource owns a
    // 'static borrow (the same trade `searchd`'s `open_watch` makes).
    let volume: &'static rgy_discovery_windows::volume::Volume = Box::leak(Box::new(
        rgy_discovery_windows::volume::Volume::open(path).ok()?,
    ));
    let indexer = mft_indexer(volume);
    Some(rgy_discovery_windows::MftSource::new(indexer))
}

/// The indexer the MFT walk reads through.
///
/// The escape hatch that forces the record path instead of the streamed
/// one is `ERS_USE_RECORDS`, and it is read *here* rather than inside the
/// backend: a library that reads the environment is a library that cannot
/// be tested, and the backend has no business knowing how it was built.
/// With the `std` feature off there is no environment to read, and the
/// streamed path is the one that is used.
#[cfg(windows)]
fn mft_indexer(
    volume: &'static rgy_discovery_windows::volume::Volume,
) -> rgy_discovery_windows::indexer::Indexer<'static> {
    #[cfg(feature = "std")]
    let streamed = std::env::var_os("ERS_USE_RECORDS").is_none();
    #[cfg(not(feature = "std"))]
    let streamed = true;
    rgy_discovery_windows::indexer::Indexer::with_records(volume, streamed)
}

/// Whether the path names a volume root.
///
/// The MFT walk enumerates a volume from its root, so it answers only for
/// a root. A drive and its separator is one ("C:\", "C:"), a UNC share's
/// root is one ("\\server\share"), and the POSIX root is one ("/").
///
/// The rule is about the *name*: a path that names a root but whose
/// volume cannot be opened is caught by the open that follows, and a path
/// that does not name a root is caught here. Both end in the portable
/// walk.
#[cfg(windows)]
fn is_volume_root(path: &str) -> bool {
    let p = path.trim_end_matches(['\\', '/']);
    // The drive and nothing else.
    if p.len() == 2 && p.as_bytes()[1] == b':' {
        return true;
    }
    // A UNC share: a server and a share, and no path under them.
    if let Some(unc) = p.strip_prefix("\\\\") {
        let mut parts = unc.split(['\\', '/']);
        return parts.next().is_some_and(|s| !s.is_empty())
            && parts.next().is_some_and(|s| !s.is_empty())
            && parts.next().is_none();
    }
    // The POSIX root.
    p.is_empty()
}

#[cfg_attr(
    not(any(
        windows,
        target_os = "linux",
        target_os = "macos",
        target_os = "freebsd"
    )),
    allow(dead_code)
)]
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
            Kind::Portable => Ok(rgy_discovery::walk::scan(&self.root, self.threads)),
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
    // On a bare-metal build there is no native backend, so the token and
    // the progress callback have nobody to hand: the parameters are still
    // part of the signature (the callers are the same everywhere), they
    // are just not read there.
    #[cfg_attr(
        not(any(
            windows,
            target_os = "linux",
            target_os = "macos",
            target_os = "freebsd"
        )),
        allow(unused_variables)
    )]
    pub fn enumerate_cancellable(
        &self,
        cancel: &Cancel,
        on_progress: &(dyn Fn(Progress) + Sync),
    ) -> Result<Vec<Listing>, Cancelled> {
        match &self.kind {
            #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "freebsd")))]
            Kind::Portable => {
                let p = rgy_discovery::PortableSource::new(self.root.clone(), self.threads);
                p.enumerate_cancellable(cancel, on_progress)
            }
            // The native backends, where this build has one. The arm is
            // gone where there is none — a bare-metal build, where the
            // `Kind` enum has a single variant and a catch-all would be
            // an unreachable pattern.
            #[cfg(any(
                windows,
                target_os = "linux",
                target_os = "macos",
                target_os = "freebsd"
            ))]
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
                    Err(_) => self.fallback(cancel, on_progress),
                }
            }
        }
    }

    /// The portable walk's answer, for a native backend that refused.
    ///
    /// The walk asks the operating system directly and needs no `std` to
    /// do it, so there is always something to fall back to.
    fn fallback(
        &self,
        cancel: &Cancel,
        on_progress: &(dyn Fn(Progress) + Sync),
    ) -> Result<Vec<Listing>, Cancelled> {
        let p = rgy_discovery::PortableSource::new(self.root.clone(), self.threads);
        p.enumerate_cancellable(cancel, on_progress)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(test)]
    extern crate std;

    use alloc::format;
    use alloc::vec;
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    use rgy_discovery::walk::{NONE, RawEnt, scan_sequential};

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

    /// The catch-up rule as `docs/design.md` §3 states it: a platform whose
    /// OS keeps a journal replays it, a platform whose OS keeps none
    /// relists the tree. Asserted for every platform, so the rule is the
    /// same on every host -- not only the one the test runs on.
    #[test]
    fn the_catch_up_rule_is_the_one_the_design_doc_states() {
        assert_eq!(catch_up_for(Platform::Windows), CatchUp::Replayed);
        assert_eq!(catch_up_for(Platform::MacOs), CatchUp::Replayed);
        assert_eq!(catch_up_for(Platform::Linux), CatchUp::Relisted);
        assert_eq!(catch_up_for(Platform::FreeBsd), CatchUp::Relisted);
        // No native backend and no feed: the tree is walked again.
        assert_eq!(catch_up_for(Platform::Other), CatchUp::Relisted);
    }

    /// The rule is stated once, so the spelling cannot drift from it: the
    /// two values the wire carries are the two the doc spells.
    #[test]
    fn the_catch_up_spellings_are_the_ones_the_doc_uses() {
        assert_eq!(CatchUp::Replayed.as_str(), "replayed");
        assert_eq!(CatchUp::Relisted.as_str(), "relisted");
    }

    /// The platform this build targets is the one the selector reports --
    /// a mis-wired `cfg` here would give every platform the wrong answer.
    #[test]
    fn the_running_platform_is_the_one_the_build_targets() {
        #[cfg(windows)]
        assert_eq!(current_platform(), Platform::Windows);
        #[cfg(target_os = "macos")]
        assert_eq!(current_platform(), Platform::MacOs);
        #[cfg(target_os = "linux")]
        assert_eq!(current_platform(), Platform::Linux);
        #[cfg(target_os = "freebsd")]
        assert_eq!(current_platform(), Platform::FreeBsd);
        #[cfg(not(any(
            windows,
            target_os = "macos",
            target_os = "linux",
            target_os = "freebsd"
        )))]
        assert_eq!(current_platform(), Platform::Other);
        // And the shortcut agrees with the rule, not with a second copy of it.
        assert_eq!(catch_up(), catch_up_for(current_platform()));
    }

    /// A volume root is a root, and a path under one is not: the MFT walk
    /// enumerates the whole volume, so it may only answer for a root.
    #[cfg(windows)]
    #[test]
    fn only_a_volume_root_names_a_volume() {
        assert!(super::is_volume_root("C:\\"));
        assert!(super::is_volume_root("C:"));
        assert!(super::is_volume_root("c:\\"));
        assert!(super::is_volume_root("\\\\server\\share"));
        assert!(super::is_volume_root("/"));
        assert!(!super::is_volume_root("C:\\Users"));
        assert!(!super::is_volume_root("\\\\server\\share\\dir"));
        assert!(!super::is_volume_root("relative"));
    }
}
