// SPDX-License-Identifier: Apache-2.0

//! The ranking corpus: what the results should be, in order.
//!
//! The name bench measures how long a query takes. It says nothing about
//! whether the answer is the right one, and a ranking change with no
//! measure of quality is a change with no evidence.
//!
//! So this is that measure: a fixture tree with names that pull in
//! different directions -- a perfect name match in a directory nobody
//! cares about, a good name match in the directory the user does, a
//! recent file and an old one with the same name, a kanji name, a hanzi
//! name -- and, for each query, the order the results should come in.
//!
//! A case that fails is a ranking bug, and the corpus says which. Every
//! ranking change (the priority ladder, the weights, the sort choice,
//! the priority rules) is measured against it.
//!
//! The expectations are what a person wants, written down. Where rgy
//! disagrees today, the disagreement is the finding.

use rgy_discovery::walk::{KIND_DIR, KIND_FILE, NONE};
use rgy_name_core::live::Live;
use rgy_name_core::name_index::Index;
use rgy_name_core::name_query::{Query, Searcher};

/// One file in the fixture tree.
struct F(&'static str, u8);

/// The fixture tree.
///
/// The directories carry the priorities: `Documents` is where the user
/// works, `node_modules` is a build artefact directory that happens to
/// hold a file whose name matches perfectly, and `Applications` is the
/// system.
fn fixture() -> Live {
    let mut names: Vec<u8> = Vec::new();
    let mut ents: Vec<rgy_discovery::walk::RawEnt> = Vec::new();

    // The root listing, id 0, holds the directories.
    let root_names = b"Documentsnode_modulesApplicationsDownloadsArchive";
    let root: Vec<F> =
        vec![F("Documents", KIND_DIR), F("node_modules", KIND_DIR), F("Applications", KIND_DIR), F("Downloads", KIND_DIR), F("Archive", KIND_DIR)];
    let mut off = 0u32;
    for (i, F(name, _)) in root.iter().enumerate() {
        ents.push(rgy_discovery::walk::RawEnt {
            name_off: off,
            name_len: name.len() as u16,
            kind: KIND_DIR,
            size: 0,
            mtime: 1_700_000_000,
            // The child listing id: 1..=5, and the root's id is 0.
            child: (i + 1) as u32,
        });
        off += name.len() as u32;
    }
    names.extend_from_slice(root_names);

    // The directories. The id is the listing id the root pointed at.
    let dirs: Vec<(u32, Vec<F>)> = vec![
        (1, vec![F("configuration.txt", KIND_FILE), F("invoice.pdf", KIND_FILE), F("日本語.txt", KIND_FILE), F("中文.pdf", KIND_FILE)]),
        (2, vec![F("config.json", KIND_FILE), F("readme.md", KIND_FILE)]),
        (3, vec![F("Safari.app", KIND_DIR)]),
        (4, vec![F("report_2024.pdf", KIND_FILE)]),
        (5, vec![F("report_2019.pdf", KIND_FILE)]),
    ];
    let mut listings = vec![rgy_discovery::walk::Listing { id: 0, names, ents }];
    // The age bonus is relative to now, so the fixture's timestamps are
    // relative to now too: a fixed timestamp would be "recent" for a
    // while and then quietly become "old", and the corpus would rot
    // instead of failing.
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as u32).unwrap_or(1_700_000_000);
    let recent = now - 3600;
    let old = now - 10 * 365 * 24 * 3600;
    for (id, files) in dirs {
        let mut names: Vec<u8> = Vec::new();
        let mut ents: Vec<rgy_discovery::walk::RawEnt> = Vec::new();
        let mut off = 0u32;
        for F(name, kind) in files {
            names.extend_from_slice(name.as_bytes());
            ents.push(rgy_discovery::walk::RawEnt {
                name_off: off,
                name_len: name.len() as u16,
                kind,
                size: 1000,
                // The recent file is an hour old and the old one ten
                // years old; the age bonus reads this.
                mtime: if name.ends_with("2019.pdf") { old } else { recent },
                child: NONE,
            });
            off += name.len() as u32;
        }
        listings.push(rgy_discovery::walk::Listing { id, names, ents });
    }

    Live::new(Index::build(listings, 0, 1_700_000_000, b"/"))
}

/// The results of `query`, as paths in order, with their scores.
fn results(live: &Live, query: &str) -> Vec<(String, i32)> {
    let searcher = Searcher { live };
    let q = Query::parse(query, "").expect("the query parses");
    searcher
        .search(&q)
        .iter()
        .map(|h| {
            let mut buf = Vec::new();
            live.base.path(h.idx as usize, &mut buf);
            (String::from_utf8_lossy(&buf).into_owned(), h.score())
        })
        .collect()
}

/// The cases: a query, and the paths that must come first, in order.
///
/// Only the head of the order is pinned. The rest of the results are
/// not what the case is about, and pinning them would make the corpus
/// break on changes nobody was testing.
const CASES: &[(&str, &[&str])] = &[
    (
        // The priority ladder, in one case: `config.json` is the better
        // name match, and `configuration.txt` is the file the user wants.
        // The name says "config" exactly; the other says it as a prefix.
        // A priority that is only a term added to the score loses this
        // one, and a priority that is a tier wins it.
        "config",
        &["/Documents/configuration.txt", "/node_modules/config.json"],
    ),
    (
        // The recent file wins over the old one with the same name.
        "report",
        &["/Downloads/report_2024.pdf", "/Archive/report_2019.pdf"],
    ),
    (
        // A kanji name, found by typing it in romaji.
        "nihongo",
        &["/Documents/日本語.txt"],
    ),
    (
        // A hanzi name, found by typing it in pinyin.
        "zhongwen",
        &["/Documents/中文.pdf"],
    ),
];

/// The ranking corpus, as a test: every case must come out in order.
#[test]
fn the_ranking_corpus_is_satisfied() {
    let live = fixture();
    let mut wrong = Vec::new();
    for (query, want) in CASES {
        let got = results(&live, query);
        for (i, expect) in want.iter().enumerate() {
            let at = got.iter().position(|(p, _)| p == expect);
            match at {
                // Not in the results at all: the query does not reach it.
                None => wrong.push(format!("`{query}`: `{expect}` is not among the results ({got:?})")),
                // There, but not where it belongs: the order is wrong.
                Some(at) if at != i => wrong.push(format!("`{query}`: `{expect}` is at {at}, not {i} ({got:?})")),
                Some(_) => {}
            }
        }
    }
    assert!(wrong.is_empty(), "the ranking corpus is not satisfied:\n{}", wrong.join("\n"));
}
