//! rgx-core: the adapter contract and the extraction pipeline.
//!
//! `no_std` + `alloc`. The `std` feature gates what genuinely needs it; see
//! `Cargo.toml` for what that costs on the way off.
//!
//! Design decisions:
//! - Adapters are **sync and work on byte slices** — no async, no streams,
//!   no subprocess in the core model. Heavy externals live in
//!   `rgx-adapters-docs` and spawn only when their binary is installed.
//! - Adapters never recurse themselves: archives return
//!   [`Outcome::Members`] and the pipeline recurses uniformly (depth limits,
//!   member caps, bail semantics all live in one place).
//! - `Outcome::Bail` mirrors rga's bail-out semantics: the file falls back
//!   to the next adapter and finally to raw-text search — an adapter
//!   declining a file is never an error.
//! - **Byte scanning is the `memchr` crate, vendored** — `pub mod memchr;`,
//!   a copy of upstream adapted to `no_std + alloc`, so this crate's
//!   dependency graph no longer carries `memchr = "2"`. See below.
//!
//! ## Byte scanning: the crate wins
//!
//! This crate used to carry `src/memchr.rs` — a 901-line byte scanner of its
//! own, AVX2 behind runtime detection with a SWAR fallback. It was measured
//! and deleted, on the evidence of the table below rather than on an inherited
//! claim that it was "4–23× slower than the crate", which had never been run.
//!
//! `cargo run --release -p rgx-core --example memchr_bench` reproduces the
//! table. Both implementations link into that one binary and run over the
//! same buffers, so it is same-process and same-input. Shapes are the ones rgx
//! actually searches: `buf64` is the searcher's 64 KiB read buffer, `buf8m`
//! one large text file end to end, `blob8m` a binary blob. Best of 7 timed
//! rounds after 2 warmup rounds. `ratio` is `in-house / crate`, so above 1.00
//! means the crate was faster.
//!
//! | case                                    | in-house | crate  | ratio | winner   |
//! |-----------------------------------------|---------:|-------:|------:|----------|
//! | memchr 1B absent (NUL sniff), 64 KiB    |   0.637us| 0.426us| 1.50x | crate    |
//! | memchr 1B absent (NUL sniff), 8 MiB     |  82.051us| 66.073us| 1.24x | crate   |
//! | memchr 1B absent, 8 MiB blob            |  83.563us| 63.589us| 1.31x | crate    |
//! | 5-byte needle absent, 8 MiB blob        | 424.769us|146.246us| 2.90x | crate    |
//! | memrchr absent byte, 8 MiB file         | 445.497us| 63.186us| 7.05x | crate    |
//! | memchr_iter newline-dense, 64 KiB       |   8.062us| 1.118us| 7.21x | crate    |
//! | memchr_iter newline-dense, 8 MiB file   |1350.960us|139.882us| 9.66x | crate    |
//! | memchr 1B present at end, 64 KiB        |   0.003us| 0.006us| 0.48x | in-house |
//! | memchr 1B present at start, 64 KiB      |   0.003us| 0.003us| 0.87x | in-house |
//! | 2-byte needle absent, 64 KiB            |   0.637us| 0.897us| 0.71x | in-house |
//! | 2-byte needle absent, 8 MiB blob        |  83.580us|109.205us| 0.77x | in-house |
//! | 3-byte needle absent, 64 KiB            |   0.632us| 0.898us| 0.70x | in-house |
//! | 3-byte needle absent, 8 MiB blob        |  83.157us|109.242us| 0.76x | in-house |
//! | 2-byte needle present (markup), 64 KiB  |   0.005us| 0.022us| 0.23x | in-house |
//! | 3-byte needle present (markup), 64 KiB  |   0.005us| 0.023us| 0.22x | in-house |
//! | 6-byte needle present (markup), 8 MiB   |   0.015us| 0.027us| 0.56x | in-house |
//! | memrchr hit at end, 64 KiB              |   0.003us| 0.005us| 0.61x | in-house |
//! | memrchr hit mid-buffer, 64 KiB          |   0.003us| 0.005us| 0.71x | in-house |
//!
//! The 11–7 scoreboard is a bad way to read this. Every row in-house wins
//! returns within the first few bytes — a needle that occurs early, a
//! terminator near the end of a read — and wins by 2 ns to 260 ns on calls
//! already under half a microsecond. Every row it loses scans the whole
//! buffer, and loses by 16 us to 1211 us. The wins are in nanoseconds because
//! there is nothing left to win; the losses are in microseconds because a
//! whole 8 MiB file is being walked. And that is the shape of the work: every
//! call site rgx has is per-file over the corpus, per-read over a 64 KiB
//! buffer, or an iterator over a large haystack. None is the early-return
//! shape that is the module's one genuine advantage.
//!
//! Two specific findings:
//!
//! - **`memrchr` has no SIMD in-house.** SWAR in both directions, 8 bytes per
//!   word, where the crate scans backwards 32 at a time — the whole 7.05x on
//!   the absent-byte row. It costs little in practice (the searcher asks for
//!   the newest terminator in freshly read bytes, nearly always at the end,
//!   where the two are within 2 ns), but it is a real gap, not noise.
//! - **`memchr_iter` is a restart loop against a bitmap.** The in-house
//!   iterator re-enters the scan at each hit; the crate's keeps a candidate
//!   mask per block and yields bits out of it. Newline-dense text is the worst
//!   case for the restart and the best for the bitmap, which is why that row
//!   is the largest. The paragraph-break walk in `src/main.rs` is the only
//!   caller.
//!
//! On the 2- and 3-byte rows: `memchr2` and `memchr3` exist in neither
//! implementation in a form that lines up and nothing in the workspace calls
//! them, so the in-house side is the substring search — anchor-scan-then-
//! confirm, which is what those two functions are for. The crossover: in-house
//! is ahead at 2 and 3 bytes (0.70–0.77x), the crate is 2.90x ahead at 5.
//!
//! Two things this did not buy, recorded so they are not re-litigated: the
//! differential harness that held the two implementations to each other went
//! with the module, and the crate's prebuilt `Finder` — which amortises
//! matcher setup across many searches — is still unused, because every search
//! here is a one-shot over a buffer. `memmem::find` remains the right entry
//! point for a single search.

#![no_std]

// macro_use brings vec!/format!/write! into every submodule, which a
// no_std crate needs and the std prelude used to provide for free.
#[macro_use]
extern crate alloc;

use alloc::borrow::Cow;
use alloc::boxed::Box;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
#[cfg(feature = "std")]
extern crate std;

pub mod base64;
pub mod byteclass;
pub mod cfb;
pub mod compress;
pub mod encoding;
pub mod error;
pub mod fixed;
pub mod frequency;
pub mod memchr;
#[cfg(feature = "std")]
pub mod memmap2;

pub mod casefold_table;
pub mod djson;
pub mod fuzzkit;
pub mod hybrid;
pub mod index;
pub mod index_io;
pub mod literal;
pub mod maskstats;
pub mod matcher;
pub mod nfa;
pub mod ngram;
pub mod numparse;
pub mod prefilter;
pub mod regex;
pub mod required;
pub mod scan;
pub mod searcher;
pub mod sha256;
pub mod sha256_hw;
// The independent scalar implementation exists only so the split-input and
// block-by-block tests have something that is not the code under test. It
// was unconditional, which put ~200 lines of test-only code in the shipped
// `rgx` binary and in `--no-default-features`.
#[cfg(any(test, doc))]
pub mod sha256_ref;
pub mod syntax;
pub mod tokenize;

#[cfg(feature = "tv")]
pub mod tv;
#[cfg(feature = "tv-runtime")]
pub mod tv_runtime;
pub mod unicode_case;
pub mod vm;
pub mod zip;

pub use error::{Context, Error, Result};

