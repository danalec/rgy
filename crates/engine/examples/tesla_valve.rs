// SPDX-License-Identifier: Apache-2.0

//! Empirical test harness for the "Tesla Valve" search & indexing architecture.
//!
//! # Concept: The Tesla Valve in Algorithmic Pipelines
//!
//! In fluid mechanics, a Tesla valve is a passive fluidic diode (US Patent 1,329,559)
//! with zero moving parts. Fluid moving in the forward direction passes through with
//! minimal resistance. Fluid moving in reverse or carrying turbulent noise is diverted
//! into angled loop pathways that re-enter and collide head-on with the main stream,
//! converting kinetic energy into destructive self-interference and choking the flow.
//!
//! In search engines and pattern matchers:
//! - **The "Fluid"**: The stream of candidate offsets or file paths.
//! - **Forward Flow (True Matches)**: Passes through in a straight line with zero
//!   resistance (branchless SIMD instructions, $O(1)$ bitwise operations, no allocations).
//! - **Moving Parts (Traditional Bottleneck)**: Scalar CPU branch jumps, line loops,
//!   thread state updates, VM thread spawning, disk `stat`/`open`/`read` syscalls.
//! - **Tesla Loops (Passive Destructive Self-Interference)**:
//!   - In SIMD Prefilters (Item 2): Secondary constraints (trailing classes, lookahead,
//!     anti-anchors) are routed in reverse register space and collided head-on (`& ~neg`
//!     or `& shifted_mask`) against the candidate stream, quenching false candidates
//!     in-register before emitting any candidates.
//!   - In Corpus N-Gram Indexing (Item 3): Multi-scale frequency-weighted signatures
//!     where query n-grams pass matching files freely, while ambiguous/frequent noise
//!     is circulated through cross-pair counter-collision loops to reject 90-99% of files
//!     in memory in microseconds without disk I/O.

use rgx_core::frequency::rank;
use rgx_core::matcher::Matcher;
use rgx_core::regex::ByteRegex;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

// ---------------------------------------------------------------------------
// Part 1: Tesla Valve SIMD Candidate Choker (Item 2)
// ---------------------------------------------------------------------------

struct TeslaPrefilterResult {
    pattern: &'static str,
    raw_candidates: usize,
    valve_candidates: usize,
    quenched_pct: f64,
    diodicity: f64,
    true_matches: usize,
}

