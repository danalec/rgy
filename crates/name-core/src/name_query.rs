// SPDX-License-Identifier: Apache-2.0

//! Name search query: parse, filter, match, and the ranker.
//!
//! This is a fresh implementation of the query layer: the token modes and
//! the filter vocabulary on top of the name index, the scalar matcher and
//! scorer, and the parallel `Searcher` that turns a query into ranked hits.
//! It is written for `no_std` + `alloc`; the only pieces that need the
//! operating system (the wall clock for the diagnostic thread-locals and the
//! environment switch) stand behind the `std` feature.
//!
//! The public surface is a contract: the data model (`Query`, `Token`,
//! `Mode`, `Axis`, `Axes`), the alternatives list, the ranked `Hit`, the
//! `Searcher` and its `Plan`, and the memo/cache types the live overlay and
//! the incremental session use. The private machinery below is one way to
//! implement it: a class-bitmap candidate narrowing over the index's name
//! bitmaps, a per-zone prefilter, an fzf-v1-style scalar scorer, and a
//! single ranked merge.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;
use core::cmp::Ordering;
use core::fmt;

use crate::live::Live;
use crate::name_index::{
    BM_APP, BM_DOT, BM_DOUBLE, BM_FIRST, BM_SECOND, CLASS_NON_ASCII, CLASSES, Index, ZONE, Zone, char_bit, doubled, mask_of, start_bit,
};
use crate::rank::{RankKey, Score, Tier};
use rgx_core::memchr::{memchr, memchr_iter, memchr2, memrchr, memrchr2};
use rgy_discovery::walk::{FLAG_HIDDEN, FLAG_MOUNT, KIND_DIR, KIND_FILE, KIND_LINK};
use rgy_par::{each_index_prepared, each_owned, for_each_owned, map_collect_into};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Every kind bit the index can carry.
const ALL_KINDS: u32 = 0b1111;
/// A fuzzy token at least this long forgives one typo.
const TYPO_MIN_LEN: usize = 5;
/// What a typo match costs against a clean one.
const TYPO_COST: i32 = 60;
/// The most positive words one query may carry.
const MAX_WORDS: usize = 8;
/// fzf-v1 scorer weights.
const SCORE_MATCH: i32 = 16;
const GAP_START: i32 = -3;
const GAP_EXT: i32 = -1;
const BONUS_BOUNDARY: i32 = 8;
const BONUS_CAMEL: i32 = 7;
const BONUS_CONSEC: i32 = 4;
/// Placement bonuses for a fuzzy match that is a prefix / stem / whole name.
const BONUS_PREFIX: i32 = 100;
const BONUS_STEM: i32 = 80;
const BONUS_WHOLE: i32 = 30;
/// One day, in seconds.
const DAY: u32 = 86_400;
/// The last year a calendar date may name (the u32 epoch's end).
const MAX_YEAR: i64 = 2106;
/// Entries per chunk handed to a scan worker.
const CHUNK: usize = 4096;

/// The media-kind aliases and the extensions each implies.
const TYPES: &[(&str, &[&str])] = &[
    ("image", &["png", "jpg", "jpeg", "gif", "bmp", "webp", "svg", "tiff", "ico", "heic"]),
    ("picture", &["png", "jpg", "jpeg", "gif", "bmp", "webp", "svg", "tiff", "ico", "heic"]),
    ("img", &["png", "jpg", "jpeg", "gif", "bmp", "webp", "svg", "tiff", "ico", "heic"]),
    ("video", &["mp4", "mkv", "avi", "mov", "wmv", "flv", "webm", "m4v"]),
    ("movie", &["mp4", "mkv", "avi", "mov", "wmv", "flv", "webm", "m4v"]),
    ("film", &["mp4", "mkv", "avi", "mov", "wmv", "flv", "webm", "m4v"]),
    ("audio", &["mp3", "wav", "flac", "aac", "ogg", "m4a", "wma"]),
    ("sound", &["mp3", "wav", "flac", "aac", "ogg", "m4a", "wma"]),
    ("music", &["mp3", "wav", "flac", "aac", "ogg", "m4a", "wma"]),
    ("archive", &["zip", "rar", "7z", "tar", "gz", "bz2", "xz", "tgz"]),
    ("zip", &["zip", "rar", "7z", "tar", "gz", "bz2", "xz", "tgz"]),
    ("compressed", &["zip", "rar", "7z", "tar", "gz", "bz2", "xz", "tgz"]),
    ("doc", &["pdf", "doc", "docx", "txt", "md", "odt", "rtf", "xls", "xlsx", "ppt", "pptx"]),
    ("code", &["rs", "c", "h", "cpp", "py", "js", "ts", "go", "java", "sh", "rb", "php", "cs", "swift", "kt"]),
    ("font", &["ttf", "otf", "woff", "woff2"]),
];

// ---------------------------------------------------------------------------
// Public data model
// ---------------------------------------------------------------------------

/// How one word of a query matches a name.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Mode {
    /// fzf-v1 fuzzy subsequence, forgiving one typo when it is long enough.
    Fuzzy,
    /// A case-insensitive substring (`'word`).
    Exact,
    /// The name starts with the word (`^word`).
    Prefix,
    /// The name ends with the word (`word$`).
    Suffix,
    /// A quoted run: the words together, in order, with the spaces between
    /// them (`"invoice pdf"`).
    Phrase,
    /// A whole word of the name (`=invoice`), not a fragment of one.
    Keyword,
}

/// One word of a query, folded for comparison.
#[derive(Clone, Debug)]
pub struct Token {
    /// The folded, lowercased text.
    pub text: Vec<u8>,
    /// The character classes the text needs.
    pub mask: u64,
    pub mode: Mode,
    pub negate: bool,
    /// Character classes a typo may leave out; 0 when typos are not taken.
    pub loose: u64,
    /// `index::start_bit` of the first letter when typos are taken.
    pub start: u64,
    pub axis: Axis,
}

/// Which part of an entry a token is about.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Axis {
    /// The file name (the default).
    Name,
    /// The extension of the name.
    Extension,
    /// A folder of the path.
    Folder,
    /// An attribute of the file (its kind, its flags).
    Attribute,
}

impl Axis {
    /// The axis named before a colon, if `prefix` is one.
    pub fn of_prefix(prefix: &str) -> Option<Axis> {
        match prefix {
            "name" => Some(Axis::Name),
            "ext" | "extension" => Some(Axis::Extension),
            "folder" => Some(Axis::Folder),
            "attr" | "attribute" => Some(Axis::Attribute),
            _ => None,
        }
    }
}

/// The token list split by axis.
#[derive(Clone, Default)]
pub struct Axes<'a> {
    /// Bits = positions in the POSITIVE token list.
    pub name: u32,
    pub ext: u32,
    pub folder: u32,
    pub attrs: Vec<&'a Token>,
    pub neg_attrs: Vec<&'a Token>,
    pub attr: u32,
    /// Every token is on the name axis.
    pub plain: bool,
    /// Name/ext tokens are present.
    pub asks_name: bool,
    /// Compiled attribute: allowed `kind & 3` bits.
    pub kinds: u8,
    pub flags_req: u8,
    pub flags_forbid: u8,
}

impl Axes<'_> {
    #[allow(dead_code)]
    #[inline(always)]
    fn attrs_ok(&self, k: u8) -> bool {
        let bit = 1u8 << (k & 3);
        let attrs = k & (FLAG_HIDDEN | FLAG_MOUNT);
        (self.kinds == 0 || self.kinds & bit != 0) && attrs & self.flags_req == self.flags_req && attrs & self.flags_forbid == 0
    }
}

/// How the content layer is to read a `grep:`/`regex:`/`sym:` pattern.
#[derive(Clone, Copy, Default, PartialEq, Debug)]
pub enum GrepMode {
    #[default]
    Literal,
    Regex,
    Symbol,
}

/// The order the hits come in.
#[derive(Clone, Copy, Default, PartialEq, Debug)]
pub enum SortBy {
    #[default]
    Score,
    Name,
    MTime,
    None,
}

/// A parsed query: an AND of tokens plus the entry filters.
#[derive(Clone)]
pub struct Query {
    pub tokens: Vec<Token>,
    pub kind: Option<u8>,
    /// `type:app` — the folder kind also takes symlinks.
    pub apps: bool,
    /// `type:link` — orthogonal to `kind`.
    pub link: bool,
    pub exts: Vec<Vec<u8>>,
    pub scope: Option<Vec<u8>>,
    pub size: (u64, u64),
    /// Any `size:` bound was given, so directories satisfy NO size bound.
    pub size_set: bool,
    pub mtime: (u32, u32),
    /// A hand-built name regex; `parse` never sets it.
    pub name_re: Option<crate::regex::Regex>,
    /// `re:`/`path:` — a case-insensitive regex over the PATH.
    pub path_re: Option<crate::regex::Regex>,
    pub limit: usize,
    pub sort: SortBy,
    /// Content search text; carried but never run by the name engine.
    pub grep: Option<String>,
    pub grep_mode: GrepMode,
    /// `(negated, lowercase whole-path pattern)` — only `*`/`?` terms.
    pub globs: Vec<(bool, Vec<u8>)>,
    /// Fail closed: an unknown `type:`, a bad `re:`, a bad `sort:`.
    pub never: bool,
    pub warnings: Vec<String>,
}

impl Default for Query {
    /// The parse defaults, not the derive: `size`/`mtime` are unbounded and
    /// `limit` is nonzero, or a hand-built query silently matches nothing.
    fn default() -> Self {
        Query {
            tokens: Vec::new(),
            kind: None,
            apps: false,
            link: false,
            exts: Vec::new(),
            scope: None,
            size: (0, u64::MAX),
            size_set: false,
            mtime: (0, u32::MAX),
            sort: SortBy::Score,
            name_re: None,
            path_re: None,
            limit: 50,
            grep: None,
            grep_mode: GrepMode::Literal,
            globs: Vec::new(),
            never: false,
            warnings: Vec::new(),
        }
    }
}

/// The scores a token can inherit from the folders of a path.
#[derive(Clone, Copy, Default)]
pub struct DirMatch {
    negated: bool,
    best: [Option<i32>; 8],
}

/// A query split on `|`: a hit is ANY alternative matching.
#[derive(Default)]
pub struct Alternatives {
    pub alts: Vec<Query>,
    pub warnings: Vec<String>,
}

/// One ranked result.
#[derive(Clone)]
pub struct Hit {
    /// The ranking key; the order of results is the order of keys.
    pub key: RankKey,
    /// Base entry, or `u32::MAX` for an overlay entry.
    pub idx: u32,
    /// Last change, for an overlay entry.
    pub mtime: u32,
    /// `Some(path)` iff this is an overlay entry.
    pub over: Option<Vec<u8>>,
}

impl Hit {
    /// The score the ranker gave this hit.
    pub fn score(&self) -> i32 {
        self.key.points()
    }
}

/// The index searcher.
pub struct Searcher<'a> {
    pub live: &'a Live,
}

/// Which mechanism the searcher used, for `--explain`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Plan {
    FailClosed,
    Empty,
    Ranked { tentative: bool },
    Driven,
    Selective,
    Full,
}

/// Debug switch: always take the full pass.
pub static FULL_PASS: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

/// The key of a set of scored names: what a later query is compared against.
#[derive(PartialEq)]
pub struct NameKey {
    #[allow(dead_code)]
    tokens: Vec<(Vec<u8>, Mode, bool, Axis)>,
    #[allow(dead_code)]
    exts: Vec<Vec<u8>>,
    #[allow(dead_code)]
    name_re: Option<String>,
    #[allow(dead_code)]
    scope: Option<(usize, usize)>,
}

/// The scored-name result one `Searcher` pass leaves in the cache.
pub struct Scored {
    #[cfg_attr(not(feature = "std"), allow(dead_code))]
    #[allow(dead_code)]
    at: u64,
    #[allow(dead_code)]
    key: NameKey,
    #[allow(dead_code)]
    names: NameTable,
    #[allow(dead_code)]
    memo: crate::sync::OnceCell<Vec<DirMemo>>,
}