/// `anyhow!` expands to `format!`, and a macro that names `alloc` only
/// works inside this crate. Re-exporting it under `$crate` is what lets the
/// macro work in a caller that is itself `no_std`.
#[doc(hidden)]
pub use alloc::format;

/// `f64::from_str` is in `std`, not `core`, so a `no_std` build cannot parse
/// a JSON float. This is the seam `json/parser.rs` was already written
/// against (`#[cfg(not(feature = "std"))] use crate::NoStdFloat`); the trait
/// it named had never been defined here, so this crate could not have
/// compiled without `std` no matter what its Cargo features claimed.
///
/// Under `std` the impl is the standard parser. Under `alloc` alone it is a
/// decimal parser over what RFC 8259 allows: an optional sign, an integer part
/// with no redundant leading zero, an optional fraction, an optional exponent.
/// Anything else is `Err`, which the caller turns into its own message.
pub trait NoStdFloat: Sized {
    /// Parse a float. The error carries no detail: the standard library's
    /// error type does not exist here, and the caller supplies the message.
    #[allow(clippy::result_unit_err)]
    fn parse(s: &str) -> Result<Self, ()>;
}

#[cfg(feature = "std")]
impl NoStdFloat for f64 {
    fn parse(s: &str) -> Result<Self, ()> {
        use core::str::FromStr;
        f64::from_str(s).map_err(|_| ())
    }
}

/// The `alloc`-only decimal parser behind the `no_std` impl.
///
/// Compiled under every feature, not just `no_std`, so that the tests can
/// hold it against the standard library's parser on a build that has one.
/// A `no_std` path nobody can test is a `no_std` path nobody should trust.
///
/// Parse a JSON number without `std`.
// `()` is the contract: see `NoStdFloat::parse`, the caller owns the message.
#[allow(clippy::result_unit_err)]
pub fn parse_decimal(s: &str) -> Result<f64, ()> {
    let b = s.as_bytes();
    let mut i = 0usize;
    let mut neg = false;
    // '-' only: RFC 8259's number grammar has no leading '+', whatever the
    // standard library's parser does with one.
    if i < b.len() && b[i] == b'-' {
        neg = true;
        i += 1;
    }

    // One mantissa and one scale, applied once at the end. Accumulating into
    //  a digit at a time instead rounds at every step and is a ULP off
    // the standard parser on ordinary input -- 3.14159 came out as
    // 3.1415900000000003, which  caught.
    let mut mant: u128 = 0;
    let mut scale: i32 = 0;
    let push = |d: u8, mant: &mut u128, _scale: &mut i32| {
        // past u128 the remaining digits are dropped and the scale absorbs
        // them; a JSON number with 39 integer digits is not a real case
        if *mant <= (u128::MAX - u128::from(d)) / 10 {
            *mant = *mant * 10 + u128::from(d);
            true
        } else {
            false
        }
    };

    let int_start = i;
    while i < b.len() && b[i].is_ascii_digit() {
        let keep = push(b[i] - b'0', &mut mant, &mut scale);
        if !keep {
            scale += 1;
        }
        i += 1;
    }
    if i == int_start {
        return Err(());
    }
    if b[int_start] == b'0' && i - int_start > 1 {
        return Err(());
    }

    if i < b.len() && b[i] == b'.' {
        i += 1;
        let frac_start = i;
        while i < b.len() && b[i].is_ascii_digit() {
            if push(b[i] - b'0', &mut mant, &mut scale) {
                scale -= 1;
            }
            i += 1;
        }
        if i == frac_start {
            return Err(());
        }
    }

    if i < b.len() && (b[i] == b'e' || b[i] == b'E') {
        i += 1;
        let mut eneg = false;
        if i < b.len() && (b[i] == b'-' || b[i] == b'+') {
            eneg = b[i] == b'-';
            i += 1;
        }
        let exp_start = i;
        let mut exp: i32 = 0;
        while i < b.len() && b[i].is_ascii_digit() {
            // saturating: an exponent this large is infinity or zero anyway
            exp = exp
                .saturating_mul(10)
                .saturating_add(i32::from(b[i] - b'0'));
            i += 1;
        }
        if i == exp_start {
            return Err(());
        }
        scale = scale.saturating_add(if eneg { -exp } else { exp });
    }
    if i != b.len() {
        return Err(());
    }

    Ok(if neg {
        -numparse::scale_decimal(mant, scale)
    } else {
        numparse::scale_decimal(mant, scale)
    })
}

#[cfg(not(feature = "std"))]
impl NoStdFloat for f64 {
    fn parse(s: &str) -> Result<Self, ()> {
        parse_decimal(s)
    }
}

/// What an adapter made of a file.
pub enum Outcome {
    /// Searchable plain text (already encoding-normalized by the adapter).
    Text(Vec<u8>),
    /// Archive members: `(virtual_inner_path, raw_bytes)` to recurse.
    Members(Vec<(String, Vec<u8>)>),
    /// "This file is not actually mine" — try the next adapter. The clean
    /// way to decline: the chain continues, and a file every adapter has
    /// declined reaches the raw searcher with its bytes intact.
    ///
    /// Declining with `bail!` also reaches the raw searcher — `process`
    /// treats an adapter `Err` as a decline plus a note rather than as a
    /// replacement for the file — but it stops the chain at the first
    /// failure, and it puts a line in the corpus that is not in the file.
    /// Reserve it for a failure worth reporting and use this for the rest.
    Bail(String),
}

/// Pipeline knobs shared by the CLI and tests.
pub struct Ctx {
    /// max archive nesting depth (rga default: 4)
    pub max_depth: usize,
    /// max members expanded per archive level
    pub max_members: usize,
    /// max bytes read from one real file before adapters see it
    pub max_file_size: usize,
}

impl Default for Ctx {
    fn default() -> Self {
        Self {
            max_depth: 4,
            max_members: 4096,
            max_file_size: 1 << 30,
        }
    }
}

/// A searchable unit produced by the pipeline.
#[derive(Debug)]
pub struct Found {
    /// e.g. `bundle.zip/inner/record.txt`
    pub path: String,
    pub bytes: Vec<u8>,
    /// adapter that produced it ("raw" = no adapter claimed the file)
    pub via: &'static str,
}

/// What an adapter's output is *for*, relative to the bytes on disk.
///
/// The distinction is the whole superset question, and it is a property of the
/// file, not of the code. A zip, a PDF, a PNG, a `.doc`: the bytes on disk are
/// not searchable text, so an adapter that reads them [`Reach::Reveal`]s
/// content ripgrep cannot reach at all, and swapping the rendering in for the
/// file costs the user nothing — rgx's answer is a strict superset of rg's and
/// that is the entire point. A `.md`, a `.html`, a `.tex`, a `.rtf`: the bytes
/// on disk *are* searchable text, so an adapter that reads them
/// [`Reach::Render`]s a second, lossy view of the same lines. Everything the
/// renderer drops is then unreachable — HTML `<script>` and `<style>` bodies,
/// LaTeX control words, RTF destination groups, Markdown link targets — and a
/// pattern that occurs only there is a line `rg` prints and rgx does not.
/// That is not a faster ripgrep, it is a strict subset of one, so the default
/// policy is to keep the file's own bytes and let the renderer be asked for
/// instead of answered with (see [`Policy`]).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Reach {
    /// The file is not searchable as it stands (compressed, or a binary
    /// container); the adapter surfaces bytes the raw searcher cannot reach.
    Reveal,
    /// The file is searchable as it stands; the adapter produces a *lossy
    /// rendering* of it and the rendering can hide what the raw bytes say.
    Render,
}

