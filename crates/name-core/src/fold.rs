// SPDX-License-Identifier: Apache-2.0

//! The comparison fold: what a name and a query are compared as.
//!
//! ## The rule
//!
//! The fold rule: *every comparison runs on a fold of the
//! path (ASCII case-fold + NFC); display and I/O keep the original
//! bytes/UTF-16.* This module is the NFC half of that fold, and the
//! ASCII half already exists where the bytes are compared
//! (`name_query::fold`).
//!
//! ## Why the fold exists
//!
//! macOS stores a file name in NFD -- "café" is "cafe" plus a combining
//! acute (U+0301) -- while a person types NFC, where that name is one
//! character (U+00E9). The two are different byte sequences, so comparing
//! the typed query against the stored name byte for byte misses a file
//! that exists. That is parity row 1.12.
//!
//! ## What the fold is not
//!
//! It is not a case-folder, and it does not change what is stored. The
//! index keeps the bytes the filesystem gave: the name that is displayed,
//! and the path that is opened, are the original ones. The fold is a view
//! for comparison, and it is applied to **both sides** of a comparison --
//! the stored name and the typed query -- because two strings are equal
//! only if their folds are.
//!
//! ## Why the case-fold stays where it is
//!
//! The ASCII case-fold is not applied to the name's bytes here, because
//! the name's case carries meaning for the scorer: a camelCase boundary is
//! a scoring bonus, and a name whose bytes were lowercased would lose it.
//! The comparison folds the case as it compares (`name_query::fold`), and
//! the fold here only settles the normalization, which has no such
//! meaning to lose.

use crate::nfc::nfc;
use alloc::borrow::Cow;

