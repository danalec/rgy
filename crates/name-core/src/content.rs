// SPDX-License-Identifier: MIT
//
// Transported from fsearch (https://github.com/noahdunnagan/fsearch), MIT
// licensed, Copyright (c) 2026 Noah Dunnagan. Its notice is in
// LICENSE-MIT-FSEARCH and in the repository's NOTICE.
//! Content search: a trigram index over the user's text files.
//!
//! Segments are immutable, mmap'd files: a doc table, and the contents first
//! indexed there: per trigram, the ids of the contents holding it (delta
//! varints), and per content bloom filters of its 5- and 7-byte substrings. Each
//! distinct file content is indexed once (most copies are git worktrees):
//! every doc names its content by a gid, and a copy indexed later, even in
//! another segment, just names the same gid. A query becomes an AND/OR of
//! trigrams, the posting lists pick candidate contents, those become the docs
//! holding them, the bloom filters drop most of those that only contain the
//! pattern's trigrams scattered about, and the rest are read fresh from disk
//! and matched for real. Results therefore never show stale content; only
//! candidate selection can trail a file written in the last couple of seconds.
//!
//! The index is kept in sync by diffing, exactly like the name index: for a
//! directory (or subtree), compare the eligible files the live name index
//! knows about with the docs we hold, reindex what changed, tombstone what
//! went away. The first build is just a sync of $HOME.

use crate::live::{Live, join};
use crate::name_index::{Blob, HDR, as_bytes, fields, header, layout, sec};
use crate::name_query::{GrepMode, Query, fold};
use crate::placeholder;
use alloc::boxed::Box;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;
use rgy_discovery::walk::KIND_FILE;
// Per-file confirmation runs on the rgx engine: literal/hybrid/fixed/VM +
// SIMD prefilter replace memchr/regex verify. The plan that narrows the
// content index is walked over the same AST the confirm is compiled from,
// so the two cannot disagree about what the pattern means. The engine is
// the only engine: a pattern its parser refuses is a pattern this search
// does not run, and the refusal is said to the caller.
use rgx_core::byteclass::ByteSet;
use rgx_core::regex::ByteRegex;
use rgx_core::syntax::Ast;

/// The `regex` crate's `escape`, which this module no longer links:
/// escape every regex metacharacter in a literal.
fn re_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 8);
    for c in s.chars() {
        if matches!(c, '\\' | '.' | '+' | '*' | '?' | '(' | ')' | '[' | ']' | '{' | '}' | '^' | '$' | '|' | '#') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// Every match start in hay, leftmost-first — the rgx engine's confirm.
fn rgx_starts(re: &ByteRegex, hay: &[u8]) -> Vec<usize> {
    let mut out = Vec::new();
    let mut at = 0;
    while let Some(m) = re.search(hay, at) {
        out.push(m.start);
        at = if m.end > m.start { m.end } else { m.start + 1 };
    }
    out
}
use crate::hmap::Map;
use std::io::Write;
use std::path::{Path, PathBuf};

pub const MAX_FILE: u64 = 1 << 20;
/// File bytes per segment build; bounds the build's transient memory.
const SEG_BYTES: u64 = 64 << 20;
/// Largest merge, in posting bytes; bounds the merge's transient memory.
const MERGE_CAP: usize = 96 << 20;
const MAGIC: &[u8; 8] = b"FSCSEG10";
/// tri_off high bit: this trigram's list is a bitset over the segment's
/// contents (cheaper than varints once more than 1 in 8 contain it).
const BITSET: u32 = 1 << 31;
/// tri_off next bit: this varint list starts with a skip table, one entry
/// per SKIP of its contents after the first SKIP: (the content before them,
/// where they start in the varints), so a search can jump ahead.
const SKIPS: u32 = 1 << 30;
const FLAGS: u32 = BITSET | SKIPS;
const SKIP: usize = 64;
/// The gid of a doc that is not text, or a doc no content holds.
const NONE: u32 = u32::MAX;

/// Directory names whose subtrees are generated, vendored, or caches.
#[rustfmt::skip]
const SKIP_DIRS: &[&[u8]] = &[
    b"node_modules", b".git", b"target", b"DerivedData", b"__pycache__", b".venv", b"venv", b"site-packages", b"Pods",
    b".next", b".turbo", b".cache", b"Library", b"dist", b"build", b".build", b".rustup", b".cargo", b".npm", b".bun",
    b".nvm", b"vendor", b".pnpm-store", b"coverage", b".Trash", b".svn", b".hg", b".gradle", b".m2", b".pyenv",
    b".rbenv", b".gem", b".conda", b"miniconda3", b"anaconda3", b".docker", b".orbstack", b".colima", b".lima",
    b".ollama", b".android", b".expo", b".terraform.d", b".wrangler", b".vscode-server", b"cache", b"Cache", b"caches",
    b"Caches",
];

/// Home-relative trees that are dependencies or app data, not your files.
#[rustfmt::skip]
const SKIP_UNDER_HOME: &[&[u8]] = &[
    b"go/pkg", b".cursor/extensions", b".vscode/extensions", b".local/share", b".local/state", b".config/gcloud", b".codex/.tmp",
];

/// Package/library bundles: their insides are app data.
#[rustfmt::skip]
const SKIP_SUFFIXES: &[&[u8]] = &[
    b".app", b".photoslibrary", b".library", b".lrlibrary", b".musiclibrary", b".tvlibrary", b".imovielibrary",
    b".xcassets", b".framework", b".bundle", b".xcarchive", b".xcresult", b".dSYM", b".salon", b".lrdata",
];

#[rustfmt::skip]
const TEXT_EXTS: &[&[u8]] = &[
    b"rs", b"c", b"h", b"cc", b"cpp", b"cxx", b"hpp", b"hh", b"m", b"mm", b"swift", b"go", b"py", b"pyi", b"js", b"mjs", b"cjs",
    b"ts", b"mts", b"cts", b"tsx", b"jsx", b"java", b"kt", b"kts", b"scala", b"rb", b"php", b"cs", b"fs", b"sh", b"zsh", b"bash",
    b"fish", b"lua", b"sql", b"html", b"htm", b"css", b"scss", b"sass", b"less", b"json", b"jsonc", b"json5", b"yaml", b"yml",
    b"toml", b"xml", b"vue", b"svelte", b"astro", b"zig", b"nim", b"hs", b"ml", b"mli", b"ex", b"exs", b"erl", b"clj", b"dart",
    b"r", b"jl", b"md", b"mdx", b"markdown", b"txt", b"text", b"rst", b"org", b"tex", b"csv", b"tsv", b"ini", b"cfg", b"conf",
    b"env", b"properties", b"plist", b"metal", b"glsl", b"wgsl", b"hlsl", b"proto", b"graphql", b"gql", b"nix", b"tf", b"hcl",
    b"gradle", b"cmake", b"mk", b"make", b"dockerfile", b"log", b"jsonl", b"ndjson", b"diff", b"patch", b"srt", b"vtt", b"rtf",
    b"svg", b"pl", b"pm", b"ps1", b"bat", b"vim", b"el", b"lisp", b"scm", b"rkt", b"elm", b"purs", b"sol", b"v", b"sv", b"vhd",
    b"asm", b"s", b"d", b"cr", b"pas", b"f90", b"cmd", b"service", b"desktop", b"gitignore", b"editorconfig", b"lock", b"sum",
];

/// A segment's own contents, the live docs holding them, and the runs of
/// consecutive gids it holds them in.
///
/// Cloning is cheap: the postings live in the mmap, which is shared.
#[derive(Clone)]
pub struct Segment {
    map: std::sync::Arc<Blob>,
    pub id: u64,
    pub ndocs: usize,
    ndocs1: usize,
    ntri: usize,
    ntri1: usize,
    plen: usize,
    paths_len: usize,
    nwords: usize,
    nlong: usize,
    /// Contents first indexed here (its own).
    nown: usize,
    nown1: usize,
    nown2: usize,
    off: [usize; NS],
    dead: Vec<u64>,
    pub live_docs: usize,
    /// Per own content, the live docs (in any segment) holding it; kept by
    /// `Content` (see `link`).
    refs: Counts,
    /// Own contents as runs of consecutive gids: (first content, first gid, length).
    runs: Vec<(u32, u32, u32)>,
}

#[derive(Clone, Copy)]
enum S {
    TriKey,
    TriOff,
    Post,
    PathOff,
    Paths,
    Size,
    Mtime,
    ByPath,
    Rank,
    ByRank,
    DocGid,
    RankGid,
    GidSorted,
    ByGid,
    OwnGid,
    OwnDoc,
    Hash,
    ByHash,
    BloomOff,
    Bloom,
    LongOff,
    Long,
}
const NS: usize = 22;

#[rustfmt::skip]
fn lens(ndocs: usize, ntri: usize, plen: usize, paths_len: usize, nwords: usize, nown: usize, nlong: usize) -> [usize; NS] {
    let (d, c) = (ndocs * 4, nown * 4);
    [ntri * 4, (ntri + 1) * 4, plen, d + 4, paths_len, ndocs * 8, d, d, ndocs, d, d, d, d, d, c, c, c * 4, c, c + 4, nwords * 8, c + 4, nlong * 8]
}

impl Segment {
    sec!(tri_key, S::TriKey, u32, ntri);
    sec!(tri_off, S::TriOff, u32, ntri1);
    sec!(post, S::Post, u8, plen);
    sec!(path_off, S::PathOff, u32, ndocs1);
    sec!(paths, S::Paths, u8, paths_len);
    sec!(size, S::Size, u64, ndocs);
    sec!(mtime, S::Mtime, u32, ndocs);
    sec!(by_path, S::ByPath, u32, ndocs);
    sec!(rank, S::Rank, i8, ndocs);
    // Doc ids, best-ranked first.
    sec!(by_rank, S::ByRank, u32, ndocs);
    // Per doc, its content's gid (NONE: not text); the same in by_rank order.
    sec!(doc_gid, S::DocGid, u32, ndocs);
    sec!(rank_gid, S::RankGid, u32, ndocs);
    // The docs by gid: the gids (ascending), and the doc ids.
    sec!(gid_sorted, S::GidSorted, u32, ndocs);
    sec!(by_gid, S::ByGid, u32, ndocs);
    // Per own content: its gid, the first doc here holding it (or NONE), a
    // hash of its bytes (two u64s), and bloom filters of its SHORT- and
    // LONG-byte substrings. And the contents in hash order.
    sec!(own_gid, S::OwnGid, u32, nown);
    sec!(own_doc, S::OwnDoc, u32, nown);
    sec!(hash, S::Hash, u64, nown2);
    sec!(by_hash, S::ByHash, u32, nown);
    sec!(bloom_off, S::BloomOff, u32, nown1);
    sec!(bloom, S::Bloom, u64, nwords);
    sec!(long_off, S::LongOff, u32, nown1);
    sec!(long, S::Long, u64, nlong);

    pub fn path(&self, d: u32) -> &[u8] {
        let o = self.path_off();
        &self.paths()[o[d as usize] as usize..o[d as usize + 1] as usize]
    }

    fn bloom_of(&self, c: u32) -> &[u64] {
        let o = self.bloom_off();
        &self.bloom()[o[c as usize] as usize..o[c as usize + 1] as usize]
    }

    fn long_of(&self, c: u32) -> &[u64] {
        let o = self.long_off();
        &self.long()[o[c as usize] as usize..o[c as usize + 1] as usize]
    }

    fn hash_of(&self, c: u32) -> [u64; 2] {
        [self.hash()[2 * c as usize], self.hash()[2 * c as usize + 1]]
    }

    /// Can own content `c` hold every gram in `probes`, by its bloom filters?
    fn may_contain(&self, c: u32, probes: &Probes) -> bool {
        let has = |words: &[u64], h: u32| bloom_bits(h, words.len()).iter().all(|&b| words.get(b / 64).is_some_and(|w| w >> (b % 64) & 1 != 0));
        let (short, long) = (self.bloom_of(c), self.long_of(c));
        probes.short.iter().all(|&h| has(short, h)) && probes.long.iter().all(|&h| has(long, h))
    }

    /// Own contents a live doc still holds.
    fn live_own(&self) -> usize {
        self.refs.live()
    }

    #[inline]
    pub fn is_dead(&self, d: u32) -> bool {
        self.dead[d as usize >> 6] & (1 << (d & 63)) != 0
    }

    fn kill(&mut self, d: u32) -> bool {
        let w = &mut self.dead[d as usize >> 6];
        if *w & (1 << (d & 63)) != 0 {
            return false;
        }
        *w |= 1 << (d & 63);
        self.live_docs -= 1;
        true
    }

    /// Live docs whose path starts with `prefix`, via the sorted permutation.
    fn with_prefix<'a>(&'a self, prefix: &'a [u8]) -> impl Iterator<Item = u32> + 'a {
        let bp = self.by_path();
        let start = bp.partition_point(|&d| self.path(d) < prefix);
        bp[start..].iter().copied().take_while(move |&d| self.path(d).starts_with(prefix)).filter(move |&d| !self.is_dead(d))
    }

    /// How many docs (live or not) have paths starting with `prefix`.
    fn count_prefix(&self, prefix: &[u8]) -> usize {
        let bp = self.by_path();
        let start = bp.partition_point(|&d| self.path(d) < prefix);
        bp[start..].partition_point(|&d| self.path(d).starts_with(prefix))
    }

    /// Live docs directly inside `prefix` (which ends in '/'), skipping each
    /// subdirectory's run of paths with one binary search.
    fn direct_children(&self, prefix: &[u8]) -> Vec<u32> {
        let bp = self.by_path();
        let mut out = Vec::new();
        let mut i = bp.partition_point(|&d| self.path(d) < prefix);
        while i < bp.len() {
            let p = self.path(bp[i]);
            if !p.starts_with(prefix) {
                break;
            }
            match p[prefix.len()..].iter().position(|&b| b == b'/') {
                None => {
                    if !self.is_dead(bp[i]) {
                        out.push(bp[i]);
                    }
                    i += 1;
                }
                Some(k) => {
                    // Jump past "prefix/sub/..." : first path >= "prefix/sub0".
                    let mut hi = p[..prefix.len() + k + 1].to_vec();
                    *hi.last_mut().unwrap() = b'/' + 1;
                    i += bp[i..].partition_point(|&d| self.path(d) < hi.as_slice());
                }
            }
        }
        out
    }

    fn list(&self, tri: u32) -> Option<List<'_>> {
        self.tri_key().binary_search(&tri).ok().map(|k| self.list_at(k))
    }

    /// The posting list of trigram `k`, or an empty one if the segment's
    /// bytes do not hold it.
    ///
    /// The offsets are read from the file, and nothing checks them against
    /// the file's length: a truncated or corrupt segment would slice past
    /// the end and panic. A corrupt index is "no index", not a crash, so
    /// the bounds are checked here and a list that does not fit reads as
    /// empty -- which rules the trigram out rather than the search down.
    fn list_at(&self, k: usize) -> List<'_> {
        let o = self.tri_off();
        let (Some(&a), Some(&b)) = (o.get(k), o.get(k + 1)) else { return List::Bits(&[]) };
        let (a, b) = ((a & !FLAGS) as usize, (b & !FLAGS) as usize);
        let post = self.post();
        if a > b || b > post.len() {
            return List::Bits(&[]);
        }
        let bytes = &post[a..b];
        if o[k] & BITSET != 0 { List::Bits(bytes) } else { List::Var(Var::new(bytes, o[k] & SKIPS != 0)) }
    }

    /// The live docs under `prefix` lie in this doc id range (exactly, for a
    /// segment built from sorted paths; a superset after a merge, or when
    /// most of the segment is under it). True if every doc is under it.
    fn doc_range(&self, prefix: &[u8]) -> (std::ops::Range<u32>, bool) {
        let bp = self.by_path();
        let (Some(&first), Some(&last)) = (bp.first(), bp.last()) else { return (0..0, false) };
        let (first, last) = (self.path(first), self.path(last));
        if last < prefix || (first > prefix && !first.starts_with(prefix)) {
            return (0..0, false);
        }
        // Paths under the prefix are one run in path order.
        if first.starts_with(prefix) && last.starts_with(prefix) {
            return (0..self.ndocs as u32, true);
        }
        let a = bp.partition_point(|&d| self.path(d) < prefix);
        let z = a + bp[a..].partition_point(|&d| self.path(d).starts_with(prefix));
        if (z - a) * 2 > bp.len() {
            return (0..self.ndocs as u32, false);
        }
        let (lo, hi) = bp[a..z].iter().fold((u32::MAX, 0), |(lo, hi), &d| (lo.min(d), hi.max(d + 1)));
        (lo.min(hi)..hi, false)
    }

    fn list_into(&self, k: usize, out: &mut Vec<u32>) {
        match self.list_at(k) {
            List::Bits(bytes) => {
                for (w, &b) in bytes.iter().enumerate() {
                    let mut b = b;
                    while b != 0 {
                        out.push(w as u32 * 8 + b.trailing_zeros());
                        b &= b - 1;
                    }
                }
            }
            List::Var(v) => out.extend(v.iter()),
        }
    }

    fn load(dir: &Path, id: u64) -> Option<Segment> {
        let f = std::fs::File::open(seg_path(dir, id)).ok()?;
        // Blob::from_file: mmap first, owned bytes when mapping fails
        // (rgx lesson — see name_index::Blob). Segments are scanned per
        // query, and on Windows a mapping measured 1.51x slower than a
        // read for scans; the owned fallback at least keeps a map failure
        // from meaning "no index".
        let map = std::sync::Arc::new(Blob::from_file(&f).ok()?);
        let [ndocs, ntri, plen, paths_len, nwords, nown, nlong] = fields(&map, MAGIC)?.map(|v| v as usize);
        let (off, total) = layout(&lens(ndocs, ntri, plen, paths_len, nwords, nown, nlong));
        if map.len() < total {
            return None;
        }
        let mut dead = vec![0u64; ndocs.div_ceil(64)];
        if let Ok(b) = std::fs::read(dead_path(dir, id)) {
            let (chunks, _) = b.as_chunks::<8>();
            for (w, c) in dead.iter_mut().zip(chunks) {
                *w = u64::from_le_bytes(*c);
            }
        }
        // The dead file is a separate file from the segment, and nothing
        // ties the two together: a stale or foreign one can have more bits
        // set than the segment has documents. Counting them all would
        // underflow, so the bits past the last document are dropped --
        // they name documents that do not exist.
        if ndocs % 64 != 0 {
            let last = dead.len() - 1;
            let keep = ndocs % 64;
            dead[last] &= (1u64 << keep) - 1;
        }
        let live_docs = ndocs - dead.iter().map(|w| w.count_ones() as usize).sum::<usize>();
        #[rustfmt::skip]
        let mut s = Segment {
            map, id, ndocs, ndocs1: ndocs + 1, ntri, ntri1: ntri + 1, plen, paths_len, nwords, nlong, nown, nown1: nown + 1, nown2: nown * 2,
            off, dead, live_docs, refs: Counts::new(nown), runs: Vec::new(),
        };
        let mut runs: Vec<(u32, u32, u32)> = Vec::new();
        for (c, &g) in s.own_gid().iter().enumerate() {
            match runs.last_mut() {
                Some((_, g0, n)) if *g0 + *n == g => *n += 1,
                _ => runs.push((c as u32, g, 1)),
            }
        }
        s.runs = runs;
        Some(s)
    }

    fn save_dead(&self, dir: &Path) {
        let bytes: Vec<u8> = self.dead.iter().flat_map(|w| w.to_le_bytes()).collect();
        let tmp = dead_path(dir, self.id).with_extension("tmp");
        if std::fs::write(&tmp, bytes).is_ok() {
            let _ = std::fs::rename(tmp, dead_path(dir, self.id));
        }
    }
}

fn seg_path(dir: &Path, id: u64) -> PathBuf {
    dir.join(format!("seg-{id:06}.fsc"))
}
fn dead_path(dir: &Path, id: u64) -> PathBuf {
    dir.join(format!("seg-{id:06}.dead"))
}

#[inline]
fn varint(b: &[u8]) -> (u32, usize) {
    let mut v = 0u32;
    for (i, &x) in b.iter().enumerate().take(5) {
        v |= ((x & 0x7f) as u32) << (7 * i);
        if x & 0x80 == 0 {
            return (v, i + 1);
        }
    }
    (v, b.len().min(5))
}

fn put_varint(out: &mut Vec<u8>, mut v: u32) {
    while v >= 0x80 {
        out.push(v as u8 | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}

/// A 128-bit hash of a file's bytes: files with the same hash are taken to
/// hold the same bytes. Four multiply-fold lanes, 32 bytes a step.
fn content_hash(b: &[u8]) -> [u64; 2] {
    #[inline]
    fn mix(x: u64, y: u64) -> u64 {
        let m = x as u128 * y as u128;
        m as u64 ^ (m >> 64) as u64
    }
    const K: [u64; 4] = [0x9E37_79B9_7F4A_7C15, 0xC2B2_AE3D_27D4_EB4F, 0x1656_67B1_9E37_79F9, 0x85EB_CA77_C2B2_AE63];
    let word = |w: &[u8], i: usize| u64::from_le_bytes(w[i * 8..i * 8 + 8].try_into().unwrap());
    let mut s = [K[0] ^ b.len() as u64, K[1], K[2], K[3] ^ (b.len() as u64).rotate_left(32)];
    let mut step = |w: &[u8]| {
        let x = [word(w, 0), word(w, 1), word(w, 2), word(w, 3)];
        s = [mix(x[0] ^ K[0], x[1] ^ s[0]), mix(x[1] ^ K[1], x[0] ^ s[1]), mix(x[2] ^ K[2], x[3] ^ s[2]), mix(x[3] ^ K[3], x[2] ^ s[3])];
    };
    let (blocks, rem) = b.as_chunks::<32>();
    for w in blocks {
        step(w);
    }
    let mut last = [0u8; 32];
    last[..rem.len()].copy_from_slice(rem);
    step(&last);
    let h0 = mix(mix(s[0] ^ K[1], s[2] ^ K[2]) ^ s[1], s[3] ^ K[0]);
    let h1 = mix(mix(s[1] ^ K[3], s[3] ^ K[1]) ^ s[0], s[2] ^ K[2]);
    [h0, h1]
}

/// A file's distinct trigrams and its end-of-file key into `tri`, and the
/// distinct hashes of its SHORT- and LONG-byte grams into `short` and `long`
/// (all cleared, unordered). One pass, with no branch on whether a gram is
/// new. `seen` is three 2 MiB bitsets, all zero on entry and exit.
fn index_grams(buf: &[u8], seen: &mut [u64], tri: &mut Vec<u32>, short: &mut Vec<u32>, long: &mut Vec<u32>) {
    tri.clear();
    short.clear();
    long.clear();
    let n = buf.len();
    if n < 2 {
        return;
    }
    let (st, rest) = seen.split_at_mut(1 << 18);
    let (ss, sl) = rest.split_at_mut(1 << 18);
    // Its last two bytes and a 0: so a two-byte search finds them too.
    let eof = (fold(buf[n - 2]) as u32) << 16 | (fold(buf[n - 1]) as u32) << 8;
    st[(eof >> 6) as usize] |= 1 << (eof & 63);
    tri.resize(n, 0);
    tri[0] = eof;
    short.resize(n, 0);
    long.resize(n, 0);
    let (mut nt, mut ns, mut nl) = (1, 0, 0);
    // A gram's hash into `out` at `k`, which moves past it if it is whole
    // and new.
    let add = |seen: &mut [u64], out: &mut [u32], k: &mut usize, h: u32, whole: bool| {
        let (w, bit) = ((h >> 6) as usize, 1u64 << (h & 63));
        out[*k] = h;
        *k += (whole & (seen[w] & bit == 0)) as usize;
        seen[w] |= bit & 0u64.wrapping_sub(whole as u64);
    };
    let mut g = (fold(buf[0]) as u64) << 8 | fold(buf[1]) as u64;
    for (i, &c) in buf.iter().enumerate().skip(2) {
        g = g << 8 | fold(c) as u64;
        add(st, tri, &mut nt, (g & 0xFF_FFFF) as u32, true);
        add(ss, short, &mut ns, gram_hash(g & ((1 << (8 * SHORT)) - 1)), i + 1 >= SHORT);
        add(sl, long, &mut nl, gram_hash(g & ((1 << (8 * LONG)) - 1)), i + 1 >= LONG);
    }
    tri.truncate(nt);
    short.truncate(ns);
    long.truncate(nl);
    for (seen, out) in [(st, &*tri), (ss, &*short), (sl, &*long)] {
        for &h in out {
            seen[(h >> 6) as usize] = 0;
        }
    }
}

/// Trigrams of a short string (query side), no scratch needed.
fn trigrams_small(s: &[u8]) -> Vec<u32> {
    let mut t: Vec<u32> = s.windows(3).map(|w| (fold(w[0]) as u32) << 16 | (fold(w[1]) as u32) << 8 | fold(w[2]) as u32).collect();
    t.sort_unstable();
    t.dedup();
    t
}

/// Bloom filters hold a content's case-folded substrings of these lengths: a
/// match holds all of its pattern's grams, so a content missing one of them
/// can't match even if it has every trigram. The longer ones rule out a
/// content holding the pattern's shorter ones only apart ("different" and
/// "reference" for "difference").
const SHORT: usize = 5;
const LONG: usize = 7;

/// A gram's 24-bit hash (its folded bytes, big-endian in the low 40 bits).
#[inline]
fn gram_hash(g: u64) -> u32 {
    (g.wrapping_mul(0x9E37_79B9_7F4A_7C15) >> 40) as u32
}

/// The two bits a gram hash sets in a bloom filter of `words` u64s.
#[inline]
fn bloom_bits(h: u32, words: usize) -> [usize; 2] {
    let at = |h: u32| ((h as u64 * words as u64 * 64) >> 24) as usize;
    [at(h), at(h.wrapping_mul(0x9E37_79B1) & 0xFF_FFFF)]
}

/// Append a bloom filter of these gram hashes to `out`: `bits` per gram, two
/// set per gram, in whole words. A gram the doc lacks still passes 24% of
/// the time at three bits, 40% at two.
fn bloom(hashes: &[u32], bits: usize, out: &mut Vec<u64>) {
    let words = (hashes.len() * bits).div_ceil(64);
    let at = out.len();
    out.resize(at + words, 0);
    for &h in hashes {
        for b in bloom_bits(h, words) {
            out[at + b / 64] |= 1 << (b % 64);
        }
    }
}

/// Keywords a definition starts with (`sym:` search).
const DEFINES: &str = "fn|func|function|def|class|struct|enum|trait|interface|type|typealias|impl|let|const|var|val|static|module|mod|protocol|extension|macro_rules!|define|typedef|union|object|record|namespace|actor";

/// The posting key for "this doc defines `name`": a hash above the 24-bit
/// trigram space, so definitions live in the same key table as trigrams.
fn symbol_key(name: &[u8]) -> u32 {
    let h = name.iter().fold(0x811c_9dc5u32, |h, &b| (h ^ b as u32).wrapping_mul(0x0100_0193));
    h | 0x8000_0000
}

/// Is `name` something the definition index records (a plain identifier)?
fn plain_identifier(name: &[u8]) -> bool {
    name.first().is_some_and(|&b| b.is_ascii_alphabetic() || b == b'_') && name.iter().all(|&b| b.is_ascii_alphanumeric() || b == b'_')
}

/// Append the sorted distinct definition keys of a buffer to `out`: every
/// identifier right after a declaring keyword, as `sym:` matches it.
fn symbols(buf: &[u8], out: &mut Vec<u32>) {
    static RE: std::sync::OnceLock<ByteRegex> = std::sync::OnceLock::new();
    let re = RE.get_or_init(|| {
        ByteRegex::new_multiline(format!(r"(?-u:\b)(?:{DEFINES})(?:<[^>\n]*>)?[ \t*&]+(?:mut[ \t]+)?[A-Za-z_][A-Za-z0-9_]*").as_bytes(), false, false)
            .expect("definition regex")
    });
    let mut keys = Vec::new();
    let mut at = 0;
    while let Some(m) = re.search(buf, at) {
        let end = m.end;
        let begin = buf[..end].iter().rposition(|&b| !(b.is_ascii_alphanumeric() || b == b'_')).map_or(0, |p| p + 1);
        keys.push(symbol_key(&buf[begin..end]));
        // The name may itself be a keyword ("static func main"): look again
        // from it, not past it.
        at = begin;
    }
    keys.sort_unstable();
    keys.dedup();
    out.extend(keys);
}

/// Paths with size and mtime, all in one buffer: a full sync holds ~500k
/// of them, and one allocation (mmap-backed, returned on drop) beats 500k.
#[derive(Default)]
pub struct Docs {
    buf: Vec<u8>,
    items: Vec<(u32, u32, u64, u32)>,
    /// What builds of these docs share: the contents already indexed (set
    /// by `diff`; docs from elsewhere are for a new, empty index).
    dedup: std::sync::OnceLock<Dedup>,
}

/// What one sync's segment builds share so each distinct content is indexed
/// once: the segments held before it, the contents built since (by hash),
/// and the gids free to hand out.
#[derive(Default)]
struct Dedup {
    held: Vec<Held>,
    built: std::sync::Mutex<Map<[u64; 2], u32>>,
    free: std::sync::Mutex<Gids>,
}

/// A segment's own contents, findable by hash while a sync builds.
struct Held {
    map: std::sync::Arc<Blob>,
    nown: usize,
    off: [usize; NS],
}

impl Held {
    fn find(&self, h: [u64; 2]) -> Option<u32> {
        let at = |s: S, k: usize| unsafe { (self.map.as_ptr().add(self.off[s as usize]) as *const u32).add(k).read() };
        let hash = |c: u32| unsafe {
            let p = (self.map.as_ptr().add(self.off[S::Hash as usize]) as *const u64).add(2 * c as usize);
            [p.read(), p.add(1).read()]
        };
        let (mut lo, mut hi) = (0, self.nown);
        while lo < hi {
            let mid = (lo + hi) / 2;
            if hash(at(S::ByHash, mid)) < h { lo = mid + 1 } else { hi = mid }
        }
        (lo < self.nown && hash(at(S::ByHash, lo)) == h).then(|| at(S::OwnGid, at(S::ByHash, lo) as usize))
    }
}

impl Dedup {
    /// The gid of a content with this hash, if one is indexed.
    fn find(&self, h: [u64; 2]) -> Option<u32> {
        self.held.iter().find_map(|s| s.find(h)).or_else(|| self.built.lock().unwrap().get(&h).copied())
    }
}

/// Gids not in use: ascending gaps, then everything from `next`.
#[derive(Default)]
struct Gids {
    gaps: Vec<(u32, u32)>,
    next: u32,
}

impl Gids {
    /// `n` gids, lowest first.
    fn take(&mut self, n: usize) -> Vec<u32> {
        let mut out = Vec::with_capacity(n);
        while out.len() < n {
            match self.gaps.first_mut() {
                Some((a, b)) => {
                    out.push(*a);
                    *a += 1;
                    if a == b {
                        self.gaps.remove(0);
                    }
                }
                None => {
                    out.push(self.next);
                    self.next += 1;
                }
            }
        }
        out
    }
}

impl Docs {
    fn push(&mut self, path: &[u8], size: u64, mtime: u32) {
        self.items.push((self.buf.len() as u32, path.len() as u32, size, mtime));
        self.buf.extend_from_slice(path);
    }
    fn path(&self, i: usize) -> &[u8] {
        let (o, l, _, _) = self.items[i];
        &self.buf[o as usize..(o + l) as usize]
    }
    pub fn len(&self) -> usize {
        self.items.len()
    }
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }
    fn sort(&mut self) {
        let buf = &self.buf;
        self.items.sort_by(|a, b| buf[a.0 as usize..(a.0 + a.1) as usize].cmp(&buf[b.0 as usize..(b.0 + b.1) as usize]));
        self.items.dedup_by(|a, b| buf[a.0 as usize..(a.0 + a.1) as usize] == buf[b.0 as usize..(b.0 + b.1) as usize]);
    }
    fn find(&self, path: &[u8]) -> Option<usize> {
        let i = self.items.partition_point(|&(o, l, _, _)| &self.buf[o as usize..(o + l) as usize] < path);
        (i < self.items.len() && self.path(i) == path).then_some(i)
    }

    /// Index ranges of about SEG_BYTES of file data each.
    pub fn batches(&self) -> Vec<std::ops::Range<usize>> {
        let (mut out, mut start, mut bytes) = (Vec::new(), 0, 0u64);
        for (i, it) in self.items.iter().enumerate() {
            if bytes >= SEG_BYTES {
                out.push(start..i);
                (start, bytes) = (i, 0);
            }
            bytes += it.2;
        }
        if start < self.items.len() {
            out.push(start..self.items.len());
        }
        out
    }
}

/// Contents smaller than this are shared only within a segment: their copies
/// cost next to nothing, and empty files are everywhere.
const SMALL: usize = 256;

/// Rank of a doc that turned out not to be text: kept so diffs know we
/// looked at it, never a candidate.
const NOT_TEXT: i8 = i8::MIN;

struct DocMeta<'a> {
    path: &'a [u8],
    size: u64,
    mtime: u32,
    rank: i8,
    gid: u32,
}