fn test_tesla_simd_choker(corpus_path: &Path) {
    println!("==============================================================================");
    println!("Part 1: Tesla Valve SIMD Candidate Choker (Item 2: Frequency Prefilters)");
    println!("Corpus: {}", corpus_path.display());
    println!("==============================================================================");

    let data = match fs::read(corpus_path) {
        Ok(d) => d,
        Err(e) => {
            println!(
                "Could not read corpus file {}: {}",
                corpus_path.display(),
                e
            );
            return;
        }
    };
    println!(
        "Corpus loaded: {} bytes ({:.2} MB)\n",
        data.len(),
        data.len() as f64 / 1_000_000.0
    );

    // Test specific complex patterns where candidate noise is high:
    // 1. `foo.*bar`: Forward stream searches `foo`; Tesla loop collides presence of `bar` within line window.
    // 2. `(pub|fn) [a-z]+`: Forward stream searches `pub ` | `fn `; Tesla loop collides trailing byte in `[a-z]`.
    // 3. `e[0-9]`: Forward stream searches `e`; Tesla loop collides subsequent digit.

    // A. Pattern: `(pub|fn) [a-z]+`
    // Standard forward nozzle: matches "pub " or "fn "
    // Tesla collision loop: trailing byte must be [a-z] (0x61..=0x7A)
    let pub_fn_res = {
        let mut raw_cands = 0usize;
        let mut valve_cands = 0usize;
        let mut true_hits = 0usize;

        let re = ByteRegex::new(br"(pub|fn) [a-z]+").unwrap();

        // SIMD-style chunked simulation
        let len = data.len();
        let mut i = 0usize;
        while i + 5 <= len {
            // Forward nozzle test: "pub " at i or "fn " at i
            let is_pub = data[i] == b'p'
                && data[i + 1] == b'u'
                && data[i + 2] == b'b'
                && data[i + 3] == b' ';
            let is_fn = data[i] == b'f' && data[i + 1] == b'n' && data[i + 2] == b' ';

            if is_pub {
                raw_cands += 1;
                // Tesla loop: check trailing byte at i + 4
                let trailing = data[i + 4];
                let is_alpha = trailing.is_ascii_lowercase();
                if is_alpha {
                    valve_cands += 1;
                    if re.is_match_at(&data[i..std::cmp::min(i + 32, len)], 0) {
                        true_hits += 1;
                    }
                }
            } else if is_fn {
                raw_cands += 1;
                // Tesla loop: check trailing byte at i + 3
                let trailing = data[i + 3];
                let is_alpha = trailing.is_ascii_lowercase();
                if is_alpha {
                    valve_cands += 1;
                    if re.is_match_at(&data[i..std::cmp::min(i + 32, len)], 0) {
                        true_hits += 1;
                    }
                }
            }
            i += 1;
        }
        TeslaPrefilterResult {
            pattern: "(pub|fn) [a-z]+",
            raw_candidates: raw_cands,
            valve_candidates: valve_cands,
            quenched_pct: 100.0 * (1.0 - valve_cands as f64 / raw_cands as f64),
            diodicity: raw_cands as f64 / valve_cands as f64,
            true_matches: true_hits,
        }
    };

    // B. Pattern: `foo.*bar`
    // Forward nozzle: matches "foo"
    // Tesla collision loop: looks ahead within the same line for "bar"
    let foo_bar_res = {
        let mut raw_cands = 0usize;
        let mut valve_cands = 0usize;
        let mut true_hits = 0usize;

        let re = ByteRegex::new(br"foo.*bar").unwrap();
        let len = data.len();
        let mut i = 0usize;
        while i + 6 <= len {
            // Forward nozzle test: "foo"
            if data[i] == b'f' && data[i + 1] == b'o' && data[i + 2] == b'o' {
                raw_cands += 1;
                // Tesla valve loop: scan forward until newline or max span (1024 bytes) for "bar"
                let end = std::cmp::min(i + 1024, len);
                let line_end = data[i..end]
                    .iter()
                    .position(|&b| b == b'\n')
                    .map(|p| i + p)
                    .unwrap_or(end);
                let slice = &data[i + 3..line_end];
                let has_bar = rgx_core::memchr::memmem::find(slice, b"bar").is_some();
                if has_bar {
                    valve_cands += 1;
                    if re.is_match_at(&data[i..line_end], 0) {
                        true_hits += 1;
                    }
                }
            }
            i += 1;
        }
        TeslaPrefilterResult {
            pattern: "foo.*bar",
            raw_candidates: raw_cands,
            valve_candidates: valve_cands,
            quenched_pct: 100.0 * (1.0 - valve_cands as f64 / raw_cands as f64),
            diodicity: raw_cands as f64 / valve_cands.max(1) as f64,
            true_matches: true_hits,
        }
    };

    // C. Pattern: `e[0-9]`
    // Forward nozzle: 'e' (rank 253, high flow)
    // Tesla loop: subsequent byte is digit (rank sum 1904, low flow)
    let e_digit_res = {
        let mut raw_cands = 0usize;
        let mut valve_cands = 0usize;
        let mut true_hits = 0usize;

        let len = data.len();
        let mut i = 0usize;
        while i + 2 <= len {
            if data[i] == b'e' {
                raw_cands += 1;
                let next = data[i + 1];
                if next.is_ascii_digit() {
                    valve_cands += 1;
                    true_hits += 1;
                }
            }
            i += 1;
        }
        TeslaPrefilterResult {
            pattern: "e[0-9]",
            raw_candidates: raw_cands,
            valve_candidates: valve_cands,
            quenched_pct: 100.0 * (1.0 - valve_cands as f64 / raw_cands as f64),
            diodicity: raw_cands as f64 / valve_cands.max(1) as f64,
            true_matches: true_hits,
        }
    };

    println!(
        "| Pattern | Forward Stream (Raw) | Tesla Valve Admitted | Quenched Noise | Diodicity (Di) | True Hits |"
    );
    println!("|---|---:|---:|---:|---:|---:|");
    for res in &[&pub_fn_res, &foo_bar_res, &e_digit_res] {
        println!(
            "| `{}` | {} | {} | **{:.2}%** | **{:.1}x** | {} |",
            res.pattern,
            res.raw_candidates,
            res.valve_candidates,
            res.quenched_pct,
            res.diodicity,
            res.true_matches,
        );
    }
    println!();
}

// ---------------------------------------------------------------------------
// Part 2: Tesla Valve Corpus N-Gram Index (Item 3)
// ---------------------------------------------------------------------------

use rgx_core::frequency::cost;

/// Maximum possible 4-gram cost is 255^4 = 4,228,250,625 (four spaces/extended bytes).
/// Noise threshold: 4-grams above 3.8 billion are pure whitespace / identical frequent runs.
const NOISE_THRESHOLD_COST: u64 = 3_800_000_000;