/// The fold of `bytes`: what a comparison of a stored name against a
/// typed query sees.
///
/// The name is returned untouched when it needs no normalization, which
/// is the common case: an ASCII name, and any name that is already in
/// NFC. A name that is not valid UTF-8 is returned untouched as well --
/// Linux holds arbitrary bytes in a file name, and a byte sequence that
/// is not text cannot be normalized, only compared.
pub fn fold(bytes: &[u8]) -> Cow<'_, [u8]> {
    // An ASCII name has no normalization to do: the case-fold happens at
    // comparison time, and ASCII has no composed/decomposed forms.
    if bytes.is_ascii() {
        return Cow::Borrowed(bytes);
    }
    let Ok(text) = core::str::from_utf8(bytes) else {
        return Cow::Borrowed(bytes);
    };
    match nfc(text) {
        Cow::Borrowed(_) => Cow::Borrowed(bytes),
        Cow::Owned(normalized) => Cow::Owned(normalized.into_bytes()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_ascii_name_is_not_copied() {
        assert!(matches!(fold(b"report.txt"), Cow::Borrowed(_)));
    }

    #[test]
    fn a_decomposed_name_is_composed() {
        // "cafe" + U+0301 is "café".
        assert_eq!(fold("cafe\u{301}.txt".as_bytes()), "café.txt".as_bytes());
        assert!(matches!(fold("cafe\u{301}.txt".as_bytes()), Cow::Owned(_)));
    }

    #[test]
    fn a_composed_name_is_left_alone() {
        assert!(matches!(fold("café.txt".as_bytes()), Cow::Borrowed(_)));
    }

    #[test]
    fn bytes_that_are_not_text_are_left_alone() {
        // Linux holds arbitrary bytes in a name: there is nothing to
        // normalize, and nothing may be lost.
        let raw = [0x66, 0x6f, 0xff, 0x6f];
        assert_eq!(fold(&raw).as_ref(), raw.as_slice());
    }

    #[test]
    fn the_fold_is_idempotent() {
        for name in ["cafe\u{301}.txt", "café.txt", "plain.txt", "\u{1E9B}\u{323}"] {
            let once = fold(name.as_bytes());
            let twice = fold(&once);
            assert_eq!(once, twice, "the fold is not idempotent on {name:?}");
        }
    }

    /// The tests below are the row's own: a name typed in NFC has to match
    /// one the filesystem stored in NFD, and the bytes that come back are
    /// the ones the filesystem gave.
    mod search {
        use crate::live::Live;
        use crate::name_index::Index;
        use crate::name_query::{Query, Searcher};
        use alloc::string::String;
        use alloc::vec;
        use alloc::vec::Vec;
        use rgy_discovery::walk::{KIND_FILE, NONE, RawEnt};

        /// An index with one file, whose name is `name`.
        fn live_with(name: &str) -> Live {
            let mut names = Vec::new();
            names.extend_from_slice(name.as_bytes());
            let ents = vec![RawEnt { name_off: 0, name_len: name.len() as u16, kind: KIND_FILE, size: 10, mtime: 1_700_000_000, child: NONE }];
            let listing = rgy_discovery::walk::Listing { id: 0, names, ents };
            Live::new(Index::build(vec![listing], 0, 1_700_000_000, b"/"))
        }

        /// The path of the first hit, as the index holds it.
        fn path_of(live: &Live, hit: &crate::name_query::Hit) -> Vec<u8> {
            let mut path = Vec::new();
            live.base.path(hit.idx as usize, &mut path);
            path
        }

        #[test]
        fn a_query_typed_in_nfc_matches_a_name_stored_in_nfd() {
            // "café.txt" as macOS stores it: "cafe" and a combining acute.
            let live = live_with("cafe\u{301}.txt");
            let s = Searcher { live: &live };
            let hits = s.search(&Query::parse("caf\u{e9}", "").expect("parses"));
            assert_eq!(hits.len(), 1, "the NFD name did not match the NFC query");
            // The path that comes back is the one the filesystem gave.
            assert_eq!(path_of(&live, &hits[0]), "/cafe\u{301}.txt".as_bytes());
        }

        #[test]
        fn a_query_typed_in_nfd_matches_a_name_stored_in_nfc() {
            let live = live_with("café.txt");
            let s = Searcher { live: &live };
            let hits = s.search(&Query::parse("cafe\u{301}", "").expect("parses"));
            assert_eq!(hits.len(), 1, "the NFC name did not match the NFD query");
            assert_eq!(path_of(&live, &hits[0]), "/café.txt".as_bytes());
        }

        /// The same name, matched by the exact and the prefix modes too:
        /// the fold is on both sides of every comparison, not only the
        /// fuzzy one.
        #[test]
        fn the_exact_and_prefix_modes_fold_as_well() {
            let live = live_with("cafe\u{301}.txt");
            let s = Searcher { live: &live };
            for query in ["'caf\u{e9}", "^caf\u{e9}"] {
                let hits = s.search(&Query::parse(query, "").expect("parses"));
                assert_eq!(hits.len(), 1, "{query:?} did not match the NFD name");
            }
        }

        /// The real filesystem: a file whose name is in NFD, found by a
        /// query typed in NFC, and opened by the path that comes back.
        ///
        /// This is the row's own test -- macOS is the only platform that
        /// stores names in NFD, but the comparison runs everywhere, so the
        /// test runs everywhere too.
        #[test]
        fn a_file_on_the_disk_matches_a_query_typed_in_nfc() {
            let dir = std::env::temp_dir().join(format!("rgy_nfd_{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).expect("the temp dir");
            // The name as macOS holds it.
            let name = "cafe\u{301}.txt";
            std::fs::write(dir.join(name), b"hello").expect("the file");

            let root = dir.as_os_str().as_encoded_bytes().to_vec();
            let live = Live::new(Index::build(rgy_discovery::walk::scan(&root, 1), 0, 1_700_000_000, b"/"));
            let s = Searcher { live: &live };
            let hits = s.search(&Query::parse("caf\u{e9}", "").expect("parses"));
            assert_eq!(hits.len(), 1, "the file on the disk did not match the NFC query");

            // The path that comes back is the one the filesystem gave, so
            // it opens the file: the fold is for matching, not for I/O.
            let path = path_of(&live, &hits[0]);
            assert!(path.ends_with(name.as_bytes()), "the path is not the name on disk: {path:?}");
            // The path is relative to the index root, which is what the
            // root and the path are joined for.
            let relative = String::from_utf8_lossy(&path[1..]).into_owned();
            let opened = std::fs::read(dir.join(relative));
            assert_eq!(opened.as_deref().ok(), Some(&b"hello"[..]), "the path does not open the file");
            let _ = std::fs::remove_dir_all(&dir);
        }
    }
}