/// One of the contents a segment being written owns.
struct ContentMeta<'a> {
    gid: u32,
    hash: [u64; 2],
    bloom: &'a [u64],
    long: &'a [u64],
    /// The first doc there holding it, or NONE.
    doc: u32,
}

/// What a doc holds, as a build finds it.
#[derive(Clone, Copy)]
enum Holds {
    /// Nothing: it is not text.
    Nothing,
    /// A content indexed before.
    Gid(u32),
    /// A content new in this build (numbered as first seen).
    New(u32),
}

/// One run of docs' output: what each doc holds, and for the contents new
/// in this build that it saw first, their (trigram, content) pairs in 256
/// buckets by the trigram's first byte, and their definition keys and two
/// bloom filters, flat.
struct Split {
    pairs: Vec<Vec<u64>>,
    syms: Vec<u32>,
    blooms: Vec<u64>,
    longs: Vec<u64>,
    docs: Vec<(usize, Holds)>,
    contents: Vec<SplitContent>,
}

/// Where a new content's data is in its split.
struct SplitContent {
    new: u32,
    hash: [u64; 2],
    small: bool,
    sym: std::ops::Range<usize>,
    bloom: std::ops::Range<usize>,
    long: std::ops::Range<usize>,
}

/// Build one segment file from docs (any order). Files that turn out not
/// to be text are recorded with no content; a file whose bytes are already
/// indexed (this sync, or before it: see `diff`) just names that content.
pub fn build_segment(dir: &Path, id: u64, docs: &Docs, range: std::ops::Range<usize>) -> Option<Segment> {
    use std::io::Read;
    // Docs in runs of 256, a run on one thread with its next 8 files open
    // and their reads started (`open_ahead`): on a cold cache those reads
    // overlap the work on earlier files. A read buffer and three 2 MiB
    // seen-sets go back to a pool after each run, so a build holds one per
    // thread.
    const RUN: usize = 256;
    const AHEAD: usize = 8;
    let dedup = docs.dedup.get_or_init(Dedup::default);
    // Contents new in this build, by hash, numbered as first seen.
    let new = std::sync::Mutex::new(Map::<[u64; 2], u32>::new());
    let scratch = std::sync::Mutex::new(Vec::new());
    let runs: Vec<std::ops::Range<usize>> = range.clone().step_by(RUN).map(|s| s..(s + RUN).min(range.end)).collect();
    let splits: Vec<Split> = rgy_par::map_collect_with(runs.len(), 1, |ri| {
        let run = runs[ri].clone();
        {
            let (mut seen, mut buf, mut short, mut long, mut tri) =
                scratch.lock().unwrap().pop().unwrap_or_else(|| (vec![0u64; 3 << 18], Vec::new(), Vec::new(), Vec::new(), Vec::new()));
            #[rustfmt::skip]
            let mut sp = Split { pairs: vec![Vec::new(); 256], syms: Vec::new(), blooms: Vec::new(), longs: Vec::new(), docs: Vec::new(), contents: Vec::new() };
            let mut ahead = std::collections::VecDeque::new();
            for i in run.clone() {
                while ahead.len() <= AHEAD && i + ahead.len() < run.end {
                    ahead.push_back(open_ahead(docs.path(i + ahead.len())));
                }
                buf.clear();
                let text = (ahead.pop_front().flatten())
                    .and_then(|f| f.take(MAX_FILE + 1).read_to_end(&mut buf).ok())
                    .is_some_and(|n| n as u64 <= MAX_FILE && rgx_core::memchr::memchr(0, &buf[..n.min(8192)]).is_none());
                let holds = if !text {
                    Holds::Nothing
                } else {
                    let h = content_hash(&buf);
                    match (buf.len() >= SMALL).then(|| dedup.find(h)).flatten() {
                        Some(g) => Holds::Gid(g),
                        None => {
                            let (k, first) = {
                                let mut m = new.lock().unwrap();
                                let n = m.len() as u32;
                                let k = *m.get_or_insert_with(h, || n);
                                (k, k == n)
                            };
                            if first {
                                let (sym, bl, lo) = (sp.syms.len(), sp.blooms.len(), sp.longs.len());
                                index_grams(&buf, &mut seen, &mut tri, &mut short, &mut long);
                                for &t in &tri {
                                    sp.pairs[(t >> 16) as usize].push((t as u64) << 32 | k as u64);
                                }
                                symbols(&buf, &mut sp.syms);
                                // Three bits a short gram; two a long one, three in big
                                // files (the costliest to read for nothing).
                                bloom(&short, 3, &mut sp.blooms);
                                bloom(&long, if buf.len() >= 16 << 10 { 3 } else { 2 }, &mut sp.longs);
                                let (bloom, long) = (bl..sp.blooms.len(), lo..sp.longs.len());
                                sp.contents.push(SplitContent { new: k, hash: h, small: buf.len() < SMALL, sym: sym..sp.syms.len(), bloom, long });
                            }
                            Holds::New(k)
                        }
                    }
                };
                sp.docs.push((i, holds));
            }
            scratch.lock().unwrap().push((seen, buf, short, long, tri));
            sp
        }
    });
    // The new contents, numbered by their first doc (docs in path order:
    // splits cover contiguous runs, in order), with gids.
    let nnew = new.into_inner().unwrap().len();
    let (mut local, mut first) = (vec![NONE; nnew], Vec::with_capacity(nnew));
    for (d, &(_, holds)) in splits.iter().flat_map(|sp| &sp.docs).enumerate() {
        if let Holds::New(k) = holds
            && local[k as usize] == NONE
        {
            local[k as usize] = first.len() as u32;
            first.push(d as u32);
        }
    }
    let gids = dedup.free.lock().unwrap().take(nnew);
    let meta: Vec<DocMeta> = (splits.iter().flat_map(|sp| &sp.docs))
        .map(|&(i, holds)| {
            let (_, _, size, mtime) = docs.items[i];
            let gid = match holds {
                Holds::Nothing => NONE,
                Holds::Gid(g) => g,
                Holds::New(k) => gids[local[k as usize] as usize],
            };
            DocMeta { path: docs.path(i), size, mtime, rank: if gid == NONE { NOT_TEXT } else { doc_rank(docs.path(i)) }, gid }
        })
        .collect();
    let mut own: Vec<Option<ContentMeta>> = (0..nnew).map(|_| None).collect();
    for sp in &splits {
        for c in &sp.contents {
            let l = local[c.new as usize] as usize;
            let (bloom, long) = (&sp.blooms[c.bloom.clone()], &sp.longs[c.long.clone()]);
            own[l] = Some(ContentMeta { gid: gids[l], hash: c.hash, bloom, long, doc: first[l] });
        }
    }
    let own: Vec<ContentMeta> = own.into_iter().map(Option::unwrap).collect();
    let shared: Vec<([u64; 2], u32)> =
        splits.iter().flat_map(|sp| &sp.contents).filter(|c| !c.small).map(|c| (c.hash, gids[local[c.new as usize] as usize])).collect();
    // Postings, built in parallel over runs of keys: a run of trigrams takes
    // its bucket of every split, the definition keys every new content's.
    let runs = key_runs();
    let parts = rgy_par::map_collect_with(runs.len(), 1, |ri| {
        let keys = runs[ri].clone();
        {
            let mut pairs: Vec<u64> = Vec::new();
            if keys.start < 1 << 24 {
                for sp in &splits {
                    pairs.extend(sp.pairs[(keys.start >> 16) as usize].iter().map(|&p| p >> 32 << 32 | local[p as u32 as usize] as u64));
                }
            } else {
                for sp in &splits {
                    for c in &sp.contents {
                        let l = local[c.new as usize] as u64;
                        pairs.extend(sp.syms[c.sym.clone()].iter().map(|&x| (x as u64) << 32 | l));
                    }
                }
            }
            pairs.sort_unstable();
            let (mut part, mut list) = (Postings::default(), Vec::new());
            for run in pairs.chunk_by(|a, b| a >> 32 == b >> 32) {
                list.clear();
                list.extend(run.iter().map(|&p| p as u32));
                part.push(nnew, (run[0] >> 32) as u32, &list);
            }
            part
        }
    });
    let seg = write_segment(dir, id, &meta, &own, parts)?;
    // This sync's later builds find these contents.
    {
        let mut built = dedup.built.lock().unwrap();
        for (h, g) in shared {
            built.insert(h, g);
        }
    }
    Some(seg)
}

/// Merge segments into one, dropping tombstoned docs and the contents no
/// live doc (anywhere) holds. Postings stay in order because contents are
/// renumbered segment by segment.
pub fn merge(dir: &Path, id: u64, segs: &[&Segment]) -> Option<Segment> {
    let mut meta = Vec::new();
    for s in segs {
        for d in (0..s.ndocs as u32).filter(|&d| !s.is_dead(d)) {
            let i = d as usize;
            meta.push(DocMeta { path: s.path(d), size: s.size()[i], mtime: s.mtime()[i], rank: s.rank()[i], gid: s.doc_gid()[i] });
        }
    }
    let (mut own, mut remap) = (Vec::new(), Vec::with_capacity(segs.len()));
    for s in segs {
        let mut r = vec![NONE; s.nown];
        for c in (0..s.nown).filter(|&c| s.refs.get(c) > 0) {
            r[c] = own.len() as u32;
            let (bloom, long) = (s.bloom_of(c as u32), s.long_of(c as u32));
            own.push(ContentMeta { gid: s.own_gid()[c], hash: s.hash_of(c as u32), bloom, long, doc: NONE });
        }
        remap.push(r);
    }
    // The own contents by their gid: a sorted vector with a binary search,
    // not a table. The gids come out of a pool and are not dense, so a
    // vector indexed by the gid would be mostly holes -- but there is one
    // entry per own content, and the search is over a corpus built once.
    let mut at: Vec<(u32, u32)> = own.iter().enumerate().map(|(c, m)| (m.gid, c as u32)).collect();
    at.sort_unstable();
    for (d, m) in meta.iter().enumerate() {
        // The content of this gid. The gids are unique -- one per own
        // content -- so the run of the gid is one entry.
        let end = at.partition_point(|&(g, _)| g <= m.gid);
        if end > 0 && at[end - 1].0 == m.gid {
            let c = at[end - 1].1;
            if own[c as usize].doc == NONE {
                own[c as usize].doc = d as u32;
            }
        }
    }
    // Postings, merged in parallel over runs of keys.
    let runs = key_runs();
    let parts = rgy_par::map_collect_with(runs.len(), 1, |ri| {
        let keys = runs[ri].clone();
        {
            let k = |s: &Segment, x: u64| s.tri_key().partition_point(|&t| (t as u64) < x);
            let mut pos: Vec<std::ops::Range<usize>> = segs.iter().map(|s| k(s, keys.start)..k(s, keys.end)).collect();
            let (mut out, mut list, mut part) = (Postings::default(), Vec::new(), Vec::new());
            while let Some(t) = segs.iter().zip(&pos).filter(|(_, p)| !p.is_empty()).map(|(s, p)| s.tri_key()[p.start]).min() {
                list.clear();
                for (si, s) in segs.iter().enumerate() {
                    if !pos[si].is_empty() && s.tri_key()[pos[si].start] == t {
                        part.clear();
                        s.list_into(pos[si].start, &mut part);
                        list.extend(part.iter().map(|&c| remap[si][c as usize]).filter(|&c| c != NONE));
                        pos[si].start += 1;
                    }
                }
                if !list.is_empty() {
                    out.push(own.len(), t, &list);
                }
            }
            out
        }
    });
    write_segment(dir, id, &meta, &own, parts)
}

/// The key space in runs that postings are built in, in parallel: the
/// trigrams 64k at a time, then the definition keys (above 2^24).
fn key_runs() -> Vec<std::ops::Range<u64>> {
    (0..257usize).map(|r| if r < 256 { (r as u64) << 16..(r as u64 + 1) << 16 } else { 1 << 24..1 << 32 }).collect()
}

/// Posting lists for a run of keys, encoded: the keys (ascending), where
/// each list starts in `post` (with its flags), and the bytes.
#[derive(Default)]
struct Postings {
    keys: Vec<u32>,
    offs: Vec<u32>,
    post: Vec<u8>,
}

impl Postings {
    /// Add `key`'s contents (ascending) in a segment owning `n`: a bitset
    /// when dense, delta varints otherwise.
    fn push(&mut self, n: usize, key: u32, list: &[u32]) {
        self.keys.push(key);
        if list.len() * 8 > n {
            self.offs.push(self.post.len() as u32 | BITSET);
            let at = self.post.len();
            self.post.resize(at + n.div_ceil(8), 0);
            for &d in list {
                self.post[at + d as usize / 8] |= 1 << (d % 8);
            }
        } else {
            let (at, skips) = (self.post.len(), list.len().saturating_sub(1) / SKIP);
            if skips > 0 {
                self.offs.push(at as u32 | SKIPS);
                self.post.extend_from_slice(&(skips as u32).to_le_bytes());
                self.post.resize(at + 4 + skips * 8, 0);
            } else {
                self.offs.push(at as u32);
            }
            let start = self.post.len();
            let mut last = 0u32;
            for (i, &d) in list.iter().enumerate() {
                if i > 0 && i % SKIP == 0 {
                    let e = at + 4 + (i / SKIP - 1) * 8;
                    let off = (self.post.len() - start) as u32;
                    self.post[e..e + 8].copy_from_slice(&((off as u64) << 32 | last as u64).to_le_bytes());
                }
                put_varint(&mut self.post, d - last);
                last = d;
            }
        }
    }
}

/// Write the segment file: docs, own contents, and their postings in runs
/// of keys (ascending).
fn write_segment(dir: &Path, id: u64, docs: &[DocMeta<'_>], own: &[ContentMeta<'_>], parts: Vec<Postings>) -> Option<Segment> {
    if docs.is_empty() && own.is_empty() {
        return None;
    }
    let (ndocs, nown) = (docs.len(), own.len());
    let (mut keys, mut tri_off, mut post) = (Vec::new(), Vec::new(), Vec::new());
    for p in &parts {
        let base = post.len() as u32;
        keys.extend_from_slice(&p.keys);
        tri_off.extend(p.offs.iter().map(|&o| ((o & !FLAGS) + base) | (o & FLAGS)));
        post.extend_from_slice(&p.post);
    }
    drop(parts);
    tri_off.push(post.len() as u32);
    let mut paths = Vec::new();
    let mut path_off = vec![0u32];
    for d in docs {
        paths.extend_from_slice(d.path);
        path_off.push(paths.len() as u32);
    }
    let size: Vec<u64> = docs.iter().map(|d| d.size).collect();
    let mtime: Vec<u32> = docs.iter().map(|d| d.mtime).collect();
    let rank: Vec<i8> = docs.iter().map(|d| d.rank).collect();
    let mut by_path: Vec<u32> = (0..ndocs as u32).collect();
    by_path.sort_by(|&a, &b| docs[a as usize].path.cmp(docs[b as usize].path));
    let mut by_rank: Vec<u32> = (0..ndocs as u32).collect();
    by_rank.sort_by_key(|&d| (rank_key(docs[d as usize].rank, docs[d as usize].mtime), d));
    let doc_gid: Vec<u32> = docs.iter().map(|d| d.gid).collect();
    let rank_gid: Vec<u32> = by_rank.iter().map(|&d| doc_gid[d as usize]).collect();
    let mut by_gid: Vec<u32> = (0..ndocs as u32).collect();
    by_gid.sort_by_key(|&d| doc_gid[d as usize]);
    let gid_sorted: Vec<u32> = by_gid.iter().map(|&d| doc_gid[d as usize]).collect();
    let own_gid: Vec<u32> = own.iter().map(|c| c.gid).collect();
    let own_doc: Vec<u32> = own.iter().map(|c| c.doc).collect();
    let hash: Vec<u64> = own.iter().flat_map(|c| c.hash).collect();
    let mut by_hash: Vec<u32> = (0..nown as u32).collect();
    by_hash.sort_by_key(|&c| own[c as usize].hash);
    let (mut bloom_off, mut blooms, mut long_off, mut longs) = (vec![0u32], Vec::new(), vec![0u32], Vec::new());
    for c in own {
        blooms.extend_from_slice(c.bloom);
        bloom_off.push(blooms.len() as u32);
        longs.extend_from_slice(c.long);
        long_off.push(longs.len() as u32);
    }

    let (off, _) = layout(&lens(ndocs, keys.len(), post.len(), paths.len(), blooms.len(), nown, longs.len()));
    let fields = [ndocs, keys.len(), post.len(), paths.len(), blooms.len(), nown, longs.len()].map(|v| v as u64);
    let hdr = header(MAGIC, &fields);
    let sections: [&[u8]; NS] = [
        as_bytes(&keys),
        as_bytes(&tri_off),
        &post,
        as_bytes(&path_off),
        &paths,
        as_bytes(&size),
        as_bytes(&mtime),
        as_bytes(&by_path),
        as_bytes(&rank),
        as_bytes(&by_rank),
        as_bytes(&doc_gid),
        as_bytes(&rank_gid),
        as_bytes(&gid_sorted),
        as_bytes(&by_gid),
        as_bytes(&own_gid),
        as_bytes(&own_doc),
        as_bytes(&hash),
        as_bytes(&by_hash),
        as_bytes(&bloom_off),
        as_bytes(&blooms),
        as_bytes(&long_off),
        as_bytes(&longs),
    ];
    let p = seg_path(dir, id);
    let tmp = p.with_extension("tmp");
    let write = || -> std::io::Result<()> {
        let mut f = std::io::BufWriter::new(std::fs::File::create(&tmp)?);
        f.write_all(&hdr)?;
        let mut at = HDR;
        for (k, sec) in sections.iter().enumerate() {
            f.write_all(&vec![0u8; off[k] - at])?;
            f.write_all(sec)?;
            at = off[k] + sec.len();
        }
        f.write_all(&vec![0u8; ((at + 63) & !63) - at])?;
        f.flush()
    };
    write().ok()?;
    std::fs::rename(&tmp, &p).ok()?;
    Segment::load(dir, id)
}

/// Should this file be in the content index?
pub fn eligible(path: &[u8], size: u64, home: &[u8]) -> bool {
    let name = &path[path.iter().rposition(|&b| b == b'/').map_or(0, |p| p + 1)..];
    name_ok(name, size) && in_scope(path, home)
}

/// The name/size half of eligibility, checkable before building a path.
fn name_ok(name: &[u8], size: u64) -> bool {
    if size > MAX_FILE {
        return false;
    }
    match name.iter().rposition(|&b| b == b'.').filter(|&p| p > 0) {
        Some(dot) => {
            let ext = &name[dot + 1..];
            TEXT_EXTS.iter().any(|x| x.eq_ignore_ascii_case(ext)) && !name.ends_with(b".min.js") && name != b"package-lock.json"
        }
        None => size <= 256 << 10,
    }
}

/// Is this path (file or directory) inside the indexed area?
pub fn in_scope(path: &[u8], home: &[u8]) -> bool {
    let Some(rest) = path.strip_prefix(home) else { return false };
    if !rest.is_empty() && rest[0] != b'/' {
        return false;
    }
    let rel = rest.strip_prefix(b"/").unwrap_or(rest);
    if SKIP_UNDER_HOME.iter().any(|p| rel.starts_with(p) && rel.get(p.len()).is_none_or(|&b| b == b'/')) {
        return false;
    }
    !rel.split(|&b| b == b'/').any(|c| SKIP_DIRS.contains(&c) || SKIP_SUFFIXES.iter().any(|x| c.len() > x.len() && c.ends_with(x)))
}

/// Does the name query let every indexed doc under its scope (if any)
/// through: no words, extensions, ranges or patterns? Then a segment wholly
/// in scope needn't be checked doc by doc.
fn only_scope(q: &Query) -> bool {
    q.tokens.is_empty()
        && q.kind_ok(KIND_FILE)
        && q.exts.is_empty()
        && q.size == (0, u64::MAX)
        && q.mtime == (0, u32::MAX)
        && q.name_re.is_none()
        && q.path_re.is_none()
}

/// Candidate order tier: your files first, then dot-dirs, logs and transcripts.
fn doc_rank(path: &[u8]) -> i8 {
    let mut r = 0i8;
    if path.split(|&b| b == b'/').any(|c| c.first() == Some(&b'.')) {
        r -= 2;
    }
    let name = &path[path.iter().rposition(|&b| b == b'/').map_or(0, |p| p + 1)..];
    if [&b".jsonl"[..], b".ndjson", b".log", b".lock", b".sum"].iter().any(|x| name.ends_with(x)) {
        r -= 1;
    }
    r
}

pub struct Content {
    pub dir: PathBuf,
    pub segs: Vec<Segment>,
    next_id: u64,
    /// Every segment's own contents as runs of gids, by first gid: (first
    /// gid, length, segment, first content).
    gids: Vec<(u32, u32, u32, u32)>,
    /// Per segment, where its docs start in one numbering of all docs; and
    /// per 256 of those numbers, the segment where the first one is.
    base: Vec<u32>,
    blocks: Vec<u32>,
    /// Per segment, its own contents held by more docs than their first one.
    copies: Vec<Copies>,
    /// Per segment, the segments whose live docs hold its contents (itself
    /// too), each with how many of them.
    users: Vec<Vec<(u32, u32)>>,
    /// Per segment, the segments owning contents its live docs hold (itself
    /// too), each with how many of them.
    sources: Vec<Vec<(u32, u32)>>,
}

/// A segment's own contents held by more docs than their first one: those
/// contents (ascending), where each one's other docs start in `docs` (the
/// next start ends them), and those docs, numbered by `Content::base`.
#[derive(Default)]
struct Copies {
    contents: Vec<u32>,
    starts: Vec<u32>,
    docs: Vec<u32>,
}