/// A cached scope and its scored name set.
type CachedScope = ((usize, usize), Arc<NameSet>);

/// The live overlay's reusable name-search state.
#[derive(Default)]
pub struct NameCache {
    last: crate::sync::Mutex<Option<Arc<Scored>>>,
    #[allow(dead_code)]
    scope: crate::sync::Mutex<Option<CachedScope>>,
    /// The last base-index answer, keyed by the whole query and the live
    /// revision. A session asks a narrowing question after every keystroke,
    /// and the launcher asks the same question again while it waits: neither
    /// should walk the index twice for the same answer. The plan travels
    /// with it so `--explain` still says which mechanism answered.
    result: crate::sync::Mutex<Option<(String, Vec<Hit>, Plan)>>,
}

impl NameCache {
    pub(crate) fn cached(&self, key: &str) -> Option<(Vec<Hit>, Plan)> {
        let g = self.result.lock().expect("the result cache lock");
        g.as_ref().filter(|(k, _, _)| k == key).map(|(_, h, p)| (h.clone(), *p))
    }

    pub(crate) fn remember(&self, key: String, hits: &[Hit], plan: Plan) {
        let mut g = self.result.lock().expect("the result cache lock");
        *g = Some((key, hits.to_vec(), plan));
    }
}

/// A per-directory memo of the best folder scores.
#[derive(Clone, Copy, Default)]
pub struct DirMemo {
    #[allow(dead_code)]
    bits: u32,
    #[allow(dead_code)]
    best: [i16; 4],
}

/// Private companions of the cache types.
#[allow(dead_code)]
struct NameTable {
    #[allow(dead_code)]
    scores: Vec<i32>,
}
#[allow(dead_code)]
struct NameSet {
    #[allow(dead_code)]
    ids: Vec<u32>,
}

// ---------------------------------------------------------------------------
// The wall clock, the byte fold, and the public module functions
// ---------------------------------------------------------------------------

/// The wall clock in seconds.
pub fn now_secs() -> u32 {
    crate::clock::unix_secs()
}

/// The wall clock in milliseconds (the session's idle timers).
pub fn now_millis() -> u64 {
    crate::clock::unix_millis()
}

/// ASCII case-fold of one byte, to lowercase. Non-ASCII bytes are untouched.
pub(crate) fn fold(b: u8) -> u8 {
    b | (((b.wrapping_sub(b'A') < 26) as u8) << 5)
}

/// The fzf-v1 score of `q` in `name`, `None` when it is not a subsequence.
pub fn fuzzy_score(name: &[u8], q: &[u8]) -> Option<i32> {
    fuzzy_score_capped(name, q, 100)
}

/// Release the scratch pools. The pools are per-call here, so this is a no-op
/// kept for the callers that name it.
pub fn trim_pools() {}

/// Drop the cache after a quiet spell.
#[cfg(feature = "std")]
impl NameCache {
    pub fn trim_if_idle(&self, idle: core::time::Duration) {
        let mut g = self.last.lock().expect("the cache lock");
        let quiet = g.as_ref().is_some_and(|s| now_millis().saturating_sub(s.at) >= idle.as_millis() as u64);
        if quiet {
            *g = None;
        }
        drop(g);
        let mut s = self.scope.lock().expect("the scope lock");
        if quiet {
            *s = None;
        }
        trim_pools();
    }
}

#[cfg(not(feature = "std"))]
impl NameCache {
    pub fn trim_if_idle(&self, _idle: core::time::Duration) {}
}

impl NameKey {
    /// The key of `q` over the entry range `scope`.
    pub fn of(q: &Query, scope: Option<(usize, usize)>) -> NameKey {
        NameKey {
            tokens: q.tokens.iter().map(|t| (t.text.clone(), t.mode, t.negate, t.axis)).collect(),
            exts: q.exts.clone(),
            name_re: q.name_re.as_ref().map(|r| r.pattern().to_string()),
            scope,
        }
    }

    /// Whether `self` can only match entries `prev` matched: every previous
    /// word is still here (grown, as a keystroke grows it), and no new
    /// constraint was dropped.
    pub fn narrows(&self, prev: &NameKey) -> bool {
        if self.scope != prev.scope || self.name_re != prev.name_re || self.exts != prev.exts {
            return false;
        }
        if self.tokens.len() < prev.tokens.len() {
            return false;
        }
        prev.tokens
            .iter()
            .all(|(text, mode, neg, axis)| self.tokens.iter().any(|(t, m, n, a)| m == mode && n == neg && a == axis && t.starts_with(text)))
    }
}

impl fmt::Display for Plan {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match *self {
            Plan::FailClosed => "fail-closed",
            Plan::Empty => "empty-scope",
            Plan::Ranked { tentative: false } => "ranked",
            Plan::Ranked { tentative: true } => "ranked (tentative)",
            Plan::Driven => "driven",
            Plan::Selective => "selective",
            Plan::Full => "full",
        };
        f.write_str(s)
    }
}

// ---------------------------------------------------------------------------
// Query: parsing
// ---------------------------------------------------------------------------

impl Query {
    /// Parse `s` into an AND of tokens and filters. `home` is used for `~`.
    pub fn parse(s: &str, home: &str) -> Result<Query, String> {
        let mut q = Query::default();
        for (word, was_quoted) in split_words(s) {
            // 1. A `*`/`?` term is a whole-path glob, unless its prefix is a
            //    pattern filter (whose `*` belongs to the pattern).
            if (word.contains('*') || word.contains('?')) && !pattern_filter_is(&word) {
                q.push_token(&word);
                continue;
            }

            // 2. An axis prefix. `ext:` is also a filter; the filter wins
            //    unless the value carries a matcher marker.
            let colon = word.split_once(':');
            let axis = colon.and_then(|(k, v)| {
                if v.is_empty() || v.contains('/') {
                    return None;
                }
                let marker = v.starts_with('\'') || v.starts_with('^') || v.starts_with('=') || v.starts_with('!') || v.ends_with('$');
                if marker || !filter_is(k) { Axis::of_prefix(k) } else { None }
            });

            // 3. A filter, or an unknown prefix (which fails closed).
            if let (None, Some((k, v))) = (axis, colon) {
                if !filter_is(k) {
                    return Err(format!("unknown filter {k}"));
                }
                q.filter(k, v, home)?;
                continue;
            }

            // 4. Path pieces become tokens on the chosen axis.
            for piece in word.split('/') {
                if !piece.is_empty() {
                    q.push_token_mode(piece, was_quoted, axis);
                }
            }
        }
        Ok(q)
    }

    /// The token list split by axis.
    pub fn axes(&self) -> Axes<'_> {
        let mut out = Axes::default();
        let mut slot = 0u32;
        let mut plain = true;
        for t in &self.tokens {
            if t.negate {
                if t.axis == Axis::Attribute {
                    out.neg_attrs.push(t);
                }
                if t.axis != Axis::Name {
                    plain = false;
                }
                continue;
            }
            let bit = 1u32 << slot.min(31);
            match t.axis {
                Axis::Name => {
                    out.name |= bit;
                    out.asks_name = true;
                }
                Axis::Extension => {
                    out.ext |= bit;
                    out.asks_name = true;
                    plain = false;
                }
                Axis::Folder => {
                    out.folder |= bit;
                    plain = false;
                }
                Axis::Attribute => {
                    out.attr |= bit;
                    out.attrs.push(t);
                    plain = false;
                    compile_attr(t, &mut out);
                }
            }
            slot += 1;
        }
        out.plain = plain;
        out
    }

    /// Add one bare word (used by `parse` for whole-path globs).
    pub fn push_token(&mut self, w: &str) {
        self.push_token_mode(w, false, None);
    }

    fn push_token_mode(&mut self, w: &str, quoted: bool, axis: Option<Axis>) {
        let mut s = w;
        let negate = match s.strip_prefix('!') {
            Some(rest) => {
                s = rest;
                true
            }
            None => false,
        };
        let mode = if quoted {
            Mode::Phrase
        } else if let Some(rest) = s.strip_prefix('\'') {
            s = rest;
            Mode::Exact
        } else if let Some(rest) = s.strip_prefix('^') {
            s = rest;
            Mode::Prefix
        } else if let Some(rest) = s.strip_suffix('$') {
            s = rest;
            Mode::Suffix
        } else if let Some(rest) = s.strip_prefix('=') {
            s = rest;
            Mode::Keyword
        } else {
            Mode::Fuzzy
        };

        // A wildcard term is a whole-path glob, not a name token.
        if s.contains('*') || s.contains('?') {
            let pat: Vec<u8> = s.bytes().map(fold).collect();
            self.globs.push((negate, pat));
            return;
        }
        if s.is_empty() {
            return;
        }
        if !negate {
            let positives = self.tokens.iter().filter(|t| !t.negate).count();
            if positives >= MAX_WORDS {
                self.warnings.push(format!("too many words: '{w}' was ignored (at most 8 words)"));
                return;
            }
        }

        let norm = crate::fold::fold(s.as_bytes());
        let text: Vec<u8> = norm.iter().map(|&b| fold(b)).collect();
        let mask = text.iter().fold(0u64, |m, &b| m | char_bit(b));
        let (loose, start) = if takes_typos(&text, mode) {
            let first = text.first().copied().unwrap_or(0);
            (mask & !char_bit(first), start_bit(first))
        } else {
            (0, 0)
        };
        self.tokens.push(Token { text, mask, mode, negate, loose, start, axis: axis.unwrap_or(Axis::Name) });
    }

    /// Apply one `key:value` filter. Returns `Ok(true)` when it consumed the
    /// word; an unknown key is the caller's business.
    pub fn filter(&mut self, k: &str, v: &str, home: &str) -> Result<bool, String> {
        match k {
            "ext" => {
                for e in v.split(',') {
                    let mut b: Vec<u8> = crate::fold::fold(e.as_bytes()).iter().map(|&x| fold(x)).collect();
                    while b.first() == Some(&b'.') {
                        b.remove(0);
                    }
                    self.exts.push(b);
                }
            }
            "type" => {
                let t = v.to_ascii_lowercase();
                match t.as_str() {
                    "file" => self.kind = Some(KIND_FILE),
                    "dir" | "directory" => self.kind = Some(KIND_DIR),
                    "link" => self.link = true,
                    "app" => {
                        self.kind = Some(KIND_DIR);
                        self.apps = true;
                        self.exts.push(b"app".to_vec());
                    }
                    _ => {
                        if let Some((_, exts)) = TYPES.iter().find(|(name, _)| *name == t.as_str()) {
                            self.kind = Some(KIND_FILE);
                            for e in *exts {
                                self.exts.push(e.as_bytes().to_vec());
                            }
                        } else {
                            self.never = true;
                            self.warnings.push(format!("unknown type: {t}"));
                        }
                    }
                }
            }
            "kind" => {
                let t = v.to_ascii_lowercase();
                match t.as_str() {
                    "file" | "f" => self.kind = Some(KIND_FILE),
                    "dir" | "folder" | "d" => self.kind = Some(KIND_DIR),
                    "link" | "symlink" | "l" => self.kind = Some(KIND_LINK),
                    _ => return Err(format!("unknown kind {v}")),
                }
            }
            "in" | "dir" => {
                let expanded = if let Some(rest) = v.strip_prefix('~') { format!("{home}{rest}") } else { v.to_string() };
                self.scope = Some(real_path(expanded.as_bytes()));
            }
            "size" => {
                self.size = range(v, parse_size)?;
                self.size_set = true;
            }
            "mtime" | "modified" => {
                self.mtime = date_range(v, now_secs())?;
            }
            "re" | "path" => {
                let pat = format!("(?i){v}");
                match crate::regex::Regex::new(&pat) {
                    Ok(re) => self.path_re = Some(re),
                    Err(e) => {
                        self.never = true;
                        self.path_re = Some(crate::regex::Regex::never());
                        self.warnings.push(format!("re:{v}: {e}"));
                    }
                }
            }
            "limit" => {
                self.limit = v.parse::<usize>().map_err(|_| "bad limit".to_string())?;
            }
            "sort" => {
                let t = v.to_ascii_lowercase();
                self.sort = match t.as_str() {
                    "score" => SortBy::Score,
                    "name" => SortBy::Name,
                    "mtime" | "modified" | "date" => SortBy::MTime,
                    "none" | "index" => SortBy::None,
                    _ => {
                        self.never = true;
                        self.warnings.push(format!("sort:{v}: unknown"));
                        SortBy::Score
                    }
                };
            }
            "grep" | "content" => {
                self.grep = Some(v.to_string());
                self.grep_mode = GrepMode::Literal;
            }
            "regex" => {
                self.grep = Some(v.to_string());
                self.grep_mode = GrepMode::Regex;
            }
            "sym" | "symbol" => {
                self.grep = Some(v.to_string());
                self.grep_mode = GrepMode::Symbol;
            }
            _ => return Ok(false),
        }
        Ok(true)
    }

    /// A clone with the content-grep text removed: the name filters stay,
    /// the grep is carried separately by the content layer.
    pub fn clone_for_scan(&self) -> Query {
        Query { grep: None, ..self.clone() }
    }

    /// Whether `kind` satisfies the `type:`/`kind:` selection.
    pub fn kind_ok(&self, kind: u8) -> bool {
        let k = kind & 3;
        match self.kind {
            None => true,
            Some(want) => {
                if k == want {
                    true
                } else {
                    // `type:app` asks for a folder, and a symlink to one is a
                    // folder too (zen-rs's rule).
                    self.apps && want == KIND_DIR && k == KIND_LINK
                }
            }
        }
    }

    /// The scalar/overlay path, with no memo.
    pub fn match_path(&self, path: &[u8], kind: u8, size: u64, mtime: u32) -> Option<i32> {
        self.match_path_with(path, kind, size, mtime, |dirs| self.dir_match(dirs))
    }

    /// The scalar/overlay path with a caller-supplied folder memo.
    pub fn match_path_with(&self, path: &[u8], kind: u8, size: u64, mtime: u32, dirs: impl FnOnce(&[u8]) -> DirMatch) -> Option<i32> {
        let p = crate::fold::fold(path);
        let p: &[u8] = &p;
        let cut = memrchr(b'/', p).unwrap_or(0);
        let dm = dirs(&p[..cut]);
        match_core(self, p, kind, size, mtime, dm)
    }

    /// The folder scores for the directory portion of a path.
    pub fn dir_match(&self, dirs: &[u8]) -> DirMatch {
        let folded = crate::fold::fold(dirs);
        let parts = split_components(&folded);
        let mut out = DirMatch::default();
        let mut slot = 0usize;
        for t in &self.tokens {
            if t.negate {
                let folder_axis = t.axis == Axis::Name || t.axis == Axis::Folder;
                if folder_axis && parts.iter().any(|c| token_matches(c, t)) {
                    out.negated = true;
                }
                continue;
            }
            let s = slot;
            slot += 1;
            if s >= 8 || (t.axis != Axis::Name && t.axis != Axis::Folder) {
                continue;
            }
            for c in &parts {
                if let Some(v) = token_score(c, !0, t) {
                    out.best[s] = Some(out.best[s].map_or(v, |b: i32| b.max(v)));
                }
            }
        }
        out
    }
}

