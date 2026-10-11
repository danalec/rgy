//! Name search query: parse, filter, match, and the scalar ranker.
//!
//! Transported from fsearch query.rs (parser + scalar scorer half). The
//! SIMD Searcher/NameCache/top-k half is PR #6 and lands later; this
//! module carries everything Query::match_path needs.
//!
//! Merged query syntax (IMPLEMENTATION_PLAN.md 4.1): fsearch's token modes
//! on top of everything-rs's filter vocabulary.

use crate::name_index::{char_bit, start_bit};
use discovery::walk::{KIND_DIR, KIND_FILE, KIND_LINK};

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Mode {
    Fuzzy,
    Exact,
    Prefix,
    Suffix,
}

#[derive(Clone, Debug)]
pub struct Token {
    pub text: Vec<u8>,
    pub mask: u64,
    pub mode: Mode,
    pub negate: bool,
    /// Char classes a typo may leave out of a matching name: all but the
    /// first letter's, or none when the token takes no typos.
    pub loose: u64,
    /// `index::start_bit` of the first letter when the token takes typos.
    pub start: u64,
}

impl Token {
    /// Can a name with mask `m` (`index::name_mask`) match? Cleanly it has
    /// every char class; with a typo, a word in it starts with this token's
    /// first letter and at most one `loose` class is missing. Branchless, so
    /// the scan over every name stays vectorized.
    #[inline(always)]
    fn fits(&self, m: u64) -> bool {
        let miss = self.mask & !m;
        (miss == 0) | ((((miss & !self.loose) | (miss & miss.wrapping_sub(1))) == 0) & (m & self.start != 0))
    }
}
const TYPO_MIN_LEN: usize = 5;
/// What a typo costs, so clean matches of the same quality rank first.
const TYPO_COST: i32 = 60;

fn takes_typos(text: &[u8], mode: Mode) -> bool {
    mode == Mode::Fuzzy && text.len() >= TYPO_MIN_LEN
}

#[derive(Clone, Default)]
pub struct Query {
    pub tokens: Vec<Token>,
    pub kind: Option<u8>,
    /// `type:app`: a folder kind also takes symlinks, since the system apps
    /// in /Applications link into the cryptex (/Applications/Safari.app).
    pub apps: bool,
    /// `type:link` (everything-rs): a reparse point / symlink, orthogonal to
    /// `kind` — `type:dir type:link` asks for a directory junction.
    pub link: bool,
    pub exts: Vec<Vec<u8>>,
    pub scope: Option<Vec<u8>>,
    pub size: (u64, u64),
    /// True once any `size:` bound was given: directories then match **no**
    /// size bound (everything-rs semantics — a directory's entry size is
    /// meaningless, so it fails the filter rather than passing it).
    pub size_set: bool,
    pub mtime: (u32, u32),
    pub name_re: Option<regex::bytes::Regex>,
    pub path_re: Option<regex::bytes::Regex>,
    pub limit: usize,
    /// Content search: handled by the content layer, carried here so one
    /// query string can say everything.
    pub grep: Option<String>,
    pub grep_mode: GrepMode,
    /// Whole-path globs from terms carrying `*`/`?` (everything-rs rule:
    /// only such terms glob; a bare term stays a fsearch token). Each entry
    /// is `(negated, lowercase pattern)`.
    pub globs: Vec<(bool, Vec<u8>)>,
    /// Fail-closed: an unknown `type:` or an uncompilable `re:` rejects the
    /// whole alternative (everything-rs), rather than silently admitting
    /// everything the filter was meant to exclude.
    pub never: bool,
    /// Why `never` was set, for the CLI to print as `warning:` lines.
    pub warnings: Vec<String>,
}

#[derive(Clone, Copy, Default, PartialEq, Debug)]
pub enum GrepMode {
    #[default]
    Literal,
    Regex,
    Symbol,
}

#[rustfmt::skip]
const TYPES: &[(&str, &[&str])] = &[
    ("image", &["png", "jpg", "jpeg", "gif", "heic", "heif", "webp", "tiff", "tif", "bmp", "svg", "raw", "cr2", "cr3", "nef", "arw", "dng", "psd", "ico", "icns", "avif", "jxl"]),
    ("video", &["mp4", "mov", "m4v", "mkv", "avi", "webm", "wmv", "flv", "mpg", "mpeg", "3gp", "hevc"]),
    ("audio", &["mp3", "m4a", "aac", "wav", "flac", "aiff", "aif", "ogg", "opus", "alac", "caf", "mid", "midi", "m4r"]),
    ("doc", &["pdf", "doc", "docx", "pages", "txt", "md", "rtf", "odt", "key", "ppt", "pptx", "numbers", "xls", "xlsx", "csv", "epub", "tex"]),
    ("code", &["rs", "c", "h", "cc", "cpp", "hpp", "m", "mm", "swift", "go", "py", "js", "mjs", "cjs", "ts", "tsx", "jsx", "java", "kt", "rb", "php", "cs", "sh", "zsh", "bash", "fish", "lua", "sql", "html", "css", "scss", "json", "yaml", "yml", "toml", "xml", "vue", "svelte", "zig", "nim", "hs", "ml", "ex", "exs", "erl", "clj", "dart", "r", "jl", "metal", "glsl", "wgsl", "proto", "graphql", "nix"]),
    ("archive", &["zip", "tar", "gz", "tgz", "bz2", "xz", "7z", "rar", "dmg", "pkg", "iso", "zst", "lz4", "xip"]),
    ("font", &["ttf", "otf", "woff", "woff2", "ttc", "dfont"]),
];