/// How many live docs hold each of a segment's own contents, in bit planes
/// (bit c of plane k: bit k of content c's count), so that the docs holding
/// any of a set of contents add up a word at a time.
#[derive(Clone, Default)]
struct Counts {
    planes: Vec<Vec<u64>>,
    n: usize,
}

impl Counts {
    fn new(n: usize) -> Counts {
        Counts { planes: Vec::new(), n }
    }

    fn get(&self, c: usize) -> u32 {
        self.planes.iter().enumerate().map(|(k, p)| ((p[c / 64] >> (c % 64) & 1) as u32) << k).sum()
    }

    fn add(&mut self, c: usize) {
        for p in &mut self.planes {
            p[c / 64] ^= 1 << (c % 64);
            if p[c / 64] >> (c % 64) & 1 != 0 {
                return;
            }
        }
        let mut p = vec![0u64; self.n.div_ceil(64)];
        p[c / 64] = 1 << (c % 64);
        self.planes.push(p);
    }

    /// (The count is above zero.)
    fn sub(&mut self, c: usize) {
        for p in &mut self.planes {
            p[c / 64] ^= 1 << (c % 64);
            if p[c / 64] >> (c % 64) & 1 == 0 {
                return;
            }
        }
    }

    /// The counts of the contents set in `bits`, added up.
    fn sum(&self, bits: &[u64]) -> usize {
        (self.planes.iter().enumerate()).map(|(k, p)| p.iter().zip(bits).map(|(a, b)| (a & b).count_ones() as usize).sum::<usize>() << k).sum()
    }

    /// How many are held at all.
    fn live(&self) -> usize {
        (0..self.n.div_ceil(64)).map(|w| self.planes.iter().fold(0, |x, p| x | p[w]).count_ones() as usize).sum()
    }
}

/// The segment and own content of gid `g`, by the table in `Content::gids`.
fn content_of(gids: &[(u32, u32, u32, u32)], g: u32) -> Option<(usize, u32)> {
    let i = gids.partition_point(|r| r.0 <= g).checked_sub(1)?;
    let (g0, n, s, c0) = gids[i];
    (g - g0 < n).then_some((s as usize, c0 + (g - g0)))
}

impl Content {
    /// Open the segments the manifest lists, without touching anything: the
    /// view of an engine that follows another process's index.
    pub fn open_shared(dir: PathBuf) -> Content {
        let manifest: Vec<u64> =
            std::fs::read_to_string(dir.join("manifest")).unwrap_or_default().split_whitespace().filter_map(|s| s.parse().ok()).collect();
        let segs: Vec<Segment> = manifest.iter().filter_map(|&id| Segment::load(&dir, id)).collect();
        let next_id = segs.iter().map(|s| s.id + 1).max().unwrap_or(1);
        #[rustfmt::skip]
        let mut c = Content { dir, segs, next_id, gids: Vec::new(), base: Vec::new(), blocks: Vec::new(), copies: Vec::new(), users: Vec::new(), sources: Vec::new() };
        c.link();
        c
    }

    /// Open as the owner: also delete what the manifest doesn't list.
    pub fn open(dir: PathBuf) -> Content {
        std::fs::create_dir_all(&dir).ok();
        let c = Content::open_shared(dir);
        // Anything not loaded (old format, crashed build) is garbage.
        let keep: Vec<String> = c.segs.iter().flat_map(|s| [format!("seg-{:06}.fsc", s.id), format!("seg-{:06}.dead", s.id)]).collect();
        for e in std::fs::read_dir(&c.dir).into_iter().flatten().flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            if name.starts_with("seg-") && !keep.contains(&name) {
                let _ = std::fs::remove_file(e.path());
            }
        }
        c
    }

    /// Link the segments through the contents they share: the gid table,
    /// each content's live docs, and which segments use which.
    fn link(&mut self) {
        self.gids = (self.segs.iter().enumerate()).flat_map(|(si, s)| s.runs.iter().map(move |&(c, g, n)| (g, n, si as u32, c))).collect();
        self.gids.sort_unstable();
        self.base = Vec::new();
        self.copies = (0..self.segs.len()).map(|_| Copies::default()).collect();
        self.users = vec![Vec::new(); self.segs.len()];
        self.sources = vec![Vec::new(); self.segs.len()];
        for s in &mut self.segs {
            s.refs = Counts::new(s.nown);
        }
        self.link_docs(0);
    }

    /// Link the live docs of the segments from `from` on (all earlier ones
    /// linked) to the contents they hold.
    fn link_docs(&mut self, from: usize) {
        let n = self.segs.len();
        while self.base.len() < n {
            let at = self.base.len();
            self.base.push(if at == 0 { 0 } else { self.base[at - 1] + self.segs[at - 1].ndocs as u32 });
        }
        let all = self.base.last().map_or(0, |&b| b as usize + self.segs[n - 1].ndocs);
        self.blocks = (0..all.div_ceil(256)).map(|k| (self.base.partition_point(|&b| b as usize <= k * 256) - 1) as u32).collect();
        let mut refs: Vec<Counts> = self.segs.iter_mut().map(|s| std::mem::take(&mut s.refs)).collect();
        // Per owner, (content, doc) of the docs that are not its first.
        let mut more: Vec<Vec<(u32, u32)>> = vec![Vec::new(); n];
        for b in from..n {
            let s = &self.segs[b];
            let mut weight = vec![0u32; n];
            // The run the last doc's gid was in: copies come in runs too.
            let mut run = (0u32, 0u32, 0u32, 0u32);
            for (d, &g) in s.doc_gid().iter().enumerate() {
                if g == NONE || s.is_dead(d as u32) {
                    continue;
                }
                if g.wrapping_sub(run.0) >= run.1 {
                    let Some(i) = self.gids.partition_point(|r| r.0 <= g).checked_sub(1).filter(|&i| g - self.gids[i].0 < self.gids[i].1) else {
                        continue;
                    };
                    run = self.gids[i];
                }
                let (a, c) = (run.2 as usize, run.3 + g - run.0);
                refs[a].add(c as usize);
                weight[a] += 1;
                if !(a == b && self.segs[a].own_doc()[c as usize] == d as u32) {
                    more[a].push((c, self.base[b] + d as u32));
                }
            }
            for (a, &w) in weight.iter().enumerate().filter(|(_, w)| **w > 0) {
                self.users[a].push((b as u32, w));
                self.sources[b].push((a as u32, w));
            }
        }
        for (a, mut add) in more.into_iter().enumerate().filter(|(_, m)| !m.is_empty()) {
            let cp = &mut self.copies[a];
            for (k, &c) in cp.contents.iter().enumerate() {
                let end = cp.starts.get(k + 1).map_or(cp.docs.len(), |&e| e as usize);
                add.extend(cp.docs[cp.starts[k] as usize..end].iter().map(|&h| (c, h)));
            }
            add.sort_unstable();
            *cp = Copies { docs: Vec::with_capacity(add.len()), ..Default::default() };
            for (k, &(c, h)) in add.iter().enumerate() {
                if cp.contents.last() != Some(&c) {
                    cp.contents.push(c);
                    cp.starts.push(k as u32);
                }
                cp.docs.push(h);
            }
            cp.contents.shrink_to_fit();
            cp.starts.shrink_to_fit();
        }
        for (s, r) in self.segs.iter_mut().zip(refs) {
            s.refs = r;
        }
    }

    /// The segment and doc numbered `h` (see `base`).
    #[inline]
    fn doc_at(&self, h: u32) -> (usize, u32) {
        let mut b = self.blocks[h as usize / 256] as usize;
        while self.base.get(b + 1).is_some_and(|&x| x <= h) {
            b += 1;
        }
        (b, h - self.base[b])
    }

    /// The segment and own content of gid `g`.
    fn content_of(&self, g: u32) -> Option<(usize, u32)> {
        content_of(&self.gids, g)
    }

    /// Every live doc holding one of the own contents `sel` (ascending) of
    /// segment `a`, as (segment, doc).
    fn holders(&self, a: usize, sel: &[u32], mut f: impl FnMut(usize, u32)) {
        let (s, cp, own) = (&self.segs[a], &self.copies[a], self.segs[a].own_doc());
        if cp.contents.is_empty() {
            for &c in sel {
                let first = own[c as usize];
                if first != NONE && !s.is_dead(first) {
                    f(a, first);
                }
            }
            return;
        }
        let mut k = 0;
        for &c in sel {
            let first = own[c as usize];
            if first != NONE && !s.is_dead(first) {
                f(a, first);
            }
            k = gallop(&cp.contents, k, c);
            if cp.contents.get(k) == Some(&c) {
                let end = cp.starts.get(k + 1).map_or(cp.docs.len(), |&e| e as usize);
                for &h in &cp.docs[cp.starts[k] as usize..end] {
                    let (b, d) = self.doc_at(h);
                    if !self.segs[b].is_dead(d) {
                        f(b, d);
                    }
                }
            }
        }
    }

    fn save_manifest(&self) {
        let s: String = self.segs.iter().map(|s| format!("{}\n", s.id)).collect();
        let tmp = self.dir.join("manifest.tmp");
        if std::fs::write(&tmp, s).is_ok() {
            let _ = std::fs::rename(tmp, self.dir.join("manifest"));
        }
    }

    pub fn docs(&self) -> usize {
        self.segs.iter().map(|s| s.live_docs).sum()
    }

    pub fn bytes(&self) -> usize {
        self.segs.iter().map(|s| s.map.len()).sum()
    }

    /// Diff what the name index wants (from `wanted`, per dir) against the
    /// docs we hold: tombstone what changed or went away, return what needs
    /// (re)indexing. Cheap; the caller builds segments off-lock.
    pub fn diff(&mut self, wants: Vec<(Vec<u8>, bool, Docs)>) -> Docs {
        let mut todo = Docs::default();
        let mut touched = vec![false; self.segs.len()];
        let mut killed = Vec::new();
        let gids = &self.gids;
        for (dir, recursive, want) in wants {
            let mut held = vec![false; want.len()];
            let lo = join(&dir, b"");
            for (si, s) in self.segs.iter_mut().enumerate() {
                let ids = if recursive { s.with_prefix(&lo).collect::<Vec<_>>() } else { s.direct_children(&lo) };
                for d in ids {
                    let (i, g) = (d as usize, s.doc_gid()[d as usize]);
                    match want.find(s.path(d)) {
                        // (A doc whose content is gone is indexed again.)
                        Some(k)
                            if want.items[k].2 == s.size()[i] && want.items[k].3 == s.mtime()[i] && (g == NONE || content_of(gids, g).is_some()) =>
                        {
                            held[k] = true
                        }
                        _ => {
                            if s.kill(d) {
                                touched[si] = true;
                                killed.push(g);
                            }
                        }
                    }
                }
            }
            for (i, h) in held.iter().enumerate() {
                if !h {
                    let (_, _, size, mtime) = want.items[i];
                    todo.push(want.path(i), size, mtime);
                }
            }
        }
        for g in killed {
            if let Some((a, c)) = self.content_of(g) {
                self.segs[a].refs.sub(c as usize);
            }
        }
        for (s, t) in self.segs.iter().zip(&touched) {
            if *t {
                s.save_dead(&self.dir);
            }
        }
        // A segment goes once no live doc is in it and none holds its contents.
        let empty: Vec<u64> = self.segs.iter().filter(|s| s.live_docs == 0 && s.live_own() == 0).map(|s| s.id).collect();
        if !empty.is_empty() {
            self.drop_segments(&empty);
            self.link();
            self.save_manifest();
        }
        todo.sort();
        if !todo.is_empty() {
            let _ = todo.dedup.set(self.dedup());
        }
        todo
    }

    /// What a sync's builds need to index each distinct content once: the
    /// segments' own contents, by hash, and the gids no segment owns.
    fn dedup(&self) -> Dedup {
        let held = self.segs.iter().filter(|s| s.nown > 0).map(|s| Held { map: s.map.clone(), nown: s.nown, off: s.off }).collect();
        let (mut gaps, mut next) = (Vec::new(), 0u32);
        for &(g, n, _, _) in &self.gids {
            if g > next {
                gaps.push((next, g));
            }
            next = next.max(g + n);
        }
        Dedup { held, built: Default::default(), free: std::sync::Mutex::new(Gids { gaps, next }) }
    }

    /// Folders holding an indexed file that changed (or went away) since
    /// `since`: an edit in place leaves its folder's mtime alone, so lost
    /// FSEvents history is recovered for the content index by an lstat per
    /// indexed file.
    pub fn changed_dirs(&self, since: u32) -> Vec<Vec<u8>> {
        // One sweep per segment: the segments are the pieces of work, and
        // each one is a run of documents.
        let mut out: Vec<Vec<u8>> = rgy_par::map_collect_with(self.segs.len(), 1, |si| {
            let s = &self.segs[si];
            let mut v = Vec::new();
            for d in 0..s.ndocs as u32 {
                if s.is_dead(d) {
                    continue;
                }
                if !crate::live::lstat(s.path(d)).is_none_or(|o| o.mtime >= since || o.size != s.size()[d as usize]) {
                    continue;
                }
                let p = s.path(d);
                v.push(p[..p.iter().rposition(|&b| b == b'/').unwrap_or(0).max(1)].to_vec());
            }
            v
        })
        .into_iter()
        .flatten()
        .collect();
        out.sort();
        out.dedup();
        out
    }

    pub fn alloc_id(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id - 1
    }

    pub fn push(&mut self, seg: Segment) {
        let si = self.segs.len() as u32;
        self.gids.extend(seg.runs.iter().map(|&(c, g, n)| (g, n, si, c)));
        self.gids.sort_unstable();
        self.segs.push(seg);
        self.copies.push(Copies::default());
        self.users.push(Vec::new());
        self.sources.push(Vec::new());
        self.link_docs(self.segs.len() - 1);
        self.save_manifest();
    }

    /// Reindex what changed under `dirs` (direct children) and `trees`
    /// (whole subtrees) -- both on-disk paths, as [`Self::changed_dirs`]
    /// and `Live::trees` produce -- from `live`'s current view. `root` is
    /// the tree the name index was built from and `home` scopes this index,
    /// the same two the queries pass to [`scan_paths`]. Returns how many
    /// docs were rebuilt (0 = already up to date).
    ///
    /// This is the live-update half of a query's freshness: [`wants`]
    /// gathers what should be indexed there, [`Self::diff`] kills what is
    /// gone or stale and lists the rest, and one segment is built from the
    /// list and pushed. Querying right after sees the edits; querying
    /// without it only misses what changed since the last sync.
    pub fn sync(&mut self, live: &Live, home: &[u8], dirs: &[Vec<u8>], trees: &[Vec<u8>], root: &[u8]) -> usize {
        let wants = wants(live, home, dirs, trees, root);
        let todo = self.diff(wants);
        let n = todo.len();
        if n == 0 {
            return 0;
        }
        let id = self.alloc_id();
        if let Some(seg) = build_segment(&self.dir, id, &todo, 0..n) {
            self.push(seg);
        }
        n
    }

    /// Tiered merging: 8 segments of the same size tier become one, so
    /// incremental updates never pile up thousands of tiny segments. Returns
    /// the group to merge (ids), capped so a merge's postings stay small.
    /// First, a segment whose docs or contents were mostly replaced is
    /// rewritten alone: every query decodes the postings of its dead
    /// contents too.
    pub fn merge_plan(&self) -> Option<Vec<u64>> {
        let stale = |s: &&Segment| {
            let own = s.live_own();
            (s.live_docs > 0 || own > 0) && (s.live_docs * 2 < s.ndocs || own * 2 < s.nown)
        };
        if let Some(s) = self.segs.iter().find(stale) {
            return Some(vec![s.id]);
        }
        let tier = |s: &Segment| (s.plen.max(1) as f64).log(4.0) as u32;
        // The segments by their tier: the tiers are the logarithm of the
        // posting length, so there are a handful of them and a vector
        // indexed by the tier is the shape.
        let mut by_tier: Vec<Vec<&Segment>> = Vec::new();
        for s in &self.segs {
            let t = tier(s) as usize;
            if by_tier.len() <= t {
                by_tier.resize_with(t + 1, Vec::new);
            }
            by_tier[t].push(s);
        }
        // The tiers are already in order.
        for v in by_tier.into_iter().filter(|v| v.len() >= 8) {
            let group: Vec<u64> = v.iter().take(8).map(|s| s.id).collect();
            let bytes: usize = v.iter().take(8).map(|s| s.plen).sum();
            if bytes <= MERGE_CAP {
                return Some(group);
            }
        }
        None
    }

    pub fn segments(&self, ids: &[u64]) -> Vec<&Segment> {
        self.segs.iter().filter(|s| ids.contains(&s.id)).collect()
    }

    /// Swap merged segments for their replacement, where the first of them
    /// was (a rewritten segment keeps its place in the ranking's tie order).
    /// Only the content worker writes, so nothing was tombstoned while the
    /// merge ran.
    pub fn replace(&mut self, ids: &[u64], seg: Segment) {
        let at = self.segs.iter().position(|s| ids.contains(&s.id)).unwrap_or(self.segs.len());
        self.drop_segments(ids);
        self.segs.insert(at, seg);
        self.link();
        self.save_manifest();
    }

    fn drop_segments(&mut self, ids: &[u64]) {
        self.segs.retain(|s| !ids.contains(&s.id));
        for id in ids {
            let _ = std::fs::remove_file(seg_path(&self.dir, *id));
            let _ = std::fs::remove_file(dead_path(&self.dir, *id));
        }
    }

    /// Candidate docs for a pattern, filtered by the name query: in groups
    /// (each with its `FIRST` best in front), and how many in all.
    fn candidates(&self, plan: &TQ, filt: &Query) -> Found {
        // A scope narrows each segment to the doc range holding its paths;
        // None: no live doc there under it.
        let prefix = filt.scope.as_ref().map(|s| [s.as_slice(), b"/"].concat());
        let only_scope = only_scope(filt);
        let scope: Vec<Option<(std::ops::Range<u32>, bool)>> = (self.segs.iter())
            .map(|s| {
                let (docs, whole) = prefix.as_ref().map_or((0..s.ndocs as u32, true), |p| s.doc_range(p));
                (s.live_docs > 0 && !docs.is_empty()).then_some((docs, !(only_scope && whole)))
            })
            .collect();
        // About a lane per 25k docs to search, at most 8: helpers start
        // within ~10 us, but past 8 lanes they get in each other's way.
        let docs: usize = scope.iter().flatten().map(|(docs, _)| docs.len()).sum();
        let lanes = |n: usize| (docs / 25_000).clamp(1, n.max(1)).min(8);
        let ranked = |s: &Segment, si: usize, d: u32| (rank_key(s.rank()[d as usize], s.mtime()[d as usize]), si as u32, d);
        if matches!(plan, TQ::All) {
            let work: Vec<usize> = (0..self.segs.len()).filter(|&si| scope[si].is_some()).collect();
            let per = par_claim(&work, lanes(work.len()), |&si| {
                let s = &self.segs[si];
                let (docs, check) = scope[si].clone().unwrap();
                let ids = if !check && docs.len() == s.ndocs {
                    let (mut bits, total) = live_text(s);
                    // A pattern with nothing to look up: walk the docs
                    // best-ranked first, to the segment's FIRST best.
                    if total * 8 > s.ndocs {
                        let mut top = Vec::with_capacity(FIRST);
                        for &d in s.by_rank() {
                            if top.len() == FIRST {
                                break;
                            }
                            let w = &mut bits[d as usize / 64];
                            if *w >> (d % 64) & 1 != 0 {
                                *w &= !(1 << (d % 64));
                                top.push(ranked(s, si, d));
                            }
                        }
                        return (Cands::Walk { si, bits, top }, total);
                    }
                    set_bits(&bits, total)
                } else {
                    docs.collect()
                };
                let mut v: Vec<Ranked> = Vec::with_capacity(ids.len());
                v.extend(
                    ids.into_iter()
                        .filter(|&d| !s.is_dead(d) && s.rank()[d as usize] != NOT_TEXT)
                        .filter(|&d| !check || filt.match_path(s.path(d), KIND_FILE, s.size()[d as usize], s.mtime()[d as usize]).is_some())
                        .map(|d| ranked(s, si, d)),
                );
                let n = v.len();
                (Cands::list(v), n)
            });
            let total = per.iter().map(|p| p.1).sum();
            return Found { per: per.into_iter().map(|p| p.0).collect(), total, dense: Vec::new() };
        }
        // Every live doc counts, unchecked: then a content found in bulk
        // counts as its live docs.
        let global =
            scope.iter().zip(&self.segs).all(|(x, s)| s.live_docs == 0 || x.as_ref().is_some_and(|(docs, check)| !check && docs.len() == s.ndocs));
        // A candidate doc under the scope.
        let admit = |b: usize, d: u32| -> Option<Ranked> {
            let (docs, check) = scope[b].as_ref()?;
            let t = &self.segs[b];
            let ok = docs.contains(&d) && (!check || filt.match_path(t.path(d), KIND_FILE, t.size()[d as usize], t.mtime()[d as usize]).is_some());
            ok.then(|| ranked(t, b, d))
        };
        // A scope holding few of the docs (up to 1 in 16): the contents its
        // docs hold, per owner, then which of those the plan selects (in the
        // range they span).
        let few = self.segs.iter().map(|s| s.ndocs).sum::<usize>() / 16;
        let under: Option<Vec<(usize, Vec<u32>)>> = (prefix.as_ref())
            .filter(|p| {
                !global && (self.segs.iter().enumerate()).filter(|(b, _)| scope[*b].is_some()).map(|(_, t)| t.count_prefix(p)).sum::<usize>() <= few
            })
            .map(|p| (0..self.segs.len()).filter(|&b| scope[b].is_some()).map(|b| (b, self.segs[b].with_prefix(p).collect())).collect());
        if let Some(under) = under {
            let mut need: Vec<Vec<(u32, u32, u32)>> = vec![Vec::new(); self.segs.len()];
            for (b, docs) in under {
                let t = &self.segs[b];
                for d in docs {
                    if let Some((a, c)) = self.content_of(t.doc_gid()[d as usize]) {
                        need[a].push((c, b as u32, d));
                    }
                }
            }
            let owners: Vec<usize> = (0..self.segs.len()).filter(|&a| !need[a].is_empty()).collect();
            let per = par_claim(&owners, lanes(owners.len()), |&a| {
                let mut top = Top::new();
                let n = needed(&self.segs[a], plan, need[a].clone(), |b, d| admit(b, d).map(|r| top.push(r)).is_some());
                (top.done(), n)
            });
            let total = per.iter().map(|p| p.1).sum();
            return Found { per: per.into_iter().map(|p| p.0).collect(), total, dense: Vec::new() };
        }
        // Per segment owning contents docs under the scope hold, the ones
        // the plan selects. A few become their docs right away (many, in
        // chunks any lane takes). When every doc counts, a common pattern's
        // many in a segment (more than 1 in 8 of its contents, and FIRST or
        // more) are set in `dense` by gid instead; once every owner is done,
        // the segments holding them are walked best-ranked first, or find
        // their few by gid (see `decide`).
        use std::sync::atomic::{AtomicUsize, Ordering::*};
        let n = self.segs.len();
        let owners: Vec<usize> =
            (0..n).filter(|&a| self.segs[a].nown > 0 && self.users[a].iter().any(|&(b, _)| scope[b as usize].is_some())).collect();
        // (Made by the first owner that needs it.)
        let dense: std::sync::OnceLock<Vec<std::sync::atomic::AtomicU64>> = Default::default();
        let has_dense = |g: u32| dense.get().and_then(|d| d.get(g as usize / 64)).is_some_and(|w| w.load(Relaxed) >> (g % 64) & 1 != 0);
        // Per owner found in bulk: the contents, and how many.
        let bulk: Vec<std::sync::OnceLock<(Vec<u64>, usize)>> = (0..n).map(|_| Default::default()).collect();
        let (next, done, next_item, total) = (AtomicUsize::new(0), AtomicUsize::new(0), AtomicUsize::new(0), AtomicUsize::new(0));
        // Chunks to expand, and how many there are (checked before locking).
        let chunks: std::sync::Mutex<Vec<Chunk>> = Default::default();
        let queued = AtomicUsize::new(0);
        let out = std::sync::Mutex::new(Vec::new());
        // (Each lane gathers the docs it finds in its own `Top`.)
        let expand = |a: usize, sel: &[u32], top: &mut Top| {
            let at = top.len();
            if global {
                // No scope to check; the ranks of the segment's docs at hand.
                let mut seg = (usize::MAX, &[][..], &[][..]);
                self.holders(a, sel, |b, d| {
                    if b != seg.0 {
                        seg = (b, self.segs[b].rank(), self.segs[b].mtime());
                    }
                    top.push((rank_key(seg.1[d as usize], seg.2[d as usize]), b as u32, d));
                });
            } else {
                self.holders(a, sel, |b, d| {
                    if let Some(r) = admit(b, d) {
                        top.push(r)
                    }
                });
            }
            total.fetch_add(top.len() - at, Relaxed);
        };
        // A selection's docs: right away, or in chunks of about 1k docs (by
        // the docs per content here) when bigger.
        let give = |a: usize, sel: Vec<u32>, top: &mut Top| {
            let s = &self.segs[a];
            let docs: usize = self.users[a].iter().map(|&(_, w)| w as usize).sum();
            let step = (1024 * s.nown / docs.max(1)).max(64);
            if sel.len() <= step {
                return expand(a, &sel, top);
            }
            let sel = std::sync::Arc::new(sel);
            let mut q = chunks.lock().unwrap();
            q.extend((step..sel.len()).step_by(step).map(|at| (a, sel.clone(), at..(at + step).min(sel.len()))));
            queued.store(q.len(), Release);
            drop(q);
            expand(a, &sel[..step], top);
        };
        let select = |a: usize, top: &mut Top| {
            let s = &self.segs[a];
            // An owner outside the scope whose contents few docs under it
            // hold: only those contents.
            if scope[a].is_none() && !global {
                let mut need = Vec::new();
                for (b, t) in self.users[a].iter().map(|&(b, _)| b as usize).filter(|&b| scope[b].is_some()).map(|b| (b, &self.segs[b])) {
                    let (gs, by_gid) = (t.gid_sorted(), t.by_gid());
                    for &(c0, g0, m) in &s.runs {
                        let lo = gs.partition_point(|&x| x < g0);
                        let at = lo..lo + gs[lo..].partition_point(|&x| x < g0 + m);
                        need.extend(at.filter(|&i| !t.is_dead(by_gid[i])).map(|i| (c0 + gs[i] - g0, b as u32, by_gid[i])));
                    }
                    if need.len() > 4096 {
                        break;
                    }
                }
                if need.len() <= 4096 {
                    let n = needed(s, plan, need, |b, d| admit(b, d).map(|r| top.push(r)).is_some());
                    total.fetch_add(n, Relaxed);
                    return;
                }
            }
            let sel = match and_dense(s, plan) {
                Some((bits, k)) if global && k * 8 > s.nown && k >= FIRST => Err((bits, k)),
                Some((bits, k)) => Ok(set_bits(&bits, k)),
                None => match eval(s, plan, 0..s.nown as u32) {
                    Some(v) => Ok(v),
                    None if global => Err((all_bits(s.nown), s.nown)),
                    None => Ok((0..s.nown as u32).collect()),
                },
            };
            match sel {
                Ok(sel) => give(a, sel, top),
                Err((bits, k)) => {
                    let max_gid = self.gids.last().map_or(0, |r| r.0 + r.1) as usize;
                    let dense = dense.get_or_init(|| (0..max_gid.div_ceil(64)).map(|_| Default::default()).collect());
                    for &(c, g, m) in &s.runs {
                        or_bits(dense, g as usize, &bits, c as usize, m as usize);
                    }
                    // Every doc counts: as many as hold them.
                    total.fetch_add(s.refs.sum(&bits), Relaxed);
                    let _ = bulk[a].set((bits, k));
                }
            }
        };
        // Once every owner is done, the segments holding contents found in
        // bulk: each walked best-ranked first to its FIRST best when they make
        // it dense in candidates, or it holds many of one owner's (a walk
        // that doesn't stop early costs no more than finding those by gid);
        // else its few found by gid.
        let items: std::sync::OnceLock<Vec<(usize, bool)>> = Default::default();
        let decide = || {
            let held = (0..n).filter(|&b| scope[b].is_some() && self.sources[b].iter().any(|&(a, _)| bulk[a as usize].get().is_some()));
            held.map(|b| {
                let (mut from, mut many) = (0, false);
                for &(a, w) in &self.sources[b] {
                    if let Some(&(_, k)) = bulk[a as usize].get() {
                        from += w as usize * k / self.segs[a as usize].nown;
                        many |= w > 2048;
                    }
                }
                (b, from * 8 > self.segs[b].ndocs || many)
            })
            .collect()
        };
        let run = |&(b, walk): &(usize, bool), top: &mut Top| {
            let t = &self.segs[b];
            if walk {
                let (by_rank, gids) = (t.by_rank(), t.rank_gid());
                let mut best = Vec::with_capacity(FIRST);
                let mut i = 0;
                while i < by_rank.len() && best.len() < FIRST {
                    if has_dense(gids[i]) && !t.is_dead(by_rank[i]) {
                        best.push(ranked(t, b, by_rank[i]));
                    }
                    i += 1;
                }
                return out.lock().unwrap().push(Cands::Bulk { si: b, top: best, from: i });
            }
            // (The owners counted these.)
            let (gs, by_gid) = (t.gid_sorted(), t.by_gid());
            for &(a, _) in &self.sources[b] {
                let Some((bits, _)) = bulk[a as usize].get() else { continue };
                for &(c0, g0, m) in &self.segs[a as usize].runs {
                    let lo = gs.partition_point(|&x| x < g0);
                    for i in lo..lo + gs[lo..].partition_point(|&x| x < g0 + m) {
                        if has(bits, c0 + gs[i] - g0) && !t.is_dead(by_gid[i]) {
                            top.push(ranked(t, b, by_gid[i]));
                        }
                    }
                }
            }
        };
        // Each lane takes owners first; then chunks and, once every owner is
        // done, what is left.
        let work = || {
            let mut top = Top::new();
            loop {
                if next.load(Relaxed) < owners.len() {
                    if let Some(&a) = owners.get(next.fetch_add(1, Relaxed)) {
                        select(a, &mut top);
                        done.fetch_add(1, Release);
                    }
                    continue;
                }
                let items = (done.load(Acquire) == owners.len()).then(|| items.get_or_init(decide));
                if let Some(item) = items.and_then(|items| items.get(next_item.fetch_add(1, Relaxed))) {
                    run(item, &mut top);
                    continue;
                }
                if queued.load(Acquire) > 0 {
                    let chunk = {
                        let mut q = chunks.lock().unwrap();
                        let c = q.pop();
                        queued.store(q.len(), Release);
                        c
                    };
                    if let Some((a, sel, r)) = chunk {
                        expand(a, &sel[r], &mut top);
                    }
                    continue;
                }
                if items.is_some() {
                    break;
                }
                std::thread::yield_now();
            }
            if top.len() > 0 {
                let c = top.done();
                out.lock().unwrap().push(c);
            }
        };
        // A lane that panicked here lost its share of the postings; the
        // index is read from the disk again on the next sync.
        let _ = run_lanes((docs / 25_000).clamp(1, 8), &work);
        let per = out.into_inner().unwrap();
        let dense = dense.into_inner().map_or(Vec::new(), |d| d.into_iter().map(|w| w.into_inner()).collect());
        Found { per, total: total.into_inner(), dense }
    }

    pub fn search(&self, g: &Grep, filt: &Query) -> GrepResult {
        self.search_covered(g, filt, None)
    }

    fn search_covered(&self, g: &Grep, filt: &Query, covered: Option<&std::collections::HashSet<Vec<u8>>>) -> GrepResult {
        let Found { mut per, total, dense } = self.candidates(&g.plan(), filt);
        // A candidate whose content's bloom filter lacks one of the pattern's
        // grams can't match: skip it without opening the file. When `plan`
        // already split the scan into `covered` and `bare`, a doc outside
        // `covered` belongs to `bare` and is skipped here too.
        let probes = g.probes();
        let path = |&(_, si, d): &Ranked| {
            let s = &self.segs[si as usize];
            let p = s.path(d);
            if covered.is_some_and(|cov| !cov.contains(p)) {
                return None;
            }
            let (a, c) = self.content_of(s.doc_gid()[d as usize])?;
            self.segs[a].may_contain(c, &probes).then_some(p)
        };
        // Most searches are done within the best few hundred candidates:
        // rank those first, and the rest only if reading gets that far.
        let t = std::time::Instant::now();
        let best = take_best(&mut per);
        let (mut r, mut checked) =
            verify_from(g, best.len(), filt.limit, READERS, t, |i| path(&best[i]), |p| match_file(g, p, &placeholder::attributes, None));
        if r.files.len() < filt.limit && checked == best.len() && best.len() < total {
            // Then the next best few thousand, and the others only if
            // reading gets past those too.
            let mut rest: Vec<Ranked> = par_claim(&per, per.len().min(8), |c| c.rest(&self.segs, &dense)).concat();
            let next = rest.len().min(8 * FIRST);
            if next < rest.len() {
                rest.select_nth_unstable(next);
            }
            rest[..next].sort_unstable();
            let mut at = 0;
            for end in [next, rest.len()] {
                if at == end {
                    continue;
                }
                if at > 0 {
                    rgy_par::sort_unstable(&mut rest[at..]);
                }
                let (more, done) = verify_from(
                    g,
                    end - at,
                    filt.limit - r.files.len(),
                    READERS,
                    t,
                    |i| path(&rest[at + i]),
                    |p| match_file(g, p, &placeholder::attributes, None),
                );
                checked += done;
                r.files.extend(more.files);
                r.read += more.read;
                r.skipped.extend(more.skipped);
                r.complete = more.complete;
                if r.error.is_none() {
                    r.error = more.error;
                }
                if r.files.len() >= filt.limit || done < end - at {
                    break;
                }
                at = end;
            }
        }
        r.complete = checked == total || r.files.len() >= filt.limit;
        // `checked - r.read` candidates were ruled out in memory without
        // opening a file; subtracting them leaves `r.candidates - r.read`
        // equal to `total - checked`, the candidates a spent budget never
        // reached.
        r.candidates = total.saturating_sub(checked.saturating_sub(r.read));
        r
    }

    /// Does this index vouch for `path`, so a scan need not read it? True
    /// only when it holds a live doc for `path` whose recorded size and
    /// mtime still match disk (so its grams are current) and the name query
    /// lets that doc through (so [`Self::search`] evaluates it and either
    /// returns it or rules it out). Everything else -- a file under a
    /// skip-dir or past the curation [`eligible`] applies, one edited since
    /// it was indexed, one no doc names -- returns false and is read from
    /// disk instead. `root` must be spelled as the index was built with, so
    /// a doc's stored path and `path` compare equal.
    pub fn covers(&self, root: &[u8], path: &[u8], home: &[u8], q: &Query) -> bool {
        let _ = (root, home);
        let Some(st) = crate::live::lstat(path) else {
            return false;
        };
        // A name that would not reach `search` is not vouched for either:
        // `search` would drop it, and the scan must then read it here.
        if q.match_path(path, KIND_FILE, st.size, st.mtime).is_none() {
            return false;
        }
        self.segs.iter().any(|s| {
            let bp = s.by_path();
            let i = bp.partition_point(|&d| s.path(d) < path);
            i < bp.len() && s.path(bp[i]) == path && {
                let d = bp[i];
                !s.is_dead(d) && s.size()[d as usize] == st.size && s.mtime()[d as usize] == st.mtime
            }
        })
    }
}