/// Which view of an adapter-claimed file rgx searches.
///
/// `Source` is the default because it is the only one of the three under which
/// rgx is a superset of ripgrep on a [`Reach::Render`] format, and because it
/// is the only one that leaves every format which *already* matches rg byte for
/// byte untouched: a [`Reach::Reveal`] adapter is unaffected in all three.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Policy {
    /// Search the file's own bytes; a `Render` adapter does not get the file.
    Source,
    /// Search the adapter's rendering, whatever it is. This is what every rgx
    /// before `Source` did.
    Render,
    /// Search both and report both. Heavier and duplicate-heavy: a pattern
    /// that occurs in the rendering almost always occurs in the source too, so
    /// most matches print twice under two line numbers, and `-c` prints the
    /// same path twice. It is not even a strict superset — where the renderer
    /// *declines* the file it produces no `Text` at all, and the union falls
    /// back to `Source` for that file, which is the behaviour, so it only
    /// recovers what `Source` recovers (measured: 134% of ripgrep on markup
    /// against `Source`'s 100%, at 34% more lines printed).
    Both,
}

// 0 means "not resolved yet"; the enum values are stored as 1..=3.
static POLICY: core::sync::atomic::AtomicU8 = core::sync::atomic::AtomicU8::new(0);

// The `ablate_mask` override, raised only by [`set_ablate_mask`]. The
// environment is still read when it is down, so an unset override leaves
// `RGX_ABLATE_MASK` in charge.
static ABLATE_MASK: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

const POLICY_SOURCE: u8 = 1;
const POLICY_RENDER: u8 = 2;
const POLICY_BOTH: u8 = 3;

fn policy_code(p: Policy) -> u8 {
    match p {
        Policy::Source => POLICY_SOURCE,
        Policy::Render => POLICY_RENDER,
        Policy::Both => POLICY_BOTH,
    }
}

fn policy_from_code(c: u8) -> Policy {
    match c {
        POLICY_RENDER => Policy::Render,
        POLICY_BOTH => Policy::Both,
        _ => Policy::Source,
    }
}

/// The environment variable that names the policy, and its accepted values.
pub const POLICY_ENV: &str = "RGX_ADAPTER_VIEW";

/// The active policy, resolved once from [`POLICY_ENV`].
///
/// `source` (the default) keeps a `Render` format's own bytes; `render` hands
/// the file to the adapter as before; `both` reports the two views side by
/// side. An unrecognised value is a `Source`, which is the conservative end:
/// it is the only policy that cannot lose a line `rg` prints.
pub fn policy() -> Policy {
    let cached = POLICY.load(core::sync::atomic::Ordering::Relaxed);
    if cached != 0 {
        return policy_from_code(cached);
    }
    let resolved = policy_from_env();
    // Two threads racing here compute the same value from the same
    // environment, so the plain store is enough.
    POLICY.store(policy_code(resolved), core::sync::atomic::Ordering::Relaxed);
    resolved
}

#[cfg(feature = "std")]
fn policy_from_env() -> Policy {
    match std::env::var(POLICY_ENV).ok().as_deref().map(str::trim) {
        Some("render") | Some("rendered") | Some("adapter") => Policy::Render,
        Some("both") | Some("union") => Policy::Both,
        _ => Policy::Source,
    }
}

#[cfg(not(feature = "std"))]
fn policy_from_env() -> Policy {
    Policy::Source
}

/// Set the policy programmatically, overriding [`POLICY_ENV`].
///
/// Intended for the CLI flag and for tests, which have to exercise both
/// policies in one process.
pub fn set_policy(p: Policy) {
    POLICY.store(policy_code(p), core::sync::atomic::Ordering::Relaxed);
}

/// Ablation instrumentation: the compiled-mask tier switched off at
/// pattern-compile time (`RGX_ABLATE_MASK`).
///
/// Set, the mask builder declines to produce a mask anchor and the
/// builder falls back to its next anchor, or to confirming every line when
/// there is none. It exists so the paper's end-to-end tier comparison can
/// run the same query with the tier present and absent.
///
/// **Contract: changing this may change cost, never answers.** The fallback
/// is a slower search over the same superset of candidates, so a result that
/// differs between settings is a bug in the fallback, not in the switch. It
/// is inert without `std`, where there is no environment to read.
#[cfg(feature = "std")]
pub fn ablate_mask() -> bool {
    ABLATE_MASK.load(core::sync::atomic::Ordering::Relaxed)
        || std::env::var_os("RGX_ABLATE_MASK").is_some()
}

#[cfg(not(feature = "std"))]
pub fn ablate_mask() -> bool {
    ABLATE_MASK.load(core::sync::atomic::Ordering::Relaxed)
}

/// Force [`ablate_mask`] on or off, overriding the environment.
///
/// For tests and in-process harnesses, which need the switch inside one
/// process the way [`set_policy`] needs the policy. Overriding off is the
/// reason this is not just a set-to-true: a test must be able to leave the
/// state it found.
pub fn set_ablate_mask(on: bool) {
    ABLATE_MASK.store(on, core::sync::atomic::Ordering::Relaxed);
}

/// Ablation instrumentation: the per-file n-gram signature switched off at
/// query-build time (`RGX_ABLATE_NGRAM`).
///
/// Set, the query signature is `None`, which is the filter's own stand-down
/// state: every file is searched. Same contract as [`ablate_mask`]: cost
/// may change, answers never do, and without `std` the switch is inert.
#[cfg(feature = "std")]
pub fn ablate_ngram() -> bool {
    std::env::var_os("RGX_ABLATE_NGRAM").is_some()
}

#[cfg(not(feature = "std"))]
pub fn ablate_ngram() -> bool {
    false
}

/// A content adapter. Implementations must be cheap to construct; tool
/// detection for external-tool adapters happens in `available()`.
pub trait Adapter: Send + Sync {
    fn name(&self) -> &'static str;
    /// lowercased extensions this adapter claims, e.g. `&["zip", "jar"]`
    fn extensions(&self) -> &'static [&'static str];
    /// false if the adapter cannot run in this environment (missing binary).
    /// In-process adapters always return true.
    fn available(&self) -> bool {
        true
    }
    /// Whether this adapter reveals a file ripgrep cannot read, or renders one
    /// it can. Defaults to [`Reach::Reveal`], which is the answer for every
    /// adapter that reads a container or a binary format; see [`Reach`].
    fn reach(&self) -> Reach {
        Reach::Reveal
    }

    /// Whether the answer to [`Self::reach`] can change once the file's bytes
    /// are in hand.
    ///
    /// [`Reach`] is a property of the FILE, and an extension does not know
    /// what a file is. `.pem` and `.der` are the same adapter and not the same
    /// question: a PEM is base64 text ripgrep reads perfectly, so a renderer
    /// that replaces those bytes with nine lines of certificate description
    /// deletes thirty-nine lines of matches; a DER is binary, so the same
    /// renderer is a strict superset and the bytes it drops were never
    /// readable content. Only an adapter that says so pays a read to find
    /// out, which is what keeps `.html` and `.md` off the adapter path.
    fn reach_needs_bytes(&self) -> bool {
        false
    }

    /// [`Self::reach`] for a file whose bytes are in hand. The default defers
    /// to the static answer, so an adapter that does not ask for the bytes is
    /// unaffected by having them.
    fn reach_for(&self, _data: &[u8]) -> Reach {
        self.reach()
    }
    fn extract(&self, data: &[u8], path: &str) -> Result<Outcome>;
}

/// Ordered adapter registry; first matching available adapter wins, and a
/// bail falls through to the next candidate (rga issue #3 semantics).
///
/// Lookups go through a per-extension index so candidate resolution is
/// O(candidates for this extension), not O(all adapters) — matters when
/// hundreds of adapters accumulate.
pub struct Registry {
    adapters: Vec<Box<dyn Adapter>>,
    by_ext: alloc::collections::BTreeMap<String, Vec<usize>>,
}