impl Query {
    /// Parse the human query language. Plain words are fuzzy tokens;
    /// `'x` exact, `^x` prefix, `x$` suffix, `!x` negate; filters are
    /// `ext: type: kind: in: size: mtime: re: path: limit: grep: regex: sym:`.
    pub fn parse(s: &str, home: &str) -> Result<Query, String> {
        let mut q = Query { size: (0, u64::MAX), mtime: (0, u32::MAX), limit: 50, ..Default::default() };
        for word in split_words(s) {
            if let Some((k, v)) = word.split_once(':')
                && q.filter(k, v, home)?
            {
                continue;
            }
            // A wildcard term is one whole-path glob — never split on `/`
            // (everything-rs: `*/src/*.rs` covers the whole path).
            if word.contains('*') || word.contains('?') {
                q.push_token(&word);
                continue;
            }
            for piece in word.split('/').filter(|p| !p.is_empty()) {
                q.push_token(piece);
            }
        }
        Ok(q)
    }

    pub fn push_token(&mut self, w: &str) {
        let (mut t, mut negate, mut mode) = (w, false, Mode::Fuzzy);
        if let Some(r) = t.strip_prefix('!') {
            (t, negate, mode) = (r, true, Mode::Exact);
        }
        if let Some(r) = t.strip_prefix('\'') {
            (t, mode) = (r, Mode::Exact);
        } else if let Some(r) = t.strip_prefix('^') {
            (t, mode) = (r, Mode::Prefix);
        } else if let Some(r) = t.strip_suffix('$') {
            (t, mode) = (r, Mode::Suffix);
        }
        // Everything-rs: a term carrying `*`/`?` globs the WHOLE PATH; bare
        // terms stay fsearch tokens (the name engine wins, the filter
        // vocabulary is layered on top).
        if t.contains('*') || t.contains('?') {
            self.globs.push((negate, t.bytes().map(|b| b.to_ascii_lowercase()).collect()));
            return;
        }
        // Positive tokens are tracked in a u8 bitset.
        if t.is_empty() || (!negate && self.tokens.iter().filter(|t| !t.negate).count() >= 8) {
            return;
        }
        let text: Vec<u8> = t.bytes().map(|b| b.to_ascii_lowercase()).collect();
        let mask = text.iter().fold(0, |m, &b| m | char_bit(b));
        let (loose, start) = if takes_typos(&text, mode) { (mask & !char_bit(text[0]), start_bit(text[0])) } else { (0, 0) };
        self.tokens.push(Token { text, mask, mode, negate, loose, start });
    }

    /// Apply filter `k:v`; false if `k` is not a filter name.
    pub fn filter(&mut self, k: &str, v: &str, home: &str) -> Result<bool, String> {
        match k {
            "ext" => self.exts.extend(v.split(',').map(|e| e.trim_start_matches('.').to_ascii_lowercase().into_bytes())),
            "type" => {
                for t in v.split(',') {
                    // everything-rs `type:` takes the kind words and its
                    // media aliases on top of fsearch's extension kinds.
                    // Unknown kinds FAIL CLOSED — the alternative matches
                    // nothing and a warning is recorded, because silently
                    // dropping a typo once meant "every path on the volume".
                    match t {
                        "file" => self.kind = Some(KIND_FILE),
                        "dir" | "directory" => self.kind = Some(KIND_DIR),
                        "link" => self.link = true,
                        "app" => {
                            self.kind = Some(KIND_DIR);
                            self.apps = true;
                            self.exts.push(b"app".to_vec());
                        }
                        _ => {
                            let alias = match t {
                                "picture" | "img" => "image",
                                "movie" | "film" => "video",
                                "sound" | "music" => "audio",
                                "zip" | "compressed" => "archive",
                                other => other,
                            };
                            match TYPES.iter().find(|(n, _)| *n == alias) {
                                Some((_, exts)) => {
                                    // Media kinds are extension sets, so
                                    // they imply "a file": a directory can
                                    // never carry an image extension.
                                    self.kind = Some(KIND_FILE);
                                    self.exts.extend(exts.iter().map(|e| e.as_bytes().to_vec()));
                                }
                                None => {
                                    self.never = true;
                                    self.warnings.push(format!("unknown type: {t}"));
                                }
                            }
                        }
                    }
                }
            }
            "kind" => {
                self.kind = Some(match v {
                    "file" | "f" => KIND_FILE,
                    "dir" | "folder" | "d" => KIND_DIR,
                    "link" | "symlink" | "l" => KIND_LINK,
                    _ => return Err(format!("unknown kind {v}")),
                })
            }
            "in" | "dir" => {
                // `dir:` is everything-rs's name for the scope filter.
                let p = v.strip_prefix('~').map_or(v.to_string(), |r| format!("{home}{r}"));
                self.scope = Some(real_path(p).trim_end_matches('/').as_bytes().to_vec());
            }
            "size" => {
                self.size = range(v, parse_size)?;
                self.size_set = true;
            }
            "mtime" | "modified" => {
                // mtime:<7d means "modified within the last 7 days".
                let now = now_secs();
                let (lo, hi) = range(v, parse_age)?;
                self.mtime = (now.saturating_sub(hi.min(now as u64) as u32), now.saturating_sub(lo as u32));
                if hi == u64::MAX {
                    self.mtime.0 = 0;
                }
            }
            // everything-rs `re:` is a PATH regex (and `path:` is its
            // fsearch alias). An uncompilable pattern fails closed — the
            // alternative matches nothing and a warning is recorded — same
            // as a positive `re:` that rejects everything rather than
            // silently admitting everything.
            "re" | "path" => match regex::bytes::Regex::new(&format!("(?i){v}")) {
                Ok(r) => self.path_re = Some(r),
                Err(e) => {
                    self.never = true;
                    self.warnings.push(format!("re:{v}: {e}"));
                }
            },
            "limit" => self.limit = v.parse().map_err(|_| "bad limit")?,
            "grep" | "content" => (self.grep, self.grep_mode) = (Some(v.to_string()), GrepMode::Literal),
            "regex" => (self.grep, self.grep_mode) = (Some(v.to_string()), GrepMode::Regex),
            "sym" | "symbol" => (self.grep, self.grep_mode) = (Some(v.to_string()), GrepMode::Symbol),
            _ => return Ok(false),
        }
        Ok(true)
    }