/// Part of an owner's selection to find the docs of: the owner, the
/// selection, and the part.
type Chunk = (usize, std::sync::Arc<Vec<u32>>, std::ops::Range<usize>);

/// A search's candidates: in groups, how many in all, and the gids of the
/// contents found in bulk.
struct Found {
    per: Vec<Cands>,
    total: usize,
    dense: Vec<u64>,
}

/// Is gid `g` set in `bits`?
#[inline]
fn has(bits: &[u64], g: u32) -> bool {
    bits.get(g as usize / 64).is_some_and(|w| w >> (g % 64) & 1 != 0)
}

/// Of the docs (`need`: content, segment, doc) holding contents of `s`, give
/// `f` those whose contents the plan selects (evaluated over the range they
/// span), and return how many it took.
fn needed(s: &Segment, plan: &TQ, mut need: Vec<(u32, u32, u32)>, mut f: impl FnMut(usize, u32) -> bool) -> usize {
    if need.is_empty() {
        return 0;
    }
    need.sort_unstable();
    let (lo, hi) = (need[0].0, need[need.len() - 1].0 + 1);
    let sel = eval(s, plan, lo..hi).unwrap_or_else(|| (lo..hi).collect());
    let (mut i, mut n) = (0, 0);
    for &(c, b, d) in &need {
        i = gallop(&sel, i, c);
        n += (sel.get(i) == Some(&c) && f(b as usize, d)) as usize;
    }
    n
}

/// The first position at or after `lo` where ascending `a` holds `x` or
/// more: exponential steps, then a binary search.
fn gallop(a: &[u32], lo: usize, x: u32) -> usize {
    if lo >= a.len() || a[lo] >= x {
        return lo;
    }
    let (mut lo, mut step) = (lo, 1);
    while lo + step < a.len() && a[lo + step] < x {
        lo += step;
        step *= 2;
    }
    let hi = (lo + step).min(a.len());
    lo + 1 + a[lo + 1..hi].partition_point(|&y| y < x)
}

/// Bits `from..from + n` of `src`, or'd into `dst` at `at`.
fn or_bits(dst: &[std::sync::atomic::AtomicU64], at: usize, src: &[u64], from: usize, n: usize) {
    use std::sync::atomic::Ordering::Relaxed;
    let mut k = 0;
    while k < n {
        let (s, t, take) = (from + k, at + k, (n - k).min(64));
        let mut w = src[s / 64] >> (s % 64);
        if s % 64 != 0 && s / 64 + 1 < src.len() {
            w |= src[s / 64 + 1] << (64 - s % 64);
        }
        if take < 64 {
            w &= (1 << take) - 1;
        }
        if w != 0 {
            dst[t / 64].fetch_or(w << (t % 64), Relaxed);
            if t % 64 != 0 && w >> (64 - t % 64) != 0 {
                dst[t / 64 + 1].fetch_or(w >> (64 - t % 64), Relaxed);
            }
        }
        k += take;
    }
}

/// All of `n` bits set.
fn all_bits(n: usize) -> Vec<u64> {
    let mut bits = vec![u64::MAX; n.div_ceil(64)];
    if let Some(w) = bits.last_mut().filter(|_| !n.is_multiple_of(64)) {
        *w = (1 << (n % 64)) - 1;
    }
    bits
}

/// A candidate's place in the ranking, packed so sorting compares ints: your
/// files before dot-dirs/logs, then most recently modified first; ties in
/// segment, then doc order. Then its segment and doc.
type Ranked = (u64, u32, u32);

/// The first part of `Ranked`.
fn rank_key(rank: i8, mtime: u32) -> u64 {
    (((127 - rank as i32) as u64) << 32) | (u32::MAX - mtime) as u64
}

/// Candidates ranked in the first round.
const FIRST: usize = 512;

/// Candidates gathered one at a time, keeping the `FIRST` best apart: below
/// `bar` (the best seen past them) they go to `best`, which is cut back to
/// `FIRST` whenever it reaches 8 times that; the others go to `rest`.
struct Top {
    best: Vec<Ranked>,
    rest: Vec<Ranked>,
    bar: Ranked,
}

impl Top {
    fn new() -> Top {
        Top { best: Vec::new(), rest: Vec::new(), bar: (u64::MAX, u32::MAX, u32::MAX) }
    }

    #[inline]
    fn push(&mut self, r: Ranked) {
        if r >= self.bar {
            return self.rest.push(r);
        }
        self.best.push(r);
        if self.best.len() == 8 * FIRST {
            self.best.select_nth_unstable(FIRST);
            self.bar = self.best[FIRST];
            self.rest.extend(self.best.drain(FIRST..));
        }
    }

    fn len(&self) -> usize {
        self.best.len() + self.rest.len()
    }

    fn done(mut self) -> Cands {
        if self.best.len() > FIRST {
            self.best.select_nth_unstable(FIRST);
            self.rest.extend(self.best.drain(FIRST..));
        }
        Cands::Two(self.best, self.rest)
    }
}

/// A group of candidates.
enum Cands {
    /// All of them, the `FIRST` best in front (unordered).
    List(Vec<Ranked>),
    /// The `FIRST` best (unordered), and the others.
    Two(Vec<Ranked>, Vec<Ranked>),
    /// A segment's docs (for a pattern with nothing to look up): `top`, its
    /// `FIRST` best (in order), and the rest, set in `bits`.
    Walk { si: usize, bits: Vec<u64>, top: Vec<Ranked> },
    /// A segment's docs holding a content found in bulk: `top`, its `FIRST`
    /// best (in order), then the rest, in rank order from `from` on.
    Bulk { si: usize, top: Vec<Ranked>, from: usize },
}

impl Cands {
    /// A list, its `FIRST` best in front.
    fn list(mut v: Vec<Ranked>) -> Cands {
        if v.len() > FIRST {
            v.select_nth_unstable(FIRST);
        }
        Cands::List(v)
    }

    /// What `take_best` left, unordered (`dense`: the gids found in bulk).
    fn rest(&self, segs: &[Segment], dense: &[u64]) -> Vec<Ranked> {
        let ranked = |s: &Segment, si: usize, d: u32| (rank_key(s.rank()[d as usize], s.mtime()[d as usize]), si as u32, d);
        match self {
            Cands::List(v) => v.clone(),
            Cands::Two(_, rest) => rest.clone(),
            Cands::Walk { si, bits, .. } => set_bits(bits, 0).into_iter().map(|d| ranked(&segs[*si], *si, d)).collect(),
            Cands::Bulk { si, from, .. } => {
                let s = &segs[*si];
                let (by_rank, gids) = (s.by_rank(), s.rank_gid());
                (*from..by_rank.len()).filter(|&i| has(dense, gids[i]) && !s.is_dead(by_rank[i])).map(|i| ranked(s, *si, by_rank[i])).collect()
            }
        }
    }
}

/// The docs set in `bits` (`n` of them), ascending.
fn set_bits(bits: &[u64], n: usize) -> Vec<u32> {
    let mut ids = Vec::with_capacity(n);
    for (w, &x) in bits.iter().enumerate() {
        let mut x = x;
        while x != 0 {
            ids.push(w as u32 * 64 + x.trailing_zeros());
            x &= x - 1;
        }
    }
    ids
}

/// `items.iter().map(f).collect()` on `lanes` lanes (see `run_lanes`): each
/// claims the next item.
fn par_claim<T: Sync, R: Send>(items: &[T], lanes: usize, f: impl Fn(&T) -> R + Sync) -> Vec<R> {
    use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};
    let next = AtomicUsize::new(0);
    let out = std::sync::Mutex::new(Vec::with_capacity(items.len()));
    let _ = run_lanes(lanes, &|| {
        let mut mine = Vec::new();
        loop {
            let i = next.fetch_add(1, Relaxed);
            let Some(x) = items.get(i) else { break };
            mine.push((i, f(x)));
        }
        out.lock().unwrap().extend(mine);
    });
    let mut out = out.into_inner().unwrap();
    out.sort_unstable_by_key(|r| r.0);
    out.into_iter().map(|r| r.1).collect()
}

/// One search's lanes, shared with the helper threads.
struct Lanes {
    n: usize,
    next: std::sync::atomic::AtomicUsize,
    done: std::sync::atomic::AtomicUsize,
    panicked: std::sync::atomic::AtomicBool,
    /// The search's job, its lifetime erased: it is only run for a lane
    /// claimed below `n`, and `run_lanes` returns after all of those end.
    job: *const (dyn Fn() + Sync),
}

// Safety: `job` is Sync and outlives every call (see the field).
unsafe impl Send for Lanes {}
unsafe impl Sync for Lanes {}

impl Lanes {
    fn work(&self) {
        use std::sync::atomic::Ordering::*;
        while self.next.fetch_add(1, Relaxed) < self.n {
            let job = std::panic::AssertUnwindSafe(|| unsafe { (*self.job)() });
            if std::panic::catch_unwind(job).is_err() {
                self.panicked.store(true, Relaxed);
            }
            self.done.fetch_add(1, Release);
        }
    }
}

/// Threads that help searches: the latest search's lanes, and a bell.
struct Helpers {
    latest: std::sync::Mutex<(u64, Option<std::sync::Arc<Lanes>>)>,
    bell: std::sync::Condvar,
}

fn helpers() -> &'static Helpers {
    static H: std::sync::OnceLock<&'static Helpers> = std::sync::OnceLock::new();
    H.get_or_init(|| {
        let h: &'static Helpers = Box::leak(Box::new(Helpers { latest: Default::default(), bell: Default::default() }));
        let n = std::thread::available_parallelism().map_or(1, |n| n.get()).max(SCAN_READERS);
        for i in 1..n {
            let spawned = std::thread::Builder::new().name(format!("fsearch-help-{i}")).spawn(move || {
                // Someone is waiting: keep off the slow cores and out of the
                // throttled IO tiers, and never download iCloud placeholders.
                // macOS QoS: keep reader helpers off the slow cores.
                set_qos_user_initiated();
                crate::no_materialize();
                let mut seen = 0;
                loop {
                    let lanes = {
                        let mut g = h.latest.lock().unwrap();
                        while g.0 == seen {
                            g = h.bell.wait(g).unwrap();
                        }
                        seen = g.0;
                        g.1.clone()
                    };
                    if let Some(l) = lanes {
                        l.work();
                    }
                }
            });
            if spawned.is_err() {
                break;
            }
        }
        h
    })
}

