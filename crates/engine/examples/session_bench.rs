// SPDX-License-Identifier: Apache-2.0

//! The incremental session's cost: a re-ranked keystroke against a fresh one.
//!
//! The question this answers is the one the session asks:
//! does the session save the work of a keystroke, and by how much?
//!
//! The measurement is the same shape as the name bench: a fixed synthetic
//! corpus, several rounds, the best of them. The two halves are the same
//! queries, one searched fresh and one through a session -- so the
//! difference is the session and nothing else.

use rgy_name_core::live::Live;
use rgy_name_core::name_index::Index;
use rgy_name_core::name_query::{Query, Searcher};
use rgy_name_core::session::Session;
use std::time::Instant;

/// The corpus: `n` files in `dirs` folders, of which `big` are large.
///
/// The large ones are what a size filter finds. A query whose names match
/// everything and whose filter cuts the result below the limit is the case
/// the session is for: the fresh scan walks every entry, and the re-rank
/// walks the few that matched.
fn synthetic_listings(n: usize, dirs: usize, big: usize) -> Vec<rgy_discovery::walk::Listing> {
    let per = (n / dirs).max(1);
    let mut out = Vec::with_capacity(dirs + 1);
    let mut root_names = Vec::new();
    let mut root_ents = Vec::new();
    for d in 0..dirs {
        let name = format!("dir_{d:04}");
        let off = root_names.len() as u32;
        root_names.extend_from_slice(name.as_bytes());
        root_ents.push(rgy_discovery::walk::RawEnt {
            name_off: off,
            name_len: name.len() as u16,
            kind: rgy_discovery::walk::KIND_DIR,
            size: 0,
            mtime: 1_700_000_000,
            child: (d as u32) + 1,
        });
    }
    out.push(rgy_discovery::walk::Listing {
        id: 0,
        names: root_names,
        ents: root_ents,
    });
    for d in 0..dirs {
        let mut names = Vec::new();
        let mut ents = Vec::new();
        for i in 0..per {
            let name = format!("entry_{d:04}_{i:06}.txt");
            let off = names.len() as u32;
            names.extend_from_slice(name.as_bytes());
            // The first file of each folder is large; the rest are small.
            let size = if i < big.div_ceil(dirs) { 100_000 } else { 10 };
            ents.push(rgy_discovery::walk::RawEnt {
                name_off: off,
                name_len: name.len() as u16,
                kind: rgy_discovery::walk::KIND_FILE,
                size,
                mtime: 1_700_000_000,
                child: rgy_discovery::walk::NONE,
            });
        }
        out.push(rgy_discovery::walk::Listing {
            id: (d as u32) + 1,
            names,
            ents,
        });
    }
    out
}

/// The best of `rounds` runs of `f`, in microseconds.
fn best(rounds: usize, mut f: impl FnMut()) -> f64 {
    let mut b = f64::MAX;
    for _ in 0..rounds {
        let t = Instant::now();
        f();
        b = b.min(t.elapsed().as_secs_f64() * 1e6);
    }
    b
}

fn main() {
    let n: usize = std::env::args()
        .nth(1)
        .and_then(|a| a.parse().ok())
        .unwrap_or(200_000);
    let dirs = 64usize;
    let rounds = 30usize;
    // The corpus: 200k names, of which a few are large. A query whose names
    // match everything and whose size filter cuts the result below the
    // limit is the case the session is for.
    let live = Live::new(Index::build(
        synthetic_listings(n, dirs, 8),
        0,
        1_700_000_000,
        b"/",
    ));
    let searcher = Searcher { live: &live };
    // The keystroke sequences a user types: each is a narrowing of the one
    // before, so the session can answer it.
    let sequences: &[(&str, &[&str])] = &[
        (
            "narrow name",
            &["entry_0000_0000", "entry_0000_00000", "entry_0000_000001"],
        ),
        ("broad name", &["report", "repor", "repo"]),
        (
            "names and size",
            &["entry size:>50k", "entry_ size:>50k", "entry_s size:>50k"],
        ),
    ];
    println!("shape | fresh us | session us | ratio | re-ranked");
    for (label, seq) in sequences {
        let seq: &[&str] = seq;
        // The fresh half: every query searched on its own.
        let fresh = best(rounds, || {
            for w in seq {
                let q = Query::parse(w, "").expect("parses");
                let _ = searcher.search(&q);
            }
        });
        // The session half: the same queries, one session.
        let session = Session::new(1);
        let mut times = Vec::new();
        let mut reranked = 0;
        for w in seq {
            let q = Query::parse(w, "").expect("parses");
            let t = Instant::now();
            let _ = session.search(&searcher, &q);
            times.push(t.elapsed().as_secs_f64() * 1e6);
            if session.last_reranked() {
                reranked += 1;
            }
        }
        let total: f64 = times.iter().sum();
        println!(
            "{label:16} | {fresh:.1} | {total:.1} | {:.2}x | {reranked}/{}",
            fresh / total,
            seq.len()
        );
    }
}