/// Whether `k` is a filter key.
fn filter_is(k: &str) -> bool {
    matches!(
        k.to_ascii_lowercase().as_str(),
        "ext"
            | "type"
            | "kind"
            | "in"
            | "dir"
            | "size"
            | "mtime"
            | "modified"
            | "re"
            | "path"
            | "limit"
            | "sort"
            | "grep"
            | "content"
            | "regex"
            | "sym"
            | "symbol"
    )
}

/// Whether a word's `:`-prefix is a pattern filter (its `*` is not a glob).
fn pattern_filter_is(w: &str) -> bool {
    match w.split_once(':') {
        Some((k, _)) => matches!(k.to_ascii_lowercase().as_str(), "re" | "path" | "grep" | "content" | "regex" | "sym" | "symbol"),
        None => false,
    }
}

/// Split on whitespace; only `"` toggles quoting (there is no backslash
/// escape). Returns `(word, was_quoted)`.
fn split_words(s: &str) -> Vec<(String, bool)> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    let mut was_quoted = false;
    let mut any = false;
    for ch in s.chars() {
        if ch == '"' {
            quoted = !quoted;
            was_quoted = true;
            any = true;
            continue;
        }
        let sep = !quoted && ch.is_whitespace();
        if sep && any {
            out.push((core::mem::take(&mut cur), was_quoted));
            was_quoted = false;
            any = false;
            continue;
        }
        if !sep {
            cur.push(ch);
            any = true;
        }
    }
    if any {
        out.push((cur, was_quoted));
    }
    out
}

/// A `|` in the value of a pattern filter belongs to the pattern, not to
/// the list of alternatives. Which filters those are is the tree's own
/// vocabulary (`re`, `path`, `grep`, `content`, `regex`, `sym`, `symbol`).
fn carries_pattern_filter(s: &str) -> bool {
    s.split_whitespace().any(|w| {
        matches!(
            w.split_once(':').map(|(k, _)| k).unwrap_or("").to_ascii_lowercase().as_str(),
            "re" | "path" | "grep" | "content" | "regex" | "sym" | "symbol"
        )
    })
}

/// Split a query on `|`, but not inside quotes, parentheses, or the value of
/// a pattern filter. `re:(a|b)` and `re:a|b` are one alternative; `alpha|alpha`
/// and `ext:rs|ext:txt` are two; `re:foo | re:bar` is two because the text
/// after the bar re-enters query syntax.
fn split_alternatives(s: &str) -> Vec<String> {
    let b = s.as_bytes();
    let mut out = Vec::new();
    let mut start = 0usize;
    let mut quoted = false;
    let mut depth = 0usize;
    for (i, &c) in b.iter().enumerate() {
        if c == b'"' {
            quoted = !quoted;
            continue;
        }
        if quoted {
            continue;
        }
        match c {
            b'(' => depth += 1,
            b')' => depth = depth.saturating_sub(1),
            b'|' if depth == 0 => {
                let before = &s[start..i];
                let after = &s[i + 1..];
                let in_pattern = carries_pattern_filter(before) && !after.chars().next().is_some_and(char::is_whitespace);
                if !in_pattern || starts_query_syntax(after) {
                    out.push(before.to_string());
                    start = i + 1;
                }
            }
            _ => {}
        }
    }
    out.push(s[start..].to_string());
    out
}

/// Whether the text after a `|` re-enters query syntax.
fn starts_query_syntax(s: &str) -> bool {
    if s.is_empty() || s.chars().next().is_some_and(char::is_whitespace) {
        return true;
    }
    if s.starts_with('!') || s.starts_with('-') {
        return true;
    }
    match s.split_once(':') {
        Some((k, _)) => filter_is(k) || matches!(k.to_ascii_lowercase().as_str(), "name" | "ext" | "extension" | "folder" | "attr" | "attribute"),
        None => false,
    }
}

/// Resolve a scope value: canonicalize it when the platform can, otherwise
/// keep the literal, then fold it and trim trailing separators.
fn real_path(v: &[u8]) -> Vec<u8> {
    let resolved = rgy_discovery::file::canonicalize(v).unwrap_or_else(|| v.to_vec());
    let norm = crate::fold::fold(&resolved);
    let mut out: Vec<u8> = norm.iter().map(|&x| fold(x)).collect();
    while out.last() == Some(&b'/') {
        out.pop();
    }
    out
}

/// Compile one attribute word into the `Axes` bit masks.
fn compile_attr(t: &Token, out: &mut Axes<'_>) {
    let w = t.text.as_slice();
    let (kind, req, forbid) = attr_spec(w);
    out.kinds |= kind;
    if t.negate {
        out.flags_forbid |= req;
    } else {
        out.flags_req |= req;
        out.flags_forbid |= forbid;
    }
}

/// `(allowed kind bits, required flags, forbidden flags)` of an attribute.
fn attr_spec(w: &[u8]) -> (u8, u8, u8) {
    if w == b"dir" || w == b"folder" || w == b"directory" {
        (1 << KIND_DIR, 0, 0)
    } else if w == b"file" {
        (1 << KIND_FILE, 0, 0)
    } else if w == b"link" || w == b"symlink" {
        (1 << KIND_LINK, 0, 0)
    } else if w == b"other" {
        (1 << 3, 0, 0)
    } else if w == b"hidden" {
        (0, FLAG_HIDDEN, 0)
    } else if w == b"mount" || w == b"volume" {
        (0, FLAG_MOUNT, 0)
    } else {
        (0, 0, 0)
    }
}

/// Whether an attribute word matches the kind byte.
fn attr_matches(t: &Token, kind: u8) -> bool {
    let (bits, req, _) = attr_spec(&t.text);
    if bits != 0 {
        return bits & (1 << (kind & 3)) != 0;
    }
    req != 0 && kind & req == req
}

/// Keep the `k` best hits by the score ranking key, discarding the rest.
/// The comparator is exactly `sort_hits`'s `SortBy::Score` order, so a
/// caller that caps here and sorts later gets the same answer as one that
/// sorted everything. `select_nth_unstable_by` is linear, not O(n log n).
fn keep_best(hits: &mut Vec<Hit>, k: usize) {
    if hits.len() <= k {
        return;
    }
    if k == 0 {
        hits.clear();
        return;
    }
    hits.select_nth_unstable_by(k, |a, b| b.key.cmp(&a.key).then_with(|| id_cmp(hit_id(a), hit_id(b))));
    hits.truncate(k);
}

// ---------------------------------------------------------------------------
// Alternatives and merged hits
// ---------------------------------------------------------------------------

impl Alternatives {
    /// Parse `s` into ORed alternatives, hoisting every warning.
    pub fn parse(s: &str, home: &str) -> Result<Self, String> {
        let mut out = Alternatives::default();
        for part in split_alternatives(s) {
            let mut alt = Query::parse(&part, home)?;
            out.warnings.append(&mut alt.warnings);
            out.alts.push(alt);
        }
        Ok(out)
    }

    /// The best score any alternative gives this path.
    pub fn match_path(&self, path: &[u8], kind: u8, size: u64, mtime: u32) -> Option<i32> {
        if self.alts.is_empty() {
            return Some(0);
        }
        self.alts.iter().filter_map(|a| a.match_path(path, kind, size, mtime)).max()
    }
}

enum HitId<'a> {
    Base(u32),
    Over(&'a [u8]),
}

fn hit_id(h: &Hit) -> HitId<'_> {
    match &h.over {
        Some(p) => HitId::Over(p.as_slice()),
        None => HitId::Base(h.idx),
    }
}

fn id_cmp(a: HitId<'_>, b: HitId<'_>) -> Ordering {
    match (a, b) {
        (HitId::Base(x), HitId::Base(y)) => x.cmp(&y),
        (HitId::Over(x), HitId::Over(y)) => x.cmp(y),
        (HitId::Base(_), HitId::Over(_)) => Ordering::Less,
        (HitId::Over(_), HitId::Base(_)) => Ordering::Greater,
    }
}