/// 256-bit (AVX2-sized) Tesla Valve signature per file.
/// 4 x u64 words: 2 forward conduit words + 2 reverse collision words.
#[derive(Clone, Copy, Default, PartialEq, Eq)]
struct TeslaSig256 {
    w: [u64; 4],
}

impl TeslaSig256 {
    #[inline(always)]
    fn insert_ngram(&mut self, gram: &[u8]) {
        // Frequency-aware valve: skip noise n-grams that saturate the filter
        if cost(gram) > NOISE_THRESHOLD_COST {
            return;
        }
        let h1 = hash_ngram_forward(gram);
        let word_fwd = (h1 >> 6) & 1; // 0 or 1
        let bit_fwd = h1 & 63;
        self.w[word_fwd as usize] |= 1u64 << bit_fwd;

        let h2 = hash_ngram_reverse(gram);
        let word_rev = 2 + ((h2 >> 6) & 1); // 2 or 3
        let bit_rev = h2 & 63;
        self.w[word_rev as usize] |= 1u64 << bit_rev;
    }

    #[inline(always)]
    fn contains_query(&self, q: &TeslaSig256) -> bool {
        // Forward conduit test (words 0..2)
        if (self.w[0] & q.w[0]) != q.w[0] || (self.w[1] & q.w[1]) != q.w[1] {
            return false;
        }
        // Tesla reverse loop collision test (words 2..4)
        if (self.w[2] & q.w[2]) != q.w[2] || (self.w[3] & q.w[3]) != q.w[3] {
            return false;
        }
        true
    }
}

/// 512-bit Tesla Valve signature per file.
/// 8 x u64 words: 4 forward conduit words + 4 reverse collision words.
#[derive(Clone, Copy, Default, PartialEq, Eq)]
struct TeslaSig512 {
    w: [u64; 8],
}

impl TeslaSig512 {
    #[inline(always)]
    fn insert_ngram(&mut self, gram: &[u8]) {
        if cost(gram) > NOISE_THRESHOLD_COST {
            return;
        }
        let h1 = hash_ngram_forward(gram);
        let word_fwd = (h1 >> 6) & 3; // 0..3
        let bit_fwd = h1 & 63;
        self.w[word_fwd as usize] |= 1u64 << bit_fwd;

        let h2 = hash_ngram_reverse(gram);
        let word_rev = 4 + ((h2 >> 6) & 3); // 4..7
        let bit_rev = h2 & 63;
        self.w[word_rev as usize] |= 1u64 << bit_rev;
    }

    #[inline(always)]
    fn contains_query(&self, q: &TeslaSig512) -> bool {
        // Forward conduit test (words 0..4)
        for i in 0..4 {
            if (self.w[i] & q.w[i]) != q.w[i] {
                return false;
            }
        }
        // Tesla reverse loop collision test (words 4..8)
        for i in 4..8 {
            if (self.w[i] & q.w[i]) != q.w[i] {
                return false;
            }
        }
        true
    }
}

