// SPDX-License-Identifier: Apache-2.0

//! Walk throughput: the sequential oracle vs the fanned-out walk.
//!
//! `engine`'s `name_bench` times the query path; this times discovery alone,
//! which is what `--index` amortises. `threads = 1` takes the inline path and
//! so *is* the sequential baseline.
//!
//! ```text
//! cargo run --release -p rgy-discovery --example walk_bench -- <root> [rounds]
//! ```
//!
//! Emits CSV on stdout (header + one row per thread count) so a regression can
//! gate a PR the way `benchmark/baseline-names.md` gates the query path.

use std::time::Instant;

fn main() {
    let mut args = std::env::args().skip(1);
    let root = args.next().unwrap_or_else(|| ".".to_string());
    let rounds: usize = args.next().and_then(|s| s.parse().ok()).unwrap_or(5).max(1);

    let root_bytes = std::path::Path::new(&root)
        .to_string_lossy()
        .as_bytes()
        .to_vec();

    // Warm the OS page cache first, so the comparison is walk cost rather
    // than whoever paid for first-touch I/O.
    drop(rgy_discovery::walk::scan_sequential(&root_bytes));

    eprintln!("root={root} rounds={rounds}");
    println!("threads,entries,min_ms,median_ms,speedup");

    let mut baseline = None;
    for threads in [1usize, 2, 4, 8, 16] {
        let mut samples = Vec::with_capacity(rounds);
        let mut entries = 0usize;
        for _ in 0..rounds {
            let t0 = Instant::now();
            let listings = rgy_discovery::walk::scan(&root_bytes, threads);
            samples.push(t0.elapsed().as_secs_f64() * 1000.0);
            entries = listings.iter().map(|l| l.ents.len()).sum();
            std::hint::black_box(&listings);
        }
        samples.sort_by(f64::total_cmp);
        let min = samples[0];
        let median = samples[samples.len() / 2];

        let speedup = *baseline.get_or_insert(min) / min;
        println!("{threads},{entries},{min:.1},{median:.1},{speedup:.2}x");
    }
}
