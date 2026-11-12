// SPDX-License-Identifier: Apache-2.0

//! The v2 name-index blob is an on-disk contract.
//!
//! A saved index from an earlier build has to load in this one. The layout
//! is data, not code (`FSIDX015`, a 4 KiB header, 22 aligned sections), so
//! it is pinned here byte for byte for the current writer, and separately
//! a blob written by the *previous* implementation is kept as a legacy
//! fixture that this reader must accept and answer from.
//!
//! The two blobs are not identical: 12 bytes in the extension-slot and
//! name-prior tables differ, because those tables are ranked at build time
//! and the ranking is an implementation choice. They are stored *in* the
//! blob, so a reader never recomputes them — a legacy blob is self-
//! describing and loads unchanged. The test below proves that.
//!
//! Regenerate the current golden with `RGY_BLOB_WRITE=1 cargo test -p
//! rgy-name-core --test index_blob_golden`. The legacy fixture is never
//! regenerated: it is the record of what an earlier build wrote.

use rgy_discovery::walk::{KIND_DIR, KIND_FILE, Listing, NONE, RawEnt};
use rgy_name_core::name_index::Index;

const GOLDEN: &[u8] = include_bytes!("index_blob.expected");
const LEGACY: &[u8] = include_bytes!("index_blob_legacy.expected");

struct F(&'static str, u8, u64, u32);

fn fixture() -> Vec<Listing> {
    let dirs = ["Documents", "src", "Archive"];
    let mut names = Vec::new();
    let mut ents = Vec::new();
    for (i, d) in dirs.iter().enumerate() {
        ents.push(RawEnt {
            name_off: names.len() as u32,
            name_len: d.len() as u16,
            kind: KIND_DIR,
            size: 0,
            mtime: 1_700_000_000,
            child: (i + 1) as u32,
        });
        names.extend_from_slice(d.as_bytes());
    }

    let children: [(u32, &[F]); 3] = [
        (
            1,
            &[
                F("configuration.txt", KIND_FILE, 1000, 1_700_000_100),
                F("invoice.pdf", KIND_FILE, 4200, 1_700_000_200),
                F("日本語.txt", KIND_FILE, 900, 1_700_000_300),
            ],
        ),
        (
            2,
            &[
                F("main.rs", KIND_FILE, 2400, 1_700_000_400),
                F("lib.rs", KIND_FILE, 1800, 1_700_000_500),
                F("data.bin", KIND_FILE, 1 << 20, 1_700_000_600),
            ],
        ),
        (3, &[F("report_2019.pdf", KIND_FILE, 2600, 1_699_000_000), F("dat", KIND_FILE, 32, 1_700_000_700)]),
    ];

    let mut out = vec![Listing { id: 0, names, ents }];
    for (id, files) in children {
        let mut n = Vec::new();
        let mut e = Vec::new();
        for F(name, kind, size, mtime) in files {
            e.push(RawEnt { name_off: n.len() as u32, name_len: name.len() as u16, kind: *kind, size: *size, mtime: *mtime, child: NONE });
            n.extend_from_slice(name.as_bytes());
        }
        out.push(Listing { id, names: n, ents: e });
    }
    out
}

fn build() -> Index {
    Index::build(fixture(), 7, 1_700_000_800, b"/")
}

fn paths(i: &Index) -> Vec<Vec<u8>> {
    (0..i.n)
        .map(|e| {
            let mut p = Vec::new();
            i.path(e, &mut p);
            p
        })
        .collect()
}

#[test]
fn the_v2_blob_is_byte_for_byte_stable() {
    let bytes = build().to_bytes();

    if std::env::var_os("RGY_BLOB_WRITE").is_some() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests").join("index_blob.expected");
        std::fs::write(&path, &bytes).expect("the golden is writable");
        return;
    }

    assert_eq!(bytes.len(), GOLDEN.len(), "the v2 blob changed size; a saved index from an earlier build would not load");
    assert_eq!(bytes, GOLDEN, "the v2 blob changed; if this is intended, regenerate with RGY_BLOB_WRITE=1 and say why");
}

/// The compatibility that actually matters: a blob an earlier build wrote
/// still loads, still round-trips, and still answers the same paths.
#[test]
fn a_legacy_blob_still_loads_and_answers() {
    let legacy = Index::from_bytes(LEGACY).expect("a blob from the previous implementation must load");
    assert_eq!(legacy.n, build().n, "the entry count survives the load");
    assert_eq!(legacy.d, build().d, "the directory count survives the load");
    assert_eq!(legacy.u, build().u, "the distinct-name count survives the load");
    assert_eq!(paths(&legacy), paths(&build()), "a legacy blob answers the same paths as a fresh build");
    assert_eq!(legacy.to_bytes(), LEGACY, "loading and re-serialising a legacy blob changes no bytes");
}

/// Whatever wrote the blob, this reader must accept it and answer paths.
#[test]
fn a_saved_blob_loads_and_answers_paths() {
    let bytes = build().to_bytes();
    let back = Index::from_bytes(&bytes).expect("a blob this build wrote must load");
    let mut p = Vec::new();
    back.path(4, &mut p);
    assert!(!p.is_empty(), "a loaded index still reconstructs paths: {p:?}");
}