#[inline(always)]
fn hash_ngram_forward(gram: &[u8]) -> u64 {
    let mut h = 0xcbf29ce484222325u64;
    for &b in gram {
        let r = rank(b) as u64;
        h ^= (b as u64) ^ (r << 4);
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

#[inline(always)]
fn hash_ngram_reverse(gram: &[u8]) -> u64 {
    let mut h = 0x9e3779b97f4a7c15u64;
    for &b in gram.iter().rev() {
        let r = rank(b) as u64;
        h ^= (b as u64) ^ (r << 5);
        h = h.rotate_left(11).wrapping_mul(0x517cc1b727220a95);
    }
    h
}

fn build_sig256(bytes: &[u8]) -> TeslaSig256 {
    let mut sig = TeslaSig256::default();
    if bytes.len() < 4 {
        return sig;
    }
    // Contiguous 4-grams (stride 1) ensures sound zero-false-negative guarantee
    let mut i = 0usize;
    while i + 4 <= bytes.len() {
        sig.insert_ngram(&bytes[i..i + 4]);
        i += 1;
    }
    sig
}

fn build_sig512(bytes: &[u8]) -> TeslaSig512 {
    let mut sig = TeslaSig512::default();
    if bytes.len() < 4 {
        return sig;
    }
    let mut i = 0usize;
    while i + 4 <= bytes.len() {
        sig.insert_ngram(&bytes[i..i + 4]);
        i += 1;
    }
    sig
}

fn build_query256(pattern: &str) -> Option<TeslaSig256> {
    let mut sig = TeslaSig256::default();
    let bytes = pattern.as_bytes();
    let mut chunks: Vec<&[u8]> = Vec::new();
    let mut start = None;
    for (i, &b) in bytes.iter().enumerate() {
        let is_lit = b.is_ascii_alphanumeric() || b == b' ' || b == b'_';
        if is_lit {
            if start.is_none() {
                start = Some(i);
            }
        } else {
            if let Some(s) = start {
                if i - s >= 4 {
                    chunks.push(&bytes[s..i]);
                }
                start = None;
            }
        }
    }
    if let Some(s) = start
        && bytes.len() - s >= 4
    {
        chunks.push(&bytes[s..bytes.len()]);
    }

    if chunks.is_empty() {
        return None;
    }

    let mut inserted = 0;
    for chunk in chunks {
        let mut i = 0usize;
        while i + 4 <= chunk.len() {
            let gram = &chunk[i..i + 4];
            if cost(gram) <= NOISE_THRESHOLD_COST {
                sig.insert_ngram(gram);
                inserted += 1;
            }
            i += 1;
        }
    }
    if inserted == 0 { None } else { Some(sig) }
}

fn build_query512(pattern: &str) -> Option<TeslaSig512> {
    let mut sig = TeslaSig512::default();
    let bytes = pattern.as_bytes();
    let mut chunks: Vec<&[u8]> = Vec::new();
    let mut start = None;
    for (i, &b) in bytes.iter().enumerate() {
        let is_lit = b.is_ascii_alphanumeric() || b == b' ' || b == b'_';
        if is_lit {
            if start.is_none() {
                start = Some(i);
            }
        } else {
            if let Some(s) = start {
                if i - s >= 4 {
                    chunks.push(&bytes[s..i]);
                }
                start = None;
            }
        }
    }
    if let Some(s) = start
        && bytes.len() - s >= 4
    {
        chunks.push(&bytes[s..bytes.len()]);
    }

    if chunks.is_empty() {
        return None;
    }

    let mut inserted = 0;
    for chunk in chunks {
        let mut i = 0usize;
        while i + 4 <= chunk.len() {
            let gram = &chunk[i..i + 4];
            if cost(gram) <= NOISE_THRESHOLD_COST {
                sig.insert_ngram(gram);
                inserted += 1;
            }
            i += 1;
        }
    }
    if inserted == 0 { None } else { Some(sig) }
}

fn test_tesla_corpus_index(tree_path: &Path) {
    println!("==============================================================================");
    println!("Part 2: Tesla Valve Corpus N-Gram Index (Item 3: Multi-File Pruning)");
    println!("Corpus directory: {}", tree_path.display());
    println!("==============================================================================");

    let t_walk = Instant::now();
    let mut files = Vec::new();
    collect_rs_files(tree_path, &mut files);
    let walk_ms = t_walk.elapsed().as_secs_f64() * 1000.0;
    println!("Found {} .rs files in {:.2} ms", files.len(), walk_ms);

    if files.is_empty() {
        println!("No .rs files found at {}", tree_path.display());
        return;
    }

    let test_set = if files.len() > 15000 {
        &files[..15000]
    } else {
        &files[..]
    };
    println!(
        "Benchmarking 256-bit and 512-bit Tesla Valve signatures over {} files...\n",
        test_set.len()
    );

    let mut sigs256: Vec<TeslaSig256> = Vec::with_capacity(test_set.len());
    let mut sigs512: Vec<TeslaSig512> = Vec::with_capacity(test_set.len());
    let mut contents: Vec<Vec<u8>> = Vec::with_capacity(test_set.len());
    let mut total_bytes = 0usize;

    for path in test_set {
        let content = fs::read(path).unwrap_or_default();
        total_bytes += content.len();
        sigs256.push(build_sig256(&content));
        sigs512.push(build_sig512(&content));
        contents.push(content);
    }
    let mb = total_bytes as f64 / 1_000_000.0;
    println!(
        "Corpus subset loaded: {} files ({:.2} MB)",
        test_set.len(),
        mb
    );
    println!(
        "256-bit index size: {:.2} KB (32 bytes/file)",
        (sigs256.len() * 32) as f64 / 1024.0
    );
    println!(
        "512-bit index size: {:.2} KB (64 bytes/file)\n",
        (sigs512.len() * 64) as f64 / 1024.0
    );

    println!("--- 256-bit Tesla Valve Signature ---");
    println!(
        "| Query Pattern | Admitted Files | Pruned Files | Prune Efficiency | Valve Filter Latency | True Positive Files | False Negatives |"
    );
    println!("|---|---:|---:|---:|---:|---:|---:|");

    let patterns = ["fn main", "pub fn", "let mut", "struct "];

    for &pat in &patterns {
        let q_sig = match build_query256(pat) {
            Some(q) => q,
            None => continue,
        };

        // Ground truth computed outside timing loop
        let ground_truth: Vec<bool> = contents
            .iter()
            .map(|c| rgx_core::memchr::memmem::find(c, pat.as_bytes()).is_some())
            .collect();
        let true_files: usize = ground_truth.iter().filter(|&&b| b).count();

        // Timed Tesla valve filtering loop
        let t_query = Instant::now();
        let mut admitted = 0usize;
        let mut pruned = 0usize;
        let mut passes = Vec::with_capacity(sigs256.len());

        for sig in &sigs256 {
            let pass = sig.contains_query(&q_sig);
            passes.push(pass);
            if pass {
                admitted += 1;
            } else {
                pruned += 1;
            }
        }
        let q_us = t_query.elapsed().as_secs_f64() * 1_000_000.0;
        let prune_pct = 100.0 * pruned as f64 / (admitted + pruned) as f64;

        // Verify zero false negatives
        let mut false_neg = 0usize;
        for i in 0..sigs256.len() {
            if !passes[i] && ground_truth[i] {
                false_neg += 1;
            }
        }

        println!(
            "| `{}` | {} | {} | **{:.2}%** | **{:.2} µs** ({:.2} ns/file) | {} | **{}** |",
            pat,
            admitted,
            pruned,
            prune_pct,
            q_us,
            (q_us * 1000.0) / sigs256.len() as f64,
            true_files,
            false_neg
        );
    }
    println!();

    println!("--- 512-bit Tesla Valve Signature ---");
    println!(
        "| Query Pattern | Admitted Files | Pruned Files | Prune Efficiency | Valve Filter Latency | True Positive Files | False Negatives |"
    );
    println!("|---|---:|---:|---:|---:|---:|---:|");

    for &pat in &patterns {
        let q_sig = match build_query512(pat) {
            Some(q) => q,
            None => continue,
        };

        let ground_truth: Vec<bool> = contents
            .iter()
            .map(|c| rgx_core::memchr::memmem::find(c, pat.as_bytes()).is_some())
            .collect();
        let true_files: usize = ground_truth.iter().filter(|&&b| b).count();

        let t_query = Instant::now();
        let mut admitted = 0usize;
        let mut pruned = 0usize;
        let mut passes = Vec::with_capacity(sigs512.len());

        for sig in &sigs512 {
            let pass = sig.contains_query(&q_sig);
            passes.push(pass);
            if pass {
                admitted += 1;
            } else {
                pruned += 1;
            }
        }
        let q_us = t_query.elapsed().as_secs_f64() * 1_000_000.0;
        let prune_pct = 100.0 * pruned as f64 / (admitted + pruned) as f64;

        let mut false_neg = 0usize;
        for i in 0..sigs512.len() {
            if !passes[i] && ground_truth[i] {
                false_neg += 1;
            }
        }

        println!(
            "| `{}` | {} | {} | **{:.2}%** | **{:.2} µs** ({:.2} ns/file) | {} | **{}** |",
            pat,
            admitted,
            pruned,
            prune_pct,
            q_us,
            (q_us * 1000.0) / sigs512.len() as f64,
            true_files,
            false_neg
        );
    }
    println!();
}

fn collect_rs_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_rs_files(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            out.push(path);
        }
    }
}

fn main() {
    println!("\n=== TESLA VALVE SEARCH & INDEXING BENCHMARK ===");
    println!("Testing the fluidic diode principle applied to string search & indexing.\n");

    let home = std::env::var("USERPROFILE").unwrap_or_else(|_| ".".into());
    let big_path = PathBuf::from(&home).join(".rgx-bench").join("big235.rs");
    if big_path.exists() {
        test_tesla_simd_choker(&big_path);
    } else {
        println!(
            "Note: Single-file corpus not found at {}",
            big_path.display()
        );
    }

    let registry = PathBuf::from(&home)
        .join(".cargo")
        .join("registry")
        .join("src");
    let tree_path = if registry.exists() {
        // find largest dir under registry
        let mut best: Option<PathBuf> = None;
        if let Ok(rd) = fs::read_dir(&registry) {
            for e in rd.flatten() {
                let p = e.path();
                if p.is_dir() {
                    best = Some(p);
                    break;
                }
            }
        }
        best.unwrap_or(PathBuf::from("crates"))
    } else {
        PathBuf::from("crates")
    };

    test_tesla_corpus_index(&tree_path);
}
