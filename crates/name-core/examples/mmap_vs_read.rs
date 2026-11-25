// SPDX-License-Identifier: Apache-2.0

//! mmap-vs-read sequential scan, per OS — closes the open measurement in
//! rgx's own numbers. rgx measured mmap 1.51x SLOWER than
//! `read()` for scans on Windows (ROADMAP §3.3) and kept its mmap path
//! alive only to check whether Linux's cheap soft faults reverse the
//! ordering. This bench answers that on whatever OS it runs on.
//!
//! Rules (rgx harness): deterministic corpus, both paths must produce the
//! same checksum before timing, warmup, min-of-N. Cold-cache rows are
//! reported only when the process can write `/proc/sys/vm/drop_caches`
//! (root); otherwise they are marked unavailable rather than faked by a
//! "first round" that still sits in page cache.
//!
//! Run: `cargo run --release -p rgy-name-core --example mmap_vs_read -- [MiB]`
//! (default 256 MiB)

use std::fs::File;
use std::io::Write;
use std::time::Instant;

use rgx_core::memmap2::Mmap;

const ROUNDS: usize = 15;

/// FNV-1a over every byte — forces a real touch of each page, so neither
/// path can be optimized into a no-op.
fn checksum(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

fn synthetic_bytes(mib: usize) -> Vec<u8> {
    let total = mib * 1024 * 1024;
    let mut out = Vec::with_capacity(total);
    // Cheap deterministic LCG: content varies per byte so a skip or an
    // overlap changes the checksum.
    let mut state: u64 = 0x1234_5678_9abc_def0;
    while out.len() < total {
        state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
        out.push((state >> 33) as u8);
    }
    out
}

fn stats(samples: &mut [f64]) -> (f64, f64) {
    samples.sort_by(|a, b| a.partial_cmp(b).unwrap());
    (samples[0], samples[samples.len() / 2])
}

fn drop_caches() -> bool {
    // Linux root only. The write itself is the privilege check — EACCES
    // on a dev box is expected and must not fake a "cold" row. Other OSes
    // (and macOS, which has no drop_caches) fall through to false.
    let sync = std::process::Command::new("sync").status();
    if !matches!(sync, Ok(s) if s.success()) {
        return false;
    }
    std::fs::write("/proc/sys/vm/drop_caches", "3").is_ok()
}

fn main() {
    let mib: usize = std::env::args().nth(1).and_then(|a| a.parse().ok()).unwrap_or(256);
    assert!(mib >= 1, "need at least 1 MiB");

    let dir = std::env::temp_dir().join(format!("rgy-mmap-vs-read-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let path = dir.join("corpus.bin");

    let corpus = synthetic_bytes(mib);
    {
        let mut f = File::create(&path).expect("create corpus");
        f.write_all(&corpus).expect("write corpus");
        f.sync_all().expect("sync corpus");
    }

    // --- counts proven equal before timing ---
    let expect = checksum(&corpus);
    let owned = std::fs::read(&path).expect("read corpus");
    let mapped = unsafe { Mmap::map(&File::open(&path).expect("open corpus")) }.expect("mmap corpus");
    let read_sum = checksum(&owned);
    let mmap_sum = checksum(&mapped);
    assert_eq!(read_sum, expect, "read path checksum diverged");
    assert_eq!(mmap_sum, expect, "mmap path checksum diverged");
    drop(owned);
    drop(mapped);

    let size_mib = mib as f64;

    // --- warmup (one scan each) ---
    {
        let owned = std::fs::read(&path).unwrap();
        std::hint::black_box(checksum(&owned));
        let mapped = unsafe { Mmap::map(&File::open(&path).unwrap()) }.unwrap();
        std::hint::black_box(checksum(&mapped));
    }

    // --- hot-cache min-of-N ---
    let mut read_ms = Vec::with_capacity(ROUNDS);
    for _ in 0..ROUNDS {
        let owned = std::fs::read(&path).unwrap();
        let t = Instant::now();
        let sum = checksum(&owned);
        let ms = t.elapsed().as_secs_f64() * 1e3;
        assert_eq!(sum, expect);
        read_ms.push(ms);
        // The read() syscall itself is part of the path's cost; time it too.
    }
    // Second hot series that includes the read() syscall, not just the scan.
    let mut read_full_ms = Vec::with_capacity(ROUNDS);
    for _ in 0..ROUNDS {
        let t = Instant::now();
        let owned = std::fs::read(&path).unwrap();
        let sum = checksum(&owned);
        let ms = t.elapsed().as_secs_f64() * 1e3;
        assert_eq!(sum, expect);
        read_full_ms.push(ms);
    }

    let mapped = unsafe { Mmap::map(&File::open(&path).unwrap()) }.unwrap();
    let mut mmap_ms = Vec::with_capacity(ROUNDS);
    for _ in 0..ROUNDS {
        let t = Instant::now();
        let sum = checksum(&mapped);
        let ms = t.elapsed().as_secs_f64() * 1e3;
        assert_eq!(sum, expect);
        mmap_ms.push(ms);
    }
    drop(mapped);

    let (read_min, read_med) = stats(&mut read_ms);
    let (read_full_min, read_full_med) = stats(&mut read_full_ms);
    let (mmap_min, mmap_med) = stats(&mut mmap_ms);

    // --- cold first-touch (root only) ---
    let cold = if drop_caches() {
        let t = Instant::now();
        let owned = std::fs::read(&path).unwrap();
        let sum = checksum(&owned);
        let read_cold = t.elapsed().as_secs_f64() * 1e3;
        assert_eq!(sum, expect);
        drop(owned);

        let _ = drop_caches();
        let t = Instant::now();
        let mapped = unsafe { Mmap::map(&File::open(&path).unwrap()) }.unwrap();
        let sum = checksum(&mapped);
        let mmap_cold = t.elapsed().as_secs_f64() * 1e3;
        assert_eq!(sum, expect);
        drop(mapped);
        Some((read_cold, mmap_cold))
    } else {
        None
    };

    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_dir(&dir);

    // --- report ---
    println!("# mmap vs read — sequential scan, {mib} MiB");
    println!("os: {}", std::env::consts::OS);
    println!("arch: {}", std::env::consts::ARCH);
    println!("rounds: {ROUNDS} (min-of-N), 1 warmup each; checksums proven equal first");
    println!();
    println!("| path | min ms | median ms | MiB/s (min) |");
    println!("|---|---|---|---|");
    let mib_s = |ms: f64| size_mib / (ms / 1e3);
    println!("| read() + scan (I/O included) | {read_full_min:.2} | {read_full_med:.2} | {:.0} |", mib_s(read_full_min));
    println!("| read() scan only (bytes resident) | {read_min:.2} | {read_med:.2} | {:.0} |", mib_s(read_min));
    println!("| mmap scan (mapping live) | {mmap_min:.2} | {mmap_med:.2} | {:.0} |", mib_s(mmap_min));
    match cold {
        Some((r, m)) => {
            println!("| read() cold first-touch | {r:.2} | — | {:.0} |", mib_s(r));
            println!("| mmap cold first-touch | {m:.2} | — | {:.0} |", mib_s(m));
            println!();
            println!("cold verdict: mmap is {:.2}x the read() cost (values > 1 mean mmap slower)", m / r);
        }
        None => {
            println!();
            println!("cold rows: unavailable (cannot write /proc/sys/vm/drop_caches; not root)");
        }
    }
    println!();
    println!("hot verdict: mmap scan is {:.2}x the read()-included cost (values > 1 mean mmap slower)", mmap_min / read_full_min);
}
