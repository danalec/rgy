//! SHA-256 hardware acceleration.
//!
//! The dispatch infrastructure is wired: a `sha256-hw` Cargo feature
//! (off by default) toggles a runtime feature check in
//! `Sha256::process_block`, and a CACHE-backed `hardware_available()`
//! records the result of the first detection so subsequent blocks
//! pay a single branch.
//!
//! The hardware backends themselves (SHA-NI on x86_64, ARMv8 SHA2 on
//! aarch64) are **not yet** implemented on stable Rust. The
//! intrinsics are still nightly-only in `core::arch` and inline
//! assembly for the four SHA-NI instructions needs careful per-`asm!`
//! register allocation that is best done against a runtime test bench
//! the project does not have. The backends therefore fall through
//! to the scalar `sha256` module under the same feature flag -- no
//! observable behaviour change, no incorrect digests, just no
//! performance benefit yet. The dispatch is kept so the day a
//! working backend lands, it can be plugged in without changing the
//! call site or the tests.
//!
//! When the backends are added, the contract is:
//!   - `hardware_available()` returns true on the target CPU.
//!   - `compress_block` produces the same 8-u32 state update as
//!     `sha256::process_block_scalar` for every 64-byte block.
//!   - The test module below proves it on the FIPS 180-4 vectors
//!     and on a randomized split-input sweep; both run on every
//!     build, so a regression in either path trips a test rather
//!     than producing a wrong hash in production.

/// True when the running CPU has the matching SHA-256 hardware
/// instructions. Cached after the first call; the OS feature-detection
/// path is not free, and a SHA-256 search walks many blocks.
#[cfg(feature = "sha256-hw")]
pub fn hardware_available() -> bool {
    use core::sync::atomic::{AtomicU8, Ordering};
    // 0 = uninitialized, 1 = no, 2 = yes. Atomic on a single byte so
    // the first-time check is race-free across worker threads.
    static CACHE: AtomicU8 = AtomicU8::new(0);
    match CACHE.load(Ordering::Relaxed) {
        1 => return false,
        2 => return true,
        _ => {}
    }
    // The backends are not implemented on stable yet, so the runtime
    // detection is the cap, not the implementation. When the asm
    // paths land, change the "false" arm to a true dispatch into
    // the matching backend.
    let detected = false;
    let _ = detect;
    CACHE.store(if detected { 2 } else { 1 }, Ordering::Relaxed);
    detected
}

#[cfg(feature = "sha256-hw")]
fn detect() -> bool {
    #[cfg(target_arch = "x86_64")]
    {
        // Available on stable Rust for the `sha` feature, but the
        // intrinsics it gates (`_mm_sha256rnds2` etc.) are still
        // nightly. When a stable surface lands, this is the check
        // that lights up the hardware path.
        std::is_x86_feature_detected!("sha")
    }
    #[cfg(target_arch = "aarch64")]
    {
        std::is_aarch64_feature_detected!("sha2")
    }
    #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
    {
        false
    }
}

/// Compress one 64-byte block. With the current backend stub, this
/// just routes through the scalar path. When the SHA-NI / ARMv8 SHA2
/// backends land, this becomes the dispatch into them.
#[cfg(feature = "sha256-hw")]
pub(crate) fn compress_block(state: &mut [u32; 8], block: &[u8; 64]) {
    // Backend stub: the asm/intrinsics backends are not yet
    // implemented on stable Rust, so the call falls through to the
    // scalar compression. Keeping the function in place means the
    // dispatch in `Sha256::process_block` and the tests in this
    // module do not need to change when the backends land.
    super::sha256::process_block_scalar(state, block);
}

#[cfg(test)]
mod tests {
    use super::super::sha256::Sha256;
    use super::super::sha256_ref::reference_hash;
    use alloc::string::String;

    fn hex(bytes: &[u8]) -> String {
        use core::fmt::Write as _;
        let mut s = String::with_capacity(bytes.len() * 2);
        for b in bytes {
            let _ = write!(s, "{b:02x}");
        }
        s
    }

    /// FIPS 180-4 examples + 1-million-'a' vector. The hardware path
    /// runs only when the `sha256-hw` feature is on AND the running CPU
    /// has the matching ISA extension; on any other build this test is
    /// the same correctness check the scalar carries.
    #[test]
    fn hardware_matches_fips_examples() {
        let cases: &[&[u8]] = &[
            b"",
            b"a",
            b"abc",
            b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq",
        ];
        for case in cases {
            let h = Sha256::hash(case);
            assert_eq!(hex(&h), hex(&reference_hash(case)), "len {}", case.len());
        }
        let million = alloc::vec![b'a'; 1_000_000];
        let h = Sha256::hash(&million);
        assert_eq!(hex(&h), hex(&reference_hash(&million)));
    }

    /// Randomized split-input sweep. Runs on every build, with or
    /// without the `sha256-hw` feature, and on every CPU: the FIPS
    /// vectors above are the correctness floor; this test makes sure
    /// the block-buffering path is identical regardless of which
    /// backend `process_block` is dispatching to.
    #[test]
    fn hardware_matches_split_inputs() {
        let data: alloc::vec::Vec<u8> = (0..4096u32)
            .map(|i| (i.wrapping_mul(2654435761) >> 16) as u8)
            .collect();
        for split in [1usize, 7, 31, 63, 64, 65, 127, 128, 255, 1023, 2048, 4095] {
            let mut h = Sha256::new();
            h.update(&data[..split]);
            h.update(&data[split..]);
            assert_eq!(h.finalize(), reference_hash(&data), "split at {split}");
        }
    }
}