/// Collapse the hits of several alternatives into one ranked list, keeping
/// the best score per REAL identity (a base entry id or an overlay path).
pub fn merge_hits(all: Vec<Hit>, limit: usize) -> Vec<Hit> {
    let mut all = all;
    all.sort_unstable_by(|a, b| id_cmp(hit_id(a), hit_id(b)));
    let mut keep: Vec<Hit> = Vec::with_capacity(all.len().min(limit.saturating_add(1)));
    let mut i = 0usize;
    while i < all.len() {
        let mut best = i;
        let mut j = i + 1;
        while j < all.len() && id_cmp(hit_id(&all[i]), hit_id(&all[j])) == Ordering::Equal {
            if all[j].key > all[best].key {
                best = j;
            }
            j += 1;
        }
        keep.push(all[best].clone());
        i = j;
    }
    keep.sort_unstable_by(|a, b| b.key.cmp(&a.key).then_with(|| id_cmp(hit_id(a), hit_id(b))));
    keep.truncate(limit);
    keep
}

// ---------------------------------------------------------------------------
// Ranges, sizes, dates
// ---------------------------------------------------------------------------

/// Parse one scalar of a range.
fn range(v: &str, parse: fn(&str) -> Option<u64>) -> Result<(u64, u64), String> {
    let s = v.trim();
    if let Some((a, b)) = s.split_once("..") {
        let lo = parse(a).ok_or_else(|| format!("bad range {v}"))?;
        let hi = parse(b).ok_or_else(|| format!("bad range {v}"))?;
        return Ok((lo, hi));
    }
    let (body, op) = if let Some(r) = s.strip_prefix(">=") {
        (r, 0)
    } else if let Some(r) = s.strip_prefix("<=") {
        (r, 1)
    } else if let Some(r) = s.strip_prefix('>') {
        (r, 2)
    } else if let Some(r) = s.strip_prefix('<') {
        (r, 3)
    } else if let Some(r) = s.strip_prefix('+') {
        (r, 0)
    } else if let Some(r) = s.strip_prefix('-') {
        (r, 1)
    } else {
        (s, 4)
    };
    let x = parse(body).ok_or_else(|| format!("bad range {v}"))?;
    let out = match op {
        0 => (x, u64::MAX),
        1 => (0, x),
        2 => match x.checked_add(1) {
            Some(v) => (v, u64::MAX),
            None => (u64::MAX, 0),
        },
        3 => match x.checked_sub(1) {
            Some(v) => (0, v),
            None => (u64::MAX, 0),
        },
        _ => (x, x),
    };
    // A strict bound that cannot be moved is the empty range.
    if out.0 > out.1 {
        return Ok((u64::MAX, 0));
    }
    Ok(out)
}

/// Parse a size: `""|b` = 1, `k|kb` = 1e3, `m|mb` = 1e6, and so on.
fn parse_size(v: &str) -> Option<u64> {
    let s = v.trim();
    let at = s.find(|c: char| !(c.is_ascii_digit() || c == '.')).unwrap_or(s.len());
    let (num, unit) = s.split_at(at);
    if num.is_empty() {
        return None;
    }
    let (whole, frac) = num.split_once('.').unwrap_or((num, ""));
    let w: u64 = whole.parse().ok()?;
    let f: u64 = if frac.is_empty() { 0 } else { frac.parse().ok()? };
    let mult: u64 = match unit.to_ascii_lowercase().as_str() {
        "" | "b" => 1,
        "k" | "kb" => 1_000,
        "m" | "mb" => 1_000_000,
        "g" | "gb" => 1_000_000_000,
        "t" | "tb" => 1_000_000_000_000,
        _ => return None,
    };
    let pow = 10u64.checked_pow(frac.len() as u32).unwrap_or(u64::MAX);
    Some(w.saturating_mul(mult).saturating_add(f.saturating_mul(mult) / pow.max(1)))
}

/// Parse an age into seconds. `s,m|min,h,""|d,w,mo,y`.
fn parse_age(v: &str) -> Option<u64> {
    let s = v.trim();
    let at = s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len());
    let (num, unit) = s.split_at(at);
    if num.is_empty() {
        return None;
    }
    let n: u64 = num.parse().ok()?;
    let mult: u64 = match unit {
        "" | "d" => DAY as u64,
        "s" => 1,
        "m" | "min" => 60,
        "h" => 3_600,
        "w" => 7 * DAY as u64,
        "mo" => 30 * DAY as u64,
        "y" => 365 * DAY as u64,
        _ => return None,
    };
    n.checked_mul(mult)
}