/// One adapter's extension list with repeats removed, order preserved.
///
/// `O(n^2)` on a list that is at most a few dozen entries long, and it borrows
/// rather than allocates, so it costs nothing on the startup path this guards.
/// See [`Registry::new`] for why a repeat is a cost and not a cosmetic issue.
fn dedup_exts(exts: &[&'static str]) -> Vec<&'static str> {
    let mut out: Vec<&'static str> = Vec::with_capacity(exts.len());
    for e in exts {
        if !out.contains(e) {
            out.push(e);
        }
    }
    out
}

impl Registry {
    /// Index `adapters` by extension.
    ///
    /// **A repeated extension in one adapter's list is collapsed, and that is
    /// a cost fix rather than tidiness.** The index pushes the adapter's index
    /// once per literal, so `"efi"` appearing twice in `Inspect::extensions`
    /// made `by_ext["efi"] = [inspect, inspect]`. A `.efi` file that then bails
    /// runs the *entire* inspection twice -- and `Inspect` is the most
    /// expensive adapter per byte in the tree, since it scans the file once
    /// per embedded signature (`inspect/src/lib.rs`). Two such lists shipped:
    /// `"efi"` in inspect and `"tlz"` in `archive`'s `Decompress`, the latter
    /// running a whole decompression twice. `dedup_exts` is the guard; the
    /// literals are fixed too, because a list that lists itself twice should
    /// not read as if it meant something.
    ///
    /// Collapsing is behaviour-preserving: the chain semantics for a repeated
    /// *identical* adapter are "run it again", which can only differ if the
    /// adapter is non-deterministic. Genuine pairs stay — `apk` is claimed by
    /// both `zip` and `inspect`, and both must run.
    pub fn new(adapters: Vec<Box<dyn Adapter>>) -> Self {
        Self::with_extra_claims(adapters, &[])
    }

    /// Index `adapters` by extension, plus `(adapter name, extension)` pairs
    /// the caller wants claimed that the adapter does not claim by default.
    ///
    /// **This exists because "which extensions should be read" is a policy
    /// question, and a hard-coded list makes it the adapter author's
    /// decision.** The concrete case: `Inspect` used to claim `o`, and every
    /// object file inside every static library reached it through the `ar`
    /// adapter -- 126,145 calls costing 641 ms on the cargo registry, all of it
    /// from two `winapi` crates that ship ~700 `.a` files between them. That is
    /// the wrong default for a corpus nobody asked to be triaged, and the wrong
    /// default is not a good enough reason to make the capability unreachable:
    /// someone reading object files out of a library on purpose is exactly who
    /// wants it, and they have no way to ask.
    ///
    /// `extra_claims` is therefore `(adapter, extension)` pairs appended after
    /// the built-in lists. Order matters and is preserved: the extra claim runs
    /// after the adapter's own, so a default claim still wins the chain's
    /// "first matching adapter" position, and an extension nobody claims
    /// simply gets no adapter.
    pub fn with_extra_claims(
        adapters: Vec<Box<dyn Adapter>>,
        extra_claims: &[(&str, &str)],
    ) -> Self {
        let mut by_ext: alloc::collections::BTreeMap<String, Vec<usize>> =
            alloc::collections::BTreeMap::new();
        for (i, a) in adapters.iter().enumerate() {
            for ext in dedup_exts(a.extensions()) {
                by_ext.entry(ext.to_string()).or_default().push(i);
            }
        }
        for (name, ext) in extra_claims {
            let Some(i) = adapters.iter().position(|a| a.name() == *name) else {
                continue;
            };
            let slot = by_ext.entry((*ext).to_string()).or_default();
            if !slot.contains(&i) {
                slot.push(i);
            }
        }
        Self { adapters, by_ext }
    }

    /// Every available adapter that could claim `ext`.
    ///
    /// The policy filter is NOT here. It used to be, and it could not be
    /// moved into [`process`] for free: under [`Policy::Source`] a `Render`
    /// adapter is dropped, and dropping it before the read is what lets the
    /// walker skip reading the file at all and go straight to the mmap path.
    /// An adapter whose answer depends on the bytes has to be asked after the
    /// read, which means the read happens -- so the cheap question is put
    /// back here, in [`Registry::may_claim`], and the real one is settled in
    /// `process` where the data is.
    pub fn candidates<'a>(&'a self, ext: &str) -> impl Iterator<Item = &'a dyn Adapter> + 'a {
        self.by_ext
            .get(ext)
            .into_iter()
            .flatten()
            .map(move |&i| &*self.adapters[i])
            .filter(|a| a.available())
    }

    /// true if any *available* adapter may claim this extension — lets callers
    /// skip reading the file entirely and use the raw mmap search path.
    ///
    /// "MAY", because an adapter that decides from the bytes cannot be
    /// answered here. It is still true for such an adapter under every
    /// policy: `Source` will drop it for a text file and `Reveal` for a
    /// binary one, and which one it is decides whether the file is read at
    /// all. The alternative -- answering "no" and never reading -- is the
    /// subset property on `.pem`.
    pub fn may_claim(&self, ext: &str) -> bool {
        let pol = policy();
        // `any`, not `next`: the list is ordered, and the policy filter used
        // to be applied before this, so the first surviving candidate was the
        // first candidate. It is not any more, and taking only the first
        // available adapter here would answer for the wrong one -- a `Render`
        // adapter ahead of a `Reveal` one for the same extension would send
        // the file down the raw path when a Reveal adapter was waiting
        // behind it to read it.
        self.candidates(ext)
            .any(|a| a.reach_needs_bytes() || pol != Policy::Source || a.reach() == Reach::Reveal)
    }

    /// Names of adapters that claim `ext` but whose external tool is not on
    /// `PATH`, so [`Self::candidates`] filters them out and [`Self::may_claim`]
    /// answers false.
    ///
    /// The file then goes down the raw passthrough, which is correct --
    /// nothing here can read it better -- but silent. A user who types
    /// `rgx ocrword scan.png` gets nothing and no reason, which is the same
    /// gap the adapter-failure note closes from the other side. A caller that
    /// wants to say WHY a claimed file was searched raw can ask here.
    #[must_use]
    pub fn unavailable_claimants(&self, ext: &str) -> Vec<&'static str> {
        self.by_ext
            .get(ext)
            .into_iter()
            .flatten()
            .map(move |&i| &*self.adapters[i])
            .filter(|a| !a.available())
            .map(|a| a.name())
            .collect()
    }
}

/// Lowercased file extension of `path` (empty slice when none).
///
/// The returned `Cow` is `Borrowed` for the common case (path already
/// lowercased, ASCII, no case to convert) and `Owned` only when a
/// non-ASCII byte forces allocation. Splits on both `/` and `\`, so a
/// Windows path like `a\b.c` is parsed the same as a POSIX path.
pub fn extension_of(path: &str) -> Cow<'_, str> {
    // Walk back to the basename: stop at the last separator (POSIX or
    // Windows), then split on the last `.`. A path with no `.` after
    // the separator returns the empty string. The .len() < path.len()
    // guard rejects the leading-component case where `.rsplit('.').next()`
    // would otherwise return the whole basename.
    let mut start = path.len();
    for (i, b) in path.bytes().enumerate().rev() {
        if b == b'/' || b == b'\\' {
            break;
        }
        start = i;
    }
    let basename = &path[start..];
    let ext = match basename.rsplit_once('.') {
        Some((_, ext)) => ext,
        None => "",
    };
    // ASCII-only fast path: copy the slice verbatim if every byte is
    // already lowercase ASCII. UTF-8 case folding (locale-sensitive,
    // surprisingly expensive) only fires on non-ASCII bytes.
    if ext
        .bytes()
        .all(|b| b.is_ascii_lowercase() || !b.is_ascii_alphabetic())
    {
        Cow::Borrowed(ext)
    } else {
        Cow::Owned(ext.to_lowercase())
    }
}

// --- opt-in per-adapter profiling (RGX_PROFILE=1) ---
static PROFILE_ON: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

pub fn profile_enabled() -> bool {
    PROFILE_ON.load(core::sync::atomic::Ordering::Relaxed)
}