/// Run `job` on `n` lanes at once, this thread taking lanes too, and return
/// when all are done. A lane goes to whichever thread claims it first, so
/// this never waits on a helper still waking up (~0.1 ms when they sleep):
/// one that wakes late finds no lane left.
///
/// Whether a lane panicked is returned, not asserted. The panic is caught
/// where it happens — the lane's share of the work is lost — and the caller
/// has to say so in the answer: the assert that used to be here re-raised a
/// panic that had already been caught, in the caller's thread, and a daemon
/// that answers on its main thread took the whole process down with it.
fn run_lanes(n: usize, job: &(dyn Fn() + Sync)) -> bool {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering::*};
    if n <= 1 {
        // One lane: the panic is caught here too, and the answer says so.
        return std::panic::catch_unwind(std::panic::AssertUnwindSafe(job)).is_err();
    }
    let h = helpers();
    let job: *const (dyn Fn() + Sync + '_) = job;
    // Safety: see `Lanes::job`; the wait below is what makes it hold.
    let job: *const (dyn Fn() + Sync + 'static) = unsafe { std::mem::transmute(job) };
    let lanes = std::sync::Arc::new(Lanes { n, next: AtomicUsize::new(0), done: AtomicUsize::new(0), panicked: AtomicBool::new(false), job });
    {
        let mut g = h.latest.lock().unwrap();
        g.0 += 1;
        g.1 = Some(lanes.clone());
    }
    for _ in 1..n {
        h.bell.notify_one();
    }
    lanes.work();
    while lanes.done.load(Acquire) < n {
        std::thread::yield_now();
    }
    lanes.panicked.load(Relaxed)
}

/// Take the `FIRST` best-ranked candidates out of the segments', in order.
fn take_best(per: &mut Vec<Cands>) -> Vec<Ranked> {
    let mut top = Vec::new();
    for c in per.iter_mut() {
        match c {
            Cands::List(v) => top.extend(v.drain(..v.len().min(FIRST))),
            Cands::Two(best, _) => top.append(best),
            Cands::Walk { top: t, .. } | Cands::Bulk { top: t, .. } => top.append(t),
        }
    }
    if top.len() > FIRST {
        top.select_nth_unstable(FIRST);
        per.push(Cands::List(top.split_off(FIRST)));
    }
    top.sort_unstable();
    top
}

/// What the name index says should be indexed under `dir` (direct
/// children only unless `recursive`).
///
/// The paths come from the name index and are **relative to its root**;
/// `build_segment` opens what it is given, so they are not openable as they
/// stand. Join the scan root onto them before building, the way
/// [`scan_paths`] does for the other way in. Handed straight to
/// `build_segment` they read as absent, the segment ends up with no grams,
/// and the indexed search answers nothing -- a query that finds a match
/// through `verify` will find none through [`Content::search`] and say so
/// with the same confidence.
/// The path a doc is opened by: `root` joined onto the name index's
/// root-relative path.
///
/// One place for this, because the name index and the content index are two
/// halves of one thing and `build_segment` opens whatever it is handed. A
/// relative path there reads as absent, the segment is built holding no
/// grams, and the indexed search answers nothing -- while `verify` over the
/// same corpus finds the match. Two answers to one question, both sure.
///
/// `scan_paths` joins the same way for the other direction, so the paths
/// [`Content::search`] returns and the ones `verify` reads are the same
/// strings.
fn openable(root: &[u8], rel: &[u8]) -> Vec<u8> {
    let rel = rel.strip_prefix(b"/").unwrap_or(rel);
    // Normalize the separators: the root arrives as the caller spelled it (`\`
    // on Windows, from `to_string_lossy`), and the rel is `/`-separated. Every
    // path this index stores or compares is the `/` form, so a feed that hands
    // back a `\` path (the overlay keys from `apply_dir`) matches here too.
    let mut full: Vec<u8> = root.iter().map(|&b| if b == b'\\' { b'/' } else { b }).collect();
    if !full.is_empty() && !full.ends_with(b"/") && !rel.is_empty() {
        full.push(b'/');
    }
    full.extend_from_slice(rel);
    full
}

/// The name-index path of the file [`openable`] names: `openable` read
/// backwards. `None` when `full` is not under `root` at all -- an overlay
/// entry outside the tree is no name index's business.
fn rel_of(root: &[u8], full: &[u8]) -> Option<Vec<u8>> {
    // Normalize separators to `/` on both sides before the prefix compare:
    // `root` is spelled by the caller (`\` on Windows) while `full` may come
    // back from a feed already normalized, and `openable` stores the `/` form.
    // A byte-wise `strip_prefix` across the two spellings finds nothing.
    let norm = |s: &[u8]| s.iter().map(|&b| if b == b'\\' { b'/' } else { b }).collect::<Vec<u8>>();
    let (nroot, nfull) = (norm(root), norm(full));
    let rest = nfull.strip_prefix(&nroot[..])?;
    if rest.is_empty() {
        // `full` is `root` itself: the tree's own rel is `/`.
        return Some(b"/".to_vec());
    }
    let rel = match rest.split_first() {
        // `openable` puts exactly one separator between root and rel.
        Some((b'/', tail)) if !nroot.ends_with(b"/") => tail,
        Some(_) if nroot.ends_with(b"/") => rest,
        // A sibling spelling (`/a/b2` under `/a`): not under `root`.
        _ => return None,
    };
    let mut p = Vec::with_capacity(rel.len() + 1);
    p.push(b'/');
    p.extend_from_slice(rel);
    Some(p)
}

/// The prefix whose `/`-children are `dir`'s: `dir` with a trailing
/// separator. Segment doc paths are [`openable`] names (`root`, `/`, rel),
/// so matching one by prefix needs that separator spelled the same way on
/// both sides -- `join(dir, b"")` adds one even where `dir` already ends in
/// one, and the doubled separator matches no doc at all (every such doc is
/// then re-indexed on each sync while the old one is never killed).
fn dir_prefix(dir: &[u8]) -> Vec<u8> {
    if dir.is_empty() || dir.ends_with(b"/") {
        dir.to_vec()
    } else {
        let mut p = Vec::with_capacity(dir.len() + 1);
        p.extend_from_slice(dir);
        p.push(b'/');
        p
    }
}

/// What the name index says should be indexed under `dir` (direct
/// children only unless `recursive`), as docs [`build_segment`] can open.
///
/// The paths the name index holds are relative to its root; `root` is the
/// on-disk tree they hang off, and the docs carry `root` joined on. Overlay
/// entries are already real paths and are pushed as they are -- the same
/// distinction [`scan_paths`] makes.
pub fn wanted(live: &Live, home: &[u8], dir: &[u8], recursive: bool, root: &[u8]) -> Docs {
    let mut want = Docs::default();
    if !in_scope(dir, home) {
        return want;
    }
    let idx = &live.base;
    let mut p = Vec::new();
    if let Some(d) = idx.lookup(dir).filter(|&e| !live.is_dead(e)).and_then(|e| idx.dir_of(e)) {
        let range = if recursive { idx.dir_start()[d as usize] as usize..idx.dir_end()[d as usize] as usize } else { idx.children(d) };
        for i in range {
            if idx.kind()[i] & 3 != KIND_FILE || live.is_dead(i as u32) || !name_ok(idx.name(i), idx.size_of(i)) {
                continue;
            }
            if recursive {
                idx.path(i, &mut p);
            } else {
                p = join(dir, idx.name(i));
            }
            if in_scope(&p, home) {
                let dp = openable(root, &p);
                // The overlay supersedes the base for a path it holds (a file
                // changed since the snapshot): its size/mtime are current and
                // the base's are stale. Skipping here, and letting the overlay
                // half below add it, keeps `want` from naming the same file
                // twice -- once stale, once fresh -- which `diff` then reads as
                // two docs: it kills the fresh one as "stale" and rebuilds the
                // stale one, so an edit never reaches a query.
                if live.over.contains_key(&dp) {
                    continue;
                }
                want.push(&dp, idx.size_of(i), idx.mtime()[i]);
            }
        }
    }
    // The overlay holds on-disk paths (the same distinction `scan_paths`
    // makes), so its range is taken in that space -- `dir` is the name
    // index's, and a prefix matched across the two finds nothing (every
    // overlay entry is then missed, or on a `/` root every real path is
    // taken for one). `rel_of` judges each key back in the name index's
    // space, the same test the base half applies to `idx.path` output.
    let lo = dir_prefix(&openable(root, dir));
    for (k, o) in live.over.range(lo.clone()..).take_while(|(k, _)| k.starts_with(&lo)) {
        if o.kind & 3 != KIND_FILE || (!recursive && k[lo.len()..].contains(&b'/')) {
            continue;
        }
        // Outside `root` the name index has no opinion, and neither does
        // this index: those are `scan_paths`'s files to read.
        if let Some(rel) = rel_of(root, k)
            && eligible(&rel, o.size, home)
        {
            want.push(k, o.size, o.mtime);
        }
    }
    want
}

/// The dirs/trees from one batch of changes, each with what should be
/// indexed there. Done under the name-index read lock only.
///
/// `dirs` and `trees` are on-disk paths -- what [`Self::diff`]'s doc
/// prefixes, [`Self::changed_dirs`] and `Live::trees` all speak -- while
/// [`wanted`] asks the name index in its own root-relative paths and
/// answers with on-disk docs. [`rel_of`] is the one crossing: a dir looked
/// up in the name index untranslated finds nothing, and the whole subtree
/// is then "indexed" on every batch (the old doc is never killed, because
/// `diff` matches its prefixes in the other space).
pub fn wants(live: &Live, home: &[u8], dirs: &[Vec<u8>], trees: &[Vec<u8>], root: &[u8]) -> Vec<(Vec<u8>, bool, Docs)> {
    let mut out: Vec<(Vec<u8>, bool)> = Vec::new();
    for (d, r) in dirs.iter().map(|d| (d, false)).chain(trees.iter().map(|d| (d, true))) {
        let Some(rel) = rel_of(root, d) else {
            // Not under `root`: nothing of it is this index's to hold.
            continue;
        };
        if in_scope(&rel, home) {
            out.push((d.clone(), r));
        } else if r && home.starts_with(&rel) {
            // A subtree containing home (e.g. "/" rescanned): sync all of home.
            out.push((openable(root, home), true));
        }
    }
    out.sort();
    out.dedup();
    out.into_iter()
        .map(|(d, r)| {
            let rel = rel_of(root, &d).unwrap_or_else(|| b"/".to_vec());
            let mut w = wanted(live, home, &rel, r, root);
            w.sort();
            (d, r, w)
        })
        .collect()
}

/// A pattern's gram hashes, as `may_contain` checks them.
struct Probes {
    short: Vec<u32>,
    long: Vec<u32>,
}

pub struct Grep {
    pub pattern: String,
    pub mode: GrepMode,
    pub max_per_file: usize,
    /// Stop reading candidates after this long (None = read them all).
    pub budget: Option<std::time::Duration>,
    /// The matching regex on the rgx engine, compiled when first needed (a
    /// plain ASCII literal mostly is not: the literal path finds those).
    re: std::sync::OnceLock<Box<ByteRegex>>,
    src: String,
    case_insensitive: bool,
    literal: Option<Literal>,
}

impl Grep {
    pub fn new(pattern: &str, mode: GrepMode) -> Result<Grep, String> {
        let smart_ci = !pattern.chars().any(|c| c.is_uppercase());
        let src = match mode {
            GrepMode::Literal => re_escape(pattern),
            GrepMode::Regex => pattern.to_string(),
            // A definition: a declaring keyword, optional generics/modifiers,
            // then the name. ASCII word boundaries keep the regex on the fast
            // DFA path even in files with non-ASCII text.
            GrepMode::Symbol => format!(r"(?-u:\b)(?:{DEFINES})(?:<[^>\n]*>)?[ \t*&]+(?:mut[ \t]+)?{}(?-u:\b)", re_escape(pattern)),
        };
        let case_insensitive = smart_ci && mode != GrepMode::Symbol;
        let short = (1..=4096).contains(&pattern.len());
        let literal = match mode {
            GrepMode::Symbol if short => Some(Literal::Definition(pattern.as_bytes().to_vec())),
            GrepMode::Literal if short && pattern.is_ascii() => Some(match case_insensitive {
                true => Literal::folded(pattern.as_bytes()),
                false => Literal::Exact(pattern.as_bytes().to_vec()),
            }),
            _ => None,
        };
        let g = Grep {
            pattern: pattern.to_string(),
            mode,
            max_per_file: 5,
            budget: Some(std::time::Duration::from_millis(250)),
            re: std::sync::OnceLock::new(),
            src,
            case_insensitive,
            literal,
        };
        if g.literal.is_none() {
            g.re.set(g.build()?).ok();
        }
        Ok(g)
    }

    /// The confirm matcher: the engine, compiled.
    ///
    /// The engine is a byte engine, and it refuses what a byte engine has no
    /// answer for: lookaround, and the Unicode property classes. The refusal
    /// is the answer -- the pattern is named and the reason is given --
    /// because a search that quietly matched nothing is
    /// indistinguishable from a search that found nothing.
    fn build(&self) -> Result<Box<ByteRegex>, String> {
        ByteRegex::new_multiline(self.src.as_bytes(), self.case_insensitive, false)
            .map(Box::new)
            .map_err(|e| format!("the pattern `{}`: {e}", self.pattern))
    }

    fn re(&self) -> &ByteRegex {
        // A literal's regex always compiles (it was 4096 bytes at most), and
        // a pattern the engine refuses never reaches here: `new` refused it.
        self.re.get_or_init(|| self.build().expect("the pattern compiles"))
    }

    /// The matching lines of a buffer the caller already has.
    ///
    /// The adapter path has the rendered text in hand — a PDF's words, an
    /// archive member's content — and needs the same confirmation the file
    /// path uses. This is that confirmation, over bytes that are not a
    /// file: the same matcher, the same line numbering, the same
    /// `max_per_file` cap.
    pub fn lines_in(&self, buf: &[u8]) -> Vec<(usize, String)> {
        let starts: Vec<usize> = match self.literal.as_ref().filter(|l| !l.needs_regex(buf)) {
            Some(l) => l.starts(buf).collect(),
            None => rgx_starts(self.re(), buf),
        };
        lines_at(self, buf, starts.into_iter())
    }

    /// Hashes of the grams every match holds (see `SHORT`).
    fn probes(&self) -> Probes {
        let grams = |n: usize| {
            let mut out: Vec<u32> = if self.mode == GrepMode::Regex {
                Vec::new()
            } else {
                self.pattern.as_bytes().windows(n).map(|w| gram_hash(w.iter().fold(0, |g, &b| g << 8 | fold(b) as u64))).collect()
            };
            out.sort_unstable();
            out.dedup();
            out
        };
        Probes { short: grams(SHORT), long: grams(LONG) }
    }

    fn plan(&self) -> TQ {
        match self.mode {
            // Exactly the docs that define it (plus rare hash collisions,
            // which reading the file weeds out).
            GrepMode::Symbol if plain_identifier(self.pattern.as_bytes()) => TQ::Tri(symbol_key(self.pattern.as_bytes())),
            GrepMode::Literal | GrepMode::Symbol => literal_plan(self.pattern.as_bytes()),
            GrepMode::Regex => {
                // The plan comes from the same parse the confirm is compiled
                // from. A pattern the parser refuses has no AST here, and
                // then the plan is "everything" -- though a refused pattern
                // never reaches this far: `new` refused it first.
                let flags = rgx_core::syntax::Flags { case_insensitive: self.case_insensitive, ..Default::default() };
                rgx_core::syntax::parse_with(self.pattern.as_bytes(), flags).map_or(TQ::All, |p| regex_plan(&p.ast))
            }
        }
    }
}

#[derive(Default)]
pub struct GrepResult {
    pub files: Vec<FileMatches>,
    pub candidates: usize,
    pub read: usize,
    /// False if the time budget ran out before every candidate was read.
    pub complete: bool,
    /// The files that were not read, and why.
    pub skipped: Vec<Skipped>,
    /// Why the search failed, when it failed.
    ///
    /// A lane that panicked is caught where it happens — the lane's share
    /// of the work is lost — and the answer says so here: a search that
    /// lost a lane is not the search that was asked for, and the reader
    /// has to know which one it got. The panic used to be re-raised in
    /// the caller's thread, and the daemon answers on its main thread.
    pub error: Option<String>,
}

/// A file the search did not read, and why.
///
/// The reason is the part that makes it actionable: "skipped" alone leaves
/// the user guessing whether the file was empty, locked, or never there.
///
/// The reasons are a fixed vocabulary -- one string per kind of refusal --
/// because the wire groups the files by them: a tree with a thousand locked
/// files is one thing that happened, and a thousand entries would be a
/// thousand lines of wire for one fact. The kinds are:
///
/// * [`SKIP_UNOPENABLE`]: the open failed;
/// * [`SKIP_NOT_A_FILE`]: it is not a regular file;
/// * [`SKIP_READ_FAILED`]: the read failed;
/// * [`placeholder::PLACEHOLDER_REASON`]: it is a cloud placeholder, and
///   reading it would download it.
///
/// A file that was read and holds no match is not in this list: it was
/// searched, and it holds nothing.
pub struct Skipped {
    /// The file's path, as the index spells it.
    pub path: Vec<u8>,
    /// Why it was not read.
    pub reason: &'static str,
}

/// Why a file the search could have read was not read: the open failed.
///
/// The open fails for the reasons every open fails for: the file is locked
/// by another process, this process may not read it, or it is gone. One
/// string covers them all -- the wire groups by the string, and a group of
/// three causes is still one thing that happened.
pub const SKIP_UNOPENABLE: &str = "it could not be opened; it is locked, unreadable, or gone";

/// Why a file that is not a regular file was not read.
///
/// A folder, a pipe, a socket, a device: the search reads files, and this
/// is not one. On Windows the open of a folder is refused before the
/// question is asked -- a folder needs `FILE_FLAG_BACKUP_SEMANTICS`, which
/// the search does not pass -- so there the reason is [`SKIP_UNOPENABLE`].
pub const SKIP_NOT_A_FILE: &str = "it is not a regular file; it is a folder, a pipe, or a device";

/// Why a file that was open was not read: the read itself failed.
///
/// The file was there and the open succeeded, and then the bytes did not
/// come -- a disk that failed, a share that went away. The search could
/// have read it and did not, so it is a hole, and the user is told.
pub const SKIP_READ_FAILED: &str = "the read failed; the file could not be read to the end";

/// Say on the standard error what was not read, and why.
///
/// A file skipped for a reason nobody can see is a file the user believes
/// was searched. The CLI and the daemon do not carry the skipped list in
/// their own output -- the daemon's protocol has no field for it -- so this
/// note is where the user sees it: the count, the reason, and the first
/// path. One line, so a whole OneDrive tree of placeholders does not bury
/// the results.
fn note_skipped(skipped: &[Skipped]) {
    let Some(first) = skipped.first() else { return };
    let n = skipped.len();
    std::eprintln!("rgy: skipped {n} file{} — {} (first: {})", if n == 1 { "" } else { "s" }, first.reason, String::from_utf8_lossy(&first.path));
}

/// Threads reading candidates (the searching one and helpers): file opens on
/// this Mac stop scaling past ~4 (Endpoint Security clients tax every open;
/// measured on hot files: 7.5 us/file at 4 threads, 9 at 8, 14 at 12).
const READERS: usize = 4;
/// Folders outside the index (`in:/etc`) are read with more threads: their
/// files are mostly small and not in the page cache, so reads wait on the
/// disk more than on the open() tax.
const SCAN_READERS: usize = 8;

/// Run `f` with this thread never downloading iCloud placeholders (as the
/// helper threads), then restore its policy. The policy dance is macOS's
/// `setiopolicy_np`; elsewhere the call is just `f`.
#[cfg(target_os = "macos")]
fn without_materializing<T>(f: impl FnOnce() -> T) -> T {
    unsafe extern "C" {
        fn getiopolicy_np(iotype: i32, scope: i32) -> i32;
        fn setiopolicy_np(iotype: i32, scope: i32, policy: i32) -> i32;
    }
    // IOPOL_TYPE_VFS_MATERIALIZE_DATALESS_FILES, IOPOL_SCOPE_THREAD
    let prior = unsafe { getiopolicy_np(3, 1) };
    crate::no_materialize();
    let r = f();
    if prior >= 0 {
        unsafe { setiopolicy_np(3, 1, prior) };
    }
    r
}

/// macOS QoS for the reader helpers: keep them off the slow cores and out
/// of the throttled IO tiers. Declared inline like `without_materializing`
/// above — name-core carries no libc dependency.
#[cfg(target_os = "macos")]
fn set_qos_user_initiated() {
    unsafe extern "C" {
        fn pthread_set_qos_class_self_np(cls: u32, relative_priority: u32) -> i32;
    }
    // Darwin qos.h: QOS_CLASS_USER_INITIATED = 0x19.
    unsafe {
        pthread_set_qos_class_self_np(0x19, 0);
    }
}

#[cfg(not(target_os = "macos"))]
fn set_qos_user_initiated() {}

#[cfg(not(target_os = "macos"))]
fn without_materializing<T>(f: impl FnOnce() -> T) -> T {
    crate::no_materialize();
    f()
}

pub struct FileMatches {
    pub path: Vec<u8>,
    pub lines: Vec<(usize, String)>,
}

/// One document an adapter rendered for a file.
///
/// An archive holds more than one document: its members are documents of
/// their own, and each carries its own path -- `archive.zip!inner.txt` --
/// because that is the path a query asks about.
pub struct Rendered {
    /// The path of the document: the file itself, or a member inside it.
    pub path: Vec<u8>,
    /// The bytes the adapter produced.
    pub bytes: Vec<u8>,
}

/// The reader for the files the plain path cannot read.
///
/// The content path reads a file's own bytes. A file the format adapters
/// claim has to be read through the adapter that claims it -- a PDF's words,
/// an archive member's content -- and the adapters are `std` while this
/// crate is `no_std`, so the caller supplies the seam and this crate stays
/// where it is.
///
/// The confirmation over the rendered bytes is this crate's: [`Grep::lines_in`]
/// is the same matcher, the same line numbering and the same cap the file
/// path uses, so an adapter's rendering is searched exactly like a file.
pub trait AdapterReader: Send + Sync {
    /// The documents the adapters rendered for `path`, or an empty vector
    /// when no adapter claims it: the plain path reads the file then.
    ///
    /// The trait has no error channel, and that is its shape: a rendering
    /// that failed and a file no adapter claims are the same empty vector
    /// here. The reader that answers it is the one that knows the
    /// difference, and the search cannot report what it was not told.
    fn rendered(&self, path: &[u8]) -> Vec<Rendered>;
}

/// Read candidates in rank order until `limit` files have matched or the
/// time budget is spent (best-ranked results first, so a cut-short search
/// still returns the ones you most likely wanted). Each read thread claims
/// the next unread candidate, so the files read are always a prefix of the
/// ranking and reading stops as soon as the `limit`th match is in.
pub fn verify(g: &Grep, paths: &[impl AsRef<[u8]> + Sync], limit: usize) -> GrepResult {
    let r = verify_paths(g, paths, limit);
    // The note on the standard error is how a caller that owns no protocol
    // learns that a file was left unread -- and why. The daemon's own
    // protocol does have a field for them (`skips`), and the daemon fills
    // it; this function is the one that has no wire to fill.
    note_skipped(&r.skipped);
    r
}

/// `verify` over the paths, with the platform's attributes and without the
/// note: the note belongs to whoever owns the whole search.
///
/// Public because the daemon reads the files itself, with the content
/// index's lock released (`plan` then this).
pub fn verify_paths(g: &Grep, paths: &[impl AsRef<[u8]> + Sync], limit: usize) -> GrepResult {
    verify_paths_with(g, paths, limit, None)
}

/// `verify_paths` with a reader for the files the adapters claim.
///
/// The files the reader renders are searched as the adapter left them; the
/// rest are read as bytes. The two sets partition the candidates, so the
/// answer is the plain scan's answer with the formats the plain scan cannot
/// read included.
pub fn verify_paths_with(g: &Grep, paths: &[impl AsRef<[u8]> + Sync], limit: usize, adapter: Option<&dyn AdapterReader>) -> GrepResult {
    verify_from(
        g,
        paths.len(),
        limit,
        SCAN_READERS,
        std::time::Instant::now(),
        |i| Some(paths[i].as_ref()),
        |p| match_file(g, p, &placeholder::attributes, adapter),
    )
    .0
}

/// `verify` over `n` candidates with `readers` threads, `path(i)` giving the
/// i-th, or None if the index already rules it out; the budget counts from
/// `t`. `attrs` says what a file's attributes are -- the platform's answer,
/// or a test's stand-in for it. Also returns how many candidates it got
/// through.
fn verify_from<'a>(
    g: &Grep,
    n: usize,
    limit: usize,
    readers: usize,
    t: std::time::Instant,
    path: impl Fn(usize) -> Option<&'a [u8]> + Sync,
    read_one: impl Fn(&[u8]) -> Outcome + Sync,
) -> (GrepResult, usize) {
    use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};
    let (next, found, read) = (AtomicUsize::new(0), AtomicUsize::new(0), AtomicUsize::new(0));
    let hits = std::sync::Mutex::new(Vec::new());
    let skipped = std::sync::Mutex::new(Vec::new());
    let work = || {
        while found.load(Relaxed) < limit && g.budget.is_none_or(|b| t.elapsed() <= b) {
            let i = next.fetch_add(1, Relaxed);
            if i >= n {
                break;
            }
            let Some(p) = path(i) else { continue };
            read.fetch_add(1, Relaxed);
            match read_one(p) {
                Outcome::Matches(ms) => {
                    found.fetch_add(ms.len(), Relaxed);
                    for m in ms {
                        hits.lock().unwrap().push((i, m));
                    }
                }
                Outcome::NoMatch => {}
                // Not read: the reason says which refusal it was. The
                // path is copied here, and only here -- a file that was
                // read costs nothing.
                Outcome::Skipped(reason) => {
                    skipped.lock().unwrap().push(Skipped { path: p.to_vec(), reason });
                }
            }
        }
        // Don't sit on a big file's worth of buffer between searches.
        READ_BUF.with_borrow_mut(|b| {
            if b.capacity() > 256 << 10 {
                *b = Vec::new();
            }
        });
    };
    let panicked = without_materializing(|| run_lanes(readers, &work));
    let mut hits = hits.into_inner().unwrap();
    // The candidates come in rank order and the documents of one candidate
    // come in the order the adapter produced them, so the path breaks the
    // tie: an archive's members are listed by name, not by whichever thread
    // got there first.
    hits.sort_unstable_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.path.cmp(&b.1.path)));
    hits.truncate(limit);
    let mut skipped = skipped.into_inner().unwrap();
    // The lanes finish in whatever order they finish in; the report is
    // sorted so the same tree always answers the same way.
    skipped.sort_by(|a, b| a.path.cmp(&b.path));
    let done = next.into_inner().min(n);
    let complete = done == n || hits.len() >= limit;
    // A lane that panicked lost its share of the reads: the answer is
    // what the other lanes found, and the answer says so.
    let error = panicked.then(|| "a search lane panicked: the answer is what the other lanes found".to_string());
    (GrepResult { files: hits.into_iter().map(|h| h.1).collect(), candidates: n, read: read.into_inner(), complete, skipped, error }, done)
}

std::thread_local! {
    /// One read buffer per read-pool thread: a fresh ~1 MB Vec per file
    /// costs page faults and an munmap every time. Kept only while a search
    /// runs if it grew past 256 KB.
    static READ_BUF: std::cell::RefCell<Vec<u8>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// What reading one candidate came to.
enum Outcome {
    /// It holds matches: a file holds one, and an archive holds one per
    /// member that matched.
    Matches(Vec<FileMatches>),
    /// It was read, and it holds none.
    NoMatch,
    /// It was not read, and why.
    ///
    /// The reason is one of the fixed strings the wire groups by (see
    /// [`Skipped`]), carried as it is and not formatted: the refusal
    /// paths are the rare ones, and the path that matched must not pay
    /// for them.
    Skipped(&'static str),
}

fn match_file(g: &Grep, path: &[u8], attrs: &Attrs<'_>, adapter: Option<&dyn AdapterReader>) -> Outcome {
    // The adapter first: a file it claims is read through it, and the bytes
    // it renders are text where the file's own bytes are not -- a PDF's
    // words, an archive member's content.
    if let Some(adapter) = adapter {
        let rendered = adapter.rendered(path);
        if !rendered.is_empty() {
            let mut out = Vec::new();
            for doc in rendered {
                let lines = g.lines_in(&doc.bytes);
                if !lines.is_empty() {
                    out.push(FileMatches { path: doc.path, lines });
                }
            }
            return if out.is_empty() { Outcome::NoMatch } else { Outcome::Matches(out) };
        }
    }
    READ_BUF.with_borrow_mut(|buf| {
        use std::io::Read;
        buf.clear();
        let (file, _size) = match open_sized_with(path, attrs) {
            Open::File(f, size) => (f, size),
            // A placeholder is never opened: the open is the download.
            Open::Placeholder => return Outcome::Skipped(placeholder::PLACEHOLDER_REASON),
            // The open refused it, and it says which refusal it was.
            Open::Refused(reason) => return Outcome::Skipped(reason),
        };
        if file.take(MAX_FILE * 4).read_to_end(buf).ok().is_none() {
            // It was open and the bytes did not come: a hole, and the
            // user is told which one it is.
            return Outcome::Skipped(SKIP_READ_FAILED);
        }
        if rgx_core::memchr::memchr(0, &buf[..buf.len().min(8192)]).is_some() {
            // Not a hole: the file was read, and it holds no text. The
            // index build refuses the same files by the same rule -- a NUL
            // in the first 8 KB -- so "binary" is a fact about the file and
            // not about this run, and a file no adapter claims holds no
            // text the search could have found. It is not reported: a tree
            // of images and archives would bury the files that really went
            // unread.
            return Outcome::NoMatch;
        }
        let lines = match g.literal.as_ref().filter(|l| !l.needs_regex(buf)) {
            Some(l) => lines_at(g, buf, l.starts(buf)),
            None => lines_at(g, buf, rgx_starts(g.re(), buf).into_iter()),
        };
        if lines.is_empty() { Outcome::NoMatch } else { Outcome::Matches(vec![FileMatches { path: path.to_vec(), lines }]) }
    })
}

/// The first `max_per_file` lines holding a match (by where matches start),
/// numbered, as shown.
fn lines_at(g: &Grep, buf: &[u8], starts: impl Iterator<Item = usize>) -> Vec<(usize, String)> {
    let mut lines = Vec::new();
    let (mut line_no, mut counted) = (1usize, 0usize);
    let mut last_line_start = usize::MAX;
    for start in starts {
        line_no += rgx_core::memchr::memchr_iter(b'\n', &buf[counted..start]).count();
        counted = start;
        let ls = rgx_core::memchr::memrchr(b'\n', &buf[..start]).map_or(0, |p| p + 1);
        if ls == last_line_start {
            continue;
        }
        last_line_start = ls;
        let le = rgx_core::memchr::memchr(b'\n', &buf[start..]).map_or(buf.len(), |p| start + p);
        lines.push((line_no, String::from_utf8_lossy(&buf[ls..le.min(ls + 400)]).trim_end().to_string()));
        if lines.len() >= g.max_per_file {
            break;
        }
    }
    lines
}

/// An ASCII literal pattern, found without a regex: its exact bytes (it has
/// an upper-case letter), or else its letters in either case. That is what
/// the case-insensitive regex matches too, but for two non-ASCII letters it
/// folds onto ASCII ones (ſ for s, the Kelvin sign for k): a file holding
/// one of those goes to the regex.
enum Literal {
    /// The pattern's own bytes, owned. Upstream's finder borrows the needle
    /// rather than taking it over, and this enum lives inside a `Grep` that
    /// outlives the string the pattern was cut from.
    Exact(Vec<u8>),
    /// A `sym:` name: as a whole word after a declaring keyword on its line
    /// (the definition regex, with its keyword part compiled once).
    Definition(Vec<u8>),
    Folded {
        needle: Vec<u8>,
        pair: (usize, usize),
        odd: Vec<Vec<u8>>,
    },
}

impl Literal {
    fn folded(pattern: &[u8]) -> Literal {
        let needle: Vec<u8> = pattern.iter().map(|&b| fold(b)).collect();
        // Scan for the two rarest bytes first.
        let common = |b: u8| match b {
            b' ' | b'e' | b't' | b'a' | b'o' | b'i' | b'n' | b's' | b'r' => 3,
            b'h' | b'l' | b'd' | b'c' | b'u' | b'm' | b'\n' | b'\t' => 2,
            b'f' | b'p' | b'g' | b'w' | b'y' | b'b' | b'.' | b',' | b'_' | b'(' | b')' | b'"' | b'=' | b'0'..=b'9' => 1,
            _ => 0,
        };
        let mut by_rarity: Vec<usize> = (0..needle.len()).collect();
        by_rarity.sort_by_key(|&i| (common(needle[i]), i));
        let (a, b) = (by_rarity[0], *by_rarity.get(1).unwrap_or(&by_rarity[0]));
        let odd = [(b's', "\u{17F}"), (b'k', "\u{212A}")].iter().filter(|(c, _)| needle.contains(c));
        let odd = odd.map(|(_, u)| u.as_bytes().to_vec()).collect();
        Literal::Folded { needle, pair: (a.min(b), a.max(b)), odd }
    }

    fn needs_regex(&self, hay: &[u8]) -> bool {
        matches!(self, Literal::Folded { odd, .. } if odd.iter().any(|n| rgx_core::memchr::memmem::find(hay, n).is_some()))
    }

    /// Where its matches start, left to right.
    fn starts<'a>(&'a self, hay: &'a [u8]) -> impl Iterator<Item = usize> + 'a {
        let len = match self {
            Literal::Exact(n) | Literal::Definition(n) => n.len(),
            Literal::Folded { needle, .. } => needle.len(),
        };
        let mut at = 0;
        std::iter::from_fn(move || {
            loop {
                let p = match self {
                    Literal::Exact(n) | Literal::Definition(n) => rgx_core::memchr::memmem::find(hay.get(at..)?, n).map(|p| at + p),
                    Literal::Folded { needle, pair, .. } => find_folded(hay, at, needle, *pair),
                }?;
                if !matches!(self, Literal::Definition(_)) {
                    at = p + len;
                    return Some(p);
                }
                at = p + 1;
                if defined_at(hay, p, len) {
                    return Some(p);
                }
            }
        })
    }
}

/// Is the name at `hay[p..p + len]` defined there: an ASCII word boundary
/// after it, and before it on its line a declaring keyword, maybe generics,
/// spaces or `*&`, maybe `mut` (what `sym:`'s regex asks, split at the name)?
fn defined_at(hay: &[u8], p: usize, len: usize) -> bool {
    static KEYWORD: std::sync::OnceLock<ByteRegex> = std::sync::OnceLock::new();
    let word = |b: Option<&u8>| b.is_some_and(|&b| b.is_ascii_alphanumeric() || b == b'_');
    if word(hay[..p + len].last()) == word(hay.get(p + len)) {
        return false;
    }
    let re = KEYWORD.get_or_init(|| {
        ByteRegex::new_multiline(format!(r"(?-u:\b)(?:{DEFINES})(?:<[^>\n]*>)?[ \t*&]+(?:mut[ \t]+)?\z").as_bytes(), false, false).unwrap()
    });
    let line = rgx_core::memchr::memrchr(b'\n', &hay[..p]).map_or(0, |i| i + 1);
    re.search(&hay[line..p], 0).is_some()
}

/// The first place at or after `from` where `hay` holds `needle` (lower
/// case) in either case; `pair` are two positions of rare bytes in it, the
/// ones checked first.
// pair and the mutation of i are the aarch64 NEON loop's; the scalar
// tail only reads i.
#[cfg_attr(not(target_arch = "aarch64"), allow(unused_variables, unused_mut))]
fn find_folded(hay: &[u8], from: usize, needle: &[u8], pair: (usize, usize)) -> Option<usize> {
    let k = needle.len();
    let last = hay.len().checked_sub(k)?;
    let at = |c: usize| hay[c..c + k].iter().zip(needle).all(|(&x, &y)| fold(x) == y);
    let mut i = from;
    #[cfg(target_arch = "aarch64")]
    // Safety: every load reads 16 bytes at i + pair.1 at most, and the loop
    // keeps i + 16 <= last + 1, so they end by hay.len().
    unsafe {
        use std::arch::aarch64::*;
        let (c1, c2) = (needle[pair.0], needle[pair.1]);
        let case = |c: u8| vdupq_n_u8(if c.is_ascii_lowercase() { 0x20 } else { 0 });
        let (v1, v2, m1, m2) = (vdupq_n_u8(c1), vdupq_n_u8(c2), case(c1), case(c2));
        while i + 16 <= last + 1 {
            let p = hay.as_ptr().add(i);
            let eq = vandq_u8(vceqq_u8(vorrq_u8(vld1q_u8(p.add(pair.0)), m1), v1), vceqq_u8(vorrq_u8(vld1q_u8(p.add(pair.1)), m2), v2));
            // Four bits per byte lane.
            let mut bits = vget_lane_u64::<0>(vreinterpret_u64_u8(vshrn_n_u16::<4>(vreinterpretq_u16_u8(eq))));
            while bits != 0 {
                let c = i + (bits.trailing_zeros() / 4) as usize;
                if at(c) {
                    return Some(c);
                }
                bits &= !(0xF << ((c - i) * 4));
            }
            i += 16;
        }
    }
    (i..=last).find(|&c| at(c))
}

/// A trigram query: which docs could possibly match.
#[derive(Debug, Clone)]
pub enum TQ {
    All,
    Tri(u32),
    /// Any trigram starting with these two bytes (the high two of three), or
    /// a doc ending in them.
    Pair(u32),
    And(Vec<TQ>),
    Or(Vec<TQ>),
}

fn literal_plan(s: &[u8]) -> TQ {
    match s {
        [] | [_] => TQ::All,
        &[a, b] => TQ::Pair((fold(a) as u32) << 16 | (fold(b) as u32) << 8),
        _ => TQ::And(trigrams_small(s).into_iter().map(TQ::Tri).collect()),
    }
}

/// A posting list as stored.
#[derive(Clone, Copy)]
enum List<'a> {
    /// Bit d: doc d has the trigram.
    Bits(&'a [u8]),
    /// Ascending doc ids as delta varints.
    Var(Var<'a>),
}

/// A varint list: its skip table (see `SKIPS`; maybe empty) and varints.
#[derive(Clone, Copy)]
struct Var<'a> {
    skips: &'a [u8],
    deltas: &'a [u8],
}

impl<'a> Var<'a> {
    /// A varint list from the segment's bytes.
    ///
    /// The skip table's length is read from the bytes, and a corrupt
    /// segment can claim more than it holds: the table is then empty and
    /// the list reads as empty, rather than slicing past the end of the
    /// file. An empty list rules the trigram out, which is what a
    /// truncated segment deserves -- the search is answered, and it is
    /// answered "not here".
    fn new(bytes: &'a [u8], skips: bool) -> Var<'a> {
        if !skips {
            return Var { skips: &[], deltas: bytes };
        }
        let Some(n) = bytes.get(..4).and_then(|b| b.try_into().ok()).map(u32::from_le_bytes) else {
            return Var { skips: &[], deltas: &[] };
        };
        let n = n as usize;
        let Some(end) = 4usize.checked_add(n.saturating_mul(8)) else {
            return Var { skips: &[], deltas: &[] };
        };
        if end > bytes.len() {
            return Var { skips: &[], deltas: &[] };
        }
        Var { skips: &bytes[4..end], deltas: &bytes[end..] }
    }

    fn iter(&self) -> Varints<'a> {
        Varints { b: self.deltas, last: 0 }
    }

    /// Skip entry `i`: (the content before its run, where the run starts).
    ///
    /// A skip table shorter than the walk expects reads as "no run": the
    /// walk then reads the deltas from the start, which is slower and
    /// still right.
    fn skip(&self, i: usize) -> (u32, usize) {
        let Some(e) = self.skips.get(8 * i..8 * i + 8).and_then(|b| b.try_into().ok()).map(u64::from_le_bytes) else {
            return (0, 0);
        };
        (e as u32, (e >> 32) as usize)
    }
}

/// A walk along a varint list that jumps ahead by its skip table.
struct Cursor<'a> {
    var: Var<'a>,
    it: Varints<'a>,
    /// Contents read so far; the last is `cur`.
    read: usize,
    cur: Option<u32>,
    /// The skip entry of the next run after `cur`, and the content before
    /// that run (u32::MAX: no next run).
    skip: usize,
    before: u32,
}

impl<'a> Cursor<'a> {
    fn new(var: Var<'a>) -> Cursor<'a> {
        let mut it = var.iter();
        let cur = it.next();
        let before = if var.skips.is_empty() { u32::MAX } else { var.skip(0).0 };
        Cursor { var, it, read: 1, cur, skip: 0, before }
    }

    fn next(&mut self) -> Option<u32> {
        self.cur = self.it.next();
        self.read += 1;
        self.cur
    }

    /// The first content from here on that is `x` or more.
    fn seek(&mut self, x: u32) -> Option<u32> {
        if self.cur.is_none_or(|c| c >= x) {
            return self.cur;
        }
        let n = self.var.skips.len() / 8;
        if (self.read - 1) / SKIP != self.skip {
            self.skip = (self.read - 1) / SKIP;
            self.before = if self.skip < n { self.var.skip(self.skip).0 } else { u32::MAX };
        }
        // A run past `cur` starts before `x`: jump to the last such.
        if self.before < x {
            let (mut lo, mut hi) = (self.skip, n);
            while hi - lo > 1 {
                let mid = (lo + hi) / 2;
                if self.var.skip(mid).0 < x { lo = mid } else { hi = mid }
            }
            let (last, at) = self.var.skip(lo);
            // The skip table's offsets are read from the file: one that
            // points past the deltas is a corrupt index, and the walk
            // continues from where it was rather than slicing past the
            // end.
            if let Some(d) = self.var.deltas.get(at..) {
                self.it = Varints { b: d, last };
                self.read = (lo + 1) * SKIP;
                self.next();
            }
        }
        while self.cur.is_some_and(|c| c < x) {
            self.next();
        }
        self.cur
    }
}

/// Decodes a delta-varint list.
struct Varints<'a> {
    b: &'a [u8],
    last: u32,
}

impl Iterator for Varints<'_> {
    type Item = u32;
    #[inline]
    fn next(&mut self) -> Option<u32> {
        let x = *self.b.first()?;
        let (v, n) = if x < 0x80 { (x as u32, 1) } else { varint(self.b) };
        self.b = &self.b[n..];
        self.last += v;
        Some(self.last)
    }
}