    /// The parts of a query that pick files for a content scan.
    pub fn clone_for_scan(&self) -> Query {
        Query { grep: None, ..self.clone() }
    }

    #[inline(always)]
    pub fn kind_ok(&self, kind: u8) -> bool {
        match self.kind {
            None => true,
            Some(want) => kind & 3 == want || (self.apps && kind & 3 == KIND_LINK),
        }
    }

    /// Does a full path pass every filter and token? Returns the match score.
    /// Used where there is no dir memo: the overlay and content-search docs.
    pub fn match_path(&self, path: &[u8], kind: u8, size: u64, mtime: u32) -> Option<i32> {
        self.match_path_with(path, kind, size, mtime, |dirs| self.dir_match(dirs))
    }

    /// `match_path`, with the folder half (`dir_match` of the path's folder
    /// part) supplied by the caller, who can memoize it per folder.
    pub fn match_path_with(&self, path: &[u8], kind: u8, size: u64, mtime: u32, dirs: impl FnOnce(&[u8]) -> DirMatch) -> Option<i32> {
        if self.never {
            return None;
        }
        // `type:link` is orthogonal to kind (everything-rs): a reparse
        // point / symlink. On fsearch's walk the symlink kind carries it;
        // a combined `type:dir type:link` needs a backend reparse flag and
        // matches nothing until one exists.
        if self.link && kind & 3 != KIND_LINK {
            return None;
        }
        if let Some(s) = &self.scope
            && !(path.starts_with(s) && path.get(s.len()) == Some(&b'/'))
        {
            return None;
        }
        let cut = path.iter().rposition(|&b| b == b'/').unwrap_or(0);
        let name = &path[cut + 1..];
        // `size:` bounds are statements about files: a directory satisfies
        // NO size bound rather than all of them (everything-rs).
        if name.is_empty()
            || !self.kind_ok(kind)
            || (!self.exts.is_empty() && !ext_ok(name, &self.exts))
            || (self.size_set && kind & 3 == KIND_DIR)
            || size < self.size.0
            || size > self.size.1
            || mtime < self.mtime.0
            || mtime > self.mtime.1
        {
            return None;
        }
        for (neg, pat) in &self.globs {
            if *neg == glob_match(pat, path) {
                return None;
            }
        }
        if self.tokens.is_empty() {
            let re_ok = self.name_re.as_ref().is_none_or(|re| re.is_match(name)) && self.path_re.as_ref().is_none_or(|re| re.is_match(path));
            return re_ok.then_some(0);
        }
        // A hit needs some token in its own name; most paths fail here,
        // before the folders are looked at.
        let mut pos = self.tokens.iter().filter(|t| !t.negate).peekable();
        let m = crate::name_index::name_mask(name);
        if pos.peek().is_some() && !pos.any(|t| t.fits(m) && token_score(name, None, m, t).is_some()) {
            return None;
        }
        if self.tokens.iter().any(|t| t.negate && token_matches(name, t)) {
            return None;
        }
        let d = dirs(&path[..cut]);
        if d.negated {
            return None;
        }
        let pos: Vec<&Token> = self.tokens.iter().filter(|t| !t.negate).collect();
        let all = (1u32 << pos.len()) - 1;
        let (mut got, mut inherited, mut score) = (0u32, 0u32, 0i32);
        for (t, tok) in pos.iter().enumerate() {
            if let Some(s) = token_score(name, None, m, tok) {
                got |= 1 << t;
                score += s;
            } else if let Some(s) = d.best[t] {
                inherited |= 1 << t;
                score += s * 3 / 4;
            }
        }
        if !pos.is_empty() && (got == 0 || (got | inherited) != all) {
            return None;
        }
        if self.name_re.as_ref().is_some_and(|re| !re.is_match(name)) || self.path_re.as_ref().is_some_and(|re| !re.is_match(path)) {
            return None;
        }
        Some(score)
    }

    /// How the folders of a path (`/a/b` for `/a/b/name`) match the tokens:
    /// the best score per positive token, and whether a negated one hits.
    pub fn dir_match(&self, dirs: &[u8]) -> DirMatch {
        let comps = || dirs.split(|&b| b == b'/').filter(|c| !c.is_empty());
        let mut d = DirMatch { negated: false, best: [None; 8] };
        d.negated = self.tokens.iter().any(|t| t.negate && comps().any(|c| token_matches(c, t)));
        for (t, tok) in self.tokens.iter().filter(|t| !t.negate).enumerate() {
            d.best[t] = comps().filter_map(|c| token_score(c, None, !0, tok)).max();
        }
        d
    }
}

/// See `Query::dir_match`.
#[derive(Clone, Copy)]
pub struct DirMatch {
    negated: bool,
    best: [Option<i32>; 8],
}