/// Days from the civil calendar to the unix epoch (Howard Hinnant's civil
/// calendar algorithm), then to seconds.
fn parse_date(v: &str) -> Option<u32> {
    let sep = if v.contains('-') { '-' } else { '/' };
    let mut it = v.split(sep);
    let y: i64 = it.next()?.parse().ok()?;
    let m: u32 = it.next()?.parse().ok()?;
    let d: u32 = it.next()?.parse().ok()?;
    if it.next().is_some() || !(1970..=MAX_YEAR).contains(&y) || !(1..=12).contains(&m) || d == 0 || d > 31 {
        return None;
    }
    let leap = y % 4 == 0 && (y % 100 != 0 || y % 400 == 0);
    let days_in_month = [31, if leap { 29 } else { 28 }, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    if d > days_in_month[(m - 1) as usize] {
        return None;
    }
    let yy = y - i64::from(m <= 2);
    let era = if yy >= 0 { yy } else { yy - 399 } / 400;
    let yoe = yy - era * 400;
    let mp = m as i64 + if m > 2 { -3 } else { 9 };
    let doy = (153 * mp + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    if days < 0 {
        return None;
    }
    let secs = days.checked_mul(DAY as i64)?;
    u32::try_from(secs).ok()
}

/// A calendar date, a date range, an age, or an operator form, as an mtime
/// range. A bad value is an error (the query fails closed at the caller).
fn date_range(v: &str, now: u32) -> Result<(u32, u32), String> {
    let s = v.trim();
    // Named windows.
    match s.to_ascii_lowercase().as_str() {
        "today" | "last_24_hours" => return Ok((now.saturating_sub(DAY), now)),
        "week" | "last_week" => return Ok((now.saturating_sub(7 * DAY), now)),
        "month" | "last_month" => return Ok((now.saturating_sub(30 * DAY), now)),
        "year" | "last_year" => return Ok((now.saturating_sub(365 * DAY), now)),
        "older" => return Ok((0, now.saturating_sub(365 * DAY))),
        _ => {}
    }
    let (body, op) = match s.chars().last() {
        Some('<') | Some('-') => (&s[..s.len() - 1], 1),
        Some('>') | Some('+') => (&s[..s.len() - 1], 2),
        Some('c') | Some('C') => (&s[..s.len() - 1], 3),
        _ => (s, 0),
    };
    if let Some((a, b)) = body.split_once("..") {
        let x = date_point(a, now).ok_or_else(|| format!("bad date {v}"))?;
        let y = date_point(b, now).ok_or_else(|| format!("bad date {v}"))?;
        return Ok((x.min(y), x.max(y)));
    }
    let t = date_point(body, now).ok_or_else(|| format!("bad date {v}"))?;
    Ok(match op {
        1 => (0, t.saturating_sub(1)),
        2 => (t, u32::MAX),
        3 => (t, t.saturating_add(DAY)),
        _ => (t, t.saturating_add(DAY)),
    })
}

/// One endpoint: a calendar date or an age relative to `now`.
fn date_point(v: &str, now: u32) -> Option<u32> {
    let s = v.trim().trim_start_matches('-').trim_start_matches('+');
    if let Some(d) = parse_date(s) {
        return Some(d);
    }
    let age = parse_age(s)?;
    let age = u32::try_from(age).unwrap_or(u32::MAX);
    Some(now.saturating_sub(age))
}

// ---------------------------------------------------------------------------
// Byte matchers and the fzf-v1 scalar scorer
// ---------------------------------------------------------------------------

#[inline]
fn eq_ci(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).all(|(&x, &y)| fold(x) == fold(y))
}

#[inline]
fn starts_with_ci(name: &[u8], q: &[u8]) -> bool {
    name.len() >= q.len() && eq_ci(&name[..q.len()], q)
}

#[inline]
fn ends_with_ci(name: &[u8], q: &[u8]) -> bool {
    name.len() >= q.len() && eq_ci(&name[name.len() - q.len()..], q)
}

/// The first case-insensitive index of `q` in `name`.
fn find_ci(name: &[u8], q: &[u8]) -> Option<usize> {
    if q.is_empty() {
        return Some(0);
    }
    if q.len() > name.len() {
        return None;
    }
    let first = q[0];
    let lo = first.to_ascii_lowercase();
    let up = first.to_ascii_uppercase();
    let mut at = 0usize;
    while at + q.len() <= name.len() {
        let hit = if lo == up { memchr(lo, &name[at..]) } else { memchr2(lo, up, &name[at..]) };
        let off = hit?;
        let p = at + off;
        if p + q.len() <= name.len() && eq_ci(&name[p..p + q.len()], q) {
            return Some(p);
        }
        at = p + 1;
    }
    None
}

/// The extension of a name: the tail after the last dot, not crossing a `/`.
fn ext_of(name: &[u8]) -> &[u8] {
    match memrchr2(b'.', b'/', name) {
        Some(i) if name[i] == b'.' => &name[i + 1..],
        _ => b"",
    }
}

/// The stem of a name: everything before the last dot (past a leading dot).
fn stem_of(name: &[u8]) -> &[u8] {
    let off = usize::from(name.len() > 1 && name[0] == b'.');
    match memrchr(b'.', &name[off..]) {
        Some(i) => &name[off..off + i],
        None => name,
    }
}

/// Whether the extension filters accept this name.
fn ext_ok(exts: &[Vec<u8>], name: &[u8]) -> bool {
    if exts.is_empty() {
        return true;
    }
    let e = ext_of(name);
    exts.iter().any(|x| eq_ci(x, e))
}

/// The byte classes a class of the fzf bonus table belongs to.
#[derive(Clone, Copy, PartialEq)]
enum Class {
    Delim,
    Lower,
    Upper,
    Digit,
    Other,
}

fn class_of(b: u8) -> Class {
    match b {
        b'a'..=b'z' => Class::Lower,
        b'A'..=b'Z' => Class::Upper,
        b'0'..=b'9' => Class::Digit,
        b' ' | b'_' | b'-' | b'.' | b'/' | b'(' | b')' | b'[' | b']' | b',' | b'+' | b'@' => Class::Delim,
        _ => Class::Other,
    }
}

fn is_delim(b: u8) -> bool {
    class_of(b) == Class::Delim
}

/// The bonus of a match at `p`, given the previously matched position.
fn bonus_of(name: &[u8], p: usize, prev: Option<usize>) -> i32 {
    if prev.is_some_and(|pp| p == pp + 1) {
        return BONUS_CONSEC;
    }
    if p == 0 {
        return BONUS_BOUNDARY;
    }
    let before = name[p - 1];
    let cur = name[p];
    if is_delim(before) && !is_delim(cur) {
        return BONUS_BOUNDARY;
    }
    if class_of(before) == Class::Lower && class_of(cur) == Class::Upper {
        return BONUS_CAMEL;
    }
    0
}

/// The positions of a leftmost fuzzy subsequence match.
fn fuzzy_positions(name: &[u8], q: &[u8]) -> Option<Vec<usize>> {
    let mut pos = Vec::with_capacity(q.len());
    let mut at = 0usize;
    for &c in q {
        let want = fold(c);
        let off = name[at..].iter().position(|&b| fold(b) == want)?;
        at += off + 1;
        pos.push(at - 1);
    }
    Some(pos)
}

/// The fzf-v1 score of `q` in `name`, capped at `cap`.
fn fuzzy_score_capped(name: &[u8], q: &[u8], cap: i32) -> Option<i32> {
    if q.is_empty() {
        return Some(0);
    }
    let pos = fuzzy_positions(name, q)?;
    let mut score = 0i32;
    let mut prev = None;
    for (k, &p) in pos.iter().enumerate() {
        let b = bonus_of(name, p, prev);
        score += SCORE_MATCH + if k == 0 { b * 2 } else { b };
        if let Some(pp) = prev {
            let gap = p.saturating_sub(pp + 1);
            score += if gap > 0 { GAP_START + (gap as i32 - 1) * GAP_EXT } else { 0 };
        }
        prev = Some(p);
    }
    let whole = eq_ci(name, q);
    let prefix = !whole && starts_with_ci(name, q);
    let stem = !whole && !prefix && starts_with_ci(stem_of(name), q);
    let placement = if whole {
        BONUS_WHOLE
    } else if prefix {
        BONUS_PREFIX
    } else if stem {
        BONUS_STEM
    } else {
        0
    };
    Some((score + placement).min(cap))
}

/// One-edit forgiveness for a long fuzzy token: try the query with one
/// byte left out (a dropped or transposed character is the common typo).
fn typo_score(name: &[u8], q: &[u8]) -> Option<i32> {
    let mut best: Option<i32> = None;
    for (at, _) in q.iter().enumerate() {
        let mut v = Vec::with_capacity(q.len());
        v.extend_from_slice(&q[..at]);
        v.extend_from_slice(&q[at + 1..]);
        if let Some(s) = fuzzy_score_capped(name, &v, 30) {
            best = Some(best.map_or(s - TYPO_COST, |b| b.max(s - TYPO_COST)));
        }
    }
    best
}

/// The score of a query against a name transliterated into another script.
fn shadow_score(name: &[u8], q: &[u8]) -> Option<i32> {
    use crate::readings::{Script, Shadow};
    let mut best: Option<i32> = None;
    for script in [Script::Romaji, Script::Pinyin, Script::Hiragana, Script::Katakana] {
        let sh = match Shadow::of(name, script) {
            Some(s) => s,
            None => continue,
        };
        let text: Vec<u8> = sh.text.iter().map(|&b| fold(b)).collect();
        if let Some(s) = fuzzy_score_capped(&text, q, 100) {
            best = Some(best.map_or(s, |b| b.max(s)));
        }
    }
    best
}

fn takes_typos(text: &[u8], mode: Mode) -> bool {
    mode == Mode::Fuzzy && text.len() >= TYPO_MIN_LEN
}

impl Token {
    /// The cheap necessary condition a name's class mask has to satisfy.
    /// A token that takes typos may leave its first letter's class out.
    #[allow(dead_code)]
    fn fits(&self, m: u64) -> bool {
        let need = if self.loose != 0 { self.loose } else { self.mask };
        need & !m == 0
    }

    #[allow(dead_code)]
    fn literal(text: Vec<u8>) -> Token {
        let mask = text.iter().fold(0u64, |m, &b| m | char_bit(b));
        Token { text, mask, mode: Mode::Fuzzy, negate: false, loose: 0, start: 0, axis: Axis::Name }
    }

    #[allow(dead_code)]
    fn known_mask(&self, clean: bool, start: bool, spaced: bool) -> u64 {
        let mut m = if clean { self.mask } else { self.loose };
        if start {
            m |= self.start;
        }
        if spaced {
            m |= 1u64 << 38;
        }
        m
    }
}

/// The delimiters of a whole word (`=keyword`).
fn is_word_delim(b: u8) -> bool {
    matches!(b, b' ' | b'_' | b'-' | b'.' | b'/' | b'(' | b')' | b'[' | b']' | b',' | b'+' | b'@')
}

/// The span of `q` as a whole word of `name`.
fn keyword_span(name: &[u8], q: &[u8]) -> Option<(usize, usize)> {
    let mut at = 0usize;
    while at + q.len() <= name.len() {
        let off = find_ci(&name[at..], q)?;
        let p = at + off;
        let before_ok = p == 0 || is_word_delim(name[p - 1]);
        let end = p + q.len();
        let after_ok = end == name.len() || is_word_delim(name[end]);
        if before_ok && after_ok {
            return Some((p, end));
        }
        at = p + 1;
    }
    None
}

fn keyword_score(name: &[u8], q: &[u8]) -> Option<i32> {
    let (start, _) = keyword_span(name, q)?;
    Some(60 - (q.len() as i32) / 3 + i32::from(start == 0) * 10)
}

/// The score of one token against one name (or one folder component).
fn token_score(name: &[u8], m: u64, t: &Token) -> Option<i32> {
    let penalty = (name.len() as i32).min(80) / 3;
    let base = match t.mode {
        Mode::Fuzzy => {
            let mut best: Option<i32> = None;
            let need = if t.loose != 0 { t.loose } else { t.mask };
            if need & !m == 0 {
                best = fuzzy_score_capped(name, &t.text, 100);
            }
            if best.is_none() && t.loose != 0 {
                best = typo_score(name, &t.text);
            }
            best.or_else(|| shadow_score(name, &t.text))
        }
        Mode::Exact | Mode::Phrase => find_ci(name, &t.text).map(|p| 40 + i32::from(p == 0) * 30 - (t.text.len() as i32) / 3),
        Mode::Prefix => starts_with_ci(name, &t.text).then(|| 60 - (t.text.len() as i32) / 3),
        Mode::Suffix => ends_with_ci(name, &t.text).then(|| 50 - (t.text.len() as i32) / 3),
        Mode::Keyword => keyword_score(name, &t.text),
    };
    base.map(|s| s - penalty)
}

/// Whether a (usually negated) token matches a name at all.
fn token_matches(name: &[u8], t: &Token) -> bool {
    let m = mask_of(name);
    if t.axis == Axis::Attribute {
        return false;
    }
    token_score(name, m, t).is_some()
}

/// Whole-path glob: `*` is a run of bytes, `?` is one byte, ASCII-case-
/// insensitive, and the pattern has to cover the whole path.
fn glob_match(pat: &[u8], path: &[u8]) -> bool {
    let mut pi = 0usize;
    let mut si = 0usize;
    let mut star: Option<usize> = None;
    let mut mark = 0usize;
    while si < path.len() {
        if pi < pat.len() && (pat[pi] == b'?' || fold(pat[pi]) == fold(path[si])) {
            pi += 1;
            si += 1;
        } else if pi < pat.len() && pat[pi] == b'*' {
            star = Some(pi);
            pi += 1;
            mark = si;
        } else if let Some(s) = star {
            pi = s + 1;
            mark += 1;
            si = mark;
        } else {
            return false;
        }
    }
    while pi < pat.len() && pat[pi] == b'*' {
        pi += 1;
    }
    pi == pat.len()
}

/// Split folded directory bytes into their components.
fn split_components(dirs: &[u8]) -> Vec<&[u8]> {
    let mut out = Vec::new();
    let mut at = 0usize;
    for i in memchr_iter(b'/', dirs) {
        if i > at {
            out.push(&dirs[at..i]);
        }
        at = i + 1;
    }
    if at < dirs.len() {
        out.push(&dirs[at..]);
    }
    out
}

/// Whether the folded path `p` is strictly under the folded scope `s`.
fn scope_ok(s: &[u8], p: &[u8]) -> bool {
    let parts: Vec<&[u8]> = s.split(|&b| b == b'/' || b == b'\\').filter(|x| !x.is_empty()).collect();
    let path: Vec<&[u8]> = p.split(|&b| b == b'/').filter(|x| !x.is_empty()).collect();
    if parts.is_empty() {
        return true;
    }
    if path.len() <= parts.len() {
        return false;
    }
    parts.iter().zip(path.iter()).all(|(a, b)| eq_ci(a, b))
}

/// The token half of a match: returns the accumulated score, or `None`.
fn score_tokens<F>(q: &Query, name: &[u8], kind: u8, dirs: F) -> Option<i32>
where
    F: FnOnce() -> DirMatch,
{
    // Attribute-axis tokens are about the kind byte, not the name.
    for t in &q.tokens {
        if t.axis == Axis::Attribute && t.negate == attr_matches(t, kind) {
            return None;
        }
    }

    let mask = mask_of(name);
    let mut slots: Vec<Option<u32>> = Vec::with_capacity(q.tokens.len());
    let mut next = 0u32;
    for t in &q.tokens {
        if t.negate {
            slots.push(None);
        } else {
            slots.push(Some(next));
            next += 1;
        }
    }

    let mut score = 0i32;
    let mut got = 0u32;
    let mut missing_name = 0u32;
    let mut need_folder = 0u32;
    let mut has_name_token = false;

    for (i, t) in q.tokens.iter().enumerate() {
        if t.negate {
            continue;
        }
        let bit = 1u32 << slots[i].unwrap_or(0).min(31);
        match t.axis {
            Axis::Name => {
                has_name_token = true;
                // `token_score` applies the class-bitmap check itself and then
                // falls back to a transliteration shadow, so a CJK name can
                // match a romaji/pinyin word whose classes it does not carry.
                match token_score(name, mask, t) {
                    Some(s) => {
                        got |= bit;
                        score += s;
                    }
                    None => missing_name |= bit,
                }
            }
            Axis::Folder => need_folder |= bit,
            Axis::Extension | Axis::Attribute => {}
        }
    }
    if has_name_token && got == 0 {
        return None;
    }

    // Negated tokens: a name match rejects; a folder match is settled by the
    // DirMatch the caller supplies.
    let mut has_dir_neg = false;
    let ext = ext_of(name);
    for t in q.tokens.iter().filter(|t| t.negate) {
        match t.axis {
            Axis::Name => {
                has_dir_neg = true;
                if token_matches(name, t) {
                    return None;
                }
            }
            Axis::Folder => has_dir_neg = true,
            Axis::Extension => {
                if token_matches(ext, t) {
                    return None;
                }
            }
            Axis::Attribute => {}
        }
    }

    // Positive extension tokens are scored on the name's own extension.
    for t in q.tokens.iter().filter(|t| !t.negate && t.axis == Axis::Extension) {
        score += token_score(ext, !0, t)?;
    }

    if missing_name != 0 || need_folder != 0 || has_dir_neg {
        let d = dirs();
        if d.negated {
            return None;
        }
        for (i, t) in q.tokens.iter().enumerate() {
            let s = match slots[i] {
                Some(v) => v,
                None => continue,
            };
            if s >= 8 {
                continue;
            }
            let bit = 1u32 << s;
            if missing_name & bit != 0 && t.axis == Axis::Name {
                score += d.best[s as usize]? * 3 / 4;
            } else if need_folder & bit != 0 && t.axis == Axis::Folder {
                score += d.best[s as usize]?;
            }
        }
    }
    Some(score)
}

/// The path-level gates: scope, whole-path globs and the path regex.
fn path_ok(q: &Query, p: &[u8]) -> bool {
    let scope = q.scope.as_ref().is_none_or(|s| scope_ok(s, p));
    let globs = q.globs.iter().all(|(neg, pat)| *neg != glob_match(pat, p));
    let re = q.path_re.as_ref().is_none_or(|r| r.is_match_bytes(p));
    scope && globs && re
}

/// The full scalar match, shared by `match_path_with` and the overlay path.
fn match_core(q: &Query, p: &[u8], kind: u8, size: u64, mtime: u32, dm: DirMatch) -> Option<i32> {
    if q.never
        || (q.link && kind & 3 != KIND_LINK)
        || !q.kind_ok(kind)
        || (q.size_set && kind & 3 == KIND_DIR)
        || size < q.size.0
        || size > q.size.1
        || mtime < q.mtime.0
        || mtime > q.mtime.1
    {
        return None;
    }
    let cut = memrchr(b'/', p).unwrap_or(0);
    let name = if cut == 0 && p.first() != Some(&b'/') { p } else { &p[cut + 1..] };
    if name.is_empty() || !ext_ok(&q.exts, name) {
        return None;
    }
    if !path_ok(q, p) {
        return None;
    }
    if q.name_re.as_ref().is_some_and(|re| !re.is_match_bytes(name)) {
        return None;
    }
    score_tokens(q, name, kind, || dm)
}

// ---------------------------------------------------------------------------
// matched_chars: the character offsets a query highlights
// ---------------------------------------------------------------------------

/// The character offsets of the matched bytes, snapped to UTF-8 boundaries
/// (and with combining marks folded into their starter).
pub fn matched_chars(name: &[u8], tokens: &[Token]) -> Vec<u32> {
    let folded = crate::fold::fold(name);
    let nm: &[u8] = &folded;
    let mut bytes: Vec<u32> = Vec::new();
    for t in tokens.iter().filter(|t| !t.negate && t.axis == Axis::Name) {
        let q = &t.text;
        if q.is_empty() {
            continue;
        }
        match t.mode {
            Mode::Fuzzy => {
                if let Some(pos) = fuzzy_positions(nm, q) {
                    bytes.extend(pos.iter().map(|&p| p as u32));
                }
            }
            Mode::Exact | Mode::Phrase => {
                if let Some(p) = find_ci(nm, q) {
                    bytes.extend((p..p + q.len()).map(|x| x as u32));
                }
            }
            Mode::Prefix => {
                if starts_with_ci(nm, q) {
                    bytes.extend((0..q.len()).map(|x| x as u32));
                }
            }
            Mode::Suffix => {
                if ends_with_ci(nm, q) {
                    bytes.extend((nm.len() - q.len()..nm.len()).map(|x| x as u32));
                }
            }
            Mode::Keyword => {
                if let Some((a, b)) = keyword_span(nm, q) {
                    bytes.extend((a..b).map(|x| x as u32));
                }
            }
        }
    }
    let mut chars = chars_of_bytes(nm, &bytes);
    chars.sort_unstable();
    chars.dedup();
    chars
}

fn chars_of_bytes(name: &[u8], positions: &[u32]) -> Vec<u32> {
    let mut map = vec![0u32; name.len() + 1];
    if let Ok(text) = core::str::from_utf8(name) {
        let mut at = 0usize;
        let mut ci = 0u32;
        for ch in text.chars() {
            let len = ch.len_utf8();
            let combining = crate::nfc::ccc(ch as u32) != 0;
            let idx = if combining { ci.saturating_sub(1) } else { ci };
            for m in map.iter_mut().skip(at).take(len) {
                *m = idx;
            }
            at += len;
            if !combining {
                ci += 1;
            }
        }
        for (k, m) in map.iter_mut().enumerate().take(name.len() + 1).skip(at) {
            *m = k as u32;
        }
    } else {
        for (i, m) in map.iter_mut().enumerate() {
            *m = i as u32;
        }
    }
    positions.iter().map(|&p| map[(p as usize).min(name.len())]).collect()
}

// ---------------------------------------------------------------------------
// The Searcher
// ---------------------------------------------------------------------------

/// Scratch buffers one scan worker owns.
#[derive(Default)]
struct Scratch {
    path_buf: Vec<u8>,
    dirs_buf: Vec<u8>,
}

/// The diagnostic state, behind `std`.
#[cfg(feature = "std")]
mod diag {
    use super::Plan;
    use alloc::vec::Vec;
    use core::cell::{Cell, RefCell};
    std::thread_local! {
        static PLAN: Cell<Option<Plan>> = const { Cell::new(None) };
        static ATTEMPTS: RefCell<Vec<&'static str>> = const { RefCell::new(Vec::new()) };
    }
    pub fn note_plan(p: Plan) {
        PLAN.with(|c| c.set(Some(p)));
    }
    pub fn last_plan() -> Option<Plan> {
        PLAN.with(|c| c.get())
    }
    pub fn attempts_reset() {
        ATTEMPTS.with(|a| a.borrow_mut().clear());
    }
    pub fn note_attempt(s: &'static str) {
        ATTEMPTS.with(|a| a.borrow_mut().push(s));
    }
    pub fn rejected_attempts() -> Vec<&'static str> {
        ATTEMPTS.with(|a| a.borrow().clone())
    }
}

