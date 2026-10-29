// SPDX-License-Identifier: Apache-2.0

//! FreeBSD discovery backend:
//! the `getdirentries` bulk walk as a [`rgy_discovery::Source`], selected by
//! `discovery-dispatch` on this target. The whole backend is
//! `cfg(target_os = "freebsd")`; on other hosts this crate compiles empty
//! and the portable walk stays the Source.
//!
//! The crate is `no_std`: it asks the kernel through calls it declares
//! itself and keeps its paths as bytes, the shape the rest of the tree
//! speaks.
//!
//! Verified against the sequential oracle on a real FreeBSD kernel by
//! `freebsd_reader_matches_sequential_oracle` below — the phase-4 suite
//! (P2.3); `cargo check --target x86_64-unknown-freebsd` is the cross-host
//! floor.

// The `no_std` comes first on purpose: a crate-level `cfg` that is false
// empties the crate, and an emptied crate has no `no_std` to declare — so
// the flag has to be set before the gate, or this crate would want the
// standard library on a target that has none.
#![no_std]
#![cfg(target_os = "freebsd")]

extern crate alloc;

mod walk;

pub use walk::{FreeBsdSource, scan};

#[cfg(test)]
mod tests {
    extern crate std;

    use super::scan;
    use alloc::string::String;
    use alloc::vec::Vec;
    use std::collections::BTreeMap;

    use rgy_discovery::walk::{KIND_DIR, KIND_FILE, Listing, NONE, scan_sequential};

    type Rows = BTreeMap<String, Vec<(String, u8, u64, u32, Option<String>)>>;

    fn fixture(tag: &str) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(alloc::format!("rgy-fbsd-{tag}-{}", std::process::id()));
        let _ = std::fs::create_dir_all(dir.join("a/aa"));
        let _ = std::fs::create_dir_all(dir.join("b"));
        std::fs::write(dir.join("top.txt"), b"t").unwrap();
        std::fs::write(dir.join("a/one.txt"), b"11").unwrap();
        std::fs::write(dir.join("a/aa/deep.txt"), b"ddd").unwrap();
        std::fs::write(dir.join("b/two.txt"), b"22").unwrap();
        dir
    }

    fn name_of(l: &Listing, e: &rgy_discovery::walk::RawEnt) -> String {
        let s = &l.names[e.name_off as usize..(e.name_off + e.name_len as u32) as usize];
        String::from_utf8_lossy(s).into_owned()
    }

    /// Directory path -> rows, so the comparison does not depend on the
    /// order `getdirentries` happened to return entries in (the same
    /// normalization `parallel_walk_matches_sequential_oracle` uses).
    fn normalize(ls: &[Listing]) -> Rows {
        let mut paths: Vec<Option<String>> = alloc::vec![None; ls.len()];
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
                paths[child] = Some(alloc::format!("{base}/{}", name_of(&ls[id], ent)));
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

    /// The acceptance rule: the FreeBSD reader
    /// matches `scan_sequential` — same names, kinds, sizes, mtimes and
    /// child links — so the backend is a speed decision, never a semantics
    /// one. Runs on the phase-4 kernel (P2.3).
    #[test]
    fn freebsd_reader_matches_sequential_oracle() {
        let dir = fixture("diff");
        let root = dir.to_string_lossy().as_bytes().to_vec();
        let want = normalize(&scan_sequential(&root));
        assert!(!want.is_empty(), "fixture produced no blocks");
        assert_eq!(
            want.len(),
            4,
            "fixture must exercise the root plus three subdirectories"
        );
        let got = normalize(&scan(dir.to_string_lossy().as_bytes()));
        assert_eq!(
            want, got,
            "the getdirentries reader diverged from the oracle"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A missing root answers no blocks rather than panicking — the same
    /// contract the portable walk holds (`a_root_that_lists_nothing_...`).
    #[test]
    fn an_absent_root_yields_no_blocks() {
        let root = std::env::temp_dir().join("rgy-fbsd-absent-zzz");
        assert!(scan(root.to_string_lossy().as_bytes()).is_empty());
    }

    /// Kinds survive the d_type path: a symlink and a directory must not
    /// come back as `KIND_OTHER` on the lstat-first logic.
    #[test]
    fn kinds_match_the_stat() {
        use std::os::unix::fs::symlink;
        let dir = fixture("kinds");
        symlink("top.txt", dir.join("alink")).unwrap();
        let ls = scan(dir.to_string_lossy().as_bytes());
        let top = ls
            .iter()
            .find(|l| l.id == 0)
            .expect("listing 0 is the root");
        let kind_of = |name: &str| {
            top.ents
                .iter()
                .find(|e| name_of(top, e) == name)
                .map(|e| e.kind)
        };
        assert_eq!(kind_of("top.txt"), Some(KIND_FILE));
        assert_eq!(kind_of("a"), Some(KIND_DIR));
        assert_eq!(kind_of("alink"), Some(rgy_discovery::walk::KIND_LINK));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