pub fn profile_init() {
    PROFILE_ON.store(true, core::sync::atomic::Ordering::Relaxed);
}

/// The shared profile map. It used to be a `static MAP` declared INSIDE
/// each of `profile_add` and `profile_dump`, which made two independent
/// `OnceLock`s: the add side wrote one and the dump side read the other,
/// permanently empty. One module-level static, one map.
#[cfg(feature = "std")]
static PROFILE_MAP: std::sync::OnceLock<
    std::sync::Mutex<std::collections::HashMap<&'static str, (u64, u64)>>,
> = std::sync::OnceLock::new();

// Per-thread nesting depth, published by `process` for the adapters below.
// See `current_depth` for why this exists and what it is for.
#[cfg(feature = "std")]
std::thread_local! {
    static DEPTH: core::cell::Cell<usize> = const { core::cell::Cell::new(0) };
}

/// The archive nesting depth of the unit currently being extracted on this
/// thread. `0` outside any [`process`] call.
///
/// **This exists because a resource budget has to know where it is being
/// spent, and the budget cannot be told.** `MemberSink::new()` (the archive
/// family's work budget) is handed a fresh allowance by every adapter, so a
/// container nested inside a container was granted a second full budget rather
/// than a share of the first — with the shipped defaults a 4 MiB zip of zips
/// reached 4 TiB of expansion, because `max_depth` bounds the DEPTH of a tree
/// and never its total width times height.
///
/// `Adapter::extract(&self, data, path)` has no room for a depth and
/// `rgx-core` is `no_std`, so a thread-local is the one place the two halves
/// can meet: [`process`] sets it on the way in, and the adapters read it when
/// they build a sink. Nothing about the `Adapter` contract changes.
///
/// Returns 0 with `std` off, which restores the pre-existing behaviour rather
/// than inventing a ceiling.
#[cfg(feature = "std")]
pub fn current_depth() -> usize {
    DEPTH.try_with(|d| d.get()).unwrap_or(0)
}

/// See the module-level docs.
#[cfg(not(feature = "std"))]
pub fn current_depth() -> usize {
    0
}

/// Restores the previous depth on drop, so an early return out of `process`
/// cannot leave a worker permanently "nested".
///
/// Only ever held across the recursion in [`process`], never across an
/// adapter call, so the value an adapter sees is the depth of the container it
/// is extracting and not the depth of the entry inside it.
#[cfg(feature = "std")]
pub struct DepthGuard {
    #[cfg(feature = "std")]
    previous: usize,
}

#[cfg(feature = "std")]
impl DepthGuard {
    pub fn enter(depth: usize) -> Self {
        let previous = DEPTH.try_with(|d| d.replace(depth)).unwrap_or(0);
        Self { previous }
    }
}

#[cfg(feature = "std")]
impl Drop for DepthGuard {
    fn drop(&mut self) {
        let _ = DEPTH.try_with(|d| d.set(self.previous));
    }
}

#[cfg(feature = "std")]
pub fn profile_add(name: &'static str, ns: u64) {
    let map = PROFILE_MAP.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()));
    let mut m = map.lock().unwrap();
    let e = m.entry(name).or_insert((0, 0));
    e.0 += 1;
    e.1 += ns;
}

#[cfg(feature = "std")]
pub fn profile_dump() -> Vec<(&'static str, u64, u64)> {
    let map = PROFILE_MAP.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()));
    let m = map.lock().unwrap();
    m.iter().map(|(&k, &(c, t))| (k, c, t)).collect()
}

/// Run `path`/`data` through the adapter chain, appending every searchable
/// unit to `out`. `depth` is the current archive nesting depth (0 = real
/// file). Unclaimed files are emitted as-is under `via = "raw"`.
///
/// Every outcome puts the file's own bytes somewhere the searcher will read
/// them. An adapter that answers with `Text` or `Members` answers *for* the
/// file, and an adapter that answers `Bail` or `Err` is followed by the raw
/// bytes. The note an `Err` leaves behind is an extra unit, never the only
/// one: a corpus where a file the walk found cannot be searched at all is
/// less searchable than a corpus with no adapter in it, which is the opposite
/// of what the adapters are for.
///
/// Returns `true` when *this* invocation fell through to the raw
/// passthrough — i.e. no adapter produced anything and the single appended
/// unit is the input bytes (modulo encoding normalization). Callers use
/// that to give the file the raw on-disk treatment instead of searching a
/// copy. A member that recursed to raw inside a `Members` expansion does
/// NOT count: the outer adapter did produce output.
pub fn process(
    reg: &Registry,
    path: &str,
    data: &[u8],
    depth: usize,
    ctx: &Ctx,
    out: &mut Vec<Found>,
) -> bool {
    // Publish the nesting depth for the adapters below, which build their work
    // budget and cannot be told otherwise -- `Adapter::extract` has no depth
    // parameter and this crate is `no_std`. See [`current_depth`].
    #[cfg(feature = "std")]
    let _depth_guard = DepthGuard::enter(depth);
    let ext = extension_of(path);
    let mut bailed: Vec<&str> = Vec::new();
    let pol = policy();
    for adapter in reg.candidates(&ext) {
        // The policy is applied HERE rather than in the registry, because an
        // adapter that answers from the bytes cannot be answered without
        // them. Under `Source` a `Render` adapter does not get the file: the
        // bytes ripgrep can already read are kept, and the rendering is
        // asked for instead of answered with. Falling through to the raw
        // passthrough at the bottom of this function is that answer.
        if pol == Policy::Source
            && adapter.reach_for(data) == Reach::Render
            && !adapter.reach_needs_bytes()
        {
            bailed.push(adapter.name());
            continue;
        }
        #[cfg(feature = "std")]
        let outcome = {
            let (o, elapsed) = if profile_enabled() {
                let t = std::time::Instant::now();
                let r = adapter.extract(data, path);
                (r, Some(t.elapsed()))
            } else {
                (adapter.extract(data, path), None)
            };
            if let Some(e) = elapsed {
                profile_add(adapter.name(), e.as_nanos() as u64);
            }
            o
        };
        #[cfg(not(feature = "std"))]
        let outcome = adapter.extract(data, path);
        match outcome {
            Ok(Outcome::Text(text)) => {
                // `Policy::Both` is the union, and the only mode in which a
                // `Render` format is searched twice. It is deliberately
                // scoped to a `Render` adapter's `Text`: a `Reveal` adapter's
                // output is not a view of the same lines (it is a view of
                // bytes the file does not contain in that form), so adding
                // the container's own bytes there would be a second answer to
                // a different question, not a fallback.
                //
                // EXCEPT when the adapter asked for the bytes and the file
                // turned out to be one ripgrep can read. Then the rendering
                // is a view of text that is also in the file, and the union
                // is the only answer that is neither a subset nor a loss:
                // searching just the file's bytes matches rg exactly and
                // throws away what the renderer found, and searching just the
                // rendering drops 39 lines of matches down to 9 on
                // `hyper-rustls-0.27.7/examples/sample.pem`. Measured over
                // the registry this is the difference between 4 files that
                // are a strict subset and 89 files that stop being a strict
                // superset -- and the union is 4 and 0.
                if policy() == Policy::Both && adapter.reach_for(data) == Reach::Render
                    || (policy() == Policy::Source
                        && adapter.reach_needs_bytes()
                        && adapter.reach_for(data) == Reach::Render)
                {
                    out.push(Found {
                        path: path.to_string(),
                        bytes: normalize_encoding(data.to_vec()),
                        via: "raw",
                    });
                }
                out.push(Found {
                    path: path.to_string(),
                    bytes: normalize_encoding(text),
                    via: adapter.name(),
                });
                return false;
            }
            Ok(Outcome::Members(members)) => {
                if depth >= ctx.max_depth {
                    out.push(Found {
                        path: path.to_string(),
                        bytes: format!("[rgx: max archive depth reached ({})]\n", depth)
                            .into_bytes(),
                        via: adapter.name(),
                    });
                    return false;
                }
                for (inner, bytes) in members.into_iter().take(ctx.max_members) {
                    let inner_path = format!("{}/{}", path, inner);
                    process(reg, &inner_path, &bytes, depth + 1, ctx, out);
                }
                return false;
            }
            Ok(Outcome::Bail(reason)) => {
                bailed.push(adapter.name());
                let _ = reason;
            }
            Err(e) => {
                // The note is an *extra* unit, never a replacement for the
                // file. This arm used to push the note and stop, which read
                // as "keep the corpus searchable" and was the opposite: every
                // extractor declines with `bail!`, and `bail!` arrives here
                // as `Err`, so a file whose adapter merely found nothing —
                // a PNG with no EXIF, a PEM the x509 walker cannot parse, a
                // `.tex` without `\documentclass` — lost its bytes entirely.
                // rgx then reported nothing for a file ripgrep reads, which
                // is the subset property one more time, reached through a
                // different door, and it cost 14,251 lines over 1,387 of the
                // 4,881 claimed files in the registry corpus.
                //
                // So the file's own bytes go in beside the note and both are
                // searched: the note still says what happened, and the bytes
                // are still there for the pattern that lives in them. The
                // rga issue #151 lesson — one bad file must not kill the
                // corpus search — holds, and now it also holds for a file
                // that is merely not what the adapter wanted.
                out.push(Found {
                    path: path.to_string(),
                    bytes: format!(
                        "[rgx: adapter '{}' failed: {}]\n",
                        adapter.name(),
                        format!("{e:#}").lines().next().unwrap_or("")
                    )
                    .into_bytes(),
                    via: adapter.name(),
                });
                out.push(Found {
                    path: path.to_string(),
                    bytes: normalize_encoding(data.to_vec()),
                    via: "raw",
                });
                return false;
            }
        }
    }
    // no adapter (or all bailed): raw passthrough — rg-compatible fallback.
    //
    // When the extension IS claimed but every claimant's tool is missing, the
    // raw path is still the right answer, but silence is not: the file type
    // was known and unreadable, which is a different situation from "nothing
    // here claims it". A one-line note beside the bytes says which, the same
    // contract the adapter-failure note above honours.
    let ext = extension_of(path);
    let missing = reg.unavailable_claimants(&ext);
    if !missing.is_empty() {
        out.push(Found {
            path: path.to_string(),
            bytes: format!(
                "[rgx: adapter '{}' skipped: its external tool is not on PATH]\n",
                missing.join("', '")
            )
            .into_bytes(),
            via: "raw",
        });
    }
    out.push(Found {
        path: path.to_string(),
        bytes: normalize_encoding(data.to_vec()),
        via: "raw",
    });
    let _ = bailed;
    true
}