#[cfg(not(feature = "std"))]
mod diag {
    use super::Plan;
    use alloc::vec::Vec;
    pub fn note_plan(_p: Plan) {}
    pub fn last_plan() -> Option<Plan> {
        None
    }
    pub fn attempts_reset() {}
    pub fn note_attempt(_s: &'static str) {}
    pub fn rejected_attempts() -> Vec<&'static str> {
        Vec::new()
    }
}

/// Whether the environment asks for the ranked plan by default.
#[cfg(feature = "std")]
fn force_ranked() -> bool {
    std::env::var_os("RGY_FORCE_RANKED").is_some()
}
#[cfg(not(feature = "std"))]
fn force_ranked() -> bool {
    false
}

impl Searcher<'_> {
    /// The plan the last search reported (the `--explain` diagnostic).
    #[cfg(feature = "std")]
    pub fn last_plan() -> Option<Plan> {
        diag::last_plan()
    }
    #[cfg(not(feature = "std"))]
    pub fn last_plan() -> Option<Plan> {
        diag::last_plan()
    }

    /// The candidate paths the last search rejected before scoring.
    #[cfg(feature = "std")]
    pub fn rejected_attempts() -> Vec<&'static str> {
        diag::rejected_attempts()
    }
    #[cfg(not(feature = "std"))]
    pub fn rejected_attempts() -> Vec<&'static str> {
        diag::rejected_attempts()
    }

    /// The base-entry range a scope selects, or `None` when it selects no
    /// entry at all (fail closed with `Plan::Empty`).
    pub fn scope_range(&self, q: &Query) -> Option<(usize, usize)> {
        let base = &self.live.base;
        let s = match &q.scope {
            Some(v) => v,
            None => return Some((1, base.n)),
        };
        let e = base.lookup(s)?;
        let d = base.dir_of(e)?;
        let lo = base.dir_start()[d as usize] as usize;
        let hi = base.dir_end()[d as usize] as usize;
        Some((lo, hi))
    }

    /// The query's hits, sorted and capped.
    pub fn search(&self, q: &Query) -> Vec<Hit> {
        diag::attempts_reset();
        if q.never {
            diag::note_plan(Plan::FailClosed);
            return Vec::new();
        }
        // Only `limit` results can survive the final truncate. Keeping just
        // that many from the base index is enough: the overlay is merged
        // afterwards and can displace only the worst of them. A top-k is
        // safe only when the ranking key is the final sort criterion --
        // `--sort name`/`mtime` would need every hit to order correctly.
        let cap = if q.sort == SortBy::Score && q.limit < usize::MAX { Some(q.limit) } else { None };
        let mut hits = self.search_base(q, cap);
        hits.extend(self.search_overlay(q));
        self.sort_hits(&mut hits, q.sort);
        hits.truncate(q.limit);
        hits
    }

    /// Every hit, with no cap.
    pub fn search_uncapped(&self, q: &Query) -> Vec<Hit> {
        let wide = Query { limit: usize::MAX, ..q.clone() };
        self.search(&wide)
    }

    /// How many entries the query matches.
    pub fn count(&self, q: &Query) -> usize {
        self.search_uncapped(q).len()
    }

    /// The OR of several alternatives, merged by real identity and capped.
    pub fn search_alternatives(&self, alts: &[Query], limit: usize) -> Vec<Hit> {
        let mut all = Vec::new();
        for q in alts {
            all.extend(self.search_uncapped(q));
        }
        merge_hits(all, limit)
    }

    /// How many DISTINCT entries a set of alternatives matches.
    pub fn count_alternatives(&self, alts: &[Query]) -> usize {
        let mut all = Vec::new();
        for q in alts {
            all.extend(self.search_uncapped(q));
        }
        merge_hits(all, usize::MAX).len()
    }

    /// The two halves of a search, before sorting and capping (the session).
    /// The session needs every hit, so this path never applies a top-k.
    pub(crate) fn search_split(&self, q: &Query) -> (Vec<Hit>, Vec<Hit>) {
        (self.search_base(q, None), self.search_overlay(q))
    }

    /// Re-score a previous result set for a narrowing query (the session).
    pub(crate) fn rerank_entries(&self, q: &Query, entries: &[(u32, u32)]) -> Vec<Hit> {
        diag::attempts_reset();
        if q.never {
            return Vec::new();
        }
        let base = &self.live.base;
        let allowed = candidate_names(q, base);
        let flags = zone_flags(q, base);
        let mut st = Scratch::default();
        let mut hits = Vec::new();
        for &(idx, _) in entries {
            if let Some(h) = self.score_base_entry(q, &mut st, idx as usize, allowed.as_deref(), &flags) {
                hits.push(h);
            }
        }
        self.sort_hits(&mut hits, q.sort);
        hits
    }

    /// The order the hits come in (`sort:`).
    pub(crate) fn sort_hits_public(&self, hits: &mut [Hit], sort: SortBy) {
        self.sort_hits(hits, sort);
    }

    fn sort_hits(&self, hits: &mut [Hit], sort: SortBy) {
        match sort {
            SortBy::Score => {
                hits.sort_unstable_by(|a, b| b.key.cmp(&a.key).then_with(|| id_cmp(hit_id(a), hit_id(b))));
            }
            SortBy::MTime => {
                hits.sort_unstable_by(|a, b| b.mtime.cmp(&a.mtime).then_with(|| b.key.cmp(&a.key)));
            }
            SortBy::Name => {
                let mut buf = Vec::new();
                let mut keys: Vec<(usize, Vec<u8>)> = hits
                    .iter()
                    .enumerate()
                    .map(|(i, h)| {
                        let p = self.hit_path(h, &mut buf);
                        (i, p)
                    })
                    .collect();
                keys.sort_unstable_by(|a, b| a.1.cmp(&b.1).then_with(|| a.0.cmp(&b.0)));
                let copy = hits.to_vec();
                for (slot, (orig, _)) in keys.iter().enumerate() {
                    hits[slot] = copy[*orig].clone();
                }
            }
            SortBy::None => {}
        }
    }

    fn hit_path(&self, h: &Hit, buf: &mut Vec<u8>) -> Vec<u8> {
        match &h.over {
            Some(p) => p.clone(),
            None => {
                self.live.base.path(h.idx as usize, buf);
                buf.clone()
            }
        }
    }

    fn plan_for(&self, q: &Query, allowed: Option<&[u64]>) -> Plan {
        if q.never {
            return Plan::FailClosed;
        }
        if q.scope.is_some() && self.scope_range(q).is_none() {
            return Plan::Empty;
        }
        if FULL_PASS.load(core::sync::atomic::Ordering::Relaxed) {
            return Plan::Full;
        }
        if force_ranked() {
            return Plan::Ranked { tentative: false };
        }
        let positives: Vec<&Token> = q.tokens.iter().filter(|t| !t.negate && t.axis == Axis::Name).collect();
        let others = q.tokens.len() > positives.len();
        if others || positives.len() > 1 {
            return Plan::Driven;
        }
        let single = positives.len() == 1 && q.scope.is_none() && q.globs.is_empty() && q.path_re.is_none();
        if single {
            let base = &self.live.base;
            let est = allowed.map(|bits| bits.iter().map(|w| w.count_ones() as usize).sum()).unwrap_or(base.u);
            return if (est as u64).saturating_mul(60) <= base.n as u64 { Plan::Selective } else { Plan::Full };
        }
        Plan::Full
    }

    /// A stable key for the base-index answer. Queries whose result depends
    /// on a compiled pattern, a glob, a scope restriction or fail-closed
    /// warnings are left out of the cache: their key would have to encode
    /// more than the parsed query carries.
    fn cache_key(&self, q: &Query, cap: Option<usize>) -> Option<String> {
        if q.name_re.is_some() || q.path_re.is_some() || !q.globs.is_empty() || q.scope.is_some() || q.never || !q.warnings.is_empty() {
            return None;
        }
        Some(format!(
            "{}/{}/{}/{:?}/{}/{}/{:?}/{:?}/{}/{:?}/{}/{:?}/{:?}/{:?}/{:?}/{:?}",
            self.live.revision,
            self.live.dead_count,
            self.live.flat.items.len(),
            q.kind,
            q.apps,
            q.link,
            q.exts,
            q.size,
            q.size_set,
            q.mtime,
            q.limit,
            q.sort,
            q.grep,
            q.grep_mode,
            q.tokens,
            cap,
        ))
    }

    fn search_base(&self, q: &Query, cap: Option<usize>) -> Vec<Hit> {
        // The two gates that need no scan are cheap and are checked before
        // everything else, cache included.
        if q.never {
            diag::note_plan(Plan::FailClosed);
            return Vec::new();
        }
        if q.scope.is_some() && self.scope_range(q).is_none() {
            diag::note_plan(Plan::Empty);
            diag::note_attempt("the scope selects no entry");
            return Vec::new();
        }
        // A cache hit must not pay for the plan: choosing a plan reads the
        // class bitmaps, which on a five-million-name index costs more than
        // the scan the cache was built to avoid.
        let key = self.cache_key(q, cap);
        if let Some(k) = key.as_deref()
            && let Some((hits, plan)) = self.live.names_cache.cached(k)
        {
            diag::note_plan(plan);
            return hits;
        }
        // On a miss the bitmaps are built ONCE and shared: the plan counts
        // their bits and the scan uses them to reject entries early. The
        // shape this replaces recomputed them in both places on every cold
        // query -- twice the walk over every class bitmap.
        let allowed = candidate_names(q, &self.live.base);
        let plan = self.plan_for(q, allowed.as_deref());
        diag::note_plan(plan);
        if plan == Plan::Empty {
            diag::note_attempt("the scope selects no entry");
            return Vec::new();
        }
        if plan == Plan::FailClosed {
            return Vec::new();
        }
        let hits = self.scan(q, allowed.as_deref(), cap);
        if let Some(k) = key {
            self.live.names_cache.remember(k, &hits, plan);
        }
        self.cache_result(q, plan);
        hits
    }

    /// One parallel pass over the base entries, narrowed by class bitmaps
    /// and zones. When `cap` is set, each worker keeps only its best `cap`
    /// hits and the merge keeps the best `cap` overall.
    ///
    /// A postings walk over the admitted distinct names was measured here
    /// and is deliberately NOT used: at 5M names it took 11 ms cold against
    /// this pass's 3.8 ms. The class bitmaps admit too many names for the
    /// extra `name_ents_of` allocations to beat a parallel per-entry
    /// rejection whose bitmap check is the first thing it does.
    fn scan(&self, q: &Query, allowed: Option<&[u64]>, cap: Option<usize>) -> Vec<Hit> {
        let base = &self.live.base;
        let (lo, hi) = self.scope_range(q).unwrap_or((1, base.n));
        let flags = zone_flags(q, base);
        if hi <= lo {
            return Vec::new();
        }
        let n = hi.saturating_sub(lo);
        let nchunks = n.div_ceil(CHUNK);
        let groups: Vec<(usize, usize)> = each_index_prepared(
            nchunks,
            crate::scan_priority::apply,
            || (),
            |_, k| {
                let a = lo + k * CHUNK;
                (a, (a + CHUNK).min(hi))
            },
        );
        let parts: Vec<Vec<Hit>> = each_owned(groups, Scratch::default, |st, _, (a, b)| {
            let mut out = Vec::new();
            for i in a..b {
                if let Some(h) = self.score_base_entry(q, st, i, allowed, &flags) {
                    out.push(h);
                }
            }
            if let Some(k) = cap {
                keep_best(&mut out, k);
            }
            out
        });
        let mut hits = Vec::new();
        for part in parts {
            hits.extend(part);
        }
        if let Some(k) = cap {
            keep_best(&mut hits, k);
        }
        hits
    }

    fn score_base_entry(&self, q: &Query, st: &mut Scratch, i: usize, allowed: Option<&[u64]>, flags: &[bool]) -> Option<Hit> {
        let base = &self.live.base;
        if i == 0 || i >= base.n || self.live.is_dead(i as u32) {
            return None;
        }
        if !flags.get(i / ZONE).copied().unwrap_or(true) {
            return None;
        }
        let kind = base.kind()[i];
        let size = base.size_of(i);
        let mtime = base.mtime()[i];
        if q.never
            || (q.link && kind & 3 != KIND_LINK)
            || !q.kind_ok(kind)
            || (q.size_set && kind & 3 == KIND_DIR)
            || size < q.size.0
            || size > q.size.1
            || mtime < q.mtime.0
            || mtime > q.mtime.1
        {
            return None;
        }
        let nid = base.ent_name()[i] as usize;
        if allowed.is_some_and(|bits| bits.get(nid >> 6).is_none_or(|w| w & (1u64 << (nid & 63)) == 0)) {
            return None;
        }

        // The full path is only needed for scope/glob/regex/folder work.
        let folded_path = if needs_path(q) {
            base.path(i, &mut st.path_buf);
            Some(crate::fold::fold(&st.path_buf).into_owned())
        } else {
            None
        };
        let raw = crate::fold::fold(base.name(i));
        let name: &[u8] = match &folded_path {
            Some(p) => match memrchr(b'/', p) {
                Some(c) => &p[c + 1..],
                None => p.as_slice(),
            },
            None => raw.as_ref(),
        };
        if name.is_empty() || !ext_ok(&q.exts, name) {
            return None;
        }
        if folded_path.as_deref().is_some_and(|p| !path_ok(q, p)) {
            return None;
        }
        if q.name_re.as_ref().is_some_and(|re| !re.is_match_bytes(name)) {
            return None;
        }
        let dirs = || {
            ancestor_dirs(base, i as u32, &mut st.dirs_buf);
            q.dir_match(&st.dirs_buf)
        };
        let s = score_tokens(q, name, kind, dirs)?;
        let score = s + base.rank_adj()[i] as i32 + rank_tweaks(base, nid, kind, mtime);
        let key = RankKey::for_entry(kind, Score::from_points(score), i as u32);
        Some(Hit { key, idx: i as u32, mtime, over: None })
    }

    /// The overlay half: entries added since the last compaction.
    fn search_overlay(&self, q: &Query) -> Vec<Hit> {
        let items: Vec<(Vec<u8>, crate::live::OEnt)> =
            self.live.flat.items.iter().filter(|(p, _)| !p.is_empty()).map(|(p, e)| (p.clone(), *e)).collect();
        let sink = crate::sync::Mutex::new(Vec::new());
        for_each_owned(items, |_, (path, e)| {
            let base_score = q.match_path_with(&path, e.kind, e.size, e.mtime, |dirs| q.dir_match(dirs));
            if let Some(s) = base_score {
                let score = s + e.prior as i32 + recency_tweak(e.mtime);
                let key = RankKey::new(Tier::Normal, Tier::Normal, Tier::Normal, Score::from_points(score), RankKey::NO_ENTRY);
                let hit = Hit { key, idx: RankKey::NO_ENTRY, mtime: e.mtime, over: Some(path) };
                let _ = sink.lock().map(|mut g| g.push(hit));
            }
        });
        let mut g = sink.lock().expect("the overlay sink");
        core::mem::take(&mut *g)
    }

    fn cache_result(&self, q: &Query, _plan: Plan) {
        let scored = Scored {
            at: now_millis(),
            key: NameKey::of(q, self.scope_range(q)),
            names: NameTable { scores: Vec::new() },
            memo: crate::sync::OnceCell::new(),
        };
        if let Ok(mut g) = self.live.names_cache.last.lock() {
            *g = Some(Arc::new(scored));
        }
    }
}

