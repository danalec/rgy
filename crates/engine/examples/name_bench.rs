//! Phase 0 bench harness (first-PR sequence #1): name search + walk
//! latency on a fixed, deterministic corpus, printing a table the plan's
//! perf gate compares against (`benchmark/baseline-names.md`).
//!
//! Rules carried over from rgx's harness: a fixed corpus with digestable
//! content, warmups before timing, min-of-N, and counts proven equal
//! before timing (a fast wrong answer is a failure). A >20% regression on
//! any row fails the PR.
//!
//! Run: `cargo run --release -p engine --example name_bench [names] [--csv]`
//!
//! `--csv` puts a machine-readable table on stdout and the human summary on
//! stderr, so a gate can diff a baseline without parsing prose:
//! `cargo run --release -p engine --example name_bench 1000000 --csv > base.csv`.

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
    out.push(Listing {
        id: 0,
        names: root_names,
        ents: root_ents,
    });
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
        out.push(Listing {
            id: d as u32 + 1,
            names,
            ents,
        });
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

/// One measured row: `kind` discriminates so build/query/walk share a single
/// header and stay valid CSV (a second header block would not be).
struct Row {
    kind: &'static str,
    query: String,
    hits: usize,
    min: f64,
    med: f64,
    p90: f64,
    entries: usize,
    elapsed_s: f64,
    corpus: usize,
    dirs: usize,
    build_ms: f64,
    /// Which `Searcher` path answered. A row whose plan changes is a
    /// regression even when the latency does not: the same query used to be
    /// answered by `selective` and is now walking everything.
    plan: String,
}

fn csv_field(s: &str) -> String {
    if s.contains([',', '"', '\n']) {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_string()
    }
}

fn print_csv(rows: &[Row]) {
    println!(
        "kind,query,hits,min_us,med_us,p90_us,entries,elapsed_s,entries_per_s,corpus,dirs,build_ms,plan"
    );
    for r in rows {
        // Entries/s is only meaningful for the walk; dividing a corpus size
        // by a query's latency would print a large meaningless number.
        let rate = if r.kind == "walk" && r.elapsed_s > 0.0 {
            format!("{:.1}", r.entries as f64 / r.elapsed_s)
        } else {
            String::new()
        };
        println!(
            "{},{},{},{:.3},{:.3},{:.3},{},{:.6},{rate},{},{},{:.3},{}",
            r.kind,
            csv_field(&r.query),
            r.hits,
            r.min,
            r.med,
            r.p90,
            r.entries,
            r.elapsed_s,
            r.corpus,
            r.dirs,
            r.build_ms,
            csv_field(&r.plan)
        );
    }
}

fn main() {
    let mut csv = false;
    let mut n = 200_000usize;
    for a in std::env::args().skip(1) {
        match a.as_str() {
            "--csv" => csv = true,
            s => {
                if let Ok(v) = s.parse() {
                    n = v;
                }
            }
        }
    }
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
    // The true match count, taken at a limit high enough that nothing
    // saturates. Timing uses the interactive default below, which would pin
    // `hits` at the limit for every broad query — and then the "a fast wrong
    // answer is a failure" rule would only ever be asserting limit == limit,
    // letting a planner that silently dropped matches pass.
    const COUNT_LIMIT: usize = 10_000_000;
    let default_limit = Query::parse("x", "")
        .map(|q| q.limit)
        .expect("bench query parses");
    let mut hits_count = Vec::new();
    for (q, _) in queries {
        let mut parsed = Query::parse(q, "").expect("bench query parses");
        parsed.limit = COUNT_LIMIT;
        let total = searcher.search(&parsed).len();
        // The saturated count must be exactly what the timed path will see:
        // this is the assertion that catches a planner dropping matches.
        let mut capped = Query::parse(q, "").unwrap();
        capped.limit = default_limit;
        let shown = searcher.search(&capped).len();
        assert_eq!(
            shown,
            total.min(default_limit),
            "{q}: capped search returned {shown}, expected {}",
            total.min(default_limit)
        );
        hits_count.push(total);
    }
    // Warmup.
    for _ in 0..3 {
        for (q, _) in queries {
            let parsed = Query::parse(q, "").unwrap();
            let _ = searcher.search(&parsed);
        }
    }

    let mut rows: Vec<Row> = Vec::new();
    if !csv {
        println!("corpus: {n} names in {dirs} dirs; build {build_ms:.1} ms");
        println!("query                          hits     min_us    med_us    p90_us");
    }
    for (qi, (q, _)) in queries.iter().enumerate() {
        let mut samples = Vec::with_capacity(rounds);
        for _ in 0..rounds {
            let parsed = Query::parse(q, "").unwrap();
            let t = Instant::now();
            let hits = searcher.search(&parsed);
            // Timed at the interactive limit; the durable match count is the
            // unsaturated one above, and the two were proven consistent
            // before timing started.
            assert!(
                hits.len() <= hits_count[qi],
                "{q}: timed search returned more hits than the full count"
            );
            samples.push(t.elapsed().as_secs_f64() * 1e6);
        }
        let (min, med, p90) = stats(&mut samples);
        // Read after the last timed search: it recorded the plan that ran.
        let plan = Searcher::last_plan()
            .map(|p| p.to_string())
            .unwrap_or_else(|| "none".into());
        if !csv {
            println!(
                "{q:<30} {:>8} {min:>10.1} {med:>10.1} {p90:>10.1}  {plan}",
                hits_count[qi]
            );
        }
        rows.push(Row {
            kind: "query",
            query: (*q).to_string(),
            hits: hits_count[qi],
            min,
            med,
            p90,
            entries: n,
            elapsed_s: med / 1e6,
            corpus: n,
            dirs,
            build_ms,
            plan,
        });
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
    eprintln!(
        "walk: {entries} entries in {elapsed:.3} s ({:.0} entries/s)",
        entries as f64 / elapsed
    );
    rows.push(Row {
        kind: "walk",
        query: String::new(),
        hits: 0,
        min: 0.0,
        med: 0.0,
        p90: 0.0,
        entries,
        elapsed_s: elapsed,
        corpus: n,
        dirs,
        build_ms,
        plan: String::new(),
    });
    let _ = std::fs::remove_dir_all(&tmp);

    if csv {
        print_csv(&rows);
    }
}

fn per_files(n: usize, dirs: usize) -> usize {
    n.div_ceil(dirs).max(1)
}