impl List<'_> {
    /// Append its docs within `docs`, ascending.
    fn decode(self, docs: std::ops::Range<u32>, out: &mut Vec<u32>) {
        match self {
            List::Bits(b) => and_bits(&[b], docs, out),
            List::Var(v) => {
                let mut c = Cursor::new(v);
                let mut d = c.seek(docs.start);
                while let Some(x) = d.filter(|&x| x < docs.end) {
                    out.push(x);
                    d = c.next();
                }
            }
        }
    }

    /// Keep the docs of `acc` (ascending) that are in the list.
    fn retain(self, acc: &mut Vec<u32>) {
        match self {
            List::Bits(b) => acc.retain(|&d| b.get(d as usize / 8).is_some_and(|x| x >> (d % 8) & 1 != 0)),
            // Few to find in a long list: jump by its skip table.
            List::Var(v) if acc.len() * 64 < v.deltas.len() => {
                let mut c = Cursor::new(v);
                acc.retain(|&d| c.seek(d) == Some(d));
            }
            List::Var(v) => {
                let mut it = v.iter();
                let mut cur = it.next();
                acc.retain(|&d| {
                    while cur.is_some_and(|c| c < d) {
                        cur = it.next();
                    }
                    cur == Some(d)
                });
            }
        }
    }
}

/// Append the docs within `docs` set in every bitset, ascending.
fn and_bits(lists: &[&[u8]], docs: std::ops::Range<u32>, out: &mut Vec<u32>) {
    let end = (docs.end as usize).div_ceil(8).min(lists.iter().map(|l| l.len()).min().unwrap_or(0));
    // A list's 64 docs from byte i (none past its end).
    let word = |l: &[u8], i: usize| match l.get(i..i + 8) {
        Some(b) => u64::from_le_bytes(b.try_into().unwrap()),
        None => l[i..].iter().rev().fold(0, |w, &b| w << 8 | b as u64),
    };
    let mut i = docs.start as usize / 8;
    while i < end {
        let mut w = u64::MAX;
        for l in lists {
            w &= word(l, i);
            if w == 0 {
                break;
            }
        }
        while w != 0 {
            let d = i as u32 * 8 + w.trailing_zeros();
            if docs.contains(&d) {
                out.push(d);
            }
            w &= w - 1;
        }
        i += 8;
    }
}

/// The live text docs of a segment, as bits, and how many.
fn live_text(s: &Segment) -> (Vec<u64>, usize) {
    let mut bits: Vec<u64> = s.dead.iter().map(|w| !w).collect();
    for (d, _) in s.rank().iter().enumerate().filter(|&(_, &r)| r == NOT_TEXT) {
        bits[d / 64] &= !(1 << (d % 64));
    }
    if let Some(w) = bits.last_mut().filter(|_| !s.ndocs.is_multiple_of(64)) {
        *w &= (1 << (s.ndocs % 64)) - 1;
    }
    let total = bits.iter().map(|w| w.count_ones() as usize).sum();
    (bits, total)
}

/// For an AND of trigrams whose lists in `s` are all bitsets: the own
/// contents holding all of them, as bits, and how many.
fn and_dense(s: &Segment, q: &TQ) -> Option<(Vec<u64>, usize)> {
    let TQ::And(qs) = q else { return None };
    let lists: Vec<&[u8]> = (qs.iter())
        .map(|q| match q {
            TQ::Tri(t) => s.list(*t).and_then(|l| if let List::Bits(b) = l { Some(b) } else { None }),
            _ => None,
        })
        .collect::<Option<_>>()?;
    if lists.is_empty() {
        return None;
    }
    let mut bits = all_bits(s.nown);
    for l in lists {
        for (w, b) in bits.iter_mut().zip(l.chunks(8)) {
            *w &= match <[u8; 8]>::try_from(b) {
                Ok(b) => u64::from_le_bytes(b),
                Err(_) => b.iter().rev().fold(0, |x, &y| x << 8 | y as u64),
            };
        }
    }
    let total = bits.iter().map(|w| w.count_ones() as usize).sum();
    Some((bits, total))
}

/// Docs within `docs` matching `q` in a segment, ascending; None means
/// every doc. An AND starts from its shortest list and only tests the
/// others, so common trigrams' long lists are never decoded in full.
fn eval(s: &Segment, q: &TQ, docs: std::ops::Range<u32>) -> Option<Vec<u32>> {
    match q {
        TQ::All => None,
        TQ::Tri(t) => {
            let mut out = Vec::new();
            if let Some(l) = s.list(*t) {
                l.decode(docs, &mut out);
            }
            Some(out)
        }
        TQ::Pair(p) => {
            let keys = s.tri_key();
            let mut bits = vec![0u64; s.nown.div_ceil(64)];
            for &k in &keys[keys.partition_point(|&k| k < *p)..keys.partition_point(|&k| k <= p | 0xFF)] {
                match s.list(k) {
                    Some(List::Bits(b)) => {
                        for (w, c) in bits.iter_mut().zip(b.chunks(8)) {
                            *w |= c.iter().rev().fold(0, |x, &y| x << 8 | y as u64);
                        }
                    }
                    Some(List::Var(v)) => v.iter().for_each(|d| bits[d as usize / 64] |= 1 << (d % 64)),
                    None => {}
                }
            }
            Some(set_bits(&bits, 0).into_iter().filter(|d| docs.contains(d)).collect())
        }
        TQ::And(qs) => {
            let (mut bits, mut vars, mut rest) = (Vec::new(), Vec::new(), Vec::new());
            for q in qs {
                match q {
                    TQ::Tri(t) => match s.list(*t) {
                        Some(List::Bits(b)) => bits.push(b),
                        Some(List::Var(b)) => vars.push(b),
                        None => return Some(Vec::new()),
                    },
                    TQ::All => {}
                    q => rest.push(q),
                }
            }
            vars.sort_by_key(|v| v.deltas.len());
            let mut acc = None;
            if let Some((&first, vars)) = vars.split_first() {
                let mut v = Vec::new();
                List::Var(first).decode(docs.clone(), &mut v);
                for &b in &bits {
                    List::Bits(b).retain(&mut v);
                }
                for &b in vars {
                    if v.is_empty() {
                        break;
                    }
                    List::Var(b).retain(&mut v);
                }
                acc = Some(v);
            } else if !bits.is_empty() {
                let mut v = Vec::new();
                and_bits(&bits, docs.clone(), &mut v);
                acc = Some(v);
            }
            for q in rest {
                if acc.as_ref().is_some_and(Vec::is_empty) {
                    break;
                }
                if let Some(r) = eval(s, q, docs.clone()) {
                    acc = Some(match acc {
                        Some(a) => intersect(&a, &r),
                        None => r,
                    });
                }
            }
            acc
        }
        TQ::Or(qs) => {
            let mut acc: Vec<u32> = Vec::new();
            for q in qs {
                acc.extend(eval(s, q, docs.clone())?);
            }
            acc.sort_unstable();
            acc.dedup();
            Some(acc)
        }
    }
}

fn intersect(a: &[u32], b: &[u32]) -> Vec<u32> {
    let mut out = Vec::with_capacity(a.len().min(b.len()));
    let (mut i, mut j) = (0, 0);
    while i < a.len() && j < b.len() {
        match a[i].cmp(&b[j]) {
            std::cmp::Ordering::Less => i += 1,
            std::cmp::Ordering::Greater => j += 1,
            std::cmp::Ordering::Equal => {
                out.push(a[i]);
                i += 1;
                j += 1;
            }
        }
    }
    out
}

/// What a regex fragment tells us about its matches, as small sets of
/// (case-folded) strings: every match is one of `exact`, starts with one of
/// `prefix` and ends with one of `suffix` (None: no small such set); and
/// every doc holding a match satisfies `q`.
struct Info {
    exact: Option<Set>,
    prefix: Option<Set>,
    suffix: Option<Set>,
    q: TQ,
}

type Set = Vec<Vec<u8>>;

const MAX_EXACT: usize = 16;

impl Info {
    fn exact(set: Set) -> Info {
        Info { exact: Some(set.clone()), prefix: Some(set.clone()), suffix: Some(set), q: TQ::All }
    }

    fn any() -> Info {
        Info { exact: None, prefix: None, suffix: None, q: TQ::All }
    }

    /// All it says about a doc, as one trigram query.
    fn query(self) -> TQ {
        match self.exact {
            Some(_) => and(self.q, exact_query(self.exact)),
            None => and(and(self.q, exact_query(self.prefix)), exact_query(self.suffix)),
        }
    }
}

/// Every string of `a` followed by one of `b`, if that's few enough.
fn cross(a: &Option<Set>, b: &Option<Set>) -> Option<Set> {
    let (a, b) = (a.as_ref()?, b.as_ref()?);
    if a.len() * b.len() > MAX_EXACT {
        return None;
    }
    let mut set: Set = a.iter().flat_map(|x| b.iter().map(move |y| [x.as_slice(), y].concat())).collect();
    set.sort();
    set.dedup();
    Some(set)
}

/// What a set of strings requires of a doc: one of them (All when there is
/// no set, it is empty, or a string in it is too short to have a trigram).
/// Trigrams all of them share are required once, not per string.
fn exact_query(set: Option<Set>) -> TQ {
    let Some(set) = set.filter(|set| !set.is_empty() && set.iter().all(|s| s.len() >= 3)) else { return TQ::All };
    let tris: Vec<Vec<u32>> = set.iter().map(|s| trigrams_small(s)).collect();
    let common: Vec<u32> = tris[0].iter().copied().filter(|t| tris.iter().all(|ts| ts.binary_search(t).is_ok())).collect();
    let rest: Vec<TQ> = tris.iter().map(|ts| TQ::And(ts.iter().filter(|t| !common.contains(t)).map(|&t| TQ::Tri(t)).collect())).collect();
    let mut q = TQ::And(common.into_iter().map(TQ::Tri).collect());
    if rest.iter().all(|r| !matches!(r, TQ::And(v) if v.is_empty())) {
        q = and(q, TQ::Or(rest));
    }
    q
}

fn and(a: TQ, b: TQ) -> TQ {
    match (a, b) {
        (TQ::All, x) | (x, TQ::All) => x,
        (TQ::And(mut x), TQ::And(y)) => {
            x.extend(y);
            TQ::And(x)
        }
        (TQ::And(mut x), y) | (y, TQ::And(mut x)) => {
            x.push(y);
            TQ::And(x)
        }
        (x, y) => TQ::And(vec![x, y]),
    }
}

/// What the pattern says about the text it matches, over the AST the byte
/// engine compiles.
///
/// The walk is over rgx-core's AST and not over another engine's HIR, and
/// that is the whole point: the plan must be implied by the match, and the
/// match is decided by the engine that compiled this very AST. A plan
/// built from a different parser's view of the pattern is a plan about a
/// different pattern.
fn info(a: &Ast) -> Info {
    match a {
        // Zero-width: it matches the empty string, which says nothing.
        Ast::Empty | Ast::Assert(_) => Info::exact(vec![Vec::new()]),
        Ast::Class(set) => class(set),
        Ast::Group { node, .. } => info(node),
        Ast::Repeat { node, min, max, .. } => {
            if *min == 0 {
                return Info::any();
            }
            let i = info(node);
            if *min == 1 && *max == Some(1) {
                return i;
            }
            // At least one copy, which starts it; one ends it.
            Info { q: and(i.q, exact_query(i.exact)), exact: None, prefix: i.prefix, suffix: i.suffix }
        }
        Ast::Concat(nodes) => nodes.iter().map(info).fold(Info::exact(vec![Vec::new()]), concat),
        Ast::Alt(arms) => {
            let parts: Vec<Info> = arms.iter().map(info).collect();
            let union = |f: fn(&Info) -> &Option<Set>| {
                let mut set = Set::new();
                for p in &parts {
                    set.extend(f(p).clone()?);
                }
                set.sort();
                set.dedup();
                (set.len() <= MAX_EXACT).then_some(set)
            };
            let (exact, prefix, suffix) = (union(|p| &p.exact), union(|p| &p.prefix), union(|p| &p.suffix));
            if exact.is_some() {
                return Info { exact, prefix, suffix, q: TQ::All };
            }
            let ors: Vec<TQ> = parts.into_iter().map(Info::query).collect();
            let q = if ors.iter().any(|q| matches!(q, TQ::All)) { TQ::All } else { TQ::Or(ors) };
            Info { exact, prefix, suffix, q }
        }
    }
}

/// Up to 8 members are each an exact string; with more, the first byte of
/// each, if there are few enough of them, starts it.
///
/// The class is a set of bytes, because the engine is a byte engine: a
/// member is one byte, and a match on it is that byte. A class with more
/// members than the index can name says nothing the trigrams can hold, so
/// the plan falls back to "no constraint" -- which is slower and never
/// wrong.
fn class(set: &ByteSet) -> Info {
    let members: Vec<u8> = set.iter().map(fold).collect();
    if members.len() <= 8 {
        let mut exact: Set = members.into_iter().map(|b| vec![b]).collect();
        exact.sort();
        exact.dedup();
        return Info::exact(exact);
    }
    let mut first: Set = members.into_iter().map(|b| vec![b]).collect();
    first.sort();
    first.dedup();
    Info { prefix: (first.len() <= MAX_EXACT).then_some(first), ..Info::any() }
}

/// `a` then `b`. A match holds a's suffix right before b's prefix; that
/// pairing is carried up in the prefix or suffix when one side is exact,
/// else required here.
fn concat(a: Info, b: Info) -> Info {
    let exact = cross(&a.exact, &b.exact);
    let ab_prefix = a.exact.as_ref().and(cross(&a.exact, &b.prefix));
    let ab_suffix = b.exact.as_ref().and(cross(&a.suffix, &b.exact));
    let mut q = and(a.q, b.q);
    if exact.is_none() && ab_prefix.is_none() && ab_suffix.is_none() {
        q = match cross(&a.suffix, &b.prefix) {
            Some(j) => and(q, exact_query(Some(j))),
            None => {
                let sa = if a.exact.is_none() { exact_query(a.suffix.clone()) } else { TQ::All };
                let pb = if b.exact.is_none() { exact_query(b.prefix.clone()) } else { TQ::All };
                and(and(q, sa), pb)
            }
        };
    }
    let prefix = if a.exact.is_some() { ab_prefix.or(a.exact) } else { a.prefix };
    let suffix = if b.exact.is_some() { ab_suffix.or(b.exact) } else { b.suffix };
    Info { exact, prefix, suffix, q }
}

fn regex_plan(a: &Ast) -> TQ {
    info(a).query()
}

/// The most candidates a scan names, whatever the request asked for.
const MAX_CANDIDATES: usize = 200_000;

/// How many candidates a file of the answer is worth.
///
/// The limit is how many files the answer carries; the candidates are the
/// files that might carry them, and most of them do not. Without a factor
/// the scan would read exactly as many candidates as the answer has files
/// and answer with the one that matched — a different answer from "ten
/// files, one of which matches".
const CANDIDATES_PER_FILE: usize = 16;

/// Files to grep where the content index does not reach (e.g. `in:/etc`),
/// picked from the name index instead of crawling, newest first. `q` is the
/// name query restricted to the files worth reading.
/// Candidate files for a content scan. `root` is the tree the name index
/// was built from: `Index::path` reconstructs scan-root-relative paths
/// (fsearch's whole-disk root is `/`), so they are joined onto `root` here
/// to become openable filesystem paths. Overlay paths are already real.
pub fn scan_paths(root: &[u8], live: &Live, mut q: Query) -> Vec<Vec<u8>> {
    q.kind = Some(KIND_FILE);
    // The candidates the request's limit buys, and no more.
    //
    // The limit is how many files the answer carries; the candidates are
    // the files that might carry them, and most of them do not. The hard
    // cap is the ceiling and not the floor: the scan used to name two
    // hundred thousand paths for a request that asked for ten files,
    // which is tens of megabytes and a full pass over the index to
    // answer ten.
    q.limit = q.limit.max(1).saturating_mul(CANDIDATES_PER_FILE).min(MAX_CANDIDATES);
    q.size = (q.size.0, q.size.1.min(MAX_FILE));
    let mut files: Vec<(Vec<u8>, u32)> = (crate::name_query::Searcher { live }.search(&q).into_iter())
        .map(|h| match h.over {
            Some(path) => {
                let m = live.over[&path].mtime;
                (path, m)
            }
            None => {
                let mut p = Vec::new();
                live.base.path(h.idx as usize, &mut p);
                (openable(root, &p), live.base.mtime()[h.idx as usize])
            }
        })
        .collect();
    files.sort_by_key(|(_, m)| std::cmp::Reverse(*m));
    files.into_iter().map(|(p, _)| p).collect()
}

/// Grep `g` over the files `q` selects under `root`, the same answer as
/// [`verify`] on [`scan_paths`], but reading only the candidates the content
/// index cannot vouch for. With no `index` that is exactly the scan. With
/// one, [`Content::covers`] splits the scan's universe in two: the index
/// answers for the files it holds a fresh, name-passing doc for (returning
/// their matches and ruling the rest out by grams, without opening them),
/// and every other candidate -- under a skip-dir, past the curation, edited
/// since it was indexed, or never named -- is read here. The two sets
/// partition the universe, so the reads never overlap and their union is
/// the scan's exact answer; the index is only a way to read less to get it.
pub fn answer(g: &Grep, q: &Query, root: &[u8], live: &Live, home: &[u8], index: Option<&Content>, limit: usize) -> GrepResult {
    answer_with(g, q, Scope { root, live, home, index, adapter: None }, limit)
}

/// The world a content search runs in.
///
/// The tree it scans, the overlay that holds the changes made since the
/// index was built, the home the content index is keyed by, the index
/// itself, and the reader for the files the format adapters claim. They are
/// one fact -- where the search runs -- and they travel as one.
pub struct Scope<'a> {
    /// The tree to scan.
    pub root: &'a [u8],
    /// The live overlay: the changes the tree made since the index was built.
    pub live: &'a Live,
    /// The home directory, which the content index is keyed by.
    pub home: &'a [u8],
    /// The content index, when there is one.
    pub index: Option<&'a Content>,
    /// The reader for the files the adapters claim, when the caller has one.
    pub adapter: Option<&'a dyn AdapterReader>,
}

/// `answer` with a reader for the files the adapters claim.
///
/// The same search, with the formats the plain path cannot read included: a
/// PDF's words and an archive member's content are searched like any other
/// file's bytes, and the file itself is left alone -- an adapter that claims
/// a file has already said what the file says.
pub fn answer_with(g: &Grep, q: &Query, scope: Scope<'_>, limit: usize) -> GrepResult {
    let Scope { root, live, home, index, adapter } = scope;
    let mut q = q.clone();
    q.limit = limit;
    let (covered, bare, mut r) = plan(g, &q, root, live, home, index);
    // `search`'s files are the covered ones that matched (its grams ruled
    // out the others); keep only those this scan's universe named, so an
    // index doc outside it never leaks in. Then read the rest.
    r.files.retain(|f| covered.contains(&f.path));
    // `verify_paths`, not `verify`: the note belongs to this search, which
    // owns both halves of it, so it is written once and says everything.
    let rest = verify_paths_with(g, &bare, limit.saturating_sub(r.files.len()), adapter);
    r.files.extend(rest.files);
    r.read += rest.read;
    r.skipped.extend(rest.skipped);
    if r.error.is_none() {
        r.error = rest.error;
    }
    r.candidates = covered.len() + bare.len();
    r.complete = r.files.len() < limit && r.complete && rest.complete && r.skipped.is_empty();
    // What was not read, and why: the CLI does not carry the list itself,
    // so the note is where the user sees it.
    note_skipped(&r.skipped);
    r
}

/// The half of [`answer`] that needs the index: which paths the index
/// covers, which it does not, and the matches it found on its own.
///
/// The caller can drop the index after this and still read the rest of the
/// files. That matters to the daemon: the index is behind a lock the
/// builder also wants, and reading a file is I/O the builder must not wait
/// for. With no index, the whole universe is read.
pub fn plan(
    g: &Grep,
    q: &Query,
    root: &[u8],
    live: &Live,
    home: &[u8],
    index: Option<&Content>,
) -> (std::collections::HashSet<Vec<u8>>, Vec<Vec<u8>>, GrepResult) {
    let paths = scan_paths(root, live, q.clone_for_scan());
    let Some(index) = index else {
        let r = GrepResult { candidates: paths.len(), complete: true, ..Default::default() };
        return (Default::default(), paths, r);
    };
    let mut covered = Vec::new();
    let mut bare = Vec::new();
    for p in &paths {
        if index.covers(root, p, home, q) {
            covered.push(p.clone());
        } else {
            bare.push(p.clone());
        }
    }
    let covered: std::collections::HashSet<Vec<u8>> = covered.into_iter().collect();
    let mut r = index.search_covered(g, q, Some(&covered));
    r.files.retain(|f| covered.contains(&f.path));
    r.skipped.retain(|s| covered.contains(&s.path));
    (covered, bare, r)
}