/// Whether a query needs the whole folded path, not just the name.
fn needs_path(q: &Query) -> bool {
    q.scope.is_some() || !q.globs.is_empty() || q.path_re.is_some() || q.name_re.is_some() || q.tokens.iter().any(|t| t.axis == Axis::Folder)
}

/// Build the `/`-joined ancestor folders of entry `e`.
fn ancestor_dirs(base: &Index, e: u32, buf: &mut Vec<u8>) {
    buf.clear();
    let mut chain: Vec<u32> = Vec::new();
    let mut cur = e;
    while cur != 0 {
        let d = base.parent()[cur as usize] as usize;
        let pe = base.dir_entry()[d];
        if pe == 0 || pe == cur {
            break;
        }
        chain.push(pe);
        cur = pe;
    }
    chain.reverse();
    for (i, &ent) in chain.iter().enumerate() {
        if i > 0 {
            buf.push(b'/');
        }
        let f = crate::fold::fold(base.name(ent as usize));
        buf.extend_from_slice(&f);
    }
}

/// Which zones can hold a match at all (kind / size / mtime bounds).
fn zone_flags(q: &Query, base: &Index) -> Vec<bool> {
    let zones = base.zones();
    let mut out = Vec::with_capacity(zones.len());
    map_collect_into(zones.len(), 64, |z| zone_ok(&zones[z], q), &mut out);
    out
}

fn zone_ok(z: &Zone, q: &Query) -> bool {
    let mut kinds = ALL_KINDS;
    if let Some(k) = q.kind {
        kinds = 1u32 << k;
        if q.apps && k == KIND_DIR {
            kinds |= 1u32 << KIND_LINK;
        }
    }
    if q.link {
        kinds = 1u32 << KIND_LINK;
    }
    if z.kinds & kinds == 0 {
        return false;
    }
    if crate::name_index::dec_size(z.max_size) < q.size.0 {
        return false;
    }
    z.max_mtime >= q.mtime.0 && z.min_mtime <= q.mtime.1
}

/// The set of distinct name ids a query can possibly match, as a bitset.
/// `None` means "every name" (queries with no positive name token).
fn candidate_names(q: &Query, base: &Index) -> Option<Vec<u64>> {
    let positives: Vec<&Token> = q.tokens.iter().filter(|t| !t.negate && t.axis == Axis::Name).collect();
    if positives.is_empty() {
        return None;
    }
    let class_mask = (1u64 << CLASSES) - 1;
    let mut out = vec![0u64; base.words];
    for t in &positives {
        let mut cur = vec![u64::MAX; base.words];
        let need = (if t.loose != 0 { t.loose } else { t.mask }) & class_mask;
        let mut m = need;
        while m != 0 {
            let c = m.trailing_zeros() as usize;
            m &= m - 1;
            if c >= CLASSES {
                continue;
            }
            let a = base.bitmap(BM_FIRST + c);
            let b = base.bitmap(BM_SECOND + c);
            for (w, slot) in cur.iter_mut().enumerate() {
                *slot &= a[w] | b[w];
            }
        }
        // A class the query uses twice must appear twice in the name. A token
        // that forgives a typo may have lost one of them, so this bound is
        // only safe for a clean token.
        let mut dbl = if t.loose != 0 { 0 } else { doubled(&t.text) & class_mask };
        while dbl != 0 {
            let c = dbl.trailing_zeros() as usize;
            dbl &= dbl - 1;
            if c >= CLASSES {
                continue;
            }
            let bm = base.bitmap(BM_DOUBLE + c);
            for (w, slot) in cur.iter_mut().enumerate() {
                *slot &= bm[w];
            }
        }
        for (w, slot) in out.iter_mut().enumerate() {
            *slot |= cur[w];
        }
    }
    // A fuzzy word may match a CJK name through its transliteration.
    if positives.iter().any(|t| t.mode == Mode::Fuzzy) {
        let a = base.bitmap(BM_FIRST + CLASS_NON_ASCII);
        let b = base.bitmap(BM_SECOND + CLASS_NON_ASCII);
        for (w, slot) in out.iter_mut().enumerate() {
            *slot |= a[w] | b[w];
        }
    }
    if !out.is_empty() && !base.u.is_multiple_of(64) {
        let last = out.len() - 1;
        out[last] &= (1u64 << (base.u % 64)) - 1;
    }
    Some(out)
}

/// The small score adjustments that are not the name match itself.
fn rank_tweaks(base: &Index, nid: usize, kind: u8, mtime: u32) -> i32 {
    let w = nid >> 6;
    let bit = 1u64 << (nid & 63);
    let mut t = 0i32;
    if base.bitmap(BM_DOT).get(w).is_some_and(|x| x & bit != 0) {
        t -= 8;
    }
    if base.bitmap(BM_APP).get(w).is_some_and(|x| x & bit != 0) {
        t += 25;
    }
    if kind & FLAG_HIDDEN != 0 {
        t -= 8;
    }
    t + recency_tweak(mtime)
}