/// Encoding post-processing (rga `postproc_encoding` parity): UTF-16 content
/// (BOM `FF FE` LE / `FE FF` BE) is transcoded to UTF-8 so plain-text search
/// actually matches it. Everything else passes through untouched.
pub fn normalize_encoding(bytes: Vec<u8>) -> Vec<u8> {
    let (le, rest) = match bytes.get(0..2) {
        Some([0xFF, 0xFE]) => (true, &bytes[2..]),
        Some([0xFE, 0xFF]) => (false, &bytes[2..]),
        _ => return bytes,
    };
    let units: Vec<u16> = rest
        .as_chunks::<2>()
        .0
        .iter()
        .map(|c| {
            if le {
                u16::from_le_bytes([c[0], c[1]])
            } else {
                u16::from_be_bytes([c[0], c[1]])
            }
        })
        .collect();
    String::from_utf16_lossy(&units).into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Upper;
    impl Adapter for Upper {
        fn name(&self) -> &'static str {
            "upper"
        }
        fn extensions(&self) -> &'static [&'static str] {
            &["up"]
        }
        fn extract(&self, data: &[u8], _path: &str) -> Result<Outcome> {
            Ok(Outcome::Text(
                String::from_utf8_lossy(data).to_uppercase().into_bytes(),
            ))
        }
    }

    /// An adapter whose answer depends on the FILE, not on the extension:
    /// text is a rendering of something ripgrep already reads, binary is a
    /// container it cannot. The `.pem` and the `.der` are the same adapter.
    struct ByBytes;
    impl Adapter for ByBytes {
        fn name(&self) -> &'static str {
            "bybytes"
        }
        fn extensions(&self) -> &'static [&'static str] {
            &["bb"]
        }
        fn reach_needs_bytes(&self) -> bool {
            true
        }
        fn reach_for(&self, data: &[u8]) -> Reach {
            if data.contains(&0) {
                Reach::Reveal
            } else {
                Reach::Render
            }
        }
        fn extract(&self, _data: &[u8], _path: &str) -> Result<Outcome> {
            Ok(Outcome::Text(
                b"RENDERED
"
                .to_vec(),
            ))
        }
    }

    /// One extension, two answers, and the default policy has to give each
    /// file the right one.
    ///
    /// The binary file has no readable bytes to keep, so it is rendered
    /// alone. The text file is searched TWICE under the default: once on its
    /// own bytes, which is what ripgrep reads, and once on the rendering,
    /// which is a view of text derived from those bytes. Either alone is
    /// wrong -- the file alone matches rg exactly and throws away what the
    /// renderer found, the rendering alone drops 39 lines of matches down to
    /// 9 on `hyper-rustls-0.27.7/examples/sample.pem` -- so the union is the
    /// only answer that is neither a subset nor a loss.
    #[test]
    fn reach_is_a_property_of_the_file_and_not_of_the_extension() {
        for (bytes, want) in [
            (
                b"plain text
"
                .as_slice(),
                "RENDERED
",
            ),
            (
                b"bin\0ary".as_slice(),
                "RENDERED
",
            ),
        ] {
            under_policy(Policy::Source, || {
                let reg = Registry::new(vec![Box::new(ByBytes)]);
                // The registry cannot answer without the bytes, so the file
                // is read: answering "no" here is the subset property again.
                assert!(reg.may_claim("bb"), "the file was never read");
                let mut out = Vec::new();
                process(&reg, "a.bb", bytes, 0, &Ctx::default(), &mut out);
                let binary = bytes.contains(&0);
                let expect = if binary { 1 } else { 2 };
                assert_eq!(out.len(), expect, "{want}");
                assert_eq!(&*out[out.len() - 1].bytes, want.as_bytes(), "{want}");
                if binary {
                    assert_eq!(out[0].via, "bybytes");
                } else {
                    // the file's own bytes first, then the rendering
                    assert_eq!(out[0].via, "raw");
                    assert_eq!(&*out[0].bytes, bytes);
                    assert_eq!(out[1].via, "bybytes");
                }
            });
        }
    }

    /// The rendering is still reachable, on request, for both files.
    #[test]
    fn the_rendering_is_still_asked_for_on_request() {
        for bytes in [
            b"plain text
"
            .as_slice(),
            b"bin\0ary".as_slice(),
        ] {
            under_policy(Policy::Render, || {
                let reg = Registry::new(vec![Box::new(ByBytes)]);
                let mut out = Vec::new();
                process(&reg, "a.bb", bytes, 0, &Ctx::default(), &mut out);
                assert_eq!(out.len(), 1);
                assert_eq!(out[0].via, "bybytes");
            });
        }
    }

    /// A `Render` adapter that does NOT ask for the bytes keeps the cheap
    /// answer, so `.html` and `.md` stay off the read path entirely.
    #[test]
    fn an_adapter_that_does_not_ask_is_still_answered_without_the_bytes() {
        struct Plain;
        impl Adapter for Plain {
            fn name(&self) -> &'static str {
                "plain"
            }
            fn extensions(&self) -> &'static [&'static str] {
                &["pl"]
            }
            fn reach(&self) -> Reach {
                Reach::Render
            }
            fn extract(&self, _d: &[u8], _p: &str) -> Result<Outcome> {
                Ok(Outcome::Text(
                    b"RENDERED
"
                    .to_vec(),
                ))
            }
        }
        under_policy(Policy::Source, || {
            let reg = Registry::new(vec![Box::new(Plain)]);
            assert!(!reg.may_claim("pl"), "a Render file is not read at all");
        });
    }

    /// A `Render` adapter ahead of a `Reveal` one for the same extension
    /// must not answer for the wrong one: the `Reveal` adapter behind it is
    /// the one that reads the file.
    #[test]
    fn may_claim_looks_past_a_render_adapter_at_a_reveal_one() {
        under_policy(Policy::Source, || {
            let reg = Registry::new(vec![Box::new(ByBytes), Box::new(Upper)]);
            assert!(
                reg.may_claim("up"),
                "the Reveal adapter behind it was missed"
            );
        });
    }

    struct Bailer;
    impl Adapter for Bailer {
        fn name(&self) -> &'static str {
            "bailer"
        }
        fn extensions(&self) -> &'static [&'static str] {
            &["up"]
        }
        fn extract(&self, _data: &[u8], _path: &str) -> Result<Outcome> {
            Ok(Outcome::Bail("not mine".into()))
        }
    }

    #[test]
    fn bail_falls_through_to_next_adapter() {
        let reg = Registry::new(vec![Box::new(Bailer), Box::new(Upper)]);
        let mut out = Vec::new();
        process(&reg, "a.up", b"hi", 0, &Ctx::default(), &mut out);
        assert_eq!(out[0].bytes, b"HI");
    }

    #[test]
    fn unclaimed_file_is_raw_passthrough() {
        let reg = Registry::new(vec![Box::new(Upper)]);
        let mut out = Vec::new();
        process(&reg, "a.txt", b"hello", 0, &Ctx::default(), &mut out);
        assert_eq!(out[0].via, "raw");
        assert_eq!(out[0].bytes, b"hello");
    }

    /// An adapter whose tool is missing: it claims the extension but
    /// `available()` is false, so `candidates` filters it out and the file
    /// goes raw. That is the right answer, but silence is not -- a user who
    /// named the file type gets a note saying which adapter was skipped, the
    /// same contract the adapter-failure note honours from the other side.
    struct Unavailable;
    impl Adapter for Unavailable {
        fn name(&self) -> &'static str {
            "unavailable"
        }
        fn extensions(&self) -> &'static [&'static str] {
            &["gone"]
        }
        fn available(&self) -> bool {
            false
        }
        fn extract(&self, _data: &[u8], _path: &str) -> Result<Outcome> {
            unreachable!("a filtered-out adapter is never asked to extract")
        }
    }

    #[test]
    fn a_claimed_extension_with_no_available_adapter_says_so() {
        let reg = Registry::new(vec![Box::new(Unavailable), Box::new(Upper)]);
        assert!(
            !reg.may_claim("gone"),
            "an unavailable claimant must not make may_claim true"
        );
        assert_eq!(
            reg.unavailable_claimants("gone"),
            alloc::vec!["unavailable"],
            "the name of the skipped adapter must be recoverable"
        );
        // The unclaimed extension stays silent: nothing claimed it.
        assert!(reg.unavailable_claimants("txt").is_empty());

        let mut out = Vec::new();
        process(&reg, "a.gone", b"payload", 0, &Ctx::default(), &mut out);
        assert_eq!(out.len(), 2, "note plus the file's own bytes, got {out:?}");
        assert!(
            out[0]
                .bytes
                .starts_with(b"[rgx: adapter 'unavailable' skipped:"),
            "the note must name the skipped adapter, got {:?}",
            String::from_utf8_lossy(&out[0].bytes)
        );
        assert_eq!(out[1].bytes, b"payload", "the bytes must still be searched");
    }

    /// A `Render` adapter: it reads a file ripgrep can already read, and its
    /// output drops the half of it that is markup. The two bytes below are
    /// the whole disagreement — `visible` survives the render, `hidden` does
    /// not, and the file is plain text either way.
    struct Renderer;
    impl Adapter for Renderer {
        fn name(&self) -> &'static str {
            "renderer"
        }
        fn extensions(&self) -> &'static [&'static str] {
            &["ren"]
        }
        fn reach(&self) -> Reach {
            Reach::Render
        }
        fn extract(&self, data: &[u8], _path: &str) -> Result<Outcome> {
            let src = String::from_utf8_lossy(data);
            let kept: String = src
                .lines()
                .filter(|l| !l.contains("hidden"))
                .collect::<Vec<_>>()
                .join("\n");
            Ok(Outcome::Text(kept.into_bytes()))
        }
    }

    /// The policy is process-wide, so the tests that move it take this lock
    /// and put it back, or a test running beside them would see a registry
    /// resolve under the wrong rule.
    static POLICY_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn under_policy<R>(p: Policy, f: impl FnOnce() -> R) -> R {
        struct Reset(Policy);
        impl Drop for Reset {
            fn drop(&mut self) {
                set_policy(self.0);
            }
        }
        let _guard = POLICY_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let was = policy();
        set_policy(p);
        let _reset = Reset(was);
        f()
    }

    /// The defect this whole policy exists for. `Source` is the default, and
    /// under it a rendering adapter is not a candidate at all: the file keeps
    /// its own bytes, which is what makes rgx a superset of ripgrep here
    /// rather than a strict subset of it.
    #[test]
    fn a_render_adapter_does_not_get_a_file_ripgrep_already_reads() {
        let reg = Registry::new(vec![Box::new(Renderer)]);
        under_policy(Policy::Source, || {
            assert!(!reg.may_claim("ren"), "the walk must take the raw path");
            let mut out = Vec::new();
            assert!(process(
                &reg,
                "a.ren",
                b"visible\nhidden\n",
                0,
                &Ctx::default(),
                &mut out
            ));
            assert_eq!(out.len(), 1);
            assert_eq!(out[0].via, "raw");
            assert_eq!(out[0].bytes, b"visible\nhidden\n");
        });
    }

    /// `Render` is the behaviour every rgx before this had, and it is what
    /// `RGX_ADAPTER_VIEW=render` restores.
    #[test]
    fn the_render_policy_keeps_the_adapter_view() {
        let reg = Registry::new(vec![Box::new(Renderer)]);
        under_policy(Policy::Render, || {
            assert!(reg.may_claim("ren"));
            let mut out = Vec::new();
            assert!(!process(
                &reg,
                "a.ren",
                b"visible\nhidden\n",
                0,
                &Ctx::default(),
                &mut out
            ));
            assert_eq!(out.len(), 1);
            assert_eq!(out[0].via, "renderer");
            assert_eq!(out[0].bytes, b"visible");
        });
    }

    /// `Both` is the union, and its cost is the point of the measurement: the
    /// same line arrives twice, once from each view, under two line numbers.
    #[test]
    fn the_both_policy_reports_the_source_and_the_rendering() {
        let reg = Registry::new(vec![Box::new(Renderer)]);
        under_policy(Policy::Both, || {
            let mut out = Vec::new();
            process(
                &reg,
                "a.ren",
                b"visible\nhidden\n",
                0,
                &Ctx::default(),
                &mut out,
            );
            assert_eq!(out.len(), 2);
            assert_eq!(out[0].via, "raw");
            assert_eq!(out[0].bytes, b"visible\nhidden\n");
            assert_eq!(out[1].via, "renderer");
            assert_eq!(out[1].bytes, b"visible");
        });
    }

    /// The policy only ever takes files *away* from adapters that were already
    /// rendering; a `Reveal` adapter — every archive, every binary container —
    /// is untouched in all three policies. This is the test that keeps the
    /// `.zst` result (rgx reading compressed content that rg cannot) from
    /// being "fixed" into parity with rg by turning the raw path loose.
    #[test]
    fn a_reveal_adapter_keeps_the_file_in_every_policy() {
        for p in [Policy::Source, Policy::Render, Policy::Both] {
            under_policy(p, || {
                let reg = Registry::new(vec![Box::new(Upper)]);
                assert!(reg.may_claim("up"), "{p:?} took the file off the adapter");
                let mut out = Vec::new();
                process(&reg, "a.up", b"hi", 0, &Ctx::default(), &mut out);
                assert_eq!(out.len(), 1, "{p:?}");
                assert_eq!(out[0].via, "upper", "{p:?}");
            });
        }
    }

    /// An adapter that found nothing, which is what every `bail!` is.
    struct Nothing;
    impl Adapter for Nothing {
        fn name(&self) -> &'static str {
            "nothing"
        }
        fn extensions(&self) -> &'static [&'static str] {
            &["nul"]
        }
        fn extract(&self, _data: &[u8], _path: &str) -> Result<Outcome> {
            bail!("no readable text")
        }
    }

    /// A file no adapter can read is still a file ripgrep reads. The `Err`
    /// arm used to push its note and stop, and because every extractor
    /// declines with `bail!`, that is the arm nearly every "nothing here"
    /// took: rgx printed nothing at all for a file the walk had found, which
    /// is a strict subset of ripgrep reached through a different door than
    /// the rendering adapters. The note is still emitted — it is the record
    /// of what happened — and the file's own bytes go in beside it.
    #[test]
    fn a_failed_adapter_leaves_the_file_searchable() {
        let reg = Registry::new(vec![Box::new(Nothing)]);
        let mut out = Vec::new();
        // an adapter did produce a record, so this is not a raw passthrough
        assert!(!process(
            &reg,
            "a.nul",
            b"ripgrep reads all of this\n",
            0,
            &Ctx::default(),
            &mut out
        ));
        assert_eq!(out.len(), 2, "{out:?}");
        assert_eq!(
            out[0].via, "nothing",
            "the note records the failure: {out:?}"
        );
        assert!(
            out[0]
                .bytes
                .starts_with(b"[rgx: adapter 'nothing' failed: "),
            "{out:?}"
        );
        assert_eq!(out[1].via, "raw");
        assert_eq!(
            out[1].bytes, b"ripgrep reads all of this\n",
            "the file, byte for byte"
        );
        assert_eq!(out[0].path, out[1].path, "one file, two views: {out:?}");
    }

    /// `Bail` is the clean decline: the chain continues, and a file no
    /// adapter wants reaches the raw searcher on its own, with no note at
    /// all. `Err` differs in two ways — it stops the chain, and it leaves a
    /// record — which is why the record is not the file.
    #[test]
    fn a_bailed_adapter_falls_through_to_the_file() {
        let reg = Registry::new(vec![Box::new(Bailer)]);
        let mut out = Vec::new();
        assert!(process(&reg, "a.up", b"hi", 0, &Ctx::default(), &mut out));
        assert_eq!(out.len(), 1, "{out:?}");
        assert_eq!(out[0].via, "raw");
        assert_eq!(out[0].bytes, b"hi");
    }

    #[test]
    fn utf16_bom_is_transcoded() {
        let mut utf16le = vec![0xFF, 0xFE];
        utf16le.extend_from_slice(
            &"ZIRCALOY"
                .encode_utf16()
                .flat_map(|u| u.to_le_bytes())
                .collect::<Vec<u8>>(),
        );
        let mut out = Vec::new();
        let reg = Registry::new(vec![]);
        process(&reg, "a.txt", &utf16le, 0, &Ctx::default(), &mut out);
        assert_eq!(out[0].bytes, b"ZIRCALOY");

        let mut utf16be = vec![0xFE, 0xFF];
        utf16be.extend_from_slice(
            &"rod"
                .encode_utf16()
                .flat_map(|u| u.to_be_bytes())
                .collect::<Vec<u8>>(),
        );
        let mut out = Vec::new();
        process(&reg, "b.txt", &utf16be, 0, &Ctx::default(), &mut out);
        assert_eq!(out[0].bytes, b"rod");
    }
}