/// Open a path for reading only if it is a regular file, never blocking:
/// O_NONBLOCK keeps a FIFO from hanging open(), the read threads'
/// "don't materialize dataless files" policy keeps iCloud placeholders from
/// being downloaded just because we searched, and a cloud placeholder is
/// refused outright: on Windows the open is the download.
///
/// Public API of a published crate: a caller outside the grep pipeline that
/// wants a file it may read gets exactly the file the pipeline would read,
/// with the same refusals.
pub fn open_regular(path: &[u8]) -> Option<std::fs::File> {
    match open_sized(path) {
        Open::File(f, _) => Some(f),
        Open::Placeholder | Open::Refused(_) => None,
    }
}

/// `open()` flags that keep a FIFO from hanging and refuse a symlink final
/// component. ABI-stable per OS; values match the libc crate's tables
/// (linux: O_NONBLOCK 0o4000, O_NOFOLLOW 0o400000 on x86_64 and 0o100000
/// on arm64; Darwin/BSD: 0x4/0x100).
/// Declared here rather than pulled from libc — name-core carries no libc
/// dependency.
///
/// TRAP: on Linux `0o200000` is O_DIRECTORY, not O_NOFOLLOW. With it,
/// open_regular fails ENOTDIR on every regular file and the grep pipeline
/// silently finds nothing — the content tests only caught it when they
/// first ran on a real Linux kernel (WSL). O_NOFOLLOW refuses a symlink
/// with ELOOP and still opens regular files; O_DIRECTORY does the reverse.
///
/// The same trap has an architecture in it: `O_NOFOLLOW` is not one number
/// on Linux either. `asm-generic/fcntl.h` gives 00400000, and arm64
/// overrides it with 0100000 -- so the number that works on x86_64 is a
/// bit the arm64 kernel does not know, and the open fails with EINVAL
/// instead of opening the file.
#[cfg(unix)]
mod open_flags {
    #[cfg(any(target_os = "linux", target_os = "android"))]
    pub const O_NONBLOCK: i32 = 0o4000;
    #[cfg(all(any(target_os = "linux", target_os = "android"), target_arch = "x86_64"))]
    pub const O_NOFOLLOW: i32 = 0o400000;
    #[cfg(all(any(target_os = "linux", target_os = "android"), target_arch = "aarch64"))]
    pub const O_NOFOLLOW: i32 = 0o100000;
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    pub const O_NONBLOCK: i32 = 0x4;
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    pub const O_NOFOLLOW: i32 = 0x100;
    // FreeBSD's `sys/sys/fcntl.h`: O_NONBLOCK 0x0004, O_NOFOLLOW 0x0100 --
    // the same two values Darwin uses, so this is the arm above by another
    // name rather than a new pair. (Do not borrow Darwin's O_DIRECTORY,
    // 0x100000: FreeBSD and Linux both use 0x20000.)
    //
    // Scoped to FreeBSD on purpose. OpenBSD, NetBSD and DragonFly are
    // unverified, so they stay in the zero arm below rather than inherit a
    // number that was read off one kernel.
    #[cfg(target_os = "freebsd")]
    pub const O_NONBLOCK: i32 = 0x4;
    #[cfg(target_os = "freebsd")]
    pub const O_NOFOLLOW: i32 = 0x100;
    // Not rgy targets: flags off; `is_file()` in open_sized is still the
    // directory guard, but a FIFO could block open() here. OpenBSD, NetBSD
    // and DragonFly land here until their values are checked.
    #[cfg(not(any(target_os = "linux", target_os = "android", target_os = "macos", target_os = "ios", target_os = "freebsd")))]
    pub const O_NONBLOCK: i32 = 0;
    #[cfg(not(any(target_os = "linux", target_os = "android", target_os = "macos", target_os = "ios", target_os = "freebsd")))]
    pub const O_NOFOLLOW: i32 = 0;
}

/// Where a file's attributes come from: the platform's answer, or a test's
/// stand-in for it. The seam exists because the recall attributes cannot be
/// set on a real file -- only a cloud filter driver sets them -- so the
/// refusal has to be exercised with an answer the test supplies.
type Attrs<'a> = dyn Fn(&[u8]) -> Option<u32> + Sync + 'a;

/// What opening a path for reading came to.
enum Open {
    /// The file, and its size.
    File(std::fs::File, u64),
    /// A cloud placeholder: reading it would download it, so it is not read.
    Placeholder,
    /// Not read, and why.
    ///
    /// The reason is carried here because this is where the refusal
    /// happens: the open knows which refusal it is, and the search reports
    /// the string the wire groups by.
    Refused(&'static str),
}

/// `open_regular`, and the file's size.
fn open_sized(path: &[u8]) -> Open {
    open_sized_with(path, &placeholder::attributes)
}

/// `open_sized`, with the attributes asked of `attrs` rather than of the
/// platform.
///
/// A cloud placeholder is never opened: the open is the download. The
/// attributes are a question about the directory entry, so asking costs one
/// metadata call and fetches nothing.
fn open_sized_with(path: &[u8], attrs: &Attrs<'_>) -> Open {
    if attrs(path).is_some_and(placeholder::is_placeholder) {
        return Open::Placeholder;
    }
    #[cfg(unix)]
    let f = {
        use std::os::unix::ffi::OsStrExt;
        use std::os::unix::fs::OpenOptionsExt;
        match std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(open_flags::O_NONBLOCK | open_flags::O_NOFOLLOW)
            .open(std::ffi::OsStr::from_bytes(path))
        {
            Ok(f) => f,
            Err(_) => return Open::Refused(SKIP_UNOPENABLE),
        }
    };
    #[cfg(not(unix))]
    let f = {
        // No unix FIFOs to hang on and no O_NOFOLLOW in std; the is_file
        // check below is the guard against opening a symlink target's dir.
        let Ok(p) = std::str::from_utf8(path) else { return Open::Refused(SKIP_UNOPENABLE) };
        match std::fs::OpenOptions::new().read(true).open(std::path::Path::new(p)) {
            Ok(f) => f,
            Err(_) => return Open::Refused(SKIP_UNOPENABLE),
        }
    };
    let Ok(m) = f.metadata() else { return Open::Refused(SKIP_UNOPENABLE) };
    if !m.is_file() {
        return Open::Refused(SKIP_NOT_A_FILE);
    }
    Open::File(f, m.len())
}

/// The acceptance rule: a FIFO under the scan root
/// must not hang `open()`. `O_NONBLOCK` turns the hang into `ENXIO` when no
/// writer exists, so `open_sized` refuses the path — promptly. A regression
/// to the zero-flags arm turns this test into a hang instead of a failure.
///
/// Runs wherever the flag arm is active (linux/macos/freebsd); on the
/// unverified BSDs the arm is deliberately zero, so the test stays out.
#[cfg(test)]
#[cfg(any(target_os = "linux", target_os = "macos", target_os = "freebsd"))]
mod open_flags_tests {
    use super::{Open, SKIP_NOT_A_FILE, SKIP_UNOPENABLE, open_sized};
    use std::os::unix::ffi::OsStrExt;

    // mkfifo(3), declared rather than pulled from libc — name-core carries
    // no libc dependency. mode_t is u32 on Linux and u16 on FreeBSD, so the
    // declaration follows the platform instead of one shape for all.
    #[cfg(not(target_os = "freebsd"))]
    unsafe extern "C" {
        fn mkfifo(path: *const std::ffi::c_char, mode: u32) -> i32;
    }
    #[cfg(target_os = "freebsd")]
    unsafe extern "C" {
        fn mkfifo(path: *const std::ffi::c_char, mode: u16) -> i32;
    }