fn recency_tweak(mtime: u32) -> i32 {
    let age = now_secs().saturating_sub(mtime);
    if age < DAY {
        10
    } else if age < 7 * DAY {
        7
    } else if age < 30 * DAY {
        4
    } else if age < 365 * DAY {
        1
    } else {
        0
    }
}

impl DirMemo {
    #[allow(dead_code)]
    fn own(&self, _slot: usize) -> i16 {
        self.best[0]
    }
    #[allow(dead_code)]
    fn under(&self, _slot: usize) -> i16 {
        self.best[1]
    }
}

// ---------------------------------------------------------------------------
// Tests (fresh, not carried from the replaced file)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::string::ToString;
    use rgy_discovery::walk::{KIND_DIR, KIND_FILE, Listing, NONE, RawEnt};

    fn one(s: &str) -> Query {
        Query::parse(s, "").expect("the query parses")
    }

    fn live_with(names: &[(&str, u8)]) -> Live {
        let mut buf = Vec::new();
        let mut ents = Vec::new();
        for (name, kind) in names {
            ents.push(RawEnt { name_off: buf.len() as u32, name_len: name.len() as u16, kind: *kind, size: 1000, mtime: 1_700_000_000, child: NONE });
            buf.extend_from_slice(name.as_bytes());
        }
        Live::new(Index::build(vec![Listing { id: 0, names: buf, ents }], 0, 1_700_000_000, b"/"))
    }

    fn paths(live: &Live, q: &Query) -> Vec<String> {
        let s = Searcher { live };
        let mut buf = Vec::new();
        s.search(q)
            .iter()
            .map(|h| {
                live.base.path(h.idx as usize, &mut buf);
                String::from_utf8_lossy(&buf).into_owned()
            })
            .collect()
    }

    #[test]
    fn every_token_mode_is_parsed() {
        let cases: &[(&str, Mode, &str)] = &[
            ("invoice", Mode::Fuzzy, "invoice"),
            ("'invoice", Mode::Exact, "invoice"),
            ("^invoice", Mode::Prefix, "invoice"),
            ("invoice$", Mode::Suffix, "invoice"),
            ("\"invoice pdf\"", Mode::Phrase, "invoice pdf"),
            ("=invoice", Mode::Keyword, "invoice"),
        ];
        for (text, mode, want) in cases {
            let q = one(text);
            assert_eq!(q.tokens.len(), 1, "{text}");
            assert_eq!(q.tokens[0].mode, *mode, "{text}");
            assert_eq!(q.tokens[0].text.as_slice(), want.as_bytes(), "{text}");
        }
    }

    #[test]
    fn negation_and_axes_are_parsed() {
        let q = one("!invoice folder:docs ext:^pdf attr:file");
        assert_eq!(q.tokens.len(), 4);
        assert!(q.tokens[0].negate);
        assert_eq!(q.tokens[1].axis, Axis::Folder);
        assert_eq!(q.tokens[2].axis, Axis::Extension);
        assert_eq!(q.tokens[3].axis, Axis::Attribute);
        // `ext:pdf` with no marker is the filter, not an axis token.
        let q = one("ext:pdf");
        assert!(q.tokens.is_empty());
        assert_eq!(q.exts.len(), 1);
    }

    #[test]
    fn fail_closed_syntax_is_refused() {
        let bad_type = one("type:banana");
        assert!(bad_type.never);
        assert!(!bad_type.warnings.is_empty());
        let bad_sort = one("sort:sideways");
        assert!(bad_sort.never);
        let bad_re = one("re:[");
        assert!(bad_re.never);
        assert!(Query::parse("nonesuch:value", "").is_err());
        assert!(Query::parse("kind:wat", "").is_err());
        assert!(Query::parse("size:12qq", "").is_err());
    }

    #[test]
    fn the_ninth_word_is_reported() {
        let mut q = Query::default();
        for w in ["a", "b", "c", "d", "e", "f", "g", "h"] {
            q.push_token(w);
        }
        assert!(q.warnings.is_empty());
        q.push_token("i");
        assert_eq!(q.tokens.len(), 8);
        assert_eq!(q.warnings.len(), 1);
        assert!(q.warnings[0].contains('i'));
        // Negated words are not capped.
        for n in 0..20 {
            q.push_token(&format!("!n{n}"));
        }
        assert_eq!(q.tokens.len(), 28);
    }

    #[test]
    fn alternatives_split_and_keep_regex_alternation() {
        let a = Alternatives::parse("ext:rs|ext:txt", "").expect("parses");
        assert_eq!(a.alts.len(), 2);
        let b = Alternatives::parse("re:(a|b)", "").expect("parses");
        assert_eq!(b.alts.len(), 1);
        let c = Alternatives::parse("re:foo | re:bar", "").expect("parses");
        assert_eq!(c.alts.len(), 2);
        assert_eq!(Alternatives::parse("", "").expect("parses").match_path(b"/x", KIND_FILE, 1, 1), Some(0));
    }

    #[test]
    fn merge_dedups_by_real_identity() {
        let k = |s: i32, e: u32| RankKey::new(Tier::Normal, Tier::Normal, Tier::Normal, Score::from_points(s), e);
        let all = vec![
            Hit { key: k(10, 7), idx: 7, mtime: 0, over: None },
            Hit { key: k(99, 7), idx: 7, mtime: 0, over: None },
            Hit { key: k(5, RankKey::NO_ENTRY), idx: RankKey::NO_ENTRY, mtime: 0, over: Some(b"/alpha".to_vec()) },
            Hit { key: k(5, RankKey::NO_ENTRY), idx: RankKey::NO_ENTRY, mtime: 0, over: Some(b"/zeta".to_vec()) },
        ];
        let out = merge_hits(all, 10);
        assert_eq!(out.len(), 3, "two base hits collapse, the overlays stay apart");
        assert_eq!(out[0].score(), 99);
        assert_eq!(out[1].over.as_deref(), Some(&b"/alpha"[..]));
    }

    #[test]
    fn clone_for_scan_drops_only_the_grep() {
        let mut q = one("invoice grep:hello ext:pdf");
        q.grep = Some("hello".to_string());
        let c = q.clone_for_scan();
        assert!(c.grep.is_none());
        assert_eq!(c.exts, q.exts);
        assert_eq!(c.tokens.len(), q.tokens.len());
        assert_eq!(c.grep_mode, GrepMode::Literal);
    }

    #[test]
    fn the_default_is_unbounded() {
        let q = Query::default();
        assert_eq!(q.size, (0, u64::MAX));
        assert_eq!(q.mtime, (0, u32::MAX));
        assert_eq!(q.limit, 50);
    }

    /// The class bitmaps are a narrowing, never a decision: scoring every
    /// entry without them must give exactly the hits scoring with them. If
    /// this fails, a bitmap wrongly refused a name that really matches.
    #[test]
    fn the_class_bitmaps_never_exclude_a_match() {
        let live = live_with(&[
            ("report_00000.pdf", KIND_FILE),
            ("Report_00001.PDF", KIND_FILE),
            ("src_00002-module.rs", KIND_FILE),
            ("file_00003.dat", KIND_FILE),
            ("notes.txt", KIND_FILE),
            ("日本語.txt", KIND_FILE),
            ("Archive", KIND_DIR),
            ("src", KIND_DIR),
        ]);
        let s = Searcher { live: &live };
        let base = &live.base;
        let identity = |hits: &[Hit]| {
            let mut v: Vec<(u32, Option<Vec<u8>>, i32)> = hits.iter().map(|h| (h.idx, h.over.clone(), h.score())).collect();
            v.sort();
            v
        };
        for text in [
            "'report_00000.pdf",
            "'notes.txt",
            "report",
            "^src",
            "dat$",
            "f1",
            "ext:pdf",
            "type:d",
            "!pdf",
            "",
            "*.pdf",
            "mod",
            "=report",
            "size:>900",
            "ext:pdf report",
        ] {
            let q = one(text);
            let narrowed = candidate_names(&q, base);
            let with = s.scan(&q, narrowed.as_deref(), None);
            let without = s.scan(&q, None, None);
            assert_eq!(identity(&with), identity(&without), "bitmap narrowing changed the answer for {text:?}");
        }
    }

    /// The result cache carries the plan that answered, and a second
    /// identical question must answer from it with the same hits. This is
    /// the shape that lets a cache hit skip `plan_for` entirely.
    #[test]
    fn a_cached_answer_keeps_its_hits_and_plan() {
        let live = live_with(&[
            ("report_00000.pdf", KIND_FILE),
            ("Report_00001.PDF", KIND_FILE),
            ("src_00002-module.rs", KIND_FILE),
            ("file_00003.dat", KIND_FILE),
        ]);
        let s = Searcher { live: &live };
        let q = one("'report_00000.pdf");
        let first = s.search(&q);
        let key = s.cache_key(&q, Some(q.limit)).expect("an exact query has a cache key");
        let (cached, plan) = live.names_cache.cached(&key).expect("the first search filled the cache");
        assert_eq!(plan, s.plan_for(&q, candidate_names(&q, &live.base).as_deref()));
        assert_eq!(cached.len(), first.len());
        let second = s.search(&q);
        assert_eq!(second.iter().map(|h| h.idx).collect::<Vec<_>>(), first.iter().map(|h| h.idx).collect::<Vec<_>>());
        assert_eq!(second.len(), 1);
    }

    #[test]
    fn scored_searches_over_a_small_live() {
        let live = live_with(&[
            ("configuration.txt", KIND_FILE),
            ("invoice.pdf", KIND_FILE),
            ("report_2024.pdf", KIND_FILE),
            ("日本語.txt", KIND_FILE),
            ("中文.pdf", KIND_FILE),
            ("Archive", KIND_DIR),
        ]);
        assert_eq!(paths(&live, &one("config")).len(), 1);
        assert_eq!(paths(&live, &one("ext:pdf")).len(), 3);
        assert_eq!(paths(&live, &one("pdf$")).len(), 3);
        assert_eq!(paths(&live, &one("^rep")).len(), 1);
        assert_eq!(paths(&live, &one("!pdf")).len(), 3);
        assert_eq!(paths(&live, &one("*.pdf")).len(), 3);
        assert_eq!(paths(&live, &one("日本語")).len(), 1);
        let q = one("type:banana");
        assert!(paths(&live, &q).is_empty());
        let s = Searcher { live: &live };
        assert_eq!(s.count(&one("")), 6);
    }

    #[test]
    fn a_strict_size_bound_excludes_the_boundary() {
        let q = one("size:>1000");
        assert_eq!(q.size, (1001, u64::MAX));
        assert!(q.size_set);
        let q = one("size:<1000");
        assert_eq!(q.size, (0, 999));
        let q = one("size:+10k");
        assert_eq!(q.size, (10_000, u64::MAX));
        let q = one("size:-1k");
        assert_eq!(q.size, (0, 1000));
    }

    #[test]
    fn the_matched_characters_are_character_offsets() {
        let q = one("port");
        assert_eq!(matched_chars(b"report.txt", &q.tokens), vec![2, 3, 4, 5]);
        let q = one("=port");
        assert_eq!(matched_chars(b"report_port.txt", &q.tokens), vec![7, 8, 9, 10]);
        let q = one("folder:docs");
        assert!(matched_chars(b"docs.txt", &q.tokens).is_empty());
    }

    #[test]
    fn a_scope_and_a_glob_are_whole_path_tests() {
        let live = live_with(&[("readme.md", KIND_FILE), ("main.rs", KIND_FILE)]);
        assert_eq!(paths(&live, &one("*.md")).len(), 1);
        assert_eq!(paths(&live, &one("src/*")).len(), 0);
    }
}
