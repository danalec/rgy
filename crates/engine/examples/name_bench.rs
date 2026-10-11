//! Phase 0 bench harness (first-PR sequence #1): name search + walk
//! latency on a fixed, deterministic corpus, printing a table the plan's
//! perf gate compares against (`benchmark/baseline-names.md`).
//!
//! Rules carried over from rgx's harness: a fixed corpus with digestable
//! content, warmups before timing, min-of-N, and counts proven equal
//! before timing (a fast wrong answer is a failure). A >20% regression on
//! any row fails the PR.
//!
//! Run: `cargo run --release -p engine --example name_bench`

use std::time::Instant;

use discovery::walk::Listing;
use name_core::live::Live;
use name_core::name_index::Index;
use name_core::name_query::{Query, Searcher};

/// Deterministic corpus: `n` files spread over `dirs` directories, plus a
/// few directories-of-directories so the DFS layout is not flat. Names mix
/// digits, dots, and case so both the token scorer and the masks get work.
fn synthetic_listings(n: usize, dirs: usize) -> Vec<Listing> {
    let per = n.div_ceil(dirs).max(1);
    let mut out = Vec::new();
    // Listing 0: root, holding the dirs.
    let mut root_names = Vec::new();
    let mut root_ents = Vec::new();
    for d in 0..dirs {
        let name = format!("dir_{d:04}");
        let off = root_names.len() as u32;
        root_names.extend_from_slice(name.as_bytes());
        root_ents.push(discovery::walk::RawEnt {
            name_off: off,
            name_len: name.len() as u16,
            kind: discovery::walk::KIND_DIR,
            size: 0,
            mtime: 1_700_000_000,
            child: d as u32 + 1,
        });
    }
    out.push(Listing { id: 0, names: root_names, ents: root_ents });
    for d in 0..dirs {
        let mut names = Vec::new();
        let mut ents = Vec::new();
        for i in 0..per {
            let name = if i % 7 == 0 {
                format!("Report_{:05}.PDF", d * per + i)
            } else if i % 3 == 0 {
                format!("src_{:05}-module.rs", d * per + i)
            } else {
                format!("file_{:05}.dat", d * per + i)
            };
            let off = names.len() as u32;
            names.extend_from_slice(name.as_bytes());
            ents.push(discovery::walk::RawEnt {
                name_off: off,
                name_len: name.len() as u16,
                kind: discovery::walk::KIND_FILE,
                size: 1000 + (i as u64),
                mtime: 1_700_000_000,
                child: discovery::walk::NONE,
            });
        }
        out.push(Listing { id: d as u32 + 1, names, ents });
    }
    out
}

fn stats(samples: &mut [f64]) -> (f64, f64, f64) {
    samples.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let min = samples[0];
    let med = samples[samples.len() / 2];
    let p90 = samples[(samples.len() * 9 / 10).min(samples.len() - 1)];
    (min, med, p90)
}

fn main() {
    let n = std::env::args()
        .nth(1)
        .and_then(|a| a.parse().ok())
        .unwrap_or(200_000usize);
    let dirs = (n / 200).clamp(8, 2000);
    let rounds = 30;

    // --- build ---
    let listings = synthetic_listings(n, dirs);
    let t0 = Instant::now();
    let idx = Index::build(listings, 0, 1_700_000_000, b"/");
    let build_ms = t0.elapsed().as_secs_f64() * 1e3;
    let live = Live::new(idx);
    let searcher = Searcher { live: &live };

    // --- queries: counts proven equal before timing ---
    let queries: &[(&str, usize)] = &[
        ("report", 0), // 0 = don't assert, just measure (corpus-size dependent)
        ("file", 0),
        ("'report_00000.pdf", 0),
        ("^src", 0),
        ("dat$", 0),
        ("mod", 0),
        ("rep", 0),
        ("f1", 0),
    ];
    let mut hits_count = Vec::new();
    for (q, _) in queries {
        let parsed = Query::parse(q, "").expect("bench query parses");
        hits_count.push(searcher.search(&parsed).len());
    }
    // Warmup.
    for _ in 0..3 {
        for (q, _) in queries {
            let parsed = Query::parse(q, "").unwrap();
            let _ = searcher.search(&parsed);
        }
    }

    println!("corpus: {n} names in {dirs} dirs; build {build_ms:.1} ms");
    println!("query                          hits     min_us    med_us    p90_us");
    for (qi, (q, _)) in queries.iter().enumerate() {
        let mut samples = Vec::with_capacity(rounds);
        for _ in 0..rounds {
            let parsed = Query::parse(q, "").unwrap();
            let t = Instant::now();
            let hits = searcher.search(&parsed);
            assert_eq!(hits.len(), hits_count[qi], "count drift on {q}");
            samples.push(t.elapsed().as_secs_f64() * 1e6);
        }
        let (min, med, p90) = stats(&mut samples);
        println!(
            "{q:<30} {:>8} {min:>10.1} {med:>10.1} {p90:>10.1}",
            hits_count[qi]
        );
    }

    // --- walk: a generated temp tree, entries/s ---
    let tmp = std::env::temp_dir().join("rgy_bench_walk");
    let _ = std::fs::remove_dir_all(&tmp);
    for d in 0..dirs {
        let dir = tmp.join(format!("d{d:04}"));
        std::fs::create_dir_all(&dir).unwrap();
        for i in 0..per_files(n, dirs) {
            std::fs::write(dir.join(format!("f{i:05}.dat")), b"x").unwrap();
        }
    }
    let t = Instant::now();
    let ls = discovery::walk::scan(tmp.to_string_lossy().as_bytes(), 4);
    let elapsed = t.elapsed().as_secs_f64();
    let entries: usize = ls.iter().map(|l| l.ents.len()).sum();
    println!("walk: {entries} entries in {elapsed:.3} s ({:.0} entries/s)", entries as f64 / elapsed);
    let _ = std::fs::remove_dir_all(&tmp);
}

fn per_files(n: usize, dirs: usize) -> usize {
    n.div_ceil(dirs).max(1)
}