    #[test]
    fn a_fifo_does_not_hang_open() {
        let dir = std::env::temp_dir().join(format!("rgy_fifo_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let p = dir.join("pipe");
        let c = std::ffi::CString::new(p.as_os_str().as_bytes()).unwrap();
        let r = unsafe { mkfifo(c.as_ptr(), 0o600) };
        assert_eq!(r, 0, "mkfifo must succeed");
        // The FIFO is refused, and the open does not hang: that is the
        // property, and it is the one every kernel agrees on.
        //
        // **Which** refusal it is depends on the kernel, and both answers
        // are honest. Most kernels answer `ENXIO` to `open(fifo,
        // O_RDONLY|O_NONBLOCK)` when there is no writer, and the path was
        // never opened: "it could not be opened". The Linux 7.x kernel the
        // CachyOS box runs answers success instead, the file type check
        // then refuses it, and the reason is "it is not a regular file" --
        // which is the truer of the two, because a FIFO is a pipe.
        //
        // Measured on the box on 2026-10-30: `open(fifo, O_RDONLY|
        // O_NONBLOCK|O_NOFOLLOW)` returns a readable descriptor there, and
        // the engine refuses it as not a regular file.
        match open_sized(c.as_bytes()) {
            Open::Refused(reason) => assert!(
                reason == SKIP_UNOPENABLE || reason == SKIP_NOT_A_FILE,
                "a writerless FIFO must be refused as a pipe or as unopenable: {reason}"
            ),
            Open::File(..) => panic!("a writerless FIFO must be refused, not opened as a file"),
            Open::Placeholder => panic!("a FIFO on the disk is not a cloud placeholder"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// A cloud placeholder is never opened, and a file that is really on the
/// disk is read exactly as before.
///
/// The recall attributes cannot be set on a real file -- only a cloud
/// filter driver sets them, and a program may not -- so the file on disk
/// here is an ordinary one and the attributes come from a stand-in. What
/// is under test is the refusal and the report, not the filesystem.
/// The seam's own test: the file the adapters claim, and the file they do
/// not.
#[cfg(test)]
mod adapter_tests {
    use super::{AdapterReader, Grep, Rendered, verify_paths, verify_paths_with};
    use crate::name_query::GrepMode;
    use alloc::string::ToString;
    use alloc::vec::Vec;

    /// A reader that claims one file and renders text for it.
    ///
    /// The stand-in for the format adapters: the caller owns the decoding,
    /// and this crate asks it per file.
    struct OneAdapter;

    impl AdapterReader for OneAdapter {
        fn rendered(&self, path: &[u8]) -> Vec<Rendered> {
            if !path.ends_with(b"rendered.bin") {
                return Vec::new();
            }
            vec![Rendered { path: path.to_vec(), bytes: b"the rendered words\n".to_vec() }]
        }
    }

    /// A file the plain path refuses is read through the adapter.
    ///
    /// The file's own bytes are binary -- the plain path refuses a file with
    /// a NUL in its first kilobyte -- and the words are in the rendering, so
    /// a match can only have come from the adapter.
    #[test]
    fn a_file_the_adapters_claim_is_read_through_them() {
        let dir = std::env::temp_dir().join(format!("rgy_adapter_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("rendered.bin");
        std::fs::write(&p, b"\x00\x01\x02 binary \xff bytes").unwrap();
        let path = p.to_string_lossy().into_owned().into_bytes();

        let g = Grep::new("rendered", GrepMode::Literal).unwrap();

        // Without the adapter the file is refused: it is binary.
        let plain = verify_paths(&g, std::slice::from_ref(&path), 10);
        assert!(plain.files.is_empty(), "the plain path read a binary file");

        // With it, the rendering is searched.
        let rendered = verify_paths_with(&g, std::slice::from_ref(&path), 10, Some(&OneAdapter));
        assert_eq!(rendered.files.len(), 1, "the rendering was not searched");
        assert_eq!(rendered.files[0].path, path);
        assert_eq!(rendered.files[0].lines, vec![(1, "the rendered words".to_string())]);

        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
mod placeholder_tests {
    use super::{Grep, Open, match_file, open_ahead_with, open_sized, open_sized_with, placeholder, verify_from};
    use crate::name_query::GrepMode;
    use alloc::vec::Vec;

    /// A file with a needle in it, and its path as the tree spells paths.
    fn temp_file(tag: &str) -> (std::path::PathBuf, Vec<u8>) {
        let dir = std::env::temp_dir().join(format!("rgy_ph_{}_{}", std::process::id(), tag));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("doc.txt");
        std::fs::write(&p, b"a needle in here\n").unwrap();
        (dir, p.to_string_lossy().into_owned().into_bytes())
    }

    /// The attributes of a placeholder: "fetch me from the cloud before you
    /// read me".
    fn recall() -> impl Fn(&[u8]) -> Option<u32> + Sync {
        |_| Some(placeholder::FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS)
    }

    #[test]
    fn a_placeholder_is_not_opened() {
        let (dir, p) = temp_file("open");
        assert!(
            matches!(open_sized_with(&p, &recall()), Open::Placeholder),
            "a placeholder must be refused before the open, not opened and found empty"
        );
        // The same file, asked the real question, opens as usual.
        assert!(matches!(open_sized(&p), Open::File(..)), "an ordinary file must still open");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_index_build_does_not_open_a_placeholder_either() {
        let (dir, p) = temp_file("build");
        assert!(open_ahead_with(&p, &recall()).is_none(), "the build reads every candidate file: that read is a download too");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_placeholder_is_skipped_and_the_skip_says_why() {
        let (dir, p) = temp_file("skip");
        let g = Grep::new("needle", GrepMode::Literal).unwrap();
        let (r, _) = verify_from(&g, 1, 10, 1, std::time::Instant::now(), |_| Some(&p[..]), |q| match_file(&g, q, &recall(), None));
        assert!(r.files.is_empty(), "the file was not read, so it cannot have matched");
        assert_eq!(r.skipped.len(), 1, "the skip must be reported, not swallowed");
        assert_eq!(r.skipped[0].path, p, "the skip must name the file");
        let reason = r.skipped[0].reason;
        assert!(reason.contains("cloud placeholder"), "the reason must name the thing: {reason}");
        assert!(reason.contains("download"), "the reason must say what reading it would have done: {reason}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_ordinary_file_is_read_and_matched_as_before() {
        let (dir, p) = temp_file("normal");
        let g = Grep::new("needle", GrepMode::Literal).unwrap();
        let (r, _) = verify_from(&g, 1, 10, 1, std::time::Instant::now(), |_| Some(&p[..]), |q| match_file(&g, q, &placeholder::attributes, None));
        assert_eq!(r.files.len(), 1, "an ordinary file is still read, and still matches");
        assert!(r.skipped.is_empty(), "an ordinary file is not skipped");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The attributes are asked of the platform, not guessed: a file on the
    /// disk has no recall bit, on any platform.
    #[test]
    fn a_file_on_the_disk_is_not_a_placeholder() {
        let (dir, p) = temp_file("real");
        assert!(!placeholder::is_placeholder_path(&p), "a real file must not be mistaken for a placeholder");
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// The files the search did not read, and the reasons it gives.
///
/// The search reads a file and finds nothing in it: that is an answer, and
/// the file is not among the files the search did not read. The search
/// cannot read a file: that is a hole, and the answer says which hole it
/// was. These tests pin both halves -- a hole that is reported, and an
/// answer that is not turned into one.
#[cfg(test)]
mod skipped_tests {
    use super::{Grep, SKIP_UNOPENABLE, verify_from};
    use crate::name_query::GrepMode;
    use alloc::string::String;
    use alloc::vec::Vec;

    /// A file with a needle in it, and its path as the tree spells paths.
    fn temp_file(tag: &str) -> (std::path::PathBuf, Vec<u8>) {
        let dir = std::env::temp_dir().join(format!("rgy_skip_{}_{}", std::process::id(), tag));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("doc.txt");
        std::fs::write(&p, b"a needle in here\n").unwrap();
        (dir, p.to_string_lossy().into_owned().into_bytes())
    }

    /// The search over one file, as the search itself runs it.
    fn search_one(p: &[u8]) -> super::GrepResult {
        let g = Grep::new("needle", GrepMode::Literal).unwrap();
        let (r, _) =
            verify_from(&g, 1, 10, 1, std::time::Instant::now(), |_| Some(p), |q| super::match_file(&g, q, &super::placeholder::attributes, None));
        r
    }

    /// A file the process cannot open is a hole, and the answer says which
    /// hole it was.
    ///
    /// The file is gone between the index naming it and the search reading
    /// it -- the same shape as a file another process holds, and the same
    /// code path: the open fails. A hole that is not reported is a file the
    /// user believes was searched.
    #[test]
    fn a_file_the_process_cannot_open_is_reported_as_skipped() {
        let dir = std::env::temp_dir().join(format!("rgy_skip_gone_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("gone.txt");
        let path = p.to_string_lossy().into_owned().into_bytes();
        // The file is not there: the open fails.
        let r = search_one(&path);
        assert!(r.files.is_empty(), "the file was not read, so it cannot have matched");
        assert_eq!(r.skipped.len(), 1, "the hole must be reported, not swallowed");
        assert_eq!(r.skipped[0].path, path, "the skip must name the file");
        assert_eq!(r.skipped[0].reason, SKIP_UNOPENABLE, "the skip must say why");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A file another process holds is a file this process cannot read, and
    /// it is the case the user meets: a file open in an editor, a database
    /// being written, a copy a backup has in hand.
    ///
    /// Windows: the file is held open with no sharing at all, so the
    /// search's own open is refused with a sharing violation.
    #[cfg(windows)]
    #[test]
    fn a_locked_file_is_reported_as_skipped() {
        use std::os::windows::fs::OpenOptionsExt;
        let (dir, p) = temp_file("locked");
        let on_disk = std::path::PathBuf::from(String::from_utf8(p.clone()).unwrap());
        let _lock = std::fs::OpenOptions::new().read(true).share_mode(0).open(&on_disk).unwrap();
        let r = search_one(&p);
        assert!(r.files.is_empty(), "the file was not read, so it cannot have matched");
        assert_eq!(r.skipped.len(), 1, "the hole must be reported, not swallowed");
        assert_eq!(r.skipped[0].reason, SKIP_UNOPENABLE, "the skip must say why");
        drop(_lock);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The same hole, the unix way: the file is there and this process may
    /// not read it.
    ///
    /// The permissions are taken away, and the test says nothing where the
    /// environment reads the file anyway -- root reads a file whose
    /// permissions say otherwise, and there is no hole to report there.
    #[cfg(unix)]
    #[test]
    fn a_file_the_process_may_not_read_is_reported_as_skipped() {
        use std::os::unix::fs::PermissionsExt;
        let (dir, p) = temp_file("noperm");
        let on_disk = std::path::PathBuf::from(String::from_utf8(p.clone()).unwrap());
        std::fs::set_permissions(&on_disk, std::fs::Permissions::from_mode(0o000)).unwrap();
        // Whether the environment can read it anyway is asked **before** the
        // permissions come back: a probe after they are restored always
        // succeeds, and the test would then take the "root reads it" branch
        // on every machine and prove nothing.
        let readable = std::fs::File::open(&on_disk).is_ok();
        let r = search_one(&p);
        // The permissions back, or the file stays and the cleanup fails.
        std::fs::set_permissions(&on_disk, std::fs::Permissions::from_mode(0o600)).unwrap();
        if readable {
            // The environment reads it anyway (root): there is no hole.
            assert!(r.skipped.is_empty(), "a file that was read is not a hole");
            let _ = std::fs::remove_dir_all(&dir);
            return;
        }
        assert!(r.files.is_empty(), "the file was not read, so it cannot have matched");
        assert_eq!(r.skipped.len(), 1, "the hole must be reported, not swallowed");
        assert_eq!(r.skipped[0].reason, SKIP_UNOPENABLE, "the skip must say why");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The other half: a file that was read and holds none of the pattern
    /// is an answer, not a hole.
    ///
    /// The search looked inside it and found nothing -- that is what "no
    /// match" means -- and the file must not be counted among the files
    /// the search did not read.
    #[test]
    fn a_file_that_was_read_and_does_not_match_is_not_skipped() {
        let dir = std::env::temp_dir().join(format!("rgy_skip_nomatch_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("other.txt");
        std::fs::write(&p, b"nothing to see here\n").unwrap();
        let path = p.to_string_lossy().into_owned().into_bytes();
        let r = search_one(&path);
        assert!(r.files.is_empty(), "the file holds no needle");
        assert!(r.skipped.is_empty(), "a file that was read is not a hole");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A file that is not text was read too, and it holds no match: it is
    /// not a hole.
    ///
    /// The plain text path refuses a file with a NUL in its first
    /// kilobyte, and the index build refuses the same files by the same
    /// rule -- so "binary" is a fact about the file and not about this
    /// run. The bytes came in and were looked at; there was no text in
    /// them. The formats that carry text are read through the adapters,
    /// and a file an adapter claims never reaches this rule at all.
    ///
    /// Reporting it would be a choice, and the wrong one: a tree of
    /// images and archives would bury the files that really went unread.
    #[test]
    fn a_binary_file_was_read_and_is_not_skipped() {
        let dir = std::env::temp_dir().join(format!("rgy_skip_binary_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("blob.bin");
        std::fs::write(&p, b"\x00\x01\x02 binary \xff bytes").unwrap();
        let path = p.to_string_lossy().into_owned().into_bytes();
        let r = search_one(&path);
        assert!(r.files.is_empty(), "a binary file holds no text");
        assert!(r.skipped.is_empty(), "a binary file was read, so it is not a hole");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A folder is not a file, and the search reads files.
    ///
    /// The reason is the platform's: on Windows the open of a folder is
    /// refused before the question is asked -- a folder needs
    /// `FILE_FLAG_BACKUP_SEMANTICS`, which the search does not pass -- so
    /// there the answer is "it could not be opened"; on unix the open
    /// succeeds and the path turns out not to be a file.
    #[test]
    fn a_folder_is_reported_as_skipped() {
        #[cfg(unix)]
        let want = {
            use super::SKIP_NOT_A_FILE;
            SKIP_NOT_A_FILE
        };
        #[cfg(not(unix))]
        let want = SKIP_UNOPENABLE;
        let dir = std::env::temp_dir().join(format!("rgy_skip_folder_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.to_string_lossy().into_owned().into_bytes();
        let r = search_one(&path);
        assert!(r.files.is_empty(), "a folder holds no lines");
        assert_eq!(r.skipped.len(), 1, "a folder the search cannot read is a hole");
        assert_eq!(r.skipped[0].reason, want, "the skip must say why");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The read failed: the file was open, and the bytes did not come.
    ///
    /// Linux has a file that says so on purpose -- `/proc/self/mem` reads
    /// as an input/output error at an address that is not mapped. Where
    /// the platform does not produce read failures there is nothing to
    /// assert, and the test says so rather than pretending.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_file_whose_read_fails_is_reported_as_skipped() {
        use super::SKIP_READ_FAILED;
        use std::io::Read;
        let Ok(f) = std::fs::File::open("/proc/self/mem") else { return };
        let mut buf = Vec::new();
        if f.take(1).read_to_end(&mut buf).is_ok() {
            // The platform read it: no read failure to report.
            return;
        }
        let r = search_one(b"/proc/self/mem");
        assert!(r.files.is_empty(), "the file was not read, so it cannot have matched");
        assert_eq!(r.skipped.len(), 1, "the hole must be reported, not swallowed");
        assert_eq!(r.skipped[0].reason, SKIP_READ_FAILED, "the skip must say why");
    }

    /// The reason travels in the outcome, and the outcome is the size it
    /// was: a read that matched must cost what it cost before.
    ///
    /// The reason is a pointer and a length, and the `Vec` variant left
    /// the space for it -- so the outcome is no bigger than the one that
    /// held the matches and nothing else.
    #[test]
    fn the_outcome_carries_the_reason_without_growing() {
        /// The outcome as it was when the only refusal was the
        /// placeholder's: the matches, or nothing. It is here to be
        /// measured, and never built.
        #[allow(dead_code)]
        enum Before {
            Matches(Vec<super::FileMatches>),
            NoMatch,
        }
        let _ = Before::Matches(Vec::new());
        let _ = Before::NoMatch;
        assert_eq!(std::mem::size_of::<super::Outcome>(), std::mem::size_of::<Before>(), "the reason must not make the outcome bigger");
    }
}

/// `open_regular` for a file about to be indexed, telling the kernel to
/// start reading it now (as much as indexing reads).
fn open_ahead(path: &[u8]) -> Option<std::fs::File> {
    open_ahead_with(path, &placeholder::attributes)
}

/// `open_ahead`, with the attributes asked of `attrs` rather than of the
/// platform.
fn open_ahead_with(path: &[u8], attrs: &Attrs<'_>) -> Option<std::fs::File> {
    let (f, len) = match open_sized_with(path, attrs) {
        Open::File(f, len) => (f, len),
        // A placeholder is not opened here either: the index build reads
        // every candidate file, and that read is a download too.
        Open::Placeholder | Open::Refused(_) => return None,
    }; // macOS F_RDADVISE: tell the kernel to start reading now. Elsewhere the
    // plain open is the whole story. Declared inline (no libc dep).
    #[cfg(target_os = "macos")]
    {
        use std::os::unix::io::AsRawFd;
        #[repr(C)]
        struct Radvisory {
            ra_offset: i64, // Darwin off_t
            ra_count: i32,
        }
        unsafe extern "C" {
            fn fcntl(fd: i32, cmd: i32, ...) -> i32;
        }
        // Darwin fcntl.h: F_RDADVISE = 44.
        let ra = Radvisory { ra_offset: 0, ra_count: len.min(MAX_FILE + 1) as i32 };
        unsafe { fcntl(f.as_raw_fd(), 44, &ra) };
    }
    #[cfg(not(target_os = "macos"))]
    let _ = len;
    Some(f)
}

#[cfg(test)]
mod grep_pipeline_tests {
    use super::*;
    use crate::live::Live;
    use crate::name_index::Index;
    use crate::name_query::{GrepMode, Query};
    use alloc::format;
    use alloc::vec::Vec;
    use rgy_discovery::PortableSource;
    use rgy_discovery::Source;

    fn tiny_live(tag: &str) -> (Live, std::path::PathBuf) {
        // Unique per test: they run in parallel and share the temp root.
        let dir = std::env::temp_dir().join(format!("rgy_grep_test_{}_{}", std::process::id(), tag));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("alpha.rs"), b"fn alpha() { let x = 1; }\n").unwrap();
        std::fs::write(dir.join("beta.txt"), b"nothing here\n").unwrap();
        let src = PortableSource::new(dir.to_string_lossy().into_owned().into_bytes(), 2);
        let live = Live::new(Index::build(src.enumerate().unwrap(), 0, 1, b"/"));
        (live, dir)
    }

    #[test]
    fn empty_token_scan_still_lists_every_file() {
        let (live, dir) = tiny_live("scan");
        let q = Query { grep: Some("alpha".into()), grep_mode: GrepMode::Literal, ..Default::default() };
        let paths = scan_paths(&dir.to_string_lossy().into_owned().into_bytes(), &live, q.clone_for_scan());
        assert_eq!(paths.len(), 2, "scan_paths over no name tokens must return every file: {paths:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn verify_finds_a_literal_in_a_file() {
        let (live, dir) = tiny_live("lit");
        let g = Grep::new("alpha", GrepMode::Literal).expect("grep compiles");
        let q = Query { grep: Some("alpha".into()), ..Default::default() };
        let paths = scan_paths(&dir.to_string_lossy().into_owned().into_bytes(), &live, q.clone_for_scan());
        let res = verify(&g, &paths, 10);
        assert_eq!(res.files.len(), 1, "alpha.rs holds the literal");
        assert!(String::from_utf8_lossy(&res.files[0].path).contains("alpha"));
        assert!(!res.files[0].lines.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn rgx_engine_confirm_agrees_with_the_literal_path() {
        // The rgx engine's regex confirm must find the same literal the
        // memchr fast path does (4.3: verification runs on rgx).
        let (live, dir) = tiny_live("rgx");
        let g = Grep::new("alpha", GrepMode::Regex).expect("regex compiles");
        let q = Query::default();
        let paths = scan_paths(&dir.to_string_lossy().into_owned().into_bytes(), &live, q.clone_for_scan());
        let res = verify(&g, &paths, 10);
        assert_eq!(res.files.len(), 1, "rgx confirm finds alpha.rs");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The two gram extractors must produce the same trigrams.
    ///
    /// `index_grams` writes the index's key table and `trigrams_small` builds
    /// the query's plan from the same string. If they disagree the query's
    /// keys are not in the table, `Segment::list` finds no posting list, and
    /// `eval` answers `Some(vec![])` -- the indexed search silently returns
    /// nothing. That is exactly what the test above saw: zero candidates for
    /// a pattern the scan found.
    #[test]
    fn the_index_and_the_query_agree_on_trigrams() {
        let buf = b"fn alpha() { let x = 1; }";
        let mut seen = vec![0u64; 3 << 18];
        let (mut tri, mut short, mut long) = (Vec::new(), Vec::new(), Vec::new());
        index_grams(buf, &mut seen, &mut tri, &mut short, &mut long);
        tri.sort_unstable();
        tri.dedup();

        let want = trigrams_small(buf);
        for &t in &want {
            assert!(
                tri.contains(&t),
                "the query looks up trigram {t:#x} which the index never wrote; \
                 the index wrote {tri:#x?}"
            );
        }

        // And the built segment's key table has to hold them too: `Segment::list`
        // binary-searches `tri_key`, so a missing key reads as "no postings" and
        // the query quietly returns nothing.
        let tmp = std::env::temp_dir().join(format!("rgy_tri_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        // `build_segment` opens the path it is given, so the docs have to
        // carry openable paths -- the name index's are relative to its root.
        std::fs::write(tmp.join("alpha.rs"), buf).unwrap();
        let mut docs = Docs::default();
        let abs = tmp.join("alpha.rs").to_string_lossy().into_owned().into_bytes();
        docs.push(&abs, buf.len() as u64, 1);
        let seg = build_segment(&tmp, 1, &docs, 0..1).expect("segment builds");
        let keys = seg.tri_key();
        for &t in &want {
            assert!(keys.binary_search(&t).is_ok(), "the segment's key table is missing trigram {t:#x}; it holds {} keys", keys.len());
        }
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// The indexed search and the scan must find the same things.
    ///
    /// These are the two ways to answer a `grep:` query and a caller has to
    /// choose between them. Until now the index had no caller and no test:
    /// `build_segment`, `Content::open` and `Content::search` were written
    /// and left. Wiring a CLI onto an untested path is how a fast wrong
    /// answer ships, so the agreement comes first.
    #[test]
    fn the_indexed_search_agrees_with_the_scan() {
        let (live, dir) = tiny_live("indexed");
        let root = dir.to_string_lossy().into_owned().into_bytes();
        let g = Grep::new("alpha", GrepMode::Literal).expect("grep compiles");
        let q = Query { grep: Some("alpha".into()), grep_mode: GrepMode::Literal, ..Default::default() };

        // What the scan path finds, reading every candidate from disk.
        let paths = scan_paths(&root, &live, q.clone_for_scan());
        let scanned = verify(&g, &paths, 10);
        assert_eq!(scanned.files.len(), 1, "the fixture holds one alpha.rs");

        // The same question through the trigram index.
        let idx = dir.join("index");
        let mut content = Content::open(idx.clone());
        // `wanted` produces docs `build_segment` can open: `root` joined onto
        // the name index's root-relative paths. (`home` is an empty prefix,
        // not `b"/"`: `in_scope` strips it and then requires the rest to start
        // with `/`, so a `"/"` prefix strips the leading separator off every
        // path and rejects them all.)
        let docs = wanted(&live, b"", b"/", true, &root);
        assert!(
            !docs.is_empty(),
            "the fixture must put something in the index -- if this is empty \
             the paths are not openable and the segment will hold no grams"
        );
        let id = content.alloc_id();
        let seg = build_segment(&idx, id, &docs, 0..docs.len()).expect("a segment must build from this fixture");
        content.push(seg);
        let found = content.search(&g, &q);

        let names = |files: &Vec<FileMatches>| -> Vec<String> {
            let mut v: Vec<String> = files.iter().map(|f| String::from_utf8_lossy(&f.path).into_owned()).collect();
            v.sort();
            v.dedup();
            v
        };
        assert_eq!(names(&found.files), names(&scanned.files), "the indexed search and the scan must not disagree about what matched");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The union (`answer`) must be the scan's exact answer, even where the
    /// content index deliberately reaches less far than the scan.
    ///
    /// `wanted`/`eligible` hold only curated text (no skip-dirs like
    /// `node_modules`, no past-1 MiB or odd-extension files), but `scan_paths`
    /// reads whatever the name index lists. So a match under `node_modules`
    /// is one the content index never holds and `Content::search` alone can
    /// never report -- the exact hole the union closes by reading the
    /// candidates the index cannot vouch for. This pins `answer` == the scan
    /// on a fixture that has such a file, while showing the index alone (the
    /// fast path's raw output) misses it.
    #[test]
    fn the_union_agrees_with_the_scan_where_the_index_reaches_less() {
        let dir = std::env::temp_dir().join(format!("rgy_union_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("node_modules")).unwrap();
        std::fs::write(dir.join("alpha.rs"), b"fn alpha() { let x = 1; }\n").unwrap();
        std::fs::write(dir.join("beta.txt"), b"nothing here\n").unwrap();
        // Under a skip-dir: in the name index (and so in scan_paths), but
        // never in the content index.
        std::fs::write(dir.join("node_modules/dep.rs"), b"fn alpha() {}\n").unwrap();
        let root = dir.to_string_lossy().into_owned().into_bytes();
        let src = PortableSource::new(root.clone(), 2);
        let live = Live::new(Index::build(src.enumerate().unwrap(), 0, 1, b"/"));
        let g = Grep::new("alpha", GrepMode::Literal).expect("grep compiles");
        let q = Query { grep: Some("alpha".into()), grep_mode: GrepMode::Literal, ..Default::default() };

        // What the scan finds: alpha.rs and node_modules/dep.rs.
        let scanned = verify(&g, &scan_paths(&root, &live, q.clone_for_scan()), 10);
        let names = |files: &Vec<FileMatches>| -> Vec<String> {
            let mut v: Vec<String> = files.iter().map(|f| String::from_utf8_lossy(&f.path).into_owned()).collect();
            v.sort();
            v.dedup();
            v
        };
        let want = names(&scanned.files);
        assert_eq!(want.len(), 2, "the scan must find both the plain file and the skip-dir one: {want:?}");

        // The content index (built through `wanted`, so no skip-dirs).
        let idx = dir.join("index");
        let mut content = Content::open(idx.clone());
        let docs = wanted(&live, b"", b"/", true, &root);
        assert!(!docs.is_empty());
        let id = content.alloc_id();
        if let Some(seg) = build_segment(&idx, id, &docs, 0..docs.len()) {
            content.push(seg);
        }

        // The index alone misses the skip-dir file: the fast path's raw
        // output is not the answer on its own.
        let indexed_only = names(&content.search(&g, &q).files);
        assert_eq!(indexed_only.len(), 1, "the index holds only the curated file: {indexed_only:?}");

        // The union closes the hole: same answer as the scan.
        let got = names(&answer(&g, &q, &root, &live, b"", Some(&content), 10).files);
        assert_eq!(got, want, "the union must equal the scan, skip-dir match included");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The live-update half of a query's freshness: an edit reaches a query
    /// only after `Live::apply_dir` records it (killing the stale base entry,
    /// overlaying the new size/mtime) and `Content::sync` reindexes what that
    /// changed. This is the loop a `Watch` drives in the daemon; here it is
    /// pinned by hand, without one, so the pipeline the watcher will call is
    /// proven before the OS-specific half lands.
    #[test]
    fn apply_dir_and_sync_reflect_an_edit() {
        let dir = std::env::temp_dir().join(format!("rgy_live_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("edit.rs"), b"fn before() {}\n").unwrap();
        let root = dir.to_string_lossy().into_owned().into_bytes();
        let src = PortableSource::new(root.clone(), 2);
        let mut live = Live::with_root(Index::build(src.enumerate().unwrap(), 0, 1, b"/"), &root);
        let idx = dir.join("index");
        let mut content = Content::open(idx.clone());
        let docs = wanted(&live, b"", b"/", true, &root);
        assert!(!docs.is_empty());
        let id = content.alloc_id();
        if let Some(seg) = build_segment(&idx, id, &docs, 0..docs.len()) {
            content.push(seg);
        }

        let before = Grep::new("before", GrepMode::Literal).expect("grep compiles");
        let after = Grep::new("after", GrepMode::Literal).expect("grep compiles");
        let q = Query::default();
        assert_eq!(content.search(&before, &q).files.len(), 1, "the original text is indexed");
        assert_eq!(content.search(&after, &q).files.len(), 0, "the new text is not there yet");

        // Edit the file; the Live and the content index must both catch up.
        std::fs::write(dir.join("edit.rs"), b"fn after() {}\n").unwrap();
        live.apply_dir(&root, true);
        let rebuilt = content.sync(&live, b"", &[], std::slice::from_ref(&root), &root);

        assert_eq!(content.search(&before, &q).files.len(), 0, "the old text is gone after the sync");
        assert_eq!(content.search(&after, &q).files.len(), 1, "the edit is indexed after the sync (sync rebuilt {rebuilt} docs)");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The other half of the loop: a deleted file must not leak its content
    /// through a query after `apply_dir` + `sync`.
    ///
    /// Two things have to happen for the file to stop answering: the name
    /// index has to stop holding it, and the content index has to stop
    /// holding its text. The Live kills the base's entry — it knows which
    /// tree the base was built from, so it can cross between the base's
    /// root-relative paths and the overlay's openable ones — and
    /// `Content::covers` is the net under both: `lstat` finds nothing, the
    /// file is not covered by the index, and it goes to the scan, which reads
    /// it as absent. The query path (`answer`) therefore returns nothing;
    /// this pins that so the raw index's stale doc can never become a wrong
    /// answer.
    #[test]
    fn apply_dir_and_sync_drop_a_deleted_file_from_the_query_path() {
        let dir = std::env::temp_dir().join(format!("rgy_del_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("gone.rs"), b"fn vanish() {}\n").unwrap();
        let root = dir.to_string_lossy().into_owned().into_bytes();
        let src = PortableSource::new(root.clone(), 2);
        let mut live = Live::with_root(Index::build(src.enumerate().unwrap(), 0, 1, b"/"), &root);
        let idx = dir.join("index");
        let mut content = Content::open(idx.clone());
        let docs = wanted(&live, b"", b"/", true, &root);
        assert!(!docs.is_empty());
        let id = content.alloc_id();
        if let Some(seg) = build_segment(&idx, id, &docs, 0..docs.len()) {
            content.push(seg);
        }

        let g = Grep::new("vanish", GrepMode::Literal).expect("grep compiles");
        let q = Query::default();
        assert_eq!(answer(&g, &q, &root, &live, b"", Some(&content), 10).files.len(), 1, "the file is there and matching before the delete");

        // Delete the file; the query must stop reporting it.
        std::fs::remove_file(dir.join("gone.rs")).unwrap();
        live.apply_dir(&root, true);
        content.sync(&live, b"", &[], std::slice::from_ref(&root), &root);

        assert_eq!(
            answer(&g, &q, &root, &live, b"", Some(&content), 10).files.len(),
            0,
            "a deleted file must not surface its content after the sync"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The Watch thread's core: a batch of change events (each naming the
    /// changed path) drives the same freshness as a hand `apply_dir`. This is
    /// what a `rgy_discovery::Watch` feed calls, pinned without an OS watch.
    #[test]
    fn apply_events_and_sync_reflect_an_edit() {
        let dir = std::env::temp_dir().join(format!("rgy_evt_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("edit.rs"), b"fn before() {}\n").unwrap();
        let root = dir.to_string_lossy().into_owned().into_bytes();
        let src = PortableSource::new(root.clone(), 2);
        let mut live = Live::with_root(Index::build(src.enumerate().unwrap(), 0, 1, b"/"), &root);
        let idx = dir.join("index");
        let mut content = Content::open(idx.clone());
        let docs = wanted(&live, b"", b"/", true, &root);
        assert!(!docs.is_empty());
        let id = content.alloc_id();
        if let Some(seg) = build_segment(&idx, id, &docs, 0..docs.len()) {
            content.push(seg);
        }

        let before = Grep::new("before", GrepMode::Literal).expect("grep compiles");
        let after = Grep::new("after", GrepMode::Literal).expect("grep compiles");
        let q = Query::default();
        assert_eq!(content.search(&before, &q).files.len(), 1);
        assert_eq!(content.search(&after, &q).files.len(), 0);

        // Edit the file; a Watch event names the changed path, and applying it
        // (then syncing) must reach the query.
        std::fs::write(dir.join("edit.rs"), b"fn after() {}\n").unwrap();
        let rec = rgy_discovery::UsnRecord {
            file_reference: 0,
            parent_reference: 0,
            usn: 0,
            reason: 0,
            file_attributes: 0,
            name: dir.join("edit.rs").to_string_lossy().into_owned(),
        };
        live.apply_events(&[rec]);
        content.sync(&live, b"", &[], std::slice::from_ref(&root), &root);

        assert_eq!(content.search(&before, &q).files.len(), 0, "the old text is gone");
        assert_eq!(content.search(&after, &q).files.len(), 1, "the edit is indexed after the Watch event");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The thread body: `run_watch_feed` consumes a feed and keeps the Live
    /// in step. A one-shot mock feed (one batch, then end) stands in for the
    /// OS watch, so the loop is pinned without an OS-specific watcher.
    #[test]
    fn run_watch_feed_applies_what_the_feed_reports() {
        struct OneShot(Option<rgy_discovery::ChangeBatch>);
        impl rgy_discovery::Watch for OneShot {
            fn next_batch(&mut self) -> Result<rgy_discovery::ChangeBatch, String> {
                self.0.take().ok_or_else(|| "feed exhausted".to_string())
            }
        }

        let dir = std::env::temp_dir().join(format!("rgy_feed_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("edit.rs"), b"fn before() {}\n").unwrap();
        let root = dir.to_string_lossy().into_owned().into_bytes();
        let src = PortableSource::new(root.clone(), 2);
        let mut live = Live::with_root(Index::build(src.enumerate().unwrap(), 0, 1, b"/"), &root);
        let idx = dir.join("index");
        let mut content = Content::open(idx.clone());
        let docs = wanted(&live, b"", b"/", true, &root);
        assert!(!docs.is_empty());
        let id = content.alloc_id();
        if let Some(seg) = build_segment(&idx, id, &docs, 0..docs.len()) {
            content.push(seg);
        }

        let before = Grep::new("before", GrepMode::Literal).expect("grep compiles");
        let after = Grep::new("after", GrepMode::Literal).expect("grep compiles");
        let q = Query::default();
        assert_eq!(content.search(&before, &q).files.len(), 1);

        // The feed reports the edit; running it must reflect it.
        std::fs::write(dir.join("edit.rs"), b"fn after() {}\n").unwrap();
        let rec = rgy_discovery::UsnRecord {
            file_reference: 0,
            parent_reference: 0,
            usn: 0,
            reason: 0,
            file_attributes: 0,
            name: dir.join("edit.rs").to_string_lossy().into_owned(),
        };
        let mut feed = OneShot(Some(rgy_discovery::ChangeBatch::Events(vec![rec])));
        assert!(live.run_watch_feed(&mut feed, &root).is_err(), "the feed ends with its error");
        content.sync(&live, b"", &[], std::slice::from_ref(&root), &root);

        assert_eq!(content.search(&before, &q).files.len(), 0, "the old text is gone");
        assert_eq!(content.search(&after, &q).files.len(), 1, "the feed's edit reached the query");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The dir-vs-file split: a directory-granular event (fsevents) names the
    /// folder whose *contents* changed and must re-list that folder, not its
    /// parent -- re-listing the parent would see only the folder's own mtime
    /// and miss the edit inside it.
    #[test]
    fn apply_events_relists_a_directory_event_on_the_directory() {
        let dir = std::env::temp_dir().join(format!("rgy_direvt_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(dir.join("sub/edit.rs"), b"fn before() {}\n").unwrap();
        let root = dir.to_string_lossy().into_owned().into_bytes();
        let src = PortableSource::new(root.clone(), 2);
        let mut live = Live::new(Index::build(src.enumerate().unwrap(), 0, 1, b"/"));
        let idx = dir.join("index");
        let mut content = Content::open(idx.clone());
        let docs = wanted(&live, b"", b"/", true, &root);
        assert!(!docs.is_empty());
        let id = content.alloc_id();
        if let Some(seg) = build_segment(&idx, id, &docs, 0..docs.len()) {
            content.push(seg);
        }

        let before = Grep::new("before", GrepMode::Literal).expect("grep compiles");
        let after = Grep::new("after", GrepMode::Literal).expect("grep compiles");
        let q = Query::default();
        assert_eq!(content.search(&before, &q).files.len(), 1);

        // Edit a file inside sub/; a directory event names sub/ (its contents
        // changed). Only re-listing sub/ (the dir branch) reaches the edit.
        std::fs::write(dir.join("sub/edit.rs"), b"fn after() {}\n").unwrap();
        let rec = rgy_discovery::UsnRecord {
            file_reference: 0,
            parent_reference: 0,
            usn: 0,
            reason: 0,
            file_attributes: rgy_discovery::FILE_ATTRIBUTE_DIRECTORY,
            name: dir.join("sub").to_string_lossy().into_owned(),
        };
        live.apply_events(&[rec]);
        content.sync(&live, b"", &[], std::slice::from_ref(&root), &root);

        assert_eq!(content.search(&before, &q).files.len(), 0, "the old text is gone");
        assert_eq!(content.search(&after, &q).files.len(), 1, "a directory event must re-list the directory, reaching the edit inside it");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The merge must not change a single answer.
    ///
    /// The segments pile up: a batch of files is a segment, and a thousand
    /// batches are a thousand segments to read on every query. The plan
    /// groups them and the merge folds a group into one. A merge rewrites
    /// the postings, renumbers the contents and drops the tombstones, so it
    /// has every chance of answering something different -- and the answer
    /// is what the user sees, so "the merge preserves the answer" is the
    /// only property worth having.
    ///
    /// The fixture is built so the plan has something to group: eight
    /// segments of one tier, each holding its own file.
    #[test]
    fn the_merge_keeps_the_answers() {
        let dir = std::env::temp_dir().join(format!("rgy_merge_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // Eight files, each with its own text: eight contents, eight
        // segments, one per file.
        for i in 0..8 {
            std::fs::write(dir.join(format!("f{i}.rs")), format!("fn f{i}() {{ let x = {i}; }}\n").as_bytes()).unwrap();
        }
        let root = dir.to_string_lossy().into_owned().into_bytes();
        let src = PortableSource::new(root.clone(), 2);
        let live = Live::new(Index::build(src.enumerate().unwrap(), 0, 1, b"/"));
        let idx = dir.join("index");
        let mut content = Content::open(idx.clone());
        let docs = wanted(&live, b"", b"/", true, &root);
        assert_eq!(docs.len(), 8, "the fixture holds eight files");
        // One segment per file: the plan groups eight same-tier segments.
        for i in 0..8 {
            let id = content.alloc_id();
            let seg = build_segment(&idx, id, &docs, i..i + 1).expect("a segment builds");
            content.push(seg);
        }
        assert_eq!(content.segs.len(), 8, "eight batches are eight segments");

        let q = Query::default();
        let names = |c: &Content| -> Vec<String> {
            let mut v: Vec<String> = c
                .segs
                .iter()
                .flat_map(|s| (0..s.ndocs as u32).map(|d| s.path(d).to_vec()))
                .map(|p| String::from_utf8_lossy(&p).into_owned())
                .collect();
            v.sort();
            v
        };
        let before = names(&content);

        // The plan must have something to group, and the merge must fold it.
        let ids = content.merge_plan().expect("eight same-tier segments must group");
        assert_eq!(ids.len(), 8, "the plan groups the whole tier: {ids:?}");
        let id = content.alloc_id();
        let segs = content.segments(&ids);
        let merged = merge(&idx, id, &segs).expect("the merge builds");
        content.replace(&ids, merged);

        assert_eq!(content.segs.len(), 1, "eight segments became one: {ids:?}");
        assert_eq!(names(&content), before, "the merge must not change the documents it holds");

        // And the answers are the same: every file still answers its own
        // text, and nothing else does.
        for i in 0..8 {
            let g = Grep::new(&format!("f{i}"), GrepMode::Literal).expect("grep compiles");
            let got = content.search(&g, &q);
            assert_eq!(got.files.len(), 1, "f{i} answers for itself and nothing else");
            assert!(String::from_utf8_lossy(&got.files[0].path).ends_with(&format!("f{i}.rs")), "f{i} is the file");
        }
        // A word no file holds still answers nothing.
        let none = Grep::new("nothinghere", GrepMode::Literal).expect("grep compiles");
        assert_eq!(content.search(&none, &q).files.len(), 0, "a word no file holds must not answer");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A stale tombstone file must not make the segment claim more dead
    /// documents than it has.
    ///
    /// The dead set is a separate file from the segment, and nothing ties
    /// the two together: a `.dead` left behind by another segment (or by
    /// another index) can have more bits set than the segment has
    /// documents. Counting them all underflows, and the segment then says
    /// it has a negative number of live documents.
    #[test]
    fn a_dead_file_bigger_than_the_segment_is_truncated() {
        let dir = std::env::temp_dir().join(format!("rgy_dead_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.rs"), b"fn a() {}\n").unwrap();
        let root = dir.to_string_lossy().into_owned().into_bytes();
        let src = PortableSource::new(root.clone(), 2);
        let live = Live::new(Index::build(src.enumerate().unwrap(), 0, 1, b"/"));
        let idx = dir.join("index");
        let mut content = Content::open(idx.clone());
        let docs = wanted(&live, b"", b"/", true, &root);
        let id = content.alloc_id();
        let seg = build_segment(&idx, id, &docs, 0..docs.len()).expect("a segment builds");
        content.push(seg);
        let seg_id = content.segs[0].id;

        // A dead file that claims 64 documents gone, for a segment with one.
        std::fs::write(dead_path(&idx, seg_id), u64::MAX.to_le_bytes()).unwrap();
        let reloaded = Content::open(idx.clone());
        let live_docs = reloaded.segs[0].live_docs;
        assert!(
            live_docs <= reloaded.segs[0].ndocs,
            "a segment cannot have more live documents than documents: {live_docs} of {}",
            reloaded.segs[0].ndocs
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A truncated segment must not panic: it reads as "no index".
    ///
    /// The posting offsets are read from the file and nothing checks them
    /// against the file's length. A corrupt segment used to slice past the
    /// end and panic; now the trigram it names reads as empty, which rules
    /// it out -- the search is answered, and it is answered "not here".
    #[test]
    fn a_truncated_segment_is_answered_not_panicked() {
        let dir = std::env::temp_dir().join(format!("rgy_trunc_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.rs"), b"fn a() {}\n").unwrap();
        let root = dir.to_string_lossy().into_owned().into_bytes();
        let src = PortableSource::new(root.clone(), 2);
        let live = Live::new(Index::build(src.enumerate().unwrap(), 0, 1, b"/"));
        let idx = dir.join("index");
        let mut content = Content::open(idx.clone());
        let docs = wanted(&live, b"", b"/", true, &root);
        let id = content.alloc_id();
        let seg = build_segment(&idx, id, &docs, 0..docs.len()).expect("a segment builds");
        content.push(seg);
        let seg_id = content.segs[0].id;
        // The mapping holds the file open: Windows refuses to rewrite it.
        drop(content);

        // Cut the segment's bytes in half: the offsets it holds now point
        // past the end.
        let path = seg_path(&idx, seg_id);
        let bytes = std::fs::read(&path).unwrap();
        std::fs::write(&path, &bytes[..bytes.len() / 2]).unwrap();

        let reloaded = Content::open(idx.clone());
        let g = Grep::new("a", GrepMode::Literal).expect("grep compiles");
        let q = Query::default();
        // The answer is "not here" -- not a panic.
        let _ = reloaded.search(&g, &q);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
/// The plan is what narrows the content index before a file is read, so it
/// has to be implied by the match and nothing more.
#[cfg(test)]
mod plan_tests {
    use super::{Grep, GrepMode, TQ};

    /// A pattern the byte engine refuses is an error, not a search that
    /// found nothing: the caller is told which pattern and why the engine
    /// would not take it.
    #[test]
    fn a_pattern_the_engine_refuses_is_an_error() {
        // A Unicode property: the byte engine has no answer for it.
        let Err(e) = Grep::new(r"\p{L}+", GrepMode::Regex) else {
            panic!("the pattern must be refused");
        };
        assert!(e.contains("unicode property"), "the reason must be said: {e}");
        assert!(e.contains(r"\p{L}+"), "the pattern must be named: {e}");
    }

    /// The same for the other thing a byte engine cannot answer: lookaround.
    #[test]
    fn a_lookaround_is_an_error_and_says_so() {
        let Err(e) = Grep::new(r"foo(?=bar)", GrepMode::Regex) else {
            panic!("the pattern must be refused");
        };
        assert!(e.contains("lookahead"), "the reason must be said: {e}");
        assert!(e.contains("foo(?=bar)"), "the pattern must be named: {e}");
    }

    /// And what the engine does take still works: the refusal is narrow,
    /// and the two tests above would also pass on an engine that refused
    /// everything.
    #[test]
    fn a_pattern_the_engine_takes_still_matches() {
        let g = Grep::new(r"fn\s+list", GrepMode::Regex).expect("the pattern compiles");
        let lines = g.lines_in(b"fn list() {}\nfn other() {}\n");
        assert_eq!(lines.len(), 1, "only the first line matches: {lines:?}");
        assert!(lines[0].1.contains("fn list"), "the line is the match: {lines:?}");
    }

    /// A literal is the case the plan is best at, and it must not fall back
    /// to "everything".
    #[test]
    fn a_literal_pattern_plans_its_trigrams() {
        let g = Grep::new("invoice", GrepMode::Regex).expect("compiles");
        assert!(!matches!(g.plan(), TQ::All), "the plan is not everything");
    }

    /// A class the index cannot name is a plan of "everything": the bytes
    /// are there, but the trigrams are three bytes long and this is one.
    #[test]
    fn a_one_byte_class_plans_everything() {
        let g = Grep::new("[0-9]", GrepMode::Regex).expect("compiles");
        assert!(matches!(g.plan(), TQ::All), "the plan is everything");
    }

    /// The plan and the confirm must be about the same pattern: a caseless
    /// literal still plans its trigrams, folded.
    #[test]
    fn a_caseless_literal_plans_its_trigrams() {
        let g = Grep::new("Invoice", GrepMode::Regex).expect("compiles");
        assert!(!matches!(g.plan(), TQ::All), "the plan is not everything");
    }
}
