// SPDX-License-Identifier: Apache-2.0

//! Behavioural oracle for the name engine.
//!
//! This is the characterisation half of replacing the engine: before any
//! implementation changes, the *current* answers to a fixed corpus and a
//! fixed set of queries are recorded, byte for byte, in
//! `engine_oracle.expected`. A replacement must reproduce that file exactly.
//!
//! The report is paths in result order, one query at a time. Scores are
//! deliberately not recorded: the age bonus reads the wall clock, so a score
//! is not a stable fact. The order is what a user sees, and it is what the
//! replacement has to keep.
//!
//! Regenerate the golden with `RGY_ORACLE_WRITE=1 cargo test -p rgy-name-core
//! --test engine_oracle`. That is a deliberate act: a rewrite that changes an
//! answer must be argued for, not silently accepted.

use rgy_discovery::walk::{KIND_DIR, KIND_FILE, NONE, RawEnt};
use rgy_name_core::live::Live;
use rgy_name_core::name_index::Index;
use rgy_name_core::name_query::{Query, Searcher};

const EXPECTED: &str = include_str!("engine_oracle.expected");

struct F(&'static str, u8, u64);

/// A corpus with names that pull in different directions, extensions for the
/// filters, folders for `dir:`, sizes for `size:`, and CJK names for the
/// reading tables. Timestamps are fixed so the age ladder is deterministic.
fn fixture() -> Live {
    let mut names: Vec<u8> = Vec::new();
    let mut ents: Vec<RawEnt> = Vec::new();
    let dirs = ["Documents", "node_modules", "src", "tests", "Archive"];
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

    let children: [(u32, &[F]); 5] = [
        (
            1,
            &[
                F("configuration.txt", KIND_FILE, 1000),
                F("invoice.pdf", KIND_FILE, 4200),
                F("日本語.txt", KIND_FILE, 900),
                F("中文.pdf", KIND_FILE, 800),
                F("report_2024.pdf", KIND_FILE, 3000),
            ],
        ),
        (2, &[F("config.json", KIND_FILE, 500), F("README.md", KIND_FILE, 120), F("f1.dat", KIND_FILE, 64)]),
        (3, &[F("main.rs", KIND_FILE, 2400), F("lib.rs", KIND_FILE, 1800), F("data.bin", KIND_FILE, 1 << 20)]),
        (4, &[F("cli.rs", KIND_FILE, 700), F("serve.rs", KIND_FILE, 1100)]),
        (5, &[F("report_2019.pdf", KIND_FILE, 2600), F("dat", KIND_FILE, 32)]),
    ];

    let mut listings = vec![rgy_discovery::walk::Listing { id: 0, names, ents }];
    for (id, files) in children {
        let mut n: Vec<u8> = Vec::new();
        let mut e: Vec<RawEnt> = Vec::new();
        for F(name, kind, size) in files {
            e.push(RawEnt { name_off: n.len() as u32, name_len: name.len() as u16, kind: *kind, size: *size, mtime: 1_700_000_000, child: NONE });
            n.extend_from_slice(name.as_bytes());
        }
        listings.push(rgy_discovery::walk::Listing { id, names: n, ents: e });
    }

    Live::new(Index::build(listings, 0, 1_700_000_000, b"/"))
}

/// Every token mode, the filter vocabulary, alternation, negation and globs.
const QUERIES: &[&str] = &[
    "config",
    "'config.json",
    "^rep",
    "pdf$",
    "=report",
    "ext:pdf",
    "ext:txt",
    "ext:rs",
    "dir:Documents",
    "dir:src",
    "type:f",
    "type:d",
    "size:>2000",
    "size:<200",
    "re:^rep.*pdf$",
    "report|invoice",
    "!pdf",
    "*.pdf",
    "src/*",
    "README",
    "日本語",
    "nihongo",
    "中文",
    "zhongwen",
    "f1",
    "file",
    "dat$",
    "^src",
    "a",
    "",
    "ext:pdf config",
    "ext:pdf !report",
    "not_a_name_at_all",
];

fn report(live: &Live) -> String {
    let mut out = String::new();
    for q in QUERIES {
        out.push_str(&format!("QUERY {q:?}\n"));
        match Query::parse(q, "") {
            Err(e) => out.push_str(&format!("ERROR {e:?}\n")),
            Ok(parsed) => {
                let hits = Searcher { live }.search(&parsed);
                for h in &hits {
                    let mut buf = Vec::new();
                    live.base.path(h.idx as usize, &mut buf);
                    out.push_str(&String::from_utf8_lossy(&buf));
                    out.push('\n');
                }
                out.push_str(&format!("COUNT {}\n", hits.len()));
            }
        }
    }
    out
}

#[test]
fn the_name_engine_answers_are_byte_for_byte_stable() {
    let live = fixture();
    let got = report(&live);

    if std::env::var_os("RGY_ORACLE_WRITE").is_some() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests").join("engine_oracle.expected");
        std::fs::write(&path, &got).expect("the golden is writable");
        return;
    }

    assert_eq!(got, EXPECTED, "the name engine's answers changed; if this is intended, regenerate with RGY_ORACLE_WRITE=1 and say why");
}