/// The real path of `p` (the index holds real paths: /etc is
/// /private/etc), or `p` if it has none. Resolving costs a few syscalls
/// (~4 us), as much as a whole search in a small folder, and searches in
/// one folder come in a row (typing): the last answer holds for a second,
/// less than the index takes to see most changes anyway.
fn real_path(p: String) -> String {
    static LAST: std::sync::Mutex<Option<(String, String, std::time::Instant)>> = std::sync::Mutex::new(None);
    let mut last = LAST.lock().unwrap();
    if let Some((from, to, at)) = &*last
        && *from == p
        && at.elapsed() < std::time::Duration::from_secs(1)
    {
        return to.clone();
    }
    let to = std::fs::canonicalize(&p).map_or_else(|_| p.clone(), |c| c.to_string_lossy().into_owned());
    *last = Some((p, to.clone(), std::time::Instant::now()));
    to
}

/// Split on spaces, keeping "double quoted" runs together.
fn split_words(s: &str) -> Vec<String> {
    let (mut out, mut cur, mut quoted) = (Vec::new(), String::new(), false);
    for c in s.chars() {
        match c {
            '"' => quoted = !quoted,
            ' ' if !quoted => {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
            }
            _ => cur.push(c),
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

fn range(v: &str, p: fn(&str) -> Option<u64>) -> Result<(u64, u64), String> {
    let bad = || format!("bad range {v}");
    if let Some(r) = v.strip_prefix(">=").or(v.strip_prefix('>')) {
        return Ok((p(r).ok_or_else(bad)?, u64::MAX));
    }
    if let Some(r) = v.strip_prefix("<=").or(v.strip_prefix('<')) {
        return Ok((0, p(r).ok_or_else(bad)?));
    }
    if let Some((a, b)) = v.split_once("..") {
        return Ok((p(a).ok_or_else(bad)?, p(b).ok_or_else(bad)?));
    }
    let x = p(v).ok_or_else(bad)?;
    Ok((x, x))
}

fn split_unit(s: &str) -> (f64, String) {
    let i = s.find(|c: char| !(c.is_ascii_digit() || c == '.')).unwrap_or(s.len());
    (s[..i].parse().unwrap_or(f64::NAN), s[i..].to_ascii_lowercase())
}

fn parse_size(s: &str) -> Option<u64> {
    let (n, u) = split_unit(s);
    let m = match u.as_str() {
        "" | "b" => 1.0,
        "k" | "kb" => 1e3,
        "m" | "mb" => 1e6,
        "g" | "gb" => 1e9,
        "t" | "tb" => 1e12,
        _ => return None,
    };
    (!n.is_nan()).then_some((n * m) as u64)
}

fn parse_age(s: &str) -> Option<u64> {
    let (n, u) = split_unit(s);
    let m = match u.as_str() {
        "s" => 1.0,
        "m" | "min" => 60.0,
        "h" => 3600.0,
        "" | "d" => 86400.0,
        "w" => 604800.0,
        "mo" => 2592000.0,
        "y" => 31536000.0,
        _ => return None,
    };
    (!n.is_nan()).then_some((n * m) as u64)
}

pub fn now_secs() -> u32 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as u32)
}

#[inline(always)]
pub(crate) fn fold(b: u8) -> u8 {
    b | (((b.wrapping_sub(b'A') < 26) as u8) << 5)
}

#[inline(always)]
fn is_subseq(name: &[u8], q: &[u8]) -> bool {
    let mut j = 0;
    for &b in name {
        if fold(b) == q[j] {
            j += 1;
            if j == q.len() {
                return true;
            }
        }
    }
    false
}

fn find_ci(name: &[u8], q: &[u8]) -> Option<usize> {
    if q.len() > name.len() {
        return None;
    }
    (0..=name.len() - q.len()).find(|&i| name[i..i + q.len()].iter().zip(q).all(|(&a, &b)| fold(a) == b))
}

#[derive(Clone, Copy, PartialEq)]
enum Class {
    Lower,
    Upper,
    Digit,
    Delim,
    Other,
}

#[inline(always)]
fn class(b: u8) -> Class {
    match b {
        b'a'..=b'z' => Class::Lower,
        b'A'..=b'Z' => Class::Upper,
        b'0'..=b'9' => Class::Digit,
        b' ' | b'_' | b'-' | b'.' | b'/' | b'(' | b')' | b'[' | b']' | b',' | b'+' | b'@' => Class::Delim,
        _ => Class::Other,
    }
}

const SCORE_MATCH: i32 = 16;
const GAP_START: i32 = -3;
const GAP_EXT: i32 = -1;
const BONUS_BOUNDARY: i32 = 8;
const BONUS_CAMEL: i32 = 7;
const BONUS_CONSEC: i32 = 4;

#[inline(always)]
fn bonus(prev: Class, cur: Class) -> i32 {
    match (prev, cur) {
        (Class::Delim, c) if c != Class::Delim => BONUS_BOUNDARY,
        (Class::Lower, Class::Upper) | (Class::Lower | Class::Upper, Class::Digit) => BONUS_CAMEL,
        _ => 0,
    }
}

/// fzf-v1 style: leftmost-ending match, shrunk from the right, then scored
/// with boundary/camel/consecutive bonuses. Returns None when no match.
pub fn fuzzy_score(name: &[u8], q: &[u8]) -> Option<i32> {
    fuzzy_score_capped(name, None, q, 100)
}

/// A name's 64 bytes in the index from its start, if it is no longer
/// (`Index::uname_wide`): what follows it is masked off.
type Wide<'a> = Option<&'a [u8; 64]>;

/// `fuzzy_score` with the whole-name/stem/prefix bonus capped at `cap`.
fn fuzzy_score_capped(name: &[u8], wide: Wide, q: &[u8], cap: i32) -> Option<i32> {
    if let Some(s) = prefix_score(name, q, cap) {
        return Some(s);
    }
    #[cfg(not(target_arch = "aarch64"))]
    let _ = wide;
    #[cfg(target_arch = "aarch64")]
    if name.len() <= 64 && q.len() <= 32 {
        if let Some(w) = wide {
            return fuzzy_masked(name, w, q, cap);
        }
        let mut buf = [0u8; 64];
        buf[..name.len()].copy_from_slice(name);
        return fuzzy_masked(name, &buf, q, cap);
    }
    // Leftmost-ending match: jump to each query byte in turn (memchr is
    // SIMD; most names fail on the first or second byte).
    let mut end = 0;
    let mut from = 0;
    for &c in q {
        end = from + find_folded(&name[from..], c)?;
        from = end + 1;
    }
    let next = |k: usize, from: usize, to: usize| Some(from + find_folded(&name[from..=to], q[k])?);
    fuzzy_from(name, q, cap, end, next, |k, to| rfind_folded(&name[..to], q[k]))
}

/// `fuzzy_score_capped` past the prefix test, for a name of at most 64
/// bytes starting `src`: each query byte's places in the name as one
/// 64-bit mask (NEON compares over the whole name at once) stand in for
/// memchr's scans, which cost several times more per name tried.
#[cfg(target_arch = "aarch64")]
fn fuzzy_masked(name: &[u8], src: &[u8; 64], q: &[u8], cap: i32) -> Option<i32> {
    use std::arch::aarch64::*;
    let valid = u64::MAX.checked_shr(64 - name.len() as u32).unwrap_or(0);
    // Safety (here and below): NEON is part of aarch64; loads stay in `src`.
    let v: [uint8x16_t; 4] = std::array::from_fn(|i| unsafe {
        let x = vld1q_u8(src.as_ptr().add(16 * i));
        vorrq_u8(x, vandq_u8(vcltq_u8(vsubq_u8(x, vdupq_n_u8(b'A')), vdupq_n_u8(26)), vdupq_n_u8(0x20)))
    });
    // Byte i of a compare weighed by bit i % 8: pairwise adds then pack
    // the 64 compares into 64 bits, position p in bit p.
    const W: [u8; 16] = [1, 2, 4, 8, 16, 32, 64, 128, 1, 2, 4, 8, 16, 32, 64, 128];
    let weight = unsafe { vld1q_u8(W.as_ptr()) };
    let places = |c: u8| unsafe {
        let c = vdupq_n_u8(c);
        let m = |i: usize| vandq_u8(vceqq_u8(v[i], c), weight);
        let s = vpaddq_u8(vpaddq_u8(m(0), m(1)), vpaddq_u8(m(2), m(3)));
        vgetq_lane_u64(vreinterpretq_u64_u8(vpaddq_u8(s, s)), 0) & valid
    };
    let mut masks = [0u64; 32];
    let (mut end, mut from) = (0, 0);
    for i in 0..q.len() {
        let c = q[i];
        let m = if let Some(prev) = q[..i].iter().rposition(|&x| x == c) { masks[prev] } else { places(c) };
        masks[i] = m;
        let x = if from < 64 { m >> from << from } else { 0 };
        if x == 0 {
            return None;
        }
        end = x.trailing_zeros() as usize;
        from = end + 1;
    }
    let next = |k: usize, from: usize, to: usize| {
        let x = masks[k] >> from << from & u64::MAX >> (63 - to);
        (x != 0).then(|| x.trailing_zeros() as usize)
    };
    let prev = |k: usize, to: usize| {
        let x = masks[k] & u64::MAX.checked_shr(64 - to as u32).unwrap_or(0);
        (x != 0).then(|| 63 - x.leading_zeros() as usize)
    };
    fuzzy_from(name, q, cap, end, next, prev)
}

/// The rest of `fuzzy_score_capped` once the leftmost-ending match ends at
/// `end`: `next(k, from, to)` finds query byte k's first place in
/// from..=to, `prev(k, to)` its last one before `to`.
#[inline(always)]
fn fuzzy_from(
    name: &[u8],
    q: &[u8],
    cap: i32,
    end: usize,
    next: impl Fn(usize, usize, usize) -> Option<usize>,
    prev: impl Fn(usize, usize) -> Option<usize>,
) -> Option<i32> {
    if q.len() == 1 {
        return Some(single_score(name, end, cap));
    }
    // Shrink from the right: the latest start that still ends at `end`.
    let mut start = end + 1;
    for k in (0..q.len()).rev() {
        start = prev(k, start)?;
    }
    // Score the greedy match from `start`, jumping between matched bytes:
    // each gap costs GAP_START then GAP_EXT per byte, a run of consecutive
    // matches carries its strongest boundary bonus along.
    let mut score = 0;
    let (mut at, mut first_bonus) = (start, 0);
    for k in 0..q.len() {
        let mut run = false;
        if k > 0 {
            let last = at;
            at = next(k, last + 1, end)?;
            run = at == last + 1;
            if !run {
                score += GAP_START + (at - last - 2) as i32 * GAP_EXT;
            }
        }
        let prev = if at == 0 { Class::Delim } else { class(name[at - 1]) };
        let mut b = bonus(prev, class(name[at]));
        if run {
            if b >= BONUS_BOUNDARY && b > first_bonus {
                first_bonus = b;
            }
            b = b.max(first_bonus).max(BONUS_CONSEC);
        } else {
            first_bonus = b;
        }
        score += SCORE_MATCH + if k == 0 { b * 2 } else { b };
    }
    // Whole-name and stem matches are what people mean most of the time. A
    // leading dot doesn't count: "zshrc" means ~/.zshrc.
    let off = (name.len() > 1 && name[0] == b'.') as usize;
    let at_stem = || end + 1 == name.iter().rposition(|&b| b == b'.').filter(|&p| p > off).unwrap_or(name.len());
    let placed = if start != off || end + 1 - start != q.len() {
        0
    } else if end + 1 == name.len() {
        100
    } else if at_stem() {
        80
    } else {
        30
    };
    Some(score + placed.min(cap) - (name.len() as i32).min(80) / 3)
}

/// `fuzzy_score_capped` of a name that starts with the query (past a
/// leading dot, unless the query starts with one): the match is the prefix,
/// so the score comes from one pass over it. None if the name does not.
#[inline]
fn prefix_score(name: &[u8], q: &[u8], cap: i32) -> Option<i32> {
    let off = (name.len() > 1 && name[0] == b'.') as usize;
    let end = off + q.len();
    if (off == 1 && q[0] == b'.') || name.len() < end || !name[off..end].iter().zip(q).all(|(&a, &b)| fold(a) == b) {
        return None;
    }
    // The first byte follows a delimiter (or nothing); each next one runs on
    // from the one before it.
    let mut first = bonus(Class::Delim, class(name[off]));
    let mut score = SCORE_MATCH + first * 2;
    for i in off + 1..end {
        let b = bonus(class(name[i - 1]), class(name[i]));
        if b >= BONUS_BOUNDARY && b > first {
            first = b;
        }
        score += SCORE_MATCH + b.max(first).max(BONUS_CONSEC);
    }
    let placed = if end == name.len() {
        100
    } else if name[end] == b'.' && !name[end + 1..].contains(&b'.') {
        80
    } else {
        30
    };
    Some(score + placed.min(cap) - (name.len() as i32).min(80) / 3)
}

/// First byte of `s` that folds to `c` (an already-lowercased query byte).
#[inline(always)]
fn find_folded(s: &[u8], c: u8) -> Option<usize> {
    if c.is_ascii_lowercase() { memchr::memchr2(c, c - 32, s) } else { memchr::memchr(c, s) }
}

/// Last byte of `s` that folds to `c`.
#[inline(always)]
fn rfind_folded(s: &[u8], c: u8) -> Option<usize> {
    if c.is_ascii_lowercase() { memchr::memrchr2(c, c - 32, s) } else { memchr::memrchr(c, s) }
}

/// `fuzzy_score` for a one-byte query matched at `i`: the general scoring
/// loop collapses to one step.
#[inline]
fn single_score(name: &[u8], i: usize, cap: i32) -> i32 {
    let prev = if i == 0 { Class::Delim } else { class(name[i - 1]) };
    let mut score = SCORE_MATCH + bonus(prev, class(name[i])) * 2;
    let off = (name.len() > 1 && name[0] == b'.') as usize;
    if i == off {
        let stem = name.iter().rposition(|&b| b == b'.').filter(|&p| p > off).unwrap_or(name.len());
        score += cap.min(if i + 1 == name.len() {
            100
        } else if i + 1 == stem {
            80
        } else {
            30
        });
    }
    score - (name.len() as i32).min(80) / 3
}

/// Best score for `q` read with one typo (see `one_edit_prefix`) at the
/// start of `name` or of a space-separated word in it: scored as if the
/// right letters had been typed, minus TYPO_COST, and never placed above a
/// prefix: "manif" is "manifest" being typed, not a typo of "manic". `m` is
/// as for `token_score`. Other word starts (`_`, `-`, camelCase) would cost
/// a scan of every name per query, ~10x the price.
fn typo_score(name: &[u8], wide: Wide, m: u64, q: &[u8]) -> Option<i32> {
    let mut best = typo_at(name, wide, (name.len() > 1 && name[0] == b'.') as usize, q);
    if m & char_bit(b' ') != 0 {
        for sp in memchr::memchr_iter(b' ', name) {
            best = best.max(typo_at(name, wide, sp + 1, q));
        }
    }
    best
}

fn typo_at(name: &[u8], wide: Wide, s: usize, q: &[u8]) -> Option<i32> {
    if name.get(s).is_none_or(|&b| fold(b) != q[0]) {
        return None;
    }
    let mut fixed = [0u8; 128];
    let fixed = fixed.get_mut(..one_edit_prefix(&name[s..], q)?)?;
    for (f, &b) in fixed.iter_mut().zip(&name[s..]) {
        *f = fold(b);
    }
    Some(fuzzy_score_capped(name, wide, fixed, 30)? - TYPO_COST)
}

/// How long a prefix of `w` the query `q` spells with exactly one edit (a
/// wrong, extra, missing or swapped letter), if it does. Digits are never
/// edited: "hat_18" is another file than "hat_98", not a typo of it.
fn one_edit_prefix(w: &[u8], q: &[u8]) -> Option<usize> {
    let starts = |w: &[u8], q: &[u8]| w.len() >= q.len() && w.iter().zip(q).all(|(&a, &b)| fold(a) == b);
    // The first difference; none means `q` is a clean prefix, not a typo.
    let i = (0..q.len()).find(|&i| i >= w.len() || fold(w[i]) != q[i])?;
    if q[i].is_ascii_digit() || w.get(i).is_some_and(u8::is_ascii_digit) {
        return None;
    }
    let rest = &q[i + 1..];
    let after = w.get(i + 1..).unwrap_or_default();
    if i + 1 < q.len() && i + 1 < w.len() && fold(w[i]) == q[i + 1] && fold(w[i + 1]) == q[i] && starts(&w[i + 2..], &q[i + 2..]) {
        return Some(q.len());
    }
    if i < w.len() && starts(after, rest) {
        return Some(q.len());
    }
    if starts(&w[i..], rest) {
        return Some(q.len() - 1);
    }
    (i < w.len() && starts(after, &q[i..])).then_some(q.len() + 1)
}

/// Score a token against a name, honoring its mode. `m` is the name's
/// `index::name_mask`, or any superset of it (`!0` when unknown): it only
/// skips work.
#[inline]
fn token_score(name: &[u8], wide: Wide, m: u64, t: &Token) -> Option<i32> {
    match t.mode {
        Mode::Fuzzy if takes_typos(&t.text, t.mode) => {
            let clean = if t.mask & !m == 0 { fuzzy_score_capped(name, wide, &t.text, 100) } else { None };
            clean.max(if m & t.start != 0 { typo_score(name, wide, m, &t.text) } else { None })
        }
        Mode::Fuzzy => fuzzy_score_capped(name, wide, &t.text, 100),
        Mode::Exact => find_ci(name, &t.text).map(|p| 40 + if p == 0 { 30 } else { 0 } - (name.len() as i32).min(80) / 3),
        Mode::Prefix => {
            (name.len() >= t.text.len() && name.iter().zip(&t.text).all(|(&a, &b)| fold(a) == b)).then(|| 60 - (name.len() as i32).min(80) / 3)
        }
        Mode::Suffix => (name.len() >= t.text.len() && name[name.len() - t.text.len()..].iter().zip(&t.text).all(|(&a, &b)| fold(a) == b))
            .then(|| 50 - (name.len() as i32).min(80) / 3),
    }
}

#[inline]
fn token_matches(name: &[u8], t: &Token) -> bool {
    match t.mode {
        Mode::Fuzzy => is_subseq(name, &t.text),
        _ => token_score(name, None, !0, t).is_some(),
    }
}

/// Top k by (score, then lower entry index): a total order, so the result
/// Does the name end in one of `exts` (ASCII-case-insensitive)?
fn ext_ok(name: &[u8], exts: &[Vec<u8>]) -> bool {
    let Some(dot) = name.iter().rposition(|&b| b == b'.') else { return false };
    let e = &name[dot + 1..];
    exts.iter().any(|x| x.len() == e.len() && x.iter().zip(e).all(|(&a, &b)| a == fold(b)))
}

/// Whole-path glob: `*` runs, `?` one byte, ASCII-case-insensitive
/// (everything-rs rule — the pattern was lowercased at parse). `pat` must
/// contain a wildcard (that is the only way a term becomes a glob).
fn glob_match(pat: &[u8], path: &[u8]) -> bool {
    fn go(p: &[u8], s: &[u8]) -> bool {
        match p.first() {
            None => s.is_empty(),
            Some(&b'*') => (0..=s.len()).any(|i| go(&p[1..], &s[i..])),
            Some(&b'?') => !s.is_empty() && go(&p[1..], &s[1..]),
            Some(&c) => !s.is_empty() && fold(s[0]) == c && go(&p[1..], &s[1..]),
        }
    }
    go(pat, path)
}

/// True when the text after a glued `|` re-enters query syntax: another
/// filter token, a `!`/`-` negation, or whitespace / end of input (a
/// trailing `|` is query-OR with an empty side). Anything else is the regex
/// engine's own alternation (`re:(a|b)`, `re:a|b`).
fn starts_query_syntax(s: &str) -> bool {
    if s.is_empty() || s.starts_with(char::is_whitespace) {
        return true;
    }
    let t = s.strip_prefix('!').or_else(|| s.strip_prefix('-')).unwrap_or(s);
    t.split_once(':').is_some_and(|(k, _)| {
        ["ext", "dir", "type", "kind", "in", "size", "mtime", "modified", "re", "path", "limit", "grep", "content", "regex", "sym"]
            .iter()
            .any(|p| k.eq_ignore_ascii_case(p))
    })
}

/// Split a query string on `|`, but not inside a `re:` / `regex:` pattern
/// (everything-rs rule). `re:(a|b)` and `re:a|b` stay one alternative;
/// `re:foo | re:bar`, `re:a\.rs$|re:b\.rs$`, and `ext:rs|ext:txt` split at
/// the query level.
fn split_alternatives(input: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut start = 0usize;
    let mut prev_pipe = None;
    let bytes: Vec<(usize, char)> = input.char_indices().collect();
    for (idx, &(i, ch)) in bytes.iter().enumerate() {
        if ch != '|' {
            continue;
        }
        let after: String = bytes[idx + 1..].iter().map(|&(_, c)| c).collect();
        // Inside a `re:` pattern the `|` belongs to the regex, unless what
        // follows re-enters query syntax.
        let seg = &input[start..i];
        let in_re = seg.rfind(':').is_some_and(|colon| {
            let word_start = seg[..colon].rfind(char::is_whitespace).map(|w| w + 1).unwrap_or(0);
            let key = &seg[word_start..colon];
            let key = key.strip_prefix('!').or_else(|| key.strip_prefix('-')).unwrap_or(key);
            key.eq_ignore_ascii_case("re") || key.eq_ignore_ascii_case("regex")
        }) && !starts_query_syntax(&after);
        if in_re {
            continue;
        }
        parts.push(&input[start..i]);
        start = i + ch.len_utf8();
        prev_pipe = Some(i);
    }
    let _ = prev_pipe;
    parts.push(&input[start..]);
    parts.into_iter().filter(|p| !p.trim().is_empty()).collect()
}

/// The merged query language's top level: `|`-separated alternatives
/// (everything-rs `OrQuery` semantics over fsearch alternatives). Each
/// alternative is an AND of tokens and filters; a hit is any alternative
/// matching. An empty parse matches everything.
#[derive(Default)]
pub struct Alternatives {
    pub alts: Vec<Query>,
    pub warnings: Vec<String>,
}

impl Alternatives {
    pub fn parse(s: &str, home: &str) -> Result<Self, String> {
        let mut out = Alternatives::default();
        for part in split_alternatives(s) {
            let mut q = Query::parse(part, home)?;
            out.warnings.append(&mut q.warnings);
            // Never-alternatives stay in the list: they match nothing, which
            // is the fail-closed answer — dropping them would turn a typo
            // into "matches everything else".
            out.alts.push(q);
        }
        Ok(out)
    }

    /// Best score over the alternatives, or `None` when none match.
    pub fn match_path(&self, path: &[u8], kind: u8, size: u64, mtime: u32) -> Option<i32> {
        if self.alts.is_empty() {
            return Some(0); // empty query matches everything
        }
        self.alts.iter().filter_map(|a| a.match_path(path, kind, size, mtime)).max()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn q(s: &str) -> Query {
        Query::parse(s, "").expect("parses")
    }

    fn hit(query: &str, path: &str, kind: u8, size: u64) -> bool {
        Alternatives::parse(query, "").unwrap().match_path(path.as_bytes(), kind, size, 0).is_some()
    }

    #[test]
    fn type_takes_the_kind_words() {
        assert_eq!(q("type:file").kind, Some(KIND_FILE));
        assert_eq!(q("type:dir").kind, Some(KIND_DIR));
        assert_eq!(q("type:directory").kind, Some(KIND_DIR));
        assert!(q("type:link").link);
    }

    #[test]
    fn media_kinds_imply_a_file_and_map_the_aliases() {
        let a = q("type:image");
        assert_eq!(a.kind, Some(KIND_FILE));
        assert!(ext_ok(b"x.png", &a.exts));
        // everything-rs aliases.
        for alias in ["picture", "img"] {
            let b = q(&format!("type:{alias}"));
            assert_eq!(b.exts, a.exts, "{alias} aliases image");
        }
    }

    #[test]
    fn unknown_type_fails_closed_with_a_warning() {
        let a = q("type:banana");
        assert!(a.never);
        assert!(!a.warnings.is_empty());
        assert!(!hit("type:banana", "/a/b.txt", KIND_FILE, 0));
    }

    #[test]
    fn dir_is_an_alias_of_in() {
        let a = q("dir:foo");
        assert!(a.scope.is_some());
    }

    #[test]
    fn size_bounds_exclude_directories() {
        assert!(!hit("size:>0", "/a/b", KIND_DIR, 5));
        assert!(hit("size:>0", "/a/b.txt", KIND_FILE, 5));
    }

    #[test]
    fn bad_re_fails_closed_and_good_re_matches_the_path() {
        let a = q("re:(unclosed");
        assert!(a.never);
        assert!(!a.warnings.is_empty());
        assert!(hit("re:foo", "/a/foo.txt", KIND_FILE, 0));
        assert!(!hit("re:zzz", "/a/foo.txt", KIND_FILE, 0));
    }

    #[test]
    fn pipe_splits_query_sides_but_not_regex_alternation() {
        assert_eq!(split_alternatives("ext:rs|ext:txt").len(), 2);
        assert_eq!(split_alternatives("re:(a|b)").len(), 1);
        assert_eq!(split_alternatives("re:a|b").len(), 1);
        assert_eq!(split_alternatives("re:foo | re:bar").len(), 2);
        assert!(hit("ext:rs|ext:txt", "/a/b.txt", KIND_FILE, 0));
        assert!(hit("ext:rs|ext:txt", "/a/b.rs", KIND_FILE, 0));
        assert!(!hit("ext:rs|ext:txt", "/a/b.md", KIND_FILE, 0));
    }

    #[test]
    fn glob_terms_match_the_whole_path() {
        // everything-rs: a wildcard term must cover the WHOLE path.
        assert!(hit("*/src/*.rs", "/p/src/main.rs", KIND_FILE, 0));
        assert!(!hit("*/src/*.rs", "/p/lib/main.rs", KIND_FILE, 0));
        assert!(!hit("src/*.rs", "/p/src/main.rs", KIND_FILE, 0));
        // Bare terms stay fsearch tokens (name-scoped), not whole-path globs.
        assert!(hit("main", "/p/src/main.rs", KIND_FILE, 0));
    }

    #[test]
    fn token_modes_survive_the_merge() {
        let a = q("'main");
        assert_eq!(a.tokens[0].mode, Mode::Exact);
        let a = q("^main");
        assert_eq!(a.tokens[0].mode, Mode::Prefix);
        let a = q("main$");
        assert_eq!(a.tokens[0].mode, Mode::Suffix);
        let a = q("!tmp");
        assert!(a.tokens[0].negate);
    }
}