#[cfg(all(test, feature = "std"))]
mod no_std_float_tests {
    use super::*;

    /// The `no_std` float parser against the standard library's, over what
    /// RFC 8259 allows plus the shapes that must be refused.
    #[test]
    fn parse_decimal_agrees_with_std() {
        let ok = [
            "0",
            "-0",
            "1",
            "-1",
            "3.14159",
            "-3.14159",
            "0.5",
            "1e3",
            "1E3",
            "1e+3",
            "1e-3",
            "-2.5e10",
            "0.0",
            "123456789.123456789",
            "0.1",
            "0.2",
            "0.3",
            "1.5e-7",
            "9.999999999999999e22",
        ];
        for s in ok {
            let want: f64 = s.parse().expect("std should accept");
            let got = parse_decimal(s).unwrap_or_else(|_| panic!("rejected {s}"));
            assert_eq!(
                got.to_bits(),
                want.to_bits(),
                "{s}: no_std {got} vs std {want}"
            );
        }

        // The extremes are exact now, not merely close. They were the whole
        // reason this parser is written over a bignum: scaling by repeated
        // multiplication gave 1e308 a ULP of error and turned
        // 2.2250738585072014e-308 into zero.
        for s in [
            "1e308",
            "1e-308",
            "2.2250738585072014e-308",
            "1e300",
            "5e-324",
            "4.9e-324",
            "1e-324",
            "2.5e-324",
            "1.7976931348623157e308",
            "1e23",
            "1e-23",
            "9.999999999999999e22",
        ] {
            let want: f64 = s.parse().expect("std should accept");
            let got = parse_decimal(s).unwrap_or_else(|_| panic!("rejected {s}"));
            assert_eq!(
                got.to_bits(),
                want.to_bits(),
                "{s}: no_std {got:?} vs std {want:?}"
            );
        }

        // Refusals. RFC 8259 allows no leading `+`, no bare `.5` or `5.`, no
        // leading zero on a multi-digit integer part, and no infinities or
        // NaN. Rust's own parser is MORE permissive than that -- it accepts
        // `.5`, `+1`, `inf` and `NaN` -- so this is a case where the two
        // deliberately disagree, and `std` is not the oracle.
        for s in [
            "", ".", "-", "+1", ".5", "5.", "1e", "1e+", "01", "-01", "00", "1.2.3", "abc", "1 2",
            "--1", "1e1e1", "0x10", "inf", "NaN", "infinity", "1,5",
        ] {
            assert!(
                parse_decimal(s).is_err(),
                "{s:?}: a JSON number parser must refuse this"
            );
        }
    }
}

#[cfg(test)]
mod extension_of_tests {
    use super::extension_of;
    use alloc::borrow::Cow;

    #[test]
    fn lowercases_uppercase_extension() {
        assert_eq!(extension_of("foo.RS").as_ref(), "rs");
        assert_eq!(extension_of("a/b/c.Py").as_ref(), "py");
    }

    #[test]
    fn handles_backslash_windows_paths() {
        assert_eq!(extension_of(r"a\b\c.rs").as_ref(), "rs");
    }

    #[test]
    fn returns_empty_for_no_extension() {
        assert_eq!(extension_of("Makefile").as_ref(), "");
        assert_eq!(extension_of("a/b/c").as_ref(), "");
    }

    #[test]
    fn empty_path_returns_empty() {
        assert_eq!(extension_of("").as_ref(), "");
    }

    #[test]
    fn ascii_lowercase_path_returns_borrowed() {
        let path = "a/b/c.rs";
        let ext = extension_of(path);
        assert!(matches!(ext, Cow::Borrowed(_)));
        assert_eq!(ext.as_ref(), "rs");
    }
}
