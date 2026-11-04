// SPDX-License-Identifier: MIT
//
// Transported from fsearch (https://github.com/noahdunnagan/fsearch), MIT
// licensed, Copyright (c) 2026 Noah Dunnagan. Its notice is in
// LICENSE-MIT-FSEARCH and in the repository's NOTICE.
//! Name search query: parse, filter, match, and the ranker.
//!
//! Transported from fsearch query.rs: the parser and the scalar scorer
//! half. The SIMD Searcher, the NameCache and the top-k half are this
//! repository's own work, written on the same design; the module carries
//! everything `Query::match_path` needs.
//!
//! Merged query syntax: fsearch's token modes
//! on top of zen-rs's filter vocabulary.

use crate::fold;
use crate::hmap::Map;
use crate::live::Live;
use crate::name_index::{BM_DOUBLE, BM_FIRST, BM_SECOND, BM_START, CLASSES, Index, ZONE, Zone, char_bit, start_bit};
use crate::rank::{RankKey, Score, Tier};
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;
use rgy_discovery::walk::{FLAG_HIDDEN, FLAG_MOUNT, KIND_DIR, KIND_FILE, KIND_LINK};

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Mode {
    Fuzzy,
    Exact,
    Prefix,
    Suffix,
    /// A quoted run: the words together, in order, with the spaces between
    /// them (`"invoice pdf"`).
    Phrase,
    /// A whole word of the name (`=invoice`), not a fragment of one.
    Keyword,
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
    /// The axis this token matches on.
    ///
    /// A token names a part of the path: the name, the extension, a folder
    /// of it, or an attribute. The name is the default -- what a bare word
    /// means -- and the others are named.
    pub axis: Axis,
}

/// The part of a path a token matches on.
///
/// The engine matches against four independent parts of a path --
/// folder, attribute, name, extension -- and the legacy contract holds a
/// list of per-folder-part matcher groups. A token says
/// which part it means; the name is the default, because that is what a
/// bare word means.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Axis {
    /// The name of the file.
    Name,
    /// The extension of the name.
    Extension,
    /// A folder of the path.
    Folder,
    /// An attribute of the file (its kind, its flags).
    Attribute,
}

/// The axes a query's tokens match on.
///
/// The bits are the positions of the tokens in the **positive** list: the
/// index space the name table's bitmap and the directory memo share. The
/// attribute tokens are a list rather than a mask: an attribute is the
/// entry's kind byte, and the scan reads that byte anyway.
#[derive(Clone, Default)]
pub struct Axes<'a> {
    /// The tokens that match the name.
    pub name: u32,
    /// The tokens that match the extension of the name.
    pub ext: u32,
    /// The tokens that match a folder of the path.
    pub folder: u32,
    /// The tokens that match an attribute of the entry.
    pub attrs: Vec<&'a Token>,
    /// The attribute tokens that must **not** match.
    pub neg_attrs: Vec<&'a Token>,
    /// The bits of the attribute tokens: an attribute is the entry's kind
    /// byte, and no bit can carry it.
    pub attr: u32,
    /// Whether every token is on the name axis.
    ///
    /// Then the scan is the scan it always was, and it costs the same: the
    /// axes are checked in the scan, per entry, and a query that does not
    /// use them pays nothing for the privilege.
    pub plain: bool,
    /// Whether the query asks anything of the name.
    ///
    /// The name-axis tokens and the extension ones (the extension is part
    /// of the name) ask something of the name; a query with no such token
    /// asks nothing, and every name is a candidate -- the scan decides.
    pub asks_name: bool,
    /// The attribute tokens, compiled to the kind byte: the kinds an entry
    /// may have (bit `k` for kind `k`), the flags it must carry and the
    /// flags it must not.
    ///
    /// The scan asks the attributes once per candidate, so they are asked
    /// as three bit tests instead of a string compare per token -- and the
    /// kinds are what a zone of the index says it holds, so a run of
    /// entries none of which has an allowed kind is skipped whole.
    pub kinds: u8,
    pub flags_req: u8,
    pub flags_forbid: u8,
}

impl Axes<'_> {
    /// Whether an entry with kind byte `k` answers the attribute tokens.
    #[inline(always)]
    fn attrs_ok(&self, k: u8) -> bool {
        (self.kinds >> (k & 3)) & 1 != 0 && k & self.flags_req == self.flags_req && k & self.flags_forbid == 0
    }
}

/// Every kind (`kind & 3`) an entry can have.
const ALL_KINDS: u8 = 0b1111;

/// What an attribute token asks of the kind byte: the kinds it allows and
/// the flag it needs. `None` for a word that names no attribute.
fn attr_spec(t: &Token) -> Option<(u8, u8)> {
    Some(match t.text.as_slice() {
        b"dir" | b"folder" | b"directory" => (1 << KIND_DIR, 0),
        b"file" => (1 << KIND_FILE, 0),
        b"link" | b"symlink" => (1 << KIND_LINK, 0),
        b"other" => (1 << 3, 0),
        b"hidden" => (ALL_KINDS, FLAG_HIDDEN),
        b"mount" | b"volume" => (ALL_KINDS, FLAG_MOUNT),
        _ => return None,
    })
}

/// The bits the name table's bitmap and the memo can carry.
///
/// Every positive token but the attribute ones: an attribute is the
/// entry's kind byte, and no bit can carry it.
fn all_mask(pos_len: usize, axes: &Axes<'_>) -> u32 {
    ((1u32 << pos_len) - 1) & !axes.attr
}

impl Axis {
    /// The axis a token names, from the prefix it carries.
    ///
    /// The prefixes are the ones the reference's own contract carries:
    /// `FolderPartMatchers` is a list per folder part, and the query
    /// grammar's facet prefix is `:`. So the axis is named before the
    /// colon: `name:`, `ext:`, `folder:` and `attr:`.
    ///
    /// `dir:` is not among them: it is rgy's alias of the `in:` filter,
    /// and the folder axis is `folder:`.
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
/// The most positive words a query carries.
///
/// The axes' mask is 32 bits wide, but the machinery around it is not:
/// the folder memo keeps one score per token in a fixed array
/// (`DirMatch::best`, eight of them) and the scan keeps four
/// (`DirMemo::best`), and a token's place among the positive words is
/// what says which slot is its own. A ninth word has no slot to be
/// scored in, so it is not carried — and the query says so, because a
/// word dropped in silence is a query that is not the one the user
/// asked.
const MAX_WORDS: usize = 8;

fn takes_typos(text: &[u8], mode: Mode) -> bool {
    mode == Mode::Fuzzy && text.len() >= TYPO_MIN_LEN
}

#[derive(Clone)]
pub struct Query {
    pub tokens: Vec<Token>,
    pub kind: Option<u8>,
    /// `type:app`: a folder kind also takes symlinks, since the system apps
    /// in /Applications link into the cryptex (/Applications/Safari.app).
    pub apps: bool,
    /// `type:link` (zen-rs): a reparse point / symlink, orthogonal to
    /// `kind` — `type:dir type:link` asks for a directory junction.
    pub link: bool,
    pub exts: Vec<Vec<u8>>,
    pub scope: Option<Vec<u8>>,
    pub size: (u64, u64),
    /// True once any `size:` bound was given: directories then match **no**
    /// size bound (zen-rs semantics — a directory's entry size is
    /// meaningless, so it fails the filter rather than passing it).
    pub size_set: bool,
    pub mtime: (u32, u32),
    pub name_re: Option<crate::regex::Regex>,
    pub path_re: Option<crate::regex::Regex>,
    pub limit: usize,
    /// The order the results come in (`sort:`).
    pub sort: SortBy,
    /// Content search: handled by the content layer, carried here so one
    /// query string can say everything.
    pub grep: Option<String>,
    pub grep_mode: GrepMode,
    /// Whole-path globs from terms carrying `*`/`?` (zen-rs rule:
    /// only such terms glob; a bare term stays a fsearch token). Each entry
    /// is `(negated, lowercase pattern)`.
    pub globs: Vec<(bool, Vec<u8>)>,
    /// Fail-closed: an unknown `type:` or an uncompilable `re:` rejects the
    /// whole alternative (zen-rs), rather than silently admitting
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

/// The order the results come in.
///
/// The reference's `SortBy` is a choice (NoSort, Score, FileName, LastWriteTime),
/// and rgy's is one too: `sort:` in the query.
///
/// The choice orders the hits the cap kept; it does not pick them. The cap
/// still takes the best by score, so `sort:mtime` is "the newest of the
/// best matches", not "the newest matches".
#[derive(Clone, Copy, Default, PartialEq, Debug)]
pub enum SortBy {
    /// The ranking: the score, with the priority tier above it.
    #[default]
    Score,
    /// The name, ascending.
    Name,
    /// The last change, newest first.
    MTime,
    /// The index's order.
    None,
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

/// Defaults are the parse defaults, not the derive: `size`/`mtime` must be
/// unbounded and `limit` must be nonzero, or a hand-built query (scan_paths,
/// tests) silently matches nothing — `derive(Default)` gives (0,0)/0 there,
/// which every file size rejects.
impl Default for Query {
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

impl Query {
    /// Parse the human query language. Plain words are fuzzy tokens;
    /// `'x` exact, `^x` prefix, `x$` suffix, `=x` word, `!x` negate, a
    /// quoted run a phrase; filters are
    /// `ext: type: kind: in: size: mtime: re: path: limit: grep: regex: sym:`.
    ///
    /// A word may also name the **axis** it matches on, before the colon:
    /// `name:`, `ext:`, `folder:` and `attr:`. A bare word
    /// is the name axis, which is what it has always meant.
    pub fn parse(s: &str, home: &str) -> Result<Query, String> {
        let mut q = Query { size: (0, u64::MAX), mtime: (0, u32::MAX), limit: 50, ..Default::default() };
        for (word, quoted) in split_words(s) {
            // A wildcard term is one whole-path glob — never split on `/`
            // (zen-rs: `*/src/*.rs` covers the whole path).
            //
            // A word whose prefix names a pattern filter is not a glob,
            // whatever its value holds: `regex:need.*` is a regex, and the
            // `*` belongs to the engine that runs it. The wildcard test
            // used to come first, and the whole word — the prefix
            // included — became a glob for a file called `regex:need.*`:
            // the content search was voided, and the tool said "nothing
            // found" for a search it never ran.
            let pattern_filter = word.split_once(':').is_some_and(|(k, _)| q.pattern_filter_is(k));
            if (word.contains('*') || word.contains('?')) && !pattern_filter {
                q.push_token(&word);
                continue;
            }
            // An axis prefix names the part of the path the word matches
            //: `name:`, `ext:`, `folder:` and `attr:`.
            //
            // The prefix is only an axis when the word carries one of the
            // matcher's own markers, or when the prefix is not also a
            // filter. `ext:` is both -- it is the extension filter and the
            // extension axis -- and there the filter wins, because a user
            // who writes `ext:pdf` means the file's extension exactly. The
            // axis form is `ext:^pdf` (and the other markers), which says
            // "the extension, matched this way".
            let (axis, word) = match word.split_once(':') {
                Some((k, v)) if !v.is_empty() && !v.contains('/') => {
                    let marked = v.starts_with(['\'', '^', '=', '!']) || v.ends_with('$');
                    match Axis::of_prefix(k) {
                        Some(a) if marked || !q.filter_is(k) => (Some(a), v.to_string()),
                        _ => (None, word.clone()),
                    }
                }
                _ => (None, word.clone()),
            };
            if axis.is_none()
                && let Some((k, v)) = word.split_once(':')
            {
                if q.filter(k, v, home)? {
                    continue;
                }
                // A prefix that is neither a filter nor an axis is a
                // mistake, and it fails closed: the word would otherwise
                // become a token, and `nosuchaxis:x` would answer the
                // files with "x" in the name.
                if !q.filter_is(k) && Axis::of_prefix(k).is_none() {
                    return Err(format!("unknown filter {k}"));
                }
            }
            for piece in word.split('/').filter(|p| !p.is_empty()) {
                q.push_token_mode(piece, quoted, axis);
            }
        }
        Ok(q)
    }

    /// The axes the tokens match on.
    ///
    /// The bits are the positions of the positive tokens, which is the
    /// index space the name table's bitmap and the directory memo use.
    pub fn axes(&self) -> Axes<'_> {
        let mut a = Axes { kinds: ALL_KINDS, ..Axes::default() };
        let mut t = 0u32;
        for tok in &self.tokens {
            if tok.negate {
                if tok.axis == Axis::Attribute {
                    a.neg_attrs.push(tok);
                    // A word that names no attribute matches nothing, so
                    // its negation rejects nothing.
                    match attr_spec(tok) {
                        Some((k, 0)) => a.kinds &= !k,
                        Some((_, f)) => a.flags_forbid |= f,
                        None => {}
                    }
                }
                continue;
            }
            match tok.axis {
                Axis::Name => a.name |= 1 << t,
                Axis::Extension => a.ext |= 1 << t,
                Axis::Folder => a.folder |= 1 << t,
                Axis::Attribute => {
                    a.attr |= 1 << t;
                    a.attrs.push(tok);
                    // A word that names no attribute matches nothing, and
                    // a positive one that matches nothing admits nothing.
                    match attr_spec(tok) {
                        Some((k, f)) => {
                            a.kinds &= k;
                            a.flags_req |= f;
                        }
                        None => a.kinds = 0,
                    }
                }
            }
            t += 1;
        }
        // The two questions the scan asks, answered once here: whether every
        // token is on the name axis (then the scan is the scan it always
        // was), and whether the query asks anything of the name (then every
        // name is a candidate and the scan decides).
        a.plain = a.ext == 0 && a.folder == 0 && a.attrs.is_empty() && a.neg_attrs.is_empty();
        a.asks_name = a.name != 0 || a.ext != 0;
        a
    }

    pub fn push_token(&mut self, w: &str) {
        self.push_token_mode(w, false, None);
    }

    /// Push a token, with the mode the caller already knows: a quoted run
    /// is a phrase, and the axis says which part of the path it matches.
    ///
    /// The markers come after the negation, so `!^x` is a negated prefix
    /// and `!"a b"` a negated phrase. `!` on its own negates the fuzzy
    /// match — `!invoice` is "not close to invoice", which is what the
    /// manual says and what the old code did not do (it negated an exact
    /// match, so `!invoice` still answered `invoce`).
    fn push_token_mode(&mut self, w: &str, quoted: bool, axis: Option<Axis>) {
        let (mut t, mut negate) = (w, false);
        if let Some(r) = t.strip_prefix('!') {
            (t, negate) = (r, true);
        }
        let mut mode = if quoted { Mode::Phrase } else { Mode::Fuzzy };
        if let Some(r) = t.strip_prefix('\'') {
            (t, mode) = (r, Mode::Exact);
        } else if let Some(r) = t.strip_prefix('^') {
            (t, mode) = (r, Mode::Prefix);
        } else if let Some(r) = t.strip_suffix('$') {
            (t, mode) = (r, Mode::Suffix);
        } else if let Some(r) = t.strip_prefix('=') {
            (t, mode) = (r, Mode::Keyword);
        }
        // Zen-rs: a term carrying `*`/`?` globs the WHOLE PATH; bare
        // terms stay fsearch tokens (the name engine wins, the filter
        // vocabulary is layered on top).
        if t.contains('*') || t.contains('?') {
            // The pattern is folded and lowercased: it is matched against
            // the folded path.
            let pattern = fold::fold(t.as_bytes());
            self.globs.push((negate, pattern.iter().map(|&b| b.to_ascii_lowercase()).collect()));
            return;
        }
        // Positive tokens are tracked in a u32 bitset (the axes' mask), and
        // the memo's per-token slots are eight wide: a query carries eight
        // words. The ninth is not dropped in silence — the query says which
        // word it did not use, because a query that ran with fewer words
        // than it was given answers a narrower question than the one asked.
        if t.is_empty() {
            return;
        }
        if !negate && self.tokens.iter().filter(|t| !t.negate).count() >= MAX_WORDS {
            let note = format!("too many words: '{w}' was ignored (at most {MAX_WORDS} words)");
            self.warnings.push(note);
            return;
        }
        // The token is folded before it is compared: a query typed in NFC
        // has to match a name the filesystem stored in NFD (parity row
        // 1.12). The case-fold stays where it is -- the bytes are compared
        // case-insensitively -- so only the normalization happens here.
        let folded = fold::fold(t.as_bytes());
        let text: Vec<u8> = folded.iter().map(|&b| b.to_ascii_lowercase()).collect();
        let mask = text.iter().fold(0, |m, &b| m | char_bit(b));
        let (loose, start) = if takes_typos(&text, mode) { (mask & !char_bit(text[0]), start_bit(text[0])) } else { (0, 0) };
        self.tokens.push(Token { text, mask, mode, negate, loose, start, axis: axis.unwrap_or(Axis::Name) });
    }

    /// Whether `k` names a filter.
    ///
    /// Some prefixes are both a filter and an axis (`ext:`), and there the
    /// filter wins unless the word carries one of the matcher's markers.
    fn filter_is(&self, k: &str) -> bool {
        matches!(
            k,
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

    /// Whether the prefix names a filter whose value is a pattern.
    ///
    /// A pattern filter is one whose value the engine reads as a pattern:
    /// a regex, a path glob, a literal to look for in the file's bytes.
    /// Such a value may hold a `*` or a `?`, and those belong to the
    /// engine that runs them — not to the name matcher.
    fn pattern_filter_is(&self, k: &str) -> bool {
        matches!(k, "re" | "path" | "grep" | "content" | "regex" | "sym" | "symbol")
    }

    /// Apply filter `k:v`; false if `k` is not a filter name.
    pub fn filter(&mut self, k: &str, v: &str, home: &str) -> Result<bool, String> {
        match k {
            "ext" => self.exts.extend(v.split(',').map(|e| {
                // The extension is folded and lowercased: it is compared
                // against the folded name.
                fold::fold(e.trim_start_matches('.').as_bytes()).iter().map(|&b| b.to_ascii_lowercase()).collect()
            })),
            "type" => {
                for t in v.split(',') {
                    // zen-rs `type:` takes the kind words and its
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
                // `dir:` is zen-rs's name for the scope filter.
                let p = v.strip_prefix('~').map_or(v.to_string(), |r| format!("{home}{r}"));
                // The scope is folded: it is compared against the folded
                // path, so both sides have to be in the same form.
                let scope = real_path(p).trim_end_matches('/').as_bytes().to_vec();
                self.scope = Some(fold::fold(&scope).into_owned());
            }
            "size" => {
                self.size = range(v, parse_size)?;
                self.size_set = true;
            }
            "mtime" | "modified" => {
                // mtime:<7d means "modified within the last 7 days".
                let now = now_secs();
                // The named windows: a vocabulary, not a mechanism. The
                // numbers are the ones The reference's parser carries (the
                // 86400-second day).
                self.mtime = match v {
                    "today" | "last_24_hours" => (now.saturating_sub(86_400), now),
                    "week" | "last_week" => (now.saturating_sub(7 * 86_400), now),
                    "month" | "last_month" => (now.saturating_sub(30 * 86_400), now),
                    "year" | "last_year" => (now.saturating_sub(365 * 86_400), now),
                    "older" => (0, now.saturating_sub(365 * 86_400)),
                    _ => {
                        // A calendar date, or a range of them, or an age.
                        // The operator suffix is what tells the three
                        // apart (see `date_range`).
                        date_range(v, now)?
                    }
                };
            }
            // zen-rs `re:` is a PATH regex (and `path:` is its
            // fsearch alias). An uncompilable pattern fails closed — the
            // alternative matches nothing and a warning is recorded — same
            // as a positive `re:` that rejects everything rather than
            // silently admitting everything.
            "re" | "path" => match crate::regex::Regex::new(&format!("(?i){v}")) {
                Ok(r) => self.path_re = Some(r),
                Err(e) => {
                    self.never = true;
                    self.warnings.push(format!("re:{v}: {e}"));
                }
            },
            "limit" => self.limit = v.parse().map_err(|_| "bad limit")?,
            "sort" => {
                // The order the results come in. The cap still picks the
                // best by score; the sort orders the hits it kept.
                self.sort = match v {
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
        // `type:link` is orthogonal to kind (zen-rs): a reparse
        // point / symlink. On fsearch's walk the symlink kind carries it;
        // a combined `type:dir type:link` needs a backend reparse flag and
        // matches nothing until one exists.
        // `size:` bounds are statements about files: a directory satisfies
        // NO size bound rather than all of them (zen-rs).
        if self.never
            || (self.link && kind & 3 != KIND_LINK)
            || !self.kind_ok(kind)
            || (self.size_set && kind & 3 == KIND_DIR)
            || size < self.size.0
            || size > self.size.1
            || mtime < self.mtime.0
            || mtime > self.mtime.1
        {
            return None;
        }
        // The attribute axis: the token matches the file's attributes --
        // its kind and its flags -- by name (`dir`, `file`, `link`,
        // `hidden`, `mount`).
        for t in &self.tokens {
            if t.axis == Axis::Attribute && t.negate == attr_matches(t, kind) {
                return None;
            }
        }
        // Every comparison runs on the fold of the path (the fold rule, rule 2): a name typed in NFC has to match one the
        // filesystem stored in NFD. The path the caller holds is the one
        // that is displayed and opened; this is the view for comparison.
        let folded = fold::fold(path);
        let path = &folded[..];
        if let Some(s) = &self.scope
            && !(path.starts_with(s) && path.get(s.len()) == Some(&b'/'))
        {
            return None;
        }
        let cut = path.iter().rposition(|&b| b == b'/').unwrap_or(0);
        let name = &path[cut + 1..];
        if name.is_empty() || (!self.exts.is_empty() && !ext_ok(name, &self.exts)) {
            return None;
        }
        for (neg, pat) in &self.globs {
            if *neg == glob_match(pat, path) {
                return None;
            }
        }
        if self.tokens.is_empty() {
            let re_ok =
                self.name_re.as_ref().is_none_or(|re| re.is_match_bytes(name)) && self.path_re.as_ref().is_none_or(|re| re.is_match_bytes(path));
            return re_ok.then_some(0);
        }
        // The tokens are grouped by the axis they match on (parity row
        // Q.1): the name, the extension, a folder of the path, or an
        // attribute. A hit with name-axis tokens needs some token in its
        // own name; most paths fail here, before the folders are looked at.
        //
        // The place a token has among the positive tokens matters:
        // `dir_match` fills the memo by that place, so a token that is not
        // the first positive one has to look up its own slot and not the
        // one before it.
        let m = crate::name_index::mask_of(name);
        let (mut has_pos_name, mut got, mut missing_name, mut need_folder, mut score) = (false, 0u32, 0u32, 0u32, 0i32);
        for (slot, tok) in self.tokens.iter().filter(|t| !t.negate).enumerate() {
            match tok.axis {
                Axis::Name => {
                    has_pos_name = true;
                    if tok.fits(m)
                        && let Some(s) = token_score(name, None, m, tok)
                    {
                        got |= 1 << slot;
                        score += s;
                    } else {
                        missing_name |= 1 << slot;
                    }
                }
                Axis::Folder => need_folder |= 1 << slot,
                Axis::Extension | Axis::Attribute => {}
            }
        }
        if has_pos_name && got == 0 {
            return None;
        }
        let mut has_dir_neg = false;
        for t in &self.tokens {
            if !t.negate {
                // The extension axis: the token matches the extension of the
                // name (the tail after the last dot, without the dot).
                if t.axis == Axis::Extension {
                    let ext = ext_of(name)?;
                    score += token_score(ext, None, !0, t)?;
                }
                continue;
            }
            match t.axis {
                Axis::Name => {
                    has_dir_neg = true;
                    if token_matches(name, t) {
                        return None;
                    }
                }
                Axis::Folder => has_dir_neg = true,
                Axis::Extension => {
                    if ext_of(name).is_some_and(|ext| token_matches(ext, t)) {
                        return None;
                    }
                }
                Axis::Attribute => {}
            }
        }
        if missing_name != 0 || need_folder != 0 || has_dir_neg {
            let d = dirs(&path[..cut]);
            if d.negated {
                return None;
            }
            let mut miss = missing_name;
            while miss != 0 {
                let slot = miss.trailing_zeros() as usize;
                miss &= miss - 1;
                score += d.best[slot]? * 3 / 4;
            }
            let mut fld = need_folder;
            while fld != 0 {
                let slot = fld.trailing_zeros() as usize;
                fld &= fld - 1;
                score += d.best[slot]?;
            }
        }
        if self.name_re.as_ref().is_some_and(|re| !re.is_match_bytes(name)) || self.path_re.as_ref().is_some_and(|re| !re.is_match_bytes(path)) {
            return None;
        }
        Some(score)
    }

    /// How the folders of a path (`/a/b` for `/a/b/name`) match the tokens:
    /// the best score per positive token, and whether a negated one hits.
    pub fn dir_match(&self, dirs: &[u8]) -> DirMatch {
        // The folders are folded: they are compared against the folded
        // token, so both sides have to be in the same form.
        let folded = fold::fold(dirs);
        let dirs = &folded[..];
        let comps = || dirs.split(|&b| b == b'/').filter(|c| !c.is_empty());
        let mut d = DirMatch { negated: false, best: [None; 8] };
        d.negated = self.tokens.iter().any(|t| t.negate && matches!(t.axis, Axis::Name | Axis::Folder) && comps().any(|c| token_matches(c, t)));
        for (t, tok) in self.tokens.iter().filter(|t| !t.negate).enumerate() {
            if matches!(tok.axis, Axis::Name | Axis::Folder) {
                d.best[t] = comps().filter_map(|c| token_score(c, None, !0, tok)).max();
            }
        }
        d
    }
}

/// The extension of a name: the tail after the last dot, without the dot.
///
/// A name with no dot has no extension, and a dotfile's extension is its own
/// name without the dot — `.bashrc` is a `bashrc`. The rule is the tail
/// after the last dot and nothing else: the extension filter and the
/// extension axis read the extension through this one function, so the two
/// forms of `ext:` cannot disagree about a name.
fn ext_of(name: &[u8]) -> Option<&[u8]> {
    let dot = name.iter().rposition(|&b| b == b'.')?;
    if dot + 1 >= name.len() {
        return None;
    }
    Some(&name[dot + 1..])
}

/// Whether a token on the attribute axis names an attribute of the file.
///
/// The attributes are the kind (`dir`, `file`, `link`, `other`) and the
/// flags (`hidden`, `system`, `mount`). The token is the attribute's name,
/// matched the way every other token is matched.
#[inline(always)]
fn attr_matches(t: &Token, kind: u8) -> bool {
    // The flags live in the kind byte itself, above the two bits that hold
    // the kind (`kind & 3`). They are tested where they sit: shifting the
    // byte first would look for the hidden flag in bit 4 and the mount flag
    // in bit 5, and nothing would ever match.
    match t.text.as_slice() {
        b"dir" | b"folder" | b"directory" => kind & 3 == KIND_DIR,
        b"file" => kind & 3 == KIND_FILE,
        b"link" | b"symlink" => kind & 3 == KIND_LINK,
        b"other" => kind & 3 == 3,
        b"hidden" => kind & FLAG_HIDDEN != 0,
        b"mount" | b"volume" => kind & FLAG_MOUNT != 0,
        _ => false,
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
///
/// The resolution is the seam's: it is a platform call, and this crate has
/// no `std` to make it with. Where there is no filesystem to ask — a
/// bare-metal target — the path stays as the caller gave it, because a
/// path that depends on a filesystem is not a path the index can hold.
fn real_path(p: String) -> String {
    static LAST: crate::sync::Mutex<Option<(String, String, u64)>> = crate::sync::Mutex::new(None);
    let mut last = LAST.lock().unwrap();
    let now = crate::clock::unix_millis();
    if let Some((from, to, at)) = &*last
        && *from == p
        && now.saturating_sub(*at) < 1000
    {
        return to.clone();
    }
    let to = rgy_discovery::file::canonicalize(p.as_bytes()).map_or_else(|| p.clone(), |c| String::from_utf8_lossy(&c).into_owned());
    *last = Some((p, to.clone(), now));
    to
}

/// Split on spaces, keeping "double quoted" runs together.
///
/// A quoted run comes back marked: it is a phrase, not a word — the phrase
/// matcher takes the words together with the spaces between them.
fn split_words(s: &str) -> Vec<(String, bool)> {
    let (mut out, mut cur, mut quoted, mut was_quoted) = (Vec::new(), String::new(), false, false);
    for c in s.chars() {
        match c {
            '"' => {
                quoted = !quoted;
                was_quoted = true;
            }
            ' ' if !quoted => {
                if !cur.is_empty() {
                    out.push((core::mem::take(&mut cur), was_quoted));
                    was_quoted = false;
                }
            }
            _ => cur.push(c),
        }
    }
    if !cur.is_empty() {
        out.push((cur, was_quoted));
    }
    out
}

fn range(v: &str, p: fn(&str) -> Option<u64>) -> Result<(u64, u64), String> {
    let bad = || format!("bad range {v}");
    // The returned bounds are inclusive and the filter rejects `x < lo ||
    // x > hi`, so a *strict* bound has to move by one. `>` and `>=` used to
    // fall through to the same arm and answer the same way, which made
    // `size:>10000` match a file of exactly 10,000 bytes and `size:<1kb`
    // match one of exactly 1kb.
    //
    // A range with `lo > hi` is empty: the filter's comparison rejects
    // every value under it, which is what `>u64::MAX` and `<0` mean.
    //
    // The signed forms are the ones the manual spells: `+10m` is "at
    // least ten megabytes" and `-1k` is "at most one kilobyte". They
    // used to be a "bad range" error, which is how a documented syntax
    // ended up not working at all.
    if let Some(r) = v.strip_prefix(">=") {
        return Ok((p(r).ok_or_else(bad)?, u64::MAX));
    }
    if let Some(r) = v.strip_prefix('>') {
        let x = p(r).ok_or_else(bad)?;
        return Ok(x.checked_add(1).map_or((u64::MAX, 0), |lo| (lo, u64::MAX)));
    }
    if let Some(r) = v.strip_prefix("<=") {
        return Ok((0, p(r).ok_or_else(bad)?));
    }
    if let Some(r) = v.strip_prefix('<') {
        let x = p(r).ok_or_else(bad)?;
        return Ok(x.checked_sub(1).map_or((u64::MAX, 0), |hi| (0, hi)));
    }
    if let Some(r) = v.strip_prefix('+') {
        return Ok((p(r).ok_or_else(bad)?, u64::MAX));
    }
    if let Some(r) = v.strip_prefix('-') {
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

/// A calendar date, as the unix second it starts at.
///
/// The dates are UTC: a date has no timezone, and the index stores a unix
/// second, so a date is a day in the same clock the index counts in. The
/// day is 86 400 seconds -- the constant The reference's parser carries.
const DAY: u64 = 86_400;

/// The last year the `mtime:` filter can name.
///
/// The filter's answer is a pair of unix seconds in a `u32` — the width
/// the index keeps a file's modification time in — and the u32 epoch ends
/// on 2106-02-07. A year past it is refused: the alternative is a
/// timestamp that wrapped into the past, and a filter that answers with
/// the wrong decade is worse than one that says it cannot answer.
const MAX_YEAR: i64 = 2106;

/// The unix second a calendar date starts at, in UTC.
///
/// The accepted forms are `YYYY-MM-DD` and `YYYY/MM/DD`. The day counts
/// from the epoch with the civil-from-days algorithm, so there is no
/// timezone and no leap-year table to get wrong: the days since 1970 are
/// what the calendar says.
///
/// A date the filter cannot hold is refused. The bound is checked before
/// the arithmetic below, because the arithmetic is what overflows on a
/// year nobody can hold: the days are counted in an `i64` and turned
/// into seconds and then into a `u32`, and a year of 999 999 999 999
/// needs more seconds than a `u64` has.
fn parse_date(s: &str) -> Option<u32> {
    let s = s.trim();
    let (y, rest) = s.split_once(['-', '/'])?;
    let (m, d) = rest.split_once(['-', '/'])?;
    let y: i64 = y.parse().ok()?;
    let m: u32 = m.parse().ok()?;
    let d: u32 = d.parse().ok()?;
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) || y < 0 {
        return None;
    }
    // The year the u32 epoch holds, and the bound the arithmetic below
    // needs: past it the day count leaves the `i64` it is counted in.
    if y > MAX_YEAR {
        return None;
    }
    // The day of the month has to exist in that month.
    let leap = (y % 4 == 0 && y % 100 != 0) || y % 400 == 0;
    let len = match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        _ if leap => 29,
        _ => 28,
    };
    if d > len {
        return None;
    }
    // Days from the civil date (Howard Hinnant's algorithm), which counts
    // from 1970-01-01.
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m as i64 + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    if days < 0 {
        return None;
    }
    // The timestamp is the `u32` the filter carries, and a date the u32
    // epoch does not reach is refused rather than truncated into the
    // past: the last day of the epoch is 2106-02-07, and the days after
    // it are not a date the filter can hold.
    u32::try_from((days as u64).checked_mul(DAY)?).ok()
}

/// The operator a date value carries.
#[derive(Clone, Copy, PartialEq)]
enum DateOp {
    /// No operator: the value is the range itself.
    None,
    /// `<`: before the value.
    Before,
    /// `>`: after the value.
    After,
    /// `c`: the created date (the reference's token).
    Created,
}

/// The date range a `mtime:` filter asks for.
///
/// The value is one of three things, and the operator suffix is what tells
/// them apart:
///
/// * a **calendar date** -- `2024-01-01` or `2024/01/01` (the separator
///   is a dash or a slash, as it is in the date parser), or a range of two
///   (`2024-01-01..2024-02-01`). A date names a whole day: it starts at
///   midnight and ends at the next midnight, so `mtime:2024-01-01` is the
///   files changed that day;
/// * an **age** -- `7d`, `2mo`. An age is counted back from now, so
///   `mtime:7d` is the last seven days;
/// * an **operator** on either -- `<` before, `>` after, and the two
///   the reference recovered verbatim: `c` (created) and `<` (before).
///
/// The comparison is the same in both: the operator bounds the **value**.
/// `<` is "less than the value" and `>` is "more than it". For an age that
/// reads backwards from the clock -- an age less than seven days is the
/// last seven days -- and the conversion to a timestamp flips the bounds.
/// For a date it reads forwards: before the date, or after it.
///
/// The `c` suffix is the one rgy cannot answer: it asks for the file's
/// **creation** time, and the index keeps the modification time. It is
/// accepted and answered with the modification time's range, because a
/// filter that answers with something else is worse than one that does not
/// answer -- and the answer is the honest one for the data rgy has.
///
/// A range with a bound the value cannot give is empty, not everything:
/// the same rule every other filter here follows.
fn date_range(v: &str, now: u32) -> Result<(u32, u32), String> {
    let bad = || format!("bad date {v}");
    // The operator suffix, if any.
    let (op, rest): (DateOp, &str) = match v.as_bytes().first() {
        Some(b'<') => (DateOp::Before, &v[1..]),
        Some(b'>') => (DateOp::After, &v[1..]),
        Some(b'c') | Some(b'C') => (DateOp::Created, &v[1..]),
        _ => (DateOp::None, v),
    };
    // A calendar date, or a range of two. The shape is the parser's: a
    // date is a year, a separator, a month, a separator and a day, and
    // the separator is a dash or a slash. `parse_date` takes both, so
    // the filter takes both — a date the parser reads is a date the
    // filter reads.
    if rest.matches(['-', '/']).count() >= 2 {
        let (lo, hi) = match rest.split_once("..") {
            Some((a, b)) => (parse_date(a).ok_or_else(bad)?, parse_date(b).ok_or_else(bad)?),
            None => {
                let at = parse_date(rest).ok_or_else(bad)?;
                (at, at.saturating_add(DAY as u32))
            }
        };
        return Ok(match op {
            // Before a date is before the day it names; after a date is
            // after the day it names. A date names a whole day, so the
            // bounds move by a day: `hi` already starts the next one.
            DateOp::Before => (0, lo.saturating_sub(1)),
            DateOp::After => (hi, u32::MAX),
            _ => (lo, hi.saturating_sub(1)),
        });
    }
    // An age, in age space: the file's age is `now - mtime`, so a bigger
    // age is an older file.
    let (alo, ahi) = match op {
        // `<A` is an age less than A, `>A` an age more than A.
        DateOp::Before => {
            let a = parse_age(rest).ok_or_else(bad)?;
            (0, a.saturating_sub(1))
        }
        DateOp::After => {
            let a = parse_age(rest).ok_or_else(bad)?;
            (a.saturating_add(1), u64::MAX)
        }
        // The created token: the same window, on the data rgy has.
        DateOp::Created => {
            let a = parse_age(rest).ok_or_else(bad)?;
            (0, a)
        }
        // `-A` and a bare `A` are the same window: the last A. The manual
        // spells it with the sign, and the bare form is the one the named
        // windows use (`today`, `week`). `+A` is the other side: at least
        // A old.
        DateOp::None => match rest.as_bytes().first() {
            Some(b'+') => {
                let a = parse_age(rest.trim_start_matches('+')).ok_or_else(bad)?;
                (a, u64::MAX)
            }
            _ => {
                let a = parse_age(rest.trim_start_matches('-')).ok_or_else(bad)?;
                (0, a)
            }
        },
    };
    // The ages become timestamps: the bigger age is the older file, so
    // the bounds swap.
    let to_time = |age: u64| now.saturating_sub(age.min(now as u64) as u32);
    Ok((to_time(ahi), to_time(alo)))
}

pub fn now_secs() -> u32 {
    // The clock is the crate's own: `clock_gettime(2)` on unix and
    // `GetSystemTimeAsFileTime` on Windows, declared in `crate::clock`. It
    // reads the same wall clock `SystemTime` did, in every build -- a
    // build without a clock would answer 0, and 0 is the epoch: the
    // `mtime:` filter would then ask for files from 1970 and the idle
    // trim would have nothing to compare against.
    crate::clock::unix_secs()
}

/// The unix millisecond, for the idle timers.
///
/// Milliseconds and not seconds: a table scored a fraction of a second
/// ago is not idle yet, and a clock that can only say "the same second"
/// cannot tell the two apart -- it would drop the cache while the user is
/// still typing.
///
/// The wall clock is not monotonic, so a clock that steps backwards reads
/// as "not idle". That is the safe direction: the cache is kept a little
/// longer than it should be, rather than dropped while it is wanted.
pub fn now_millis() -> u64 {
    crate::clock::unix_millis()
}

#[inline(always)]
pub(crate) fn fold(b: u8) -> u8 {
    b | (((b.wrapping_sub(b'A') < 26) as u8) << 5)
}

/// The name of a path: the last component.
fn name_of(path: &[u8]) -> &[u8] {
    match path.iter().rposition(|&b| b == b'/') {
        Some(i) => &path[i + 1..],
        None => path,
    }
}

/// The first place in `name` where `q` is, ignoring ASCII case.
///
/// The query is folded and lowercased, so the name's byte at the match is
/// either the byte itself or its upper case: the two are searched with
/// memchr and the rest of the query is compared.
///
/// The naive scan compared every byte of the name against every byte of
/// the query, and this runs per candidate.
fn find_ci(name: &[u8], q: &[u8]) -> Option<usize> {
    if q.len() > name.len() {
        return None;
    }
    if q.is_empty() {
        return Some(0);
    }
    let (lower, upper) = (q[0].to_ascii_lowercase(), q[0].to_ascii_uppercase());
    // The last place the whole query still fits: a first byte further out
    // than this is a byte the name has and the query does not.
    let last = name.len() - q.len();
    let mut at = 0usize;
    while at <= last {
        let window = &name[at..=last];
        let hit = if lower == upper {
            rgx_core::memchr::memchr(lower, window).map(|i| at + i)
        } else {
            match (rgx_core::memchr::memchr(lower, window).map(|i| at + i), rgx_core::memchr::memchr(upper, window).map(|i| at + i)) {
                (Some(a), Some(b)) => Some(a.min(b)),
                (Some(a), None) => Some(a),
                (None, Some(b)) => Some(b),
                (None, None) => None,
            }
        };
        let i = hit?;
        if name[i + 1..i + q.len()].iter().zip(&q[1..]).all(|(&a, &b)| fold(a) == b) {
            return Some(i);
        }
        at = i + 1;
    }
    None
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
    #[cfg(not(any(target_arch = "aarch64", target_arch = "x86_64")))]
    let _ = wide;
    #[cfg(any(target_arch = "aarch64", target_arch = "x86_64"))]
    if name.len() <= 64 && q.len() <= 32 {
        if let Some(w) = wide {
            return fuzzy_masked(name, w, q, cap);
        }
        let mut buf = [0u8; 64];
        buf[..name.len()].copy_from_slice(name);
        return fuzzy_masked(name, &buf, q, cap);
    }
    fuzzy_scalar::<NoMatch>(name, q, cap, &mut NoMatch)
}

/// The memchr-based matcher: `fuzzy_masked`'s scalar twin, still the path
/// for names longer than the 64-byte window. The wide scorer must agree
/// with this exactly — `tests::sse2_masked_matches_the_scalar_matcher`
/// pins that.
fn fuzzy_scalar<M: Matched>(name: &[u8], q: &[u8], cap: i32, matched: &mut M) -> Option<i32> {
    // Leftmost-ending match: jump to each query byte in turn (memchr is
    // SIMD; most names fail on the first or second byte).
    let mut end = 0;
    let mut from = 0;
    for &c in q {
        end = from + find_folded(&name[from..], c)?;
        from = end + 1;
    }
    let next = |k: usize, from: usize, to: usize| Some(from + find_folded(&name[from..=to], q[k])?);
    fuzzy_from(name, q, cap, end, next, |k, to| rfind_folded(&name[..to], q[k]), matched)
}

/// `fuzzy_score_capped` past the prefix test, for a name of at most 64
/// bytes starting `src`: each query byte's places in the name as one
/// 64-bit mask (NEON compares over the whole name at once) stand in for
/// memchr's scans, which cost several times more per name tried.
#[cfg(target_arch = "aarch64")]
fn fuzzy_masked(name: &[u8], src: &[u8; 64], q: &[u8], cap: i32) -> Option<i32> {
    use core::arch::aarch64::*;
    let valid = u64::MAX.checked_shr(64 - name.len() as u32).unwrap_or(0);
    // Safety (here and below): NEON is part of aarch64; loads stay in `src`.
    let v: [uint8x16_t; 4] = core::array::from_fn(|i| unsafe {
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
    fuzzy_from(name, q, cap, end, next, prev, &mut NoMatch)
}

/// The x86_64 arm: the same 64-byte position-mask algorithm on SSE2 (the
/// baseline SIMD of x86_64, so no runtime dispatch). fsearch is NEON-only;
/// this is the wide scorer the merge plan asks for on Windows/Linux, where
/// `_mm_movemask_epi8` packs the 64 per-byte compares far more directly
/// than the NEON weight/pairwise-add scheme.
#[cfg(target_arch = "x86_64")]
fn fuzzy_masked(name: &[u8], src: &[u8; 64], q: &[u8], cap: i32) -> Option<i32> {
    use core::arch::x86_64::*;
    let valid = u64::MAX.checked_shr(64 - name.len() as u32).unwrap_or(0);
    // Fold each 16-byte chunk to lowercase ASCII, wrapping like the NEON
    // arm: `(x - 'A') < 26` unsigned then `x | 0x20`.
    let v: [__m128i; 4] = core::array::from_fn(|i| unsafe {
        let x = _mm_loadu_si128(src.as_ptr().add(16 * i) as *const __m128i);
        let t = _mm_sub_epi8(x, _mm_set1_epi8(b'A' as i8));
        // Unsigned (x - 'A') < 26 via the xor-0x80 signed-compare trick.
        let lt26 =
            _mm_cmplt_epi8(_mm_xor_si128(t, _mm_set1_epi8(0x80u8 as i8)), _mm_xor_si128(_mm_set1_epi8(26u8 as i8), _mm_set1_epi8(0x80u8 as i8)));
        _mm_or_si128(x, _mm_and_si128(lt26, _mm_set1_epi8(0x20)))
    });
    // Byte i of a compare lands in bit i of the u64: movemask returns one
    // bit per byte already, so the NEON weight/pack dance is unnecessary.
    let places = |c: u8| {
        let mut bits = 0u64;
        for (i, chunk) in v.iter().enumerate() {
            // Safety: SSE2 is baseline on x86_64; loads stay in `src`.
            let eq = unsafe {
                let cc = _mm_set1_epi8(c as i8);
                _mm_movemask_epi8(_mm_cmpeq_epi8(*chunk, cc)) as u64
            };
            bits |= eq << (16 * i);
        }
        bits & valid
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
    fuzzy_from(name, q, cap, end, next, prev, &mut NoMatch)
}

/// Where the matcher writes the bytes it matched.
///
/// The search that scores does not want them and does not pay for them
/// (`NoMatch` is a zero-sized type and the call is compiled out); the one
/// that shows the span does.
trait Matched {
    /// The matcher matched the byte at `at`.
    fn record(&mut self, at: usize);
}

/// The matcher that matches nothing down.
struct NoMatch;

impl Matched for NoMatch {
    #[inline(always)]
    fn record(&mut self, _at: usize) {}
}

impl Matched for Vec<u32> {
    #[inline(always)]
    fn record(&mut self, at: usize) {
        self.push(at as u32);
    }
}

/// The rest of `fuzzy_score_capped` once the leftmost-ending match ends at
/// `end`: `next(k, from, to)` finds query byte k's first place in
/// from..=to, `prev(k, to)` its last one before `to`.
#[inline(always)]
fn fuzzy_from<M: Matched>(
    name: &[u8],
    q: &[u8],
    cap: i32,
    end: usize,
    next: impl Fn(usize, usize, usize) -> Option<usize>,
    prev: impl Fn(usize, usize) -> Option<usize>,
    matched: &mut M,
) -> Option<i32> {
    if q.len() == 1 {
        matched.record(end);
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
        matched.record(at);
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
    if c.is_ascii_lowercase() { rgx_core::memchr::memchr2(c, c - 32, s) } else { rgx_core::memchr::memchr(c, s) }
}

/// Last byte of `s` that folds to `c`.
#[inline(always)]
fn rfind_folded(s: &[u8], c: u8) -> Option<usize> {
    if c.is_ascii_lowercase() { rgx_core::memchr::memrchr2(c, c - 32, s) } else { rgx_core::memchr::memrchr(c, s) }
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
        for sp in rgx_core::memchr::memchr_iter(b' ', name) {
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
            let typo = if m & t.start != 0 { typo_score(name, wide, m, &t.text) } else { None };
            clean.max(typo).or_else(|| shadow_score(name, &t.text))
        }
        Mode::Fuzzy => fuzzy_score_capped(name, wide, &t.text, 100).or_else(|| shadow_score(name, &t.text)),
        Mode::Exact => find_ci(name, &t.text).map(|p| 40 + if p == 0 { 30 } else { 0 } - (name.len() as i32).min(80) / 3),
        Mode::Prefix => {
            (name.len() >= t.text.len() && name.iter().zip(&t.text).all(|(&a, &b)| fold(a) == b)).then(|| 60 - (name.len() as i32).min(80) / 3)
        }
        Mode::Suffix => (name.len() >= t.text.len() && name[name.len() - t.text.len()..].iter().zip(&t.text).all(|(&a, &b)| fold(a) == b))
            .then(|| 50 - (name.len() as i32).min(80) / 3),
        // The phrase: the words together, in order, with the spaces between
        // them. It is the match that a name has to carry whole.
        Mode::Phrase => find_ci(name, &t.text).map(|p| 40 + if p == 0 { 30 } else { 0 } - (name.len() as i32).min(80) / 3),
        // The word: the query is a whole word of the name, not a fragment
        // of one. `port` is the word of `report_port.txt` and not of
        // `report`.
        Mode::Keyword => keyword_score(name, &t.text),
    }
}

/// The score of a name that has the query as a whole word.
///
/// The words are the runs between the delimiters (the same ones the
/// scorer's boundary bonus knows: space, `_`, `-`, `.`, `/`, the brackets,
/// the comma, the plus and the at). A word that matches is the tightest
/// match there is, so it scores like the exact mode does.
fn keyword_score(name: &[u8], q: &[u8]) -> Option<i32> {
    let (start, _) = keyword_span(name, q)?;
    Some(40 + if start == 0 { 30 } else { 0 } - (name.len() as i32).min(80) / 3)
}

/// The bytes of the word of `name` that is the query.
fn keyword_span(name: &[u8], q: &[u8]) -> Option<(usize, usize)> {
    if q.is_empty() {
        return None;
    }
    let mut start = 0usize;
    for i in 0..name.len() {
        let delim = class(name[i]) == Class::Delim;
        if !delim && i + 1 != name.len() {
            continue;
        }
        let end = if delim { i } else { i + 1 };
        let word = &name[start..end];
        if word.len() == q.len() && word.iter().zip(q).all(|(&a, &b)| fold(a) == b) {
            return Some((start, end));
        }
        start = i + 1;
    }
    None
}

/// The characters of a name that the query matched.
///
/// The offsets are in characters from the start of the name: the first
/// character is 0. The matcher works on bytes, so the bytes it matched are
/// snapped to the characters they belong to — a character is matched whole
/// or not at all.
///
/// A name that matched through a typo, or through its shadow (the pinyin or
/// the kana reading), has no positions: the span would be in another text.
pub fn matched_chars(name: &[u8], tokens: &[Token]) -> Vec<u32> {
    // The comparison runs on the fold of the name (the fold rule,
    // rule 2), and so does the highlight: the token is folded, so a name
    // the filesystem stored in NFD has to be folded here as well. Without
    // it the comparison misses a name that does match, and the row shows
    // no span at all.
    let folded = fold::fold(name);
    let mut out: Vec<u32> = Vec::new();
    for t in tokens.iter().filter(|t| !t.negate) {
        // A token on another axis matched another part of the path -- the
        // folder, the attribute, the extension. It says nothing about the
        // name, so it highlights nothing in it.
        if t.axis != Axis::Name {
            continue;
        }
        match t.mode {
            Mode::Fuzzy => {
                if let Some(bytes) = fuzzy_matched_bytes(&folded, &t.text) {
                    out.extend(chars_of_bytes(&folded, &bytes));
                }
            }
            Mode::Exact | Mode::Phrase => {
                if let Some(p) = find_ci(&folded, &t.text) {
                    out.extend(chars_of_range(&folded, p..p + t.text.len()));
                }
            }
            Mode::Prefix => {
                // The prefix has to be the prefix. A token that matched
                // through a folder of the path did not match the name's
                // first bytes, and highlighting them would be a lie.
                if starts_with_ci(&folded, &t.text) {
                    out.extend(chars_of_range(&folded, 0..t.text.len().min(folded.len())));
                }
            }
            Mode::Suffix => {
                if ends_with_ci(&folded, &t.text) {
                    let s = folded.len().saturating_sub(t.text.len());
                    out.extend(chars_of_range(&folded, s..folded.len()));
                }
            }
            Mode::Keyword => {
                if let Some((s, e)) = keyword_span(&folded, &t.text) {
                    out.extend(chars_of_range(&folded, s..e));
                }
            }
        }
    }
    // The positions are the folded name's characters, and the caller shows
    // the raw one.
    let mut out = raw_positions(name, &folded, out);
    out.sort_unstable();
    out.dedup();
    out
}

/// Whether `name` starts with `q`, ignoring ASCII case.
///
/// The query is folded and lowercased, so only the name's case is in
/// question.
fn starts_with_ci(name: &[u8], q: &[u8]) -> bool {
    q.len() <= name.len() && name[..q.len()].iter().zip(q).all(|(a, b)| a.to_ascii_lowercase() == *b)
}

/// Whether `name` ends with `q`, ignoring ASCII case.
fn ends_with_ci(name: &[u8], q: &[u8]) -> bool {
    q.len() <= name.len() && name[name.len() - q.len()..].iter().zip(q).all(|(a, b)| a.to_ascii_lowercase() == *b)
}

/// The raw name's characters that the folded name's characters stand for.
///
/// The fold composes: the raw name "cafe" plus a combining acute is five
/// characters, and the folded "café" is four. The folded character came
/// from the raw's base character and the marks that followed it, so it
/// maps to the base character -- and the marks belong to the same glyph,
/// so they are highlighted with it.
///
/// The walk is honest about the cases it cannot follow: when the two
/// names do not line up (Hangul, where the fold composes two starters),
/// the answer is no positions at all. A span in the wrong place is worse
/// than no span.
fn raw_positions(raw: &[u8], folded: &[u8], positions: Vec<u32>) -> Vec<u32> {
    if positions.is_empty() {
        return positions;
    }
    // The fold did nothing: the positions are already the raw's.
    if raw == folded {
        return positions;
    }
    let mut out = Vec::with_capacity(positions.len());
    let mut wanted = positions.into_iter().peekable();
    let mut raw_char = 0u32;
    let mut folded_char = 0u32;
    let mut ri = 0usize;
    let mut fi = 0usize;
    while fi < folded.len() {
        // The folded character at `fi` came from the raw character at
        // `ri`, and from the marks that followed it.
        while wanted.peek().is_some_and(|w| *w < folded_char) {
            wanted.next();
        }
        if wanted.peek().is_some_and(|w| *w == folded_char) {
            wanted.next();
            out.push(raw_char);
            // The marks are part of the same glyph.
            let mut m = ri + utf8_len(raw[ri]);
            let mut k = raw_char + 1;
            while m < raw.len() && combining_class(raw, m) != 0 {
                out.push(k);
                k += 1;
                m += utf8_len(raw[m]);
            }
        }
        // Advance the raw past the base and the marks that were composed
        // with it.
        let mut next = ri + utf8_len(raw[ri]);
        let mut marks = 0u32;
        while next < raw.len() && combining_class(raw, next) != 0 {
            next += utf8_len(raw[next]);
            marks += 1;
        }
        ri = next;
        raw_char += 1 + marks;
        fi += utf8_len(folded[fi]);
        folded_char += 1;
    }
    // The two names did not line up: the answer is no positions.
    if raw_char != raw.iter().filter(|b| **b & 0xc0 != 0x80).count() as u32 {
        return Vec::new();
    }
    out
}

/// The length in bytes of the UTF-8 character that starts with `b`.
fn utf8_len(b: u8) -> usize {
    if b < 0x80 {
        1
    } else if b >> 5 == 0b110 {
        2
    } else if b >> 4 == 0b1110 {
        3
    } else if b >> 3 == 0b11110 {
        4
    } else {
        1
    }
}

/// The canonical combining class of the character at `at`.
///
/// Zero for a starter, and for a byte that is not text.
fn combining_class(bytes: &[u8], at: usize) -> u8 {
    let Some(&b) = bytes.get(at) else { return 0 };
    let len = utf8_len(b).min(bytes.len() - at);
    let Ok(text) = core::str::from_utf8(&bytes[at..at + len]) else {
        return 0;
    };
    let Some(c) = text.chars().next() else { return 0 };
    crate::nfc::ccc(c as u32)
}

/// The characters of `name` that the bytes in `bytes` fall in.
fn chars_of_bytes(name: &[u8], bytes: &[u32]) -> Vec<u32> {
    if name.is_ascii() {
        return bytes.to_vec();
    }
    let mut out = Vec::with_capacity(bytes.len());
    let mut chars = 0u32;
    let mut bi = 0usize;
    for (i, b) in name.iter().enumerate() {
        // A continuation byte is 10xxxxxx: it belongs to the character
        // before it and is not one of its own.
        let is_start = b & 0xc0 != 0x80;
        let idx = i as u32;
        while bi < bytes.len() && bytes[bi] < idx {
            bi += 1;
        }
        if bi < bytes.len() && bytes[bi] == idx {
            out.push(if is_start { chars } else { chars - 1 });
            bi += 1;
        }
        if is_start {
            chars += 1;
        }
    }
    out
}

/// The characters of `name` that the bytes in `range` fall in.
fn chars_of_range(name: &[u8], range: core::ops::Range<usize>) -> Vec<u32> {
    if name.is_ascii() {
        return (range.start as u32..range.end as u32).collect();
    }
    let mut out = Vec::with_capacity(range.len());
    let mut chars = 0u32;
    for (i, b) in name.iter().enumerate() {
        let is_start = b & 0xc0 != 0x80;
        if i >= range.start && i < range.end {
            out.push(if is_start { chars } else { chars - 1 });
        }
        if is_start {
            chars += 1;
        }
    }
    out
}

/// The bytes a fuzzy match of `q` in `name` covers.
///
/// The same match the scorer takes: the prefix when the name starts with
/// the query, the leftmost-ending match otherwise.
fn fuzzy_matched_bytes(name: &[u8], q: &[u8]) -> Option<Vec<u32>> {
    if q.is_empty() {
        return None;
    }
    let off = (name.len() > 1 && name[0] == b'.') as usize;
    if prefix_score(name, q, 100).is_some() {
        return Some((off..off + q.len()).map(|i| i as u32).collect());
    }
    let mut at = Vec::new();
    fuzzy_scalar(name, q, 100, &mut at)?;
    Some(at)
}

/// The score of the query against the name read in another script.
///
/// `nihongo` finds 日本語 and `zhongwen` finds 中文: the name is
/// transliterated (see [`crate::readings`]) and the query is matched
/// against that. A name with no kana and no Han character has no
/// shadow, and the direct match already covers it.
///
/// The query says which shadow: an ASCII query is romaji or pinyin, and a
/// query with kana in it is kana.
fn shadow_score(name: &[u8], q: &[u8]) -> Option<i32> {
    use crate::readings::{Script, Shadow};
    if !name.iter().any(|&b| b >= 0xE3) {
        return None;
    }
    let is_ascii = q.is_ascii();
    let scripts: &[Script] = if is_ascii { &[Script::Romaji, Script::Pinyin] } else { &[Script::Hiragana, Script::Katakana] };
    let mut best: Option<i32> = None;
    for s in scripts {
        let Some(sh) = Shadow::of(name, *s) else {
            if !is_ascii {
                break;
            }
            continue;
        };
        if let Some(score) = fuzzy_score(&sh.text, q) {
            best = Some(best.map_or(score, |b: i32| b.max(score)));
        }
    }
    best
}

/// Does the name match the token?
///
/// The negation asks the same question the match does, so what `!x` keeps
/// is what `x` would not have found: the fuzzy match forgives one typo in
/// a short word, and the negation has to forgive it too, or `!invoice`
/// answers `invoce`.
#[inline]
fn token_matches(name: &[u8], t: &Token) -> bool {
    token_score(name, None, !0, t).is_some()
}

/// Does the name end in one of `exts` (ASCII-case-insensitive)?
///
/// The extension is the one `ext_of` names, so the filter and the axis
/// read it the same way and cannot disagree about a dotfile.
fn ext_ok(name: &[u8], exts: &[Vec<u8>]) -> bool {
    let Some(e) = ext_of(name) else { return false };
    exts.iter().any(|x| x.len() == e.len() && x.iter().zip(e).all(|(&a, &b)| a == fold(b)))
}

/// Whole-path glob: `*` runs, `?` one byte, ASCII-case-insensitive
/// (zen-rs rule — the pattern was lowercased at parse). `pat` must
/// contain a wildcard (that is the only way a term becomes a glob).
fn glob_match(pat: &[u8], path: &[u8]) -> bool {
    let (mut pi, mut si) = (0usize, 0usize);
    let mut star: Option<(usize, usize)> = None;
    while si < path.len() {
        match pat.get(pi) {
            Some(&b'*') => {
                while pat.get(pi) == Some(&b'*') {
                    pi += 1;
                }
                if pi == pat.len() {
                    return true;
                }
                star = Some((pi, si));
            }
            Some(&b'?') => {
                pi += 1;
                si += 1;
            }
            Some(&c) if fold(path[si]) == c => {
                pi += 1;
                si += 1;
            }
            _ => {
                let Some((sp, ss)) = &mut star else { return false };
                *ss += 1;
                si = *ss;
                pi = *sp;
            }
        }
    }
    while pat.get(pi) == Some(&b'*') {
        pi += 1;
    }
    pi == pat.len()
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
        [
            "name",
            "ext",
            "extension",
            "folder",
            "attr",
            "attribute",
            "dir",
            "type",
            "kind",
            "in",
            "size",
            "mtime",
            "modified",
            "re",
            "path",
            "limit",
            "sort",
            "grep",
            "content",
            "regex",
            "sym",
            "symbol",
        ]
        .iter()
        .any(|p| k.eq_ignore_ascii_case(p))
    })
}

/// Split a query string on `|`, but not inside a `re:` / `regex:` pattern
/// (zen-rs rule). `re:(a|b)` and `re:a|b` stay one alternative;
/// `re:foo | re:bar`, `re:a\.rs$|re:b\.rs$`, and `ext:rs|ext:txt` split at
/// the query level.
fn split_alternatives(input: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut start = 0usize;
    let mut prev_pipe = None;
    for (i, ch) in input.char_indices() {
        if ch != '|' {
            continue;
        }
        let after = &input[i + 1..];
        // Inside a `re:` pattern the `|` belongs to the regex, unless what
        // follows re-enters query syntax.
        let seg = &input[start..i];
        let in_re = seg.rfind(':').is_some_and(|colon| {
            let word_start = seg[..colon].rfind(char::is_whitespace).map(|w| w + 1).unwrap_or(0);
            let key = &seg[word_start..colon];
            let key = key.strip_prefix('!').or_else(|| key.strip_prefix('-')).unwrap_or(key);
            key.eq_ignore_ascii_case("re") || key.eq_ignore_ascii_case("regex")
        }) && !starts_query_syntax(after);
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
/// (zen `|` semantics merged over fsearch alternatives). Each
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
    use alloc::format;
    use alloc::string::String;
    use alloc::vec;
    use alloc::vec::Vec;

    fn q(s: &str) -> Query {
        Query::parse(s, "").expect("parses")
    }

    /// A pattern filter keeps its pattern.
    ///
    /// `regex:need.*` is a regex, and the `*` belongs to the engine that
    /// runs it. The wildcard test used to come first, and the whole word
    /// — the prefix included — became a glob for a file called
    /// `regex:need.*`: the content search was voided, and the tool said
    /// "nothing found" for a search it never ran. The same for `grep:`
    /// and `sym:`.
    #[test]
    fn a_pattern_filter_keeps_its_pattern() {
        let regex = q("regex:need.*");
        assert_eq!(regex.grep.as_deref(), Some("need.*"), "the pattern reaches the engine whole");
        assert_eq!(regex.grep_mode, GrepMode::Regex, "and it is a regex");
        assert!(regex.globs.is_empty(), "and it is not a glob for a file's name");

        let literal = q("grep:needle");
        assert_eq!(literal.grep.as_deref(), Some("needle"), "a literal grep keeps its text");
        assert_eq!(literal.grep_mode, GrepMode::Literal, "and it is a literal");

        let symbol = q("sym:need*");
        assert_eq!(symbol.grep.as_deref(), Some("need*"), "a symbol pattern keeps its text too");
        assert_eq!(symbol.grep_mode, GrepMode::Symbol, "and it is a symbol");

        // A bare word with a wildcard is still a glob for the name.
        let glob = q("*.rs");
        assert!(glob.grep.is_none(), "a bare wildcard is not a content search");
        assert_eq!(glob.globs.len(), 1, "and it is a whole-path glob");
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

    /// The four axes: the name, the extension, a folder and an attribute.
    ///
    /// The engine matches against four independent parts of a path
    ///. A word says which part it means: `name:`,
    /// `ext:`, `folder:` and `attr:`. A bare word is the name axis, which
    /// is what it has always meant.
    #[test]
    fn a_word_says_which_axis_it_matches() {
        // The name axis is the default.
        assert_eq!(q("invoice").tokens[0].axis, Axis::Name);
        assert_eq!(q("name:invoice").tokens[0].axis, Axis::Name);
        assert_eq!(q("ext:^pdf").tokens[0].axis, Axis::Extension);
        assert_eq!(q("folder:documents").tokens[0].axis, Axis::Folder);
        assert_eq!(q("attr:hidden").tokens[0].axis, Axis::Attribute);
        // The axis survives the matcher's own markers.
        assert_eq!(q("ext:^pdf").tokens[0].axis, Axis::Extension);
        assert_eq!(q("folder:!docs").tokens[0].axis, Axis::Folder);
        // An unknown prefix is not an axis: it keeps the old meaning.
        assert!(Query::parse("nosuchaxis:x", "").is_err(), "an unknown prefix is not a filter");
    }

    /// The extension axis matches the extension, not the name.
    #[test]
    fn the_extension_axis_matches_the_extension() {
        // The axis form carries one of the matcher's markers, so it is not
        // read as the extension filter.
        assert!(hit("ext:^pdf", "/a/report.pdf", KIND_FILE, 10));
        assert!(!hit("ext:^pdf", "/a/pdf-report.txt", KIND_FILE, 10));
        // The filter form is what it has always been: the extension, and
        // the whole name is not matched by it.
        assert!(hit("ext:pdf", "/a/report.pdf", KIND_FILE, 10));
        assert!(!hit("ext:pdf", "/a/pdf-report.txt", KIND_FILE, 10));
        // The name axis is the old behaviour: it matches the name, which
        // includes the extension.
        assert!(hit("pdf", "/a/pdf-report.txt", KIND_FILE, 10));
        assert!(hit("pdf", "/a/report.pdf", KIND_FILE, 10));
    }

    /// The two forms of `ext:` agree about a dotfile.
    ///
    /// `ext:pdf` is the filter ("the extension is pdf") and `ext:^pdf` is
    /// the axis ("the extension, starting with pdf"), and they are the
    /// same extension read the same way: the tail after the last dot. A
    /// dotfile's extension is its own name without the dot, so `.bashrc`
    /// is a `bashrc` under both.
    #[test]
    fn the_two_forms_of_ext_agree_about_a_dotfile() {
        // The filter form takes the dotfile.
        assert!(hit("ext:bashrc", "/x/.bashrc", KIND_FILE, 10), "the filter takes it");
        // And the axis form does too: it used to say no, and the two forms
        // of the same filter gave two different answers.
        assert!(hit("ext:^bashrc", "/x/.bashrc", KIND_FILE, 10), "the axis form takes it as well");
        // A name with an extension is the same under both.
        assert!(hit("ext:pdf", "/x/report.pdf", KIND_FILE, 10));
        assert!(hit("ext:^pdf", "/x/report.pdf", KIND_FILE, 10));
        // And a name with no dot has no extension, under either form.
        assert!(!hit("ext:bashrc", "/x/README", KIND_FILE, 10), "no dot, no extension");
        assert!(!hit("ext:^bashrc", "/x/README", KIND_FILE, 10), "no dot, no extension");
    }

    /// The folder axis matches a folder of the path.
    #[test]
    fn the_folder_axis_matches_a_folder() {
        // The word is in the folder, not in the name.
        assert!(hit("folder:documents", "/x/documents/report.txt", KIND_FILE, 10));
        assert!(!hit("folder:documents", "/x/report.txt", KIND_FILE, 10));
        // The name axis does not look in the folders for its own word.
        assert!(!hit("documents", "/x/documents/report.txt", KIND_FILE, 10));
    }

    /// The attribute axis matches the file's attributes.
    #[test]
    fn the_attribute_axis_matches_the_attributes() {
        assert!(hit("attr:dir", "/x/report", KIND_DIR, 10));
        assert!(!hit("attr:dir", "/x/report.txt", KIND_FILE, 10));
        assert!(hit("attr:file", "/x/report.txt", KIND_FILE, 10));
        assert!(hit("attr:hidden", "/x/report.txt", KIND_FILE | FLAG_HIDDEN, 10));
        assert!(!hit("attr:hidden", "/x/report.txt", KIND_FILE, 10));
    }

    /// The flags sit in the kind byte, above the two bits that hold the
    /// kind, and they are read where they sit.
    ///
    /// The test above used to pass `FLAG_HIDDEN << 2`, which is the bit the
    /// shifted lookup was looking at: the shift was in the code and in the
    /// fixture, so the two cancelled and the bug was invisible. The flag is
    /// the flag here, and a file that is hidden and not a directory is
    /// hidden.
    #[test]
    fn the_attribute_flags_are_read_where_they_sit() {
        // Hidden, and a file besides.
        assert!(hit("attr:hidden", "/x/report.txt", KIND_FILE | FLAG_HIDDEN, 10));
        assert!(!hit("attr:hidden", "/x/report.txt", KIND_FILE | FLAG_MOUNT, 10));
        // A mount point, and not hidden.
        assert!(hit("attr:mount", "/x/mnt", KIND_DIR | FLAG_MOUNT, 10));
        assert!(!hit("attr:mount", "/x/mnt", KIND_DIR | FLAG_HIDDEN, 10));
        // The kind and the flags do not tread on each other: a hidden
        // directory is a directory.
        assert!(hit("attr:dir", "/x/mnt", KIND_DIR | FLAG_HIDDEN | FLAG_MOUNT, 10));
        assert!(hit("attr:hidden", "/x/mnt", KIND_DIR | FLAG_HIDDEN | FLAG_MOUNT, 10));
        // The old lookup would have found the hidden flag of this one: the
        // shift put it in bit 4, where the code was looking.
        assert!(!hit("attr:hidden", "/x/report.txt", KIND_FILE | (FLAG_HIDDEN << 2), 10));
    }

    /// A tree with a folder "docs" holding "invoice.txt", and a file
    /// "docs.txt" beside it.
    ///
    /// The name "docs" is both a folder's and a file's, so an axis can be
    /// told from the name axis: the folder axis matches the folder, and
    /// the name axis would match the file as well.
    fn axis_tree() -> Live {
        use rgy_discovery::walk::{KIND_DIR, KIND_FILE, Listing, NONE, RawEnt};
        let listings = vec![
            Listing {
                id: 0,
                names: b"docsdocs.txt".to_vec(),
                ents: vec![
                    RawEnt { name_off: 0, name_len: 4, kind: KIND_DIR, size: 0, mtime: 7, child: 1 },
                    RawEnt { name_off: 4, name_len: 8, kind: KIND_FILE, size: 3, mtime: 7, child: NONE },
                ],
            },
            Listing {
                id: 1,
                names: b"invoice.txt".to_vec(),
                ents: vec![RawEnt { name_off: 0, name_len: 11, kind: KIND_FILE, size: 3, mtime: 7, child: NONE }],
            },
        ];
        Live::new(Index::build(listings, 0, 1_700_000_000, b"/"))
    }

    /// The folder axis matches a folder of the path, and not a name.
    ///
    /// The file "docs.txt" is not under a folder named "docs"; the entry
    /// under the folder "docs" is. The axis is not the name axis with
    /// another spelling: matched by the name, the file answers too -- and
    /// it did, because the token was scored against the name.
    ///
    /// The search goes through the index here, not through `match_path`:
    /// the overlay and the content search answer the axes, and the index
    /// is where the answer was wrong.
    #[test]
    fn the_folder_axis_matches_a_folder_of_the_path_and_not_a_name() {
        let live = axis_tree();
        let s = Searcher { live: &live };
        // The entry under the folder "docs" matches; the file whose name is
        // "docs.txt" does not.
        let hits = s.search(&q("folder:docs"));
        assert_eq!(hits.len(), 1, "the entry under the folder matches, and the file named docs.txt does not");
        // With a name token as well, the same answer: the name says which
        // file, and the folder says where it has to be.
        let hits = s.search(&q("folder:docs invoice"));
        assert_eq!(hits.len(), 1, "the entry under the folder matches");
    }

    /// A name token asks the memo about its own slot.
    ///
    /// The overlay's folder memo holds the best folder score per positive
    /// token, and the place a token has among the positive ones is what
    /// says which slot is its. The name-axis tokens were numbered among
    /// themselves instead, so a query with a token on another axis before
    /// them asked the memo about a different token -- and the answer
    /// changed: the word the folder carried was asked of the wrong slot,
    /// and the file under the folder stopped answering.
    #[test]
    fn a_name_token_asks_the_memo_about_its_own_slot() {
        let live = axis_tree();
        let s = Searcher { live: &live };
        // The extension token comes first, so the folder's word is not in
        // the first slot.
        let query = "ext:^txt docs invoice";
        assert!(hit(query, "/docs/invoice.txt", KIND_FILE, 10), "the overlay inherits the folder's word");
        let hits = s.search(&q(query));
        assert_eq!(hits.len(), 1, "the index inherits it too");
    }

    /// A bare word is still answered by a folder of the path.
    ///
    /// This is what a bare word has always meant -- the word is the file's
    /// name, or a folder above it -- and the two paths have to agree on it:
    /// the overlay inherits the folder's word, and so did the index.
    #[test]
    fn a_bare_word_is_inherited_from_a_folder() {
        let live = axis_tree();
        let s = Searcher { live: &live };
        assert!(hit("invoice docs", "/docs/invoice.txt", KIND_FILE, 10), "the overlay inherits the folder's word");
        let hits = s.search(&q("invoice docs"));
        assert_eq!(hits.len(), 1, "the index inherits it too");
    }

    /// The name that matches a folder token has to be a candidate.
    ///
    /// The folder token is answered by the memo, and the memo is built out
    /// of the name table: the folder's own name hit is what carries the
    /// token's bit. So the names that match a folder token have to be
    /// candidates -- and the table's candidates were the names that could
    /// match a name-axis token, and the folder's own name matches none.
    #[test]
    fn the_name_that_matches_a_folder_token_is_a_candidate() {
        let live = axis_tree();
        let s = Searcher { live: &live };
        let query = q("folder:docs invoice");
        let pos: Vec<&Token> = query.tokens.iter().filter(|t| !t.negate).collect();
        let neg: Vec<&Token> = query.tokens.iter().filter(|t| t.negate).collect();
        let scored = s.names(&query, &pos, &neg, 0, s.live.base.n);
        let axes = query.axes();
        let carried = scored.names.iter().any(|(_, h)| h.bits as u32 & axes.folder == axes.folder);
        assert!(carried, "the name that matches the folder token is in the table");
    }

    /// The scan that walks the table answers the folder axis, and the same
    /// answer the driven scan gives.
    ///
    /// There are two scans: the driven one, which walks the path of every
    /// candidate it finds, and the one that walks the name table. The
    /// driven one always got the folder axis right; the other one did not,
    /// because the folder's own name was not among the table's candidates
    /// -- so the memo had no bit for the folder token, and the answer was
    /// nothing where the driven scan said one hit.
    ///
    /// Two answers to one question, and a question they answer differently
    /// is a bug in one of them.
    #[test]
    fn the_table_scan_answers_the_folder_axis() {
        let live = axis_tree();
        let s = Searcher { live: &live };
        let query = q("folder:docs invoice");
        let pos: Vec<&Token> = query.tokens.iter().filter(|t| !t.negate).collect();
        let neg: Vec<&Token> = query.tokens.iter().filter(|t| t.negate).collect();
        let scored = s.names(&query, &pos, &neg, 0, s.live.base.n);
        let axes = query.axes();
        let scan = Scan {
            q: &query,
            live: &live,
            names: &scored.names,
            npos: pos.len(),
            need_dirs: true,
            all: all_mask(pos.len(), &axes),
            axes,
            now: now_secs(),
        };
        let driven = s.search(&query);
        let table = scan.selective(1, s.live.base.n);
        assert_eq!(driven.len(), 1, "the driven scan finds the entry under the folder");
        assert_eq!(table.len(), driven.len(), "the table scan answers the same");
    }

    /// And the negated folder token, which poisons the memo of the folder
    /// it matches.
    ///
    /// `folder:!docs invoice` says "the invoice, not under a folder named
    /// docs". The poisoned memo is what says so, and the memo is built out
    /// of the folder's own name hit -- so the negated folder's name has to
    /// be a candidate as well, or the rejection never happens.
    #[test]
    fn the_table_scan_answers_the_negated_folder_axis() {
        let live = axis_tree();
        let s = Searcher { live: &live };
        let query = q("folder:!docs invoice");
        let pos: Vec<&Token> = query.tokens.iter().filter(|t| !t.negate).collect();
        let neg: Vec<&Token> = query.tokens.iter().filter(|t| t.negate).collect();
        let scored = s.names(&query, &pos, &neg, 0, s.live.base.n);
        let axes = query.axes();
        let scan = Scan {
            q: &query,
            live: &live,
            names: &scored.names,
            npos: pos.len(),
            need_dirs: true,
            all: all_mask(pos.len(), &axes),
            axes,
            now: now_secs(),
        };
        let driven = s.search(&query);
        let table = scan.selective(1, s.live.base.n);
        assert_eq!(driven.len(), 0, "the entry under the folder docs is rejected");
        assert_eq!(table.len(), driven.len(), "the table scan answers the same");
    }

    /// The attribute axis matches the entry's kind.
    ///
    /// The attribute is the kind byte -- the kind and the flags -- and the
    /// scan reads that byte anyway.
    #[test]
    fn the_attribute_axis_matches_the_kind_of_the_entry() {
        let live = axis_tree();
        let s = Searcher { live: &live };
        let hits = s.search(&q("attr:dir"));
        assert_eq!(hits.len(), 1, "the folder matches the attribute");
        let hits = s.search(&q("attr:file"));
        assert_eq!(hits.len(), 2, "the two files match the attribute");
    }

    /// The extension axis matches the extension of the name.
    ///
    /// The extension is at the end of the name, so the prefilter -- which
    /// is about the name's start -- says nothing about it.
    #[test]
    fn the_extension_axis_matches_the_extension_of_the_name() {
        let live = axis_tree();
        let s = Searcher { live: &live };
        let hits = s.search(&q("ext:^txt"));
        assert_eq!(hits.len(), 2, "the two .txt names match the extension");
        assert!(s.search(&q("ext:^md")).is_empty(), "no name has that extension");
        // The extension axis and the name axis together: the name says
        // which file, the extension says what it has to be.
        let hits = s.search(&q("ext:^txt invoice"));
        assert_eq!(hits.len(), 1, "the invoice.txt matches");
    }

    /// More negated tokens than a bitmask can hold.
    ///
    /// The parser caps the positive tokens at eight, but a negated token
    /// is not capped: the bitmask that used to carry them overflowed at
    /// the thirty-third, and the shift panicked -- in a debug build,
    /// which is what the CI runs, and in the daemon, which is a service
    /// that dies with it.
    #[test]
    fn more_negated_tokens_than_a_bitmask_holds() {
        let live = axis_tree();
        let s = Searcher { live: &live };
        let negs: Vec<String> = (0..40).map(|i| format!("!w{i}")).collect();
        let hits = s.search(&q(&negs.join(" ")));
        // Nothing is named w0..w39, so every entry answers.
        assert_eq!(hits.len(), 3, "the query answers every entry");
    }

    /// And a negated token still rejects its own name, wherever it sits.
    ///
    /// The rejection happens in the scan, so the names a negated token
    /// matches have to be candidates: the prefilter has to carry the
    /// token that rejects them, or the rejection never happens.
    #[test]
    fn a_negated_token_among_many_still_rejects_its_name() {
        let live = axis_tree();
        let s = Searcher { live: &live };
        let mut negs: Vec<String> = (0..40).map(|i| format!("!w{i}")).collect();
        negs[32] = "!invoice".to_string();
        let hits = s.search(&q(&negs.join(" ")));
        assert_eq!(hits.len(), 2, "the entry named invoice.txt is rejected, and the other two answer");
    }

    /// The attribute axis asks the entry's kind, and it combines with the
    /// other axes: `attr:file invoice` is a file named invoice.
    ///
    /// The attribute token is not a name, so it cannot be the one that
    /// drives the scan: driving on it selects nothing, and the query
    /// answers "nothing" instead of the file.
    #[test]
    fn the_attribute_axis_combines_with_a_name() {
        let live = axis_tree();
        let s = Searcher { live: &live };
        let hits = s.search(&q("attr:file invoice"));
        assert_eq!(hits.len(), 1, "the file named invoice.txt is the answer");
        // And the kind is the question: no folder is named invoice.
        assert!(s.search(&q("attr:dir invoice")).is_empty(), "no folder is named invoice");
        // The attribute alone asks nothing of the name.
        assert_eq!(s.search(&q("attr:file")).len(), 2, "the two files answer");
    }

    /// The extension axis is the name's own extension.
    ///
    /// The folder "backup.txt" has the extension txt, and the file under it
    /// is "invoice.md" -- which does not. A folder's name cannot satisfy
    /// the extension axis of the entries under it: that is what the folder
    /// axis is for.
    #[test]
    fn the_extension_axis_is_the_name_own_extension() {
        let live = ext_tree();
        let s = Searcher { live: &live };
        // The two names that end in txt: the folder and the file. Both are
        // the entry's own name, so both have the extension.
        assert_eq!(s.search(&q("ext:^txt")).len(), 2, "the folder backup.txt and the file invoice.txt");
        assert_eq!(s.search(&q("ext:^md")).len(), 1, "the file named invoice.md");
        // With a name word as well: the file under the folder whose name
        // ends in txt does not have the extension txt.
        let hits = s.search(&q("ext:^txt invoice"));
        assert_eq!(hits.len(), 1, "the extension is the name's own, so only invoice.txt answers");
    }

    /// A negated folder token is about the path's folders, not the name.
    ///
    /// `folder:!docs` says "not under a folder named docs": the file
    /// "docs.txt" is beside that folder, not under it, so it answers.
    #[test]
    fn a_negated_folder_token_is_not_a_negated_name() {
        let live = axis_tree();
        let s = Searcher { live: &live };
        let hits = s.search(&q("folder:!docs"));
        assert_eq!(hits.len(), 2, "the folder docs and the file docs.txt answer; the one under docs does not");
    }

    /// The same word on another axis is another question.
    ///
    /// The name table is cached under what it depends on, and the axis is
    /// part of that: "docs" is the name, "folder:docs" is a folder of the
    /// path, and the table for one is not the table for the other.
    #[test]
    fn the_same_word_on_another_axis_is_another_table() {
        let live = axis_tree();
        let s = Searcher { live: &live };
        // The name "docs" answers the folder and the file.
        assert_eq!(s.search(&q("docs")).len(), 2, "the folder and the file are named docs");
        // The folder axis answers only what is under the folder.
        let hits = s.search(&q("folder:docs"));
        assert_eq!(hits.len(), 1, "only the entry under the folder docs answers");
    }

    /// The index path and the overlay path have to answer the same.
    ///
    /// The overlay answers by matching the path it is given; the index
    /// answers by the name table and the memo. They are two answers to
    /// one question, and a question they answer differently is a bug in
    /// one of them.
    #[test]
    fn the_index_path_and_the_overlay_path_agree() {
        use rgy_discovery::walk::{KIND_DIR, KIND_FILE, Listing, NONE, RawEnt};
        // root: docs/ (dir), invoice/ (dir), docs.txt, invoice.txt
        // docs/invoice.txt, invoice/child.txt
        let listings = vec![
            Listing {
                id: 0,
                names: b"docsdocs.txtinvoiceinvoice.txt".to_vec(),
                ents: vec![
                    RawEnt { name_off: 0, name_len: 4, kind: KIND_DIR, size: 0, mtime: 7, child: 1 },
                    RawEnt { name_off: 4, name_len: 8, kind: KIND_FILE, size: 3, mtime: 7, child: NONE },
                    RawEnt { name_off: 12, name_len: 7, kind: KIND_DIR, size: 0, mtime: 7, child: 2 },
                    RawEnt { name_off: 19, name_len: 11, kind: KIND_FILE, size: 3, mtime: 7, child: NONE },
                ],
            },
            Listing {
                id: 1,
                names: b"invoice.txt".to_vec(),
                ents: vec![RawEnt { name_off: 0, name_len: 11, kind: KIND_FILE, size: 3, mtime: 7, child: NONE }],
            },
            Listing {
                id: 2,
                names: b"child.txt".to_vec(),
                ents: vec![RawEnt { name_off: 0, name_len: 9, kind: KIND_FILE, size: 3, mtime: 7, child: NONE }],
            },
        ];
        let live = Live::new(Index::build(listings, 0, 1_700_000_000, b"/"));
        let s = Searcher { live: &live };
        let paths = ["/docs/invoice.txt", "/invoice/child.txt", "/docs.txt", "/invoice.txt"];
        for query in [
            "!invoice",
            "folder:!docs",
            "!docs",
            "attr:dir invoice",
            "ext:^txt invoice",
            "folder:docs invoice",
            // A bare word is answered by a folder of the path, and the two
            // paths have to agree on that too.
            "invoice docs",
            "docs invoice",
            // The extension axis is the name's own extension, so the folder's
            // extension cannot answer it.
            "ext:^txt docs",
        ] {
            let hits = s.search(&q(query));
            let mut names: Vec<String> = hits
                .iter()
                .map(|h| {
                    let mut p = Vec::new();
                    live.base.path(h.idx as usize, &mut p);
                    String::from_utf8_lossy(&p).to_string()
                })
                .collect();
            names.sort();
            for path in paths {
                let over = hit(query, path, KIND_FILE, 10);
                let by_index = names.iter().any(|n| n == path);
                assert_eq!(over, by_index, "the two paths disagree on {query} for {path}: overlay says {over}, index says {by_index}");
            }
        }
    }

    #[test]
    fn the_overlay_scan_answers_non_name_axes_and_negated_attrs() {
        use rgy_discovery::walk::{KIND_FILE, Listing};
        let mut live = Live::new(Index::build(vec![Listing { id: 0, names: Vec::new(), ents: Vec::new() }], 0, 1_700_000_000, b"/"));
        for path in [b"/docs/invoice.txt".as_slice(), b"/docs/dir.txt".as_slice()] {
            let o = crate::live::OEnt::new(path, KIND_FILE, 10, 7);
            live.flat.masks.push(o.mask);
            live.flat.items.push((path.to_vec(), o));
        }
        let s = Searcher { live: &live };
        assert_eq!(s.search(&q("folder:docs")).len(), 2, "overlay prefilter keeps folder-axis queries");
        assert_eq!(s.search(&q("attr:file")).len(), 2, "overlay prefilter keeps attr-axis queries");
        assert_eq!(s.search(&q("attr:!dir dir")).len(), 1, "negated attr:dir does not reject a file named dir.txt in overlay");

        let idx_live = axis_tree();
        let idx_s = Searcher { live: &idx_live };
        assert_eq!(idx_s.search(&q("attr:!file docs")).len(), 1, "negated attr:file does not reject a folder named docs in index");
        let all_cands: Vec<(u32, u32)> = (1..idx_live.base.n as u32).map(|i| (i, idx_live.base.ent_name()[i as usize])).collect();
        assert_eq!(
            idx_s.rerank_entries(&q("attr:dir"), &all_cands).len(),
            idx_s.search(&q("attr:dir")).len(),
            "rerank_entries enforces non-name axes"
        );
        assert_eq!(
            idx_s.rerank_entries(&q("folder:docs invoice"), &all_cands).len(),
            idx_s.search(&q("folder:docs invoice")).len(),
            "rerank_entries enforces folder axis"
        );
    }

    /// A tree with a folder "backup.txt" holding "invoice.md", and a file
    /// "invoice.txt" beside it: the folder's name ends in txt, the file
    /// under it does not.
    fn ext_tree() -> Live {
        use rgy_discovery::walk::{KIND_DIR, KIND_FILE, Listing, NONE, RawEnt};
        let listings = vec![
            Listing {
                id: 0,
                names: b"backup.txtinvoice.txt".to_vec(),
                ents: vec![
                    RawEnt { name_off: 0, name_len: 10, kind: KIND_DIR, size: 0, mtime: 7, child: 1 },
                    RawEnt { name_off: 10, name_len: 11, kind: KIND_FILE, size: 3, mtime: 7, child: NONE },
                ],
            },
            Listing {
                id: 1,
                names: b"invoice.md".to_vec(),
                ents: vec![RawEnt { name_off: 0, name_len: 10, kind: KIND_FILE, size: 3, mtime: 7, child: NONE }],
            },
        ];
        Live::new(Index::build(listings, 0, 1_700_000_000, b"/"))
    }

    /// A negated token is about its own axis, and about nothing else.
    ///
    /// `ext:!txt` says "not a .txt". The folder "backup.txt" is a .txt by
    /// name; the file under it is a .md, and a folder's extension says
    /// nothing about the file's own. `attr:!hidden` says "not hidden"; a
    /// file called "hidden" that is not hidden is kept, because the
    /// attribute axis is the kind byte and not the name.
    ///
    /// The index path and the overlay path are two answers to one question,
    /// and a question they answer differently is a bug in one of them.
    #[test]
    fn a_negated_token_is_about_its_own_axis_and_nothing_else() {
        use rgy_discovery::walk::{KIND_DIR, KIND_FILE, Listing, NONE, RawEnt};
        // root: backup.txt/ (dir), backup.txt/invoice.md, invoice.txt, hidden
        let listings = vec![
            Listing {
                id: 0,
                names: b"backup.txtinvoice.txthidden".to_vec(),
                ents: vec![
                    RawEnt { name_off: 0, name_len: 10, kind: KIND_DIR, size: 0, mtime: 7, child: 1 },
                    RawEnt { name_off: 10, name_len: 11, kind: KIND_FILE, size: 3, mtime: 7, child: NONE },
                    RawEnt { name_off: 21, name_len: 6, kind: KIND_FILE, size: 3, mtime: 7, child: NONE },
                ],
            },
            Listing {
                id: 1,
                names: b"invoice.md".to_vec(),
                ents: vec![RawEnt { name_off: 0, name_len: 10, kind: KIND_FILE, size: 3, mtime: 7, child: NONE }],
            },
        ];
        let live = Live::new(Index::build(listings, 0, 1_700_000_000, b"/"));
        let s = Searcher { live: &live };
        let names = |query: &str| {
            let hits = s.search(&q(query));
            let mut v: Vec<String> = hits
                .iter()
                .map(|h| {
                    let mut p = Vec::new();
                    live.base.path(h.idx as usize, &mut p);
                    String::from_utf8_lossy(&p).to_string()
                })
                .collect();
            v.sort();
            v
        };
        // The index path.
        assert_eq!(
            names("ext:!txt"),
            vec!["/backup.txt/invoice.md".to_string(), "/hidden".to_string()],
            "a negated extension rejects the .txt names, not what is under them"
        );
        assert_eq!(
            names("attr:!hidden"),
            vec!["/backup.txt".to_string(), "/backup.txt/invoice.md".to_string(), "/hidden".to_string(), "/invoice.txt".to_string(),],
            "a negated attribute is about the kind byte, not about the name"
        );
        // The overlay path, which has to answer the same way.
        for (query, path, expect) in [
            ("ext:!txt", "/backup.txt", false),
            ("ext:!txt", "/backup.txt/invoice.md", true),
            ("ext:!txt", "/invoice.txt", false),
            ("ext:!txt", "/hidden", true),
            ("attr:!hidden", "/backup.txt", true),
            ("attr:!hidden", "/backup.txt/invoice.md", true),
            ("attr:!hidden", "/hidden", true),
            ("attr:!hidden", "/invoice.txt", true),
        ] {
            assert_eq!(hit(query, path, KIND_FILE, 10), expect, "the overlay path answers {query} for {path} differently");
        }
    }

    /// The axes combine: a name word and a folder word are two
    /// conditions, not one.
    #[test]
    fn the_axes_combine() {
        assert!(hit("invoice folder:documents", "/x/documents/invoice.pdf", KIND_FILE, 10));
        assert!(!hit("invoice folder:documents", "/x/other/invoice.pdf", KIND_FILE, 10));
        assert!(!hit("invoice folder:documents", "/x/documents/receipt.pdf", KIND_FILE, 10));
    }

    /// The date facet: a calendar date, an age, and the operator suffix
    /// that tells them apart.
    ///
    /// The two operator tokens are the ones The reference's binary carries
    /// verbatim: `c` (created) and `<` (before). rgy's index keeps the
    /// modification time, so `c` answers with it -- the honest answer for
    /// the data rgy has -- and `<` is the "before" bound.
    #[test]
    fn the_date_facet_takes_dates_ages_and_operators() {
        // A calendar date names a whole day.
        let d = q("mtime:2024-01-01");
        assert_eq!(d.mtime, (1_704_067_200, 1_704_067_200 + 86_399), "a date is the day it names");
        // A range of two dates: both days are in it, whole.
        let r = q("mtime:2024-01-01..2024-01-31");
        assert_eq!(r.mtime.0, 1_704_067_200);
        assert_eq!(r.mtime.1, 1_704_067_200 + 30 * 86_400 - 1, "the range ends at the last second of the second day");
        // The operator the reference recovered: before.
        let before = q("mtime:<2024-01-01");
        assert_eq!(before.mtime, (0, 1_704_067_199), "before a date excludes the date itself");
        // And the other side, which the same vocabulary makes possible.
        let after = q("mtime:>2024-01-01");
        assert_eq!(after.mtime.0, 1_704_067_200 + 86_400, "after a date starts at the next day");
        // The created token: accepted, and answered with what rgy has.
        let created = q("mtime:c2024-01-01");
        assert_eq!(created.mtime, d.mtime, "the created token answers with the modification time");
        // An age is a window, counted back from now.
        let now = now_secs();
        let week = q("mtime:7d");
        assert_eq!(week.mtime, (now - 7 * 86_400, now), "an age is the window it names");
        // The manual spells the same window with a sign.
        assert_eq!(q("mtime:-7d").mtime, week.mtime, "the signed form is the same window");
        // A bad date fails closed rather than matching everything.
        assert!(Query::parse("mtime:2024-13-01", "").is_err(), "a month that does not exist is refused");
        assert!(Query::parse("mtime:2023-02-29", "").is_err(), "a day that does not exist is refused");
        assert!(Query::parse("mtime:notadate", "").is_err(), "a date that is not one is refused");
    }

    /// A date the filter cannot hold is refused, not wrapped.
    ///
    /// The filter's answer is a pair of unix seconds in a `u32` — the
    /// width the index keeps a file's modification time in — and the u32
    /// epoch ends on 2106-02-07. A year past that end is more seconds
    /// than the u32 holds, and a timestamp that wrapped is a date in the
    /// past: the filter would answer with files from the wrong decade.
    #[test]
    fn a_date_the_filter_cannot_hold_is_refused() {
        // A year past the u32 epoch: the seconds it needs do not fit the u32
        // the filter carries, so the old arithmetic answered with a date
        // in the past.
        let why = match Query::parse("mtime:99999999-01-01", "") {
            Ok(_) => panic!("a year of 99 999 999 is refused"),
            Err(e) => e,
        };
        assert!(why.contains("bad date"), "and it says why: {why}");
        // A year whose seconds do not fit the u64 they are counted in.
        let huge = Query::parse("mtime:999999999999-01-01", "");
        assert!(huge.is_err(), "a year of 999 999 999 999 is refused");
        // A year past the u32 epoch, and the day past its end.
        assert!(Query::parse("mtime:2107-01-01", "").is_err(), "a year past the epoch is refused");
        assert!(Query::parse("mtime:2106-02-08", "").is_err(), "a day past the epoch is refused");
        // And the last day the epoch holds is accepted: the timestamp is
        // the second that day starts at.
        let last = q("mtime:2106-02-07");
        assert_eq!(last.mtime.0, 4_294_944_000, "the last day of the u32 epoch");
        assert_eq!(last.mtime.1, u32::MAX - 1, "and the last second the epoch holds");
    }

    /// The date filter takes what the date parser takes.
    ///
    /// The parser takes `YYYY/MM/DD` as well as `YYYY-MM-DD`, and the
    /// filter that reads the parser's dates has to read them both: a
    /// user who writes the documented form was told the date is not a
    /// date.
    #[test]
    fn the_date_filter_takes_the_slash_form_too() {
        let dash = q("mtime:2024-01-01");
        let slash = q("mtime:2024/01/01");
        assert_eq!(slash.mtime, dash.mtime, "the slash form is the same day");
        assert_eq!(dash.mtime.0, 1_704_067_200, "and it is the day the manual names");
        // The range takes it as well.
        let range = q("mtime:2024/01/01..2024/01/31");
        assert_eq!(range.mtime, q("mtime:2024-01-01..2024-01-31").mtime, "the range takes it too");
    }

    /// The ninth word is said, not dropped in silence.
    ///
    /// The query is a list of words and every word narrows the answer,
    /// so a word the query does not carry is a word the user has to be
    /// told about: the answer to a narrower question looks like the
    /// answer to the one that was asked.
    #[test]
    fn the_ninth_word_is_reported() {
        let eight = "one two three four five six seven eight";
        let carried = q(eight);
        let words = |query: &Query| query.tokens.iter().filter(|t| !t.negate).count();
        assert_eq!(words(&carried), 8, "eight words are carried");
        assert!(carried.warnings.is_empty(), "and eight words are quiet: {:?}", carried.warnings);
        // The ninth does not fit, and the query says which word it did
        // not use.
        let nine = q(&format!("{eight} nine"));
        assert_eq!(words(&nine), 8, "the query carries eight words");
        assert_eq!(nine.warnings.len(), 1, "and says so: {:?}", nine.warnings);
        assert!(nine.warnings[0].contains("nine"), "the word it dropped is named: {:?}", nine.warnings);
        // The warning travels with the answer, as the other query
        // warnings do.
        let alts = Alternatives::parse(&format!("{eight} nine"), "").expect("the query parses");
        assert_eq!(alts.warnings.len(), 1, "the warning is the alternative's: {:?}", alts.warnings);
    }

    /// The signed size bounds the manual spells have to work.
    ///
    /// The manual says `size:+10m` is "bigger than 10 MB" and `size:-1k`
    /// is "at most one kilobyte". Both used to be a "bad range" error: a
    /// documented syntax that did not work at all.
    #[test]
    fn the_signed_size_bounds_work() {
        assert_eq!(q("size:+10m").size, (10_000_000, u64::MAX), "`+` is at least");
        assert_eq!(q("size:-1k").size, (0, 1_000), "`-` is at most");
        // The strict forms keep their own meaning.
        assert_eq!(q("size:>10m").size, (10_000_001, u64::MAX), "`>` is more than");
        assert_eq!(q("size:<1k").size, (0, 999), "`<` is less than");
    }

    /// A leap day is a day: the calendar says so.
    #[test]
    fn a_leap_day_is_the_day_the_calendar_says() {
        // 2024 is a leap year, 2023 is not.
        assert_eq!(parse_date("2024-02-29"), Some(1_709_164_800));
        assert_eq!(parse_date("2023-02-29"), None);
        // The day before and the day after are one day apart.
        let a = parse_date("2024-03-01").unwrap();
        let b = parse_date("2024-02-29").unwrap();
        assert_eq!(a - b, 86_400, "the leap day is a whole day");
    }

    #[test]
    fn media_kinds_imply_a_file_and_map_the_aliases() {
        let a = q("type:image");
        assert_eq!(a.kind, Some(KIND_FILE));
        assert!(ext_ok(b"x.png", &a.exts));
        // zen-rs aliases.
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

    /// A tiny in-memory index for the Searcher fast paths. `hit()` above
    /// only covers `match_path`; the CLI runs `Searcher::search`, whose
    /// `ranked`/`Scan` paths once ignored `never`, `globs`, and `link` —
    /// `walk type:banana` returned hits and exit 0. These pin that level.
    fn tiny_live() -> Live {
        let mut names = Vec::new();
        let mut ents = Vec::new();
        for (name, kind) in [("readme.md", KIND_FILE), ("walk.rs", KIND_FILE), ("alias", KIND_LINK), ("notes.txt", KIND_FILE)] {
            let off = names.len() as u32;
            names.extend_from_slice(name.as_bytes());
            ents.push(rgy_discovery::walk::RawEnt {
                name_off: off,
                name_len: name.len() as u16,
                kind,
                size: 10,
                mtime: 1_700_000_000,
                child: rgy_discovery::walk::NONE,
            });
        }
        let listing = rgy_discovery::walk::Listing { id: 0, names, ents };
        Live::new(Index::build(vec![listing], 0, 1_700_000_000, b"/"))
    }

    /// An index with a name written in another script, so the shadow path
    /// has something to find: `日本語.txt` is read にほんご, and a query in
    /// romaji or in kana has to reach it.
    fn cjk_live() -> Live {
        let mut names = Vec::new();
        let mut ents = Vec::new();
        for name in ["日本語.txt", "にほんご.md", "中文.pdf"] {
            let off = names.len() as u32;
            names.extend_from_slice(name.as_bytes());
            ents.push(rgy_discovery::walk::RawEnt {
                name_off: off,
                name_len: name.len() as u16,
                kind: KIND_FILE,
                size: 10,
                mtime: 1_700_000_000,
                child: rgy_discovery::walk::NONE,
            });
        }
        let listing = rgy_discovery::walk::Listing { id: 0, names, ents };
        Live::new(Index::build(vec![listing], 0, 1_700_000_000, b"/"))
    }

    /// A name written in one script, found by a query in another.
    ///
    /// The four ways a query and a name can be in different scripts:
    /// romaji over kanji, kana over kanji, romaji over kana, and pinyin over
    /// hanzi. Each of them is the same shadow, read a different way.
    #[test]
    fn a_query_in_another_script_finds_the_name() {
        let live = cjk_live();
        let searcher = Searcher { live: &live };
        for (query, expect) in [
            ("nihongo", "日本語.txt"),   // romaji over kanji
            ("にほんご", "にほんご.md"), // kana over kana
            ("nihongo", "にほんご.md"),  // romaji over kana
            ("zhongwen", "中文.pdf"),    // pinyin over hanzi
        ] {
            let hits = searcher.search(&q(query));
            let found = hits.iter().any(|h| {
                let mut buf = Vec::new();
                live.base.path(h.idx as usize, &mut buf);
                buf == format!("/{expect}").as_bytes()
            });
            assert!(found, "`{query}` finds `{expect}`");
        }
    }

    /// A name written in one script is not found by a query in another when
    /// the two do not correspond: `nihongo` is not `chinese`.
    #[test]
    fn a_query_does_not_find_an_unrelated_name() {
        let live = cjk_live();
        let searcher = Searcher { live: &live };
        let hits = searcher.search(&q("chinese"));
        assert!(hits.is_empty(), "a query nobody wrote finds nothing");
    }

    /// The query cache is dropped once nobody is typing.
    ///
    /// A broad query leaves tens of megabytes of spare hit buffers and a
    /// scored name table behind. They are worth keeping only while the
    /// user is typing, so a quiet spell gives them back -- and the daemon
    /// has no idle moment of its own, so a thread of its own does it.
    ///
    /// The `std` build only: "idle" is the wall clock's word, and without
    /// a clock there is no idle moment to decide. The test stands down
    /// with the thing it measures.
    #[test]
    #[cfg(feature = "std")]
    fn the_query_cache_is_dropped_after_a_quiet_spell() {
        let live = many_live(100);
        let searcher = Searcher { live: &live };
        let _ = searcher.search(&q("entry"));
        assert!(live.names_cache.last.lock().unwrap().is_some(), "a search caches its table");
        live.names_cache.trim_if_idle(core::time::Duration::from_millis(0));
        assert!(live.names_cache.last.lock().unwrap().is_none(), "a quiet spell drops it");
    }

    /// An index with more entries than the interactive limit, so a count that
    /// quietly inherited `limit` would return the limit instead of the truth.
    fn many_live(n: usize) -> Live {
        let mut names = Vec::new();
        let mut ents = Vec::new();
        for i in 0..n {
            let name = format!("entry_{i:06}.txt");
            let off = names.len() as u32;
            names.extend_from_slice(name.as_bytes());
            ents.push(rgy_discovery::walk::RawEnt {
                name_off: off,
                name_len: name.len() as u16,
                kind: KIND_FILE,
                size: 10,
                mtime: 1_700_000_000,
                child: rgy_discovery::walk::NONE,
            });
        }
        let listing = rgy_discovery::walk::Listing { id: 0, names, ents };
        Live::new(Index::build(vec![listing], 0, 1_700_000_000, b"/"))
    }

    /// An index whose entries repeat one set of distinct names, the shape a
    /// toolchain tree has: every installed release ships the same crate
    /// sources, so entries outnumber distinct names several times over.
    /// Names span the alphabet and the index sorts them into id order, so
    /// ids come out clustered by first letter — the arrangement a fixed
    /// stride samples worst.
    fn dedup_live(dirs: usize, per_letter: usize) -> Live {
        let mut names = Vec::new();
        let mut ents = Vec::new();
        for _ in 0..dirs {
            for c in b'a'..=b'z' {
                for i in 0..per_letter {
                    let name = format!("{}{i:05}.rs", c as char);
                    let off = names.len() as u32;
                    names.extend_from_slice(name.as_bytes());
                    ents.push(rgy_discovery::walk::RawEnt {
                        name_off: off,
                        name_len: name.len() as u16,
                        kind: KIND_FILE,
                        size: 10,
                        mtime: 1_700_000_000,
                        child: rgy_discovery::walk::NONE,
                    });
                }
            }
        }
        let listing = rgy_discovery::walk::Listing { id: 0, names, ents };
        Live::new(Index::build(vec![listing], 0, 1_700_000_000, b"/"))
    }

    /// `rankable` samples every 64th bitmap word and multiplies by 64 to
    /// stand in for the whole table. The estimate is crude: on this fixture
    /// it reads 4,032 where the truth is 1,000, and 0 where the truth is
    /// also 1,000, because a token's names cluster by first letter and a
    /// cluster can sit entirely between two stride points. What the sample
    /// is actually for is the one comparison `rankable` makes — is this
    /// query broad enough for `ranked` to be worth trying — so that is what
    /// is pinned here, not agreement between the two numbers.
    ///
    /// Both ways to get that wrong are covered: a sample so coarse that
    /// broad and narrow queries read alike (`ranked` is dead code), and one
    /// so biased that a cluster decides it (`ranked` is skipped for the
    /// queries it would have won). Neither is a wrong answer — `ranked`
    /// answers `None` and the scan falls through — but both cost time, and
    /// neither shows up without measuring the decision against the truth.
    #[test]
    fn the_sampled_estimate_drives_the_same_decision_as_an_exhaustive_count() {
        let live = dedup_live(2, 1_000);
        let idx = &live.base;
        assert!(idx.words > 64, "need several stride points, got {} words", idx.words);

        let (mut broad, mut narrow) = (0, 0);
        for tok in ["a", "b", "r", "s", "_", "rs", "a_", "0", "5", "z", "aa", "rs"] {
            let q = q(tok);
            let tb = TokenBits::new(idx, &q.tokens[0]);
            let exact: f64 = (0..idx.words).map(|w| tb.word(w).0.count_ones() as f64).sum();
            let sampled = (0..idx.words).step_by(64).map(|w| tb.word(w).0.count_ones() as f64).sum::<f64>() * 64.0;
            assert_eq!(
                sampled >= RANKED_MIN,
                exact >= RANKED_MIN,
                "{tok:?}: sampled {sampled:.0} (takes ranked: {}) but exhaustive {exact:.0} \
                 does not",
                sampled >= RANKED_MIN
            );
            broad += usize::from(exact >= RANKED_MIN);
            narrow += usize::from(exact < RANKED_MIN);
        }
        // Without both, the loop above is one-sided and would pass on an
        // estimate that always says "narrow" (ranked never runs) just as
        // happily as on a correct one.
        assert!(broad > 0 && narrow > 0, "the fixture put every token on one side of RANKED_MIN");
    }

    /// A scan of a root that listed nothing — a path that does not exist, a
    /// file where a directory was expected, a permission wall — produces no
    /// listings, and `Index::build` has to describe the root anyway rather
    /// than walk off the end of an empty block stream. The result must be a
    /// usable index holding nothing but the root, and every query against it
    /// must answer zero rather than panic.
    #[test]
    fn an_unlistable_root_builds_the_root_only_index() {
        let idx = Index::build(Vec::new(), 0, 1_700_000_000, b"/");
        assert_eq!(idx.n, 1, "only the root entry itself");
        let live = Live::new(idx);
        let s = Searcher { live: &live };
        for tok in ["a", "main", "*.rs", "ext:rs", "type:file"] {
            let parsed = Query::parse(tok, "").expect("parses");
            assert_eq!(s.count(&parsed), 0, "{tok:?} matched in an empty tree");
            assert!(s.search(&parsed).is_empty(), "{tok:?} returned a hit in an empty tree");
        }
        // And the root resolves to a path rather than reading out of bounds.
        let mut buf = Vec::new();
        live.base.path(0, &mut buf);
        assert_eq!(buf, b"/");
    }

    /// `count` must agree with an uncapped `search` on every query shape,
    /// and must ignore the caller's limit. A separate "count only" match
    /// logic would be free to drift here; this is what stops it.
    ///
    /// `broad` queries must match more than the default limit — without that,
    /// a count that inherited the limit would look correct.
    #[test]
    fn count_never_disagrees_with_uncapped_search() {
        let live = many_live(300);
        let s = Searcher { live: &live };
        let cases: &[(&str, bool)] = &[
            ("entry", true),
            ("entry_000", true),
            ("^entry", true),
            ("txt$", true),
            ("ext:txt", true),
            ("entry !000010", true),
            // Narrow by construction: still checked for agreement, but they
            // cannot prove anything about the limit being ignored.
            ("entry_000001.txt", false),
            ("zzzznomatch", false),
            ("type:file entry", true),
        ];
        for &(query, broad) in cases {
            let mut full = Query::parse(query, "").expect("parses");
            full.limit = usize::MAX / 2;
            let want = s.search(&full).len();

            // Same query at the default limit, counted.
            let capped = Query::parse(query, "").expect("parses");
            let got = s.count(&capped);
            assert_eq!(got, want, "count disagrees for {query:?}");

            // `search_uncapped` is what the CLI unions `|` alternatives over;
            // it must also ignore the limit. This case caught a real bug —
            // skipping only the outer `truncate` still returned `q.limit`
            // hits, because `search_base` cuts internally.
            assert_eq!(s.search_uncapped(&capped).len(), want, "search_uncapped disagrees for {query:?}");

            if broad {
                assert!(
                    want > capped.limit,
                    "{query:?} matched {want}, not above the limit {} — the fixture \
                     cannot detect a count that inherited the limit",
                    capped.limit
                );
            }
        }
    }

    #[test]
    fn count_of_a_fail_closed_query_is_zero() {
        let live = many_live(50);
        let s = Searcher { live: &live };
        let bad = Query::parse("type:banana", "").expect("parses");
        assert!(bad.never);
        assert_eq!(s.count(&bad), 0);
    }

    /// An overlay hit and a base hit differ only in which entry they name,
    /// and the merge has to tell them apart by that. Every overlay entry
    /// carries `idx == u32::MAX`, so a merge keyed on `idx` keeps one of them
    /// and drops the rest -- a query matching several entries added since the
    /// last compaction answered as if only one existed.
    ///
    /// This is the regression that made `searchctl` and the TUI silently lose
    /// live entries, so it is written against `merge_hits` directly: the
    /// defect was in the merge, not in the matcher.
    #[test]
    fn the_merge_keeps_every_distinct_overlay_entry() {
        let overlay = |name: &str, score: i32| Hit {
            key: rank_key(Tier::Normal, score, RankKey::NO_ENTRY),
            idx: u32::MAX,
            mtime: 0,
            over: Some(format!("/new/{name}").into_bytes()),
        };
        let merged = merge_hits(vec![overlay("one", 10), overlay("two", 20), overlay("three", 30)], 50);
        assert_eq!(merged.len(), 3, "three distinct entries, not one");
        let mut names: Vec<String> = merged.iter().map(|h| String::from_utf8(h.over.clone().expect("overlay")).unwrap()).collect();
        names.sort();
        assert_eq!(names, ["/new/one", "/new/three", "/new/two"]);
    }

    /// One entry matched by two alternatives appears once, with the better
    /// of the two scores -- not twice, and not with the worse one.
    #[test]
    fn the_merge_takes_the_best_score_per_entry() {
        let base = |idx: u32, score: i32| Hit { key: rank_key(Tier::Normal, score, idx), idx, mtime: 0, over: None };
        let merged = merge_hits(vec![base(7, 10), base(7, 99), base(8, 5)], 50);
        assert_eq!(merged.len(), 2, "entry 7 matched twice but is one entry");
        let seven = merged.iter().find(|h| h.idx == 7).expect("entry 7");
        assert_eq!(seven.score(), 99, "the better of the two scores wins");
    }

    /// Two overlay entries tying on score must come out in the same order
    /// every run. A map's iteration order is not that, and both entries share
    /// `idx`, so an order keyed on `idx` alone leaves them unordered -- which
    /// is how a stable-looking result list turns into one that reshuffles.
    #[test]
    fn the_merge_order_is_total_not_map_iteration_order() {
        let overlay =
            |name: &str| Hit { key: rank_key(Tier::Normal, 5, RankKey::NO_ENTRY), idx: u32::MAX, mtime: 0, over: Some(name.as_bytes().to_vec()) };
        for _ in 0..20 {
            let merged = merge_hits(vec![overlay("/zeta"), overlay("/alpha"), overlay("/mid")], 50);
            let paths: Vec<&[u8]> = merged.iter().map(|h| h.over.as_ref().unwrap().as_slice()).collect();
            assert_eq!(paths, [&b"/alpha"[..], &b"/mid"[..], &b"/zeta"[..]]);
        }
    }

    /// Base entries sort before overlay entries on a score tie, which is what
    /// `idx ascending` used to give (a real index is always below the
    /// `u32::MAX` overlay sentinel).
    #[test]
    fn the_merge_puts_base_entries_first_on_a_tie() {
        let merged = merge_hits(
            vec![
                Hit { key: rank_key(Tier::Normal, 5, RankKey::NO_ENTRY), idx: u32::MAX, mtime: 0, over: Some(b"/new/x".to_vec()) },
                Hit { key: rank_key(Tier::Normal, 5, 3), idx: 3, mtime: 0, over: None },
            ],
            50,
        );
        assert_eq!(merged[0].idx, 3);
        assert!(merged[1].over.is_some());
    }

    /// `--explain` is only worth anything if every arm is reachable. If a plan
    /// could never be taken, reporting it would be decoration.
    ///
    /// The `std` build only: the plan lives in thread-local storage, and
    /// without it there is no plan to report. The test stands down with the
    /// thing it reads.
    #[test]
    #[cfg(feature = "std")]
    fn the_reported_plan_is_the_path_that_actually_ran() {
        let live = many_live(300);
        let s = Searcher { live: &live };

        // A filter that cannot match: recorded as failing closed, not as
        // whatever scan path happened to be skipped.
        let bad = Query::parse("type:banana", "").expect("parses");
        s.search(&bad);
        assert_eq!(Searcher::last_plan(), Some(Plan::FailClosed));

        // A real query takes some concrete path, and it is one of ours.
        for query in ["entry", "^entry", "entry txt", "entry !000010", "ext:txt"] {
            let parsed = Query::parse(query, "").expect("parses");
            s.search(&parsed);
            let plan = Searcher::last_plan().unwrap_or_else(|| panic!("no plan recorded for {query:?}"));
            assert!(matches!(plan, Plan::Ranked { .. } | Plan::Driven | Plan::Selective | Plan::Full), "{query:?} reported {plan}");
        }
    }

    fn searched(query: &str) -> Vec<u32> {
        let live = tiny_live();
        let parsed = Query::parse(query, "").expect("parses");
        let s = Searcher { live: &live };
        s.search(&parsed).into_iter().map(|h| h.idx).collect()
    }

    /// Hit paths (not entry indices — `Index::build` DFS-orders entries).
    fn searched_paths(query: &str) -> Vec<String> {
        let live = tiny_live();
        let parsed = Query::parse(query, "").expect("parses");
        let s = Searcher { live: &live };
        let mut out = Vec::new();
        let mut buf = Vec::new();
        for h in s.search(&parsed) {
            buf.clear();
            live.base.path(h.idx as usize, &mut buf);
            out.push(String::from_utf8_lossy(&buf).into_owned());
        }
        out
    }

    #[test]
    fn searcher_fails_closed_on_unknown_type() {
        assert!(searched("type:banana").is_empty(), "never must gate every Searcher path");
        assert!(searched("walk type:banana").is_empty(), "a token plus never must not rank");
    }

    #[test]
    fn searcher_applies_whole_path_globs() {
        let md = searched_paths("*.md");
        assert_eq!(md.len(), 1, "only readme.md: {md:?}");
        assert!(md[0].ends_with("readme.md"), "{md:?}");
        let walk = searched_paths("*walk.rs");
        assert_eq!(walk.len(), 1, "only walk.rs: {walk:?}");
        assert!(walk[0].ends_with("walk.rs"), "{walk:?}");
    }

    #[test]
    fn searcher_honors_type_link() {
        let links = searched_paths("type:link");
        assert_eq!(links.len(), 1, "only the symlink entry: {links:?}");
        assert!(links[0].ends_with("alias"), "{links:?}");
        assert!(searched("type:link type:file").is_empty(), "file ∧ link is impossible");
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
        // The index scan path (`Searcher::search`) must also exclude
        // directories when any `size:` bound is set, including `size:<10kb`
        // and `size:>=0`.
        let live = axis_tree();
        let s = Searcher { live: &live };
        assert_eq!(s.search(&q("size:<10kb")).len(), 2, "only the two files in axis_tree match size:<10kb");
        assert_eq!(s.search(&q("size:>=0")).len(), 2, "only the two files in axis_tree match size:>=0");
    }

    /// `>` and `>=` are different questions and had been answering both as
    /// `>=`. Found by an independent count: `size:>10000` matched a file of
    /// exactly 10,000 bytes that `Get-ChildItem -Where Length -gt 10000`
    /// correctly left out.
    #[test]
    fn a_strict_size_bound_excludes_the_boundary() {
        // `>`: strictly more.
        assert!(!hit("size:>10000", "/a/b.txt", KIND_FILE, 10_000));
        assert!(hit("size:>10000", "/a/b.txt", KIND_FILE, 10_001));
        // `>=`: more or equal.
        assert!(hit("size:>=10000", "/a/b.txt", KIND_FILE, 10_000));
        assert!(hit("size:>=10000", "/a/b.txt", KIND_FILE, 10_001));
        // `<`: strictly less.
        assert!(!hit("size:<10000", "/a/b.txt", KIND_FILE, 10_000));
        assert!(hit("size:<10000", "/a/b.txt", KIND_FILE, 9_999));
        // `<=`: less or equal.
        assert!(hit("size:<=10000", "/a/b.txt", KIND_FILE, 10_000));
        assert!(hit("size:<=10000", "/a/b.txt", KIND_FILE, 9_999));
    }

    /// The strict bounds have to stay strict at the ends of the range, where
    /// moving by one would wrap: nothing is `>` the largest value or `<` 0.
    #[test]
    fn a_strict_size_bound_stays_strict_at_the_end_of_the_range() {
        assert!(!hit("size:>18446744073709551615", "/a/b.txt", KIND_FILE, u64::MAX));
        assert!(!hit("size:<0", "/a/b.txt", KIND_FILE, 0));
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
        // zen-rs: a wildcard term must cover the WHOLE path.
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

    /// A quoted run is a phrase: the words together, with the spaces.
    #[test]
    fn a_quoted_run_is_a_phrase() {
        let a = q("\"invoice pdf\"");
        assert_eq!(a.tokens[0].mode, Mode::Phrase);
        assert_eq!(a.tokens[0].text, b"invoice pdf");
        assert!(hit("\"invoice pdf\"", "/p/Invoice PDF 2024.pdf", KIND_FILE, 0));
        // The phrase has to be whole: the words in the other order are not
        // the phrase.
        assert!(!hit("\"invoice pdf\"", "/p/pdf_invoice.pdf", KIND_FILE, 0));
        // And the words of a phrase are not two loose tokens: the name has
        // to carry the space between them.
        assert!(!hit("\"invoice pdf\"", "/p/invoiceandpdf.pdf", KIND_FILE, 0));
    }

    /// The `=` marker asks for a whole word of the name.
    #[test]
    fn the_word_marker_asks_for_a_whole_word() {
        let a = q("=port");
        assert_eq!(a.tokens[0].mode, Mode::Keyword);
        assert!(hit("=port", "/p/report_port.txt", KIND_FILE, 0));
        // A fragment of a word is not the word.
        assert!(!hit("=port", "/p/report.txt", KIND_FILE, 0));
        assert!(!hit("=port", "/p/porting.txt", KIND_FILE, 0));
    }

    /// `!` negates the fuzzy match, which is what the manual says: the old
    /// code negated an exact match, so `!invoice` still answered `invoce`.
    #[test]
    fn the_negation_negates_the_fuzzy_match() {
        assert!(!hit("!invoice", "/p/invoice.pdf", KIND_FILE, 0));
        assert!(!hit("!invoice", "/p/invoce.pdf", KIND_FILE, 0));
        assert!(hit("!invoice", "/p/receipt.pdf", KIND_FILE, 0));
    }

    /// The positions the matcher matched, in characters.
    #[test]
    fn the_matched_characters_are_the_span() {
        // The fuzzy match is the leftmost one: the "port" of "report".
        let t = q("port");
        assert_eq!(matched_chars(b"report_port.txt", &t.tokens), vec![2, 3, 4, 5]);
        // The word matcher takes the word.
        let t = q("=port");
        assert_eq!(matched_chars(b"report_port.txt", &t.tokens), vec![7, 8, 9, 10]);
        // The phrase takes the words and the space between them.
        let t = q("\"invoice pdf\"");
        assert_eq!(matched_chars(b"my invoice pdf file.txt", &t.tokens), vec![3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13]);
        // A name that did not match has no positions.
        let t = q("zzz");
        assert!(matched_chars(b"report_port.txt", &t.tokens).is_empty());
    }

    /// The prefix highlights the name's first bytes only when the name
    /// starts with them.
    ///
    /// A prefix token that matched through a folder of the path did not
    /// match the name's first bytes, and highlighting them anyway was a
    /// lie about where the score came from.
    #[test]
    fn a_prefix_highlights_the_name_only_when_the_name_starts_with_it() {
        let t = q("^src");
        assert_eq!(matched_chars(b"src/main.rs", &t.tokens), vec![0, 1, 2]);
        assert!(matched_chars(b"main.rs", &t.tokens).is_empty(), "the name does not start with the token, so it has no span");
    }

    /// The suffix is the same: it has to be the suffix.
    #[test]
    fn a_suffix_highlights_the_name_only_when_the_name_ends_with_it() {
        let t = q("pdf$");
        assert_eq!(matched_chars(b"invoice.pdf", &t.tokens), vec![8, 9, 10]);
        assert!(matched_chars(b"pdf_invoice.txt", &t.tokens).is_empty());
    }

    /// A token on another axis says nothing about the name.
    ///
    /// `folder:documents` matches a folder of the path. The name of a
    /// file called "documents.txt" is not that folder, and highlighting
    /// the name's bytes for it would be a lie.
    #[test]
    fn a_token_on_another_axis_highlights_nothing_in_the_name() {
        let t = q("folder:documents");
        assert!(matched_chars(b"documents.txt", &t.tokens).is_empty(), "the folder token does not speak about the name");
        let t = q("attr:hidden");
        assert!(matched_chars(b"hidden.txt", &t.tokens).is_empty());
    }

    /// The span is the name's characters, in a name the filesystem stored
    /// in NFD.
    ///
    /// macOS stores "café" as "cafe" plus a combining acute. The token is
    /// folded, so the comparison has to be on the folded name -- and the
    /// positions have to come back to the raw one, which has one character
    /// more.
    #[test]
    fn the_span_is_the_raw_name_s_characters_in_a_decomposed_name() {
        let t = q("café");
        // "cafe" plus a combining acute, then ".txt".
        let raw = "cafe\u{301}.txt".as_bytes();
        assert_eq!(matched_chars(raw, &t.tokens), vec![0, 1, 2, 3, 4]);
        // The same name, already composed: the same span.
        let t = q("café");
        assert_eq!(matched_chars("café.txt".as_bytes(), &t.tokens), vec![0, 1, 2, 3]);
    }

    /// The named time windows: a vocabulary, not a mechanism.
    #[test]
    fn the_named_time_windows_are_ranges() {
        let now = now_secs();
        let today = q("mtime:today");
        assert_eq!(today.mtime.1, now, "the window ends now");
        assert!(today.mtime.0 <= now.saturating_sub(86_399), "the window is the last day");
        let older = q("mtime:older");
        assert_eq!(older.mtime.0, 0, "older starts at the beginning");
        assert!(older.mtime.1 < now.saturating_sub(364 * 86_400), "older is older than a year");
    }

    /// The positions are characters, not bytes: a name in another script
    /// has the span where the characters are, whole.
    #[test]
    fn the_positions_snap_to_the_characters() {
        let t = q("日本");
        assert_eq!(matched_chars("日本語.txt".as_bytes(), &t.tokens), vec![0, 1]);
    }

    /// The substring matcher looks for the first byte and compares the rest.
    ///
    /// The first byte at the very end of the name is not a match: the query
    /// does not fit there.
    #[test]
    fn the_substring_matcher_does_not_run_past_the_name() {
        assert_eq!(find_ci(b"invoice.pdf", b"invoice"), Some(0));
        assert_eq!(find_ci(b"INVOICE.pdf", b"invoice"), Some(0));
        assert_eq!(find_ci(b"my invoice.pdf", b"invoice"), Some(3));
        // The first byte is there and the rest is not.
        assert_eq!(find_ci(b"invo", b"invoice"), None);
        assert_eq!(find_ci(b"inv", b"invoice"), None);
        assert_eq!(find_ci(b"invoice", b"invoice"), Some(0));
    }

    /// `sort:` chooses the order the results come in.
    #[test]
    fn the_sort_choice_orders_the_hits() {
        assert_eq!(q("invoice").sort, SortBy::Score, "the score is the default");
        assert_eq!(q("sort:name").sort, SortBy::Name);
        assert_eq!(q("sort:mtime").sort, SortBy::MTime);
        assert_eq!(q("sort:none").sort, SortBy::None);
        // An unknown choice fails closed, like an unknown `type:`.
        let bad = q("sort:bogus");
        assert!(bad.never, "an unknown sort matches nothing");
        assert!(!bad.warnings.is_empty(), "and says so");
    }
}

impl Token {
    /// A token for a lowercase literal a name must contain.
    fn literal(text: Vec<u8>) -> Token {
        let mask = text.iter().fold(0, |m, &b| m | char_bit(b));
        Token { text, mask, mode: Mode::Exact, negate: false, loose: 0, start: 0, axis: Axis::Name }
    }

    /// A stand-in for a name's mask (see `token_score`) from what the name
    /// bitmaps tell: whether the name can hold this token cleanly, whether a
    /// word in it starts like the token, whether it has a space. Each bit is
    /// set unless the work it gates is sure to fail.
    #[inline(always)]
    fn known_mask(&self, clean: bool, start: bool, spaced: bool) -> u64 {
        (if clean { self.mask } else { 0 }) | (if start { self.start } else { 0 }) | (if spaced { char_bit(b' ') } else { 0 })
    }
}

/// A name `Searcher::ranked` scored, and the bound on its entries' scores.
struct Cand {
    bound: i32,
    /// The tier of the name's best folder, and so the best tier any of its
    /// entries is in.
    ///
    /// The bound is on the key, and the key starts with the tier, so the
    /// bound has to carry it too.
    tier: Tier,
    k: u32,
    score: i16,
    flags: u8,
}

/// A value alone on its cache line: per-thread scratch that is written in a
/// hot loop must not share a line with another thread's.
#[derive(Clone, Copy)]
#[repr(align(128))]
struct Line<T>(T);

/// A token's `TokenBits::words` for a pair of words, on its own line.
type Pair = Line<[(u64, u64, u64); 2]>;

/// One token's view of the name bitmaps (`index::BM_FIRST` and on): which
/// of 64 names at a time it can match, before any name is read.
struct TokenBits<'a> {
    /// Each char's class: (first-half, second-half) bitmaps.
    seq: Vec<[&'a [u64]; 2]>,
    /// The first char's class, then the token's other classes and the ones
    /// it has twice (as the bitmap of names with them twice, in both
    /// slots), rarest first.
    classes: Vec<[&'a [u64]; 2]>,
    /// Typo-taking tokens: names with a word starting with the token's
    /// first letter (or one hashing alike).
    start: Option<&'a [u64]>,
    /// Extension-axis tokens: every matching name must contain a dot, and
    /// a typo match starts after that dot rather than at a word start of
    /// the whole name.
    dot: Option<[&'a [u64]; 2]>,
    typo_ext: bool,
    /// Names with a byte at U+0080 or above: they may be read in another
    /// script, and the class filter cannot see that.
    other: [&'a [u64]; 2],
}

impl<'a> TokenBits<'a> {
    fn new(idx: &'a Index, t: &Token) -> TokenBits<'a> {
        let class = |b: u8| char_bit(b).trailing_zeros() as usize;
        let pair = |c: usize| [idx.bitmap(BM_FIRST + c), idx.bitmap(BM_SECOND + c)];
        let first = class(t.text[0]);
        let (counts, doubles) = (idx.class_counts(), idx.double_counts());
        // The other classes, and the classes the token has twice (which a
        // name needs twice too), rarest first.
        let twice = crate::name_index::doubled(&t.text);
        let mut rest: Vec<(u32, [&[u64]; 2])> =
            (0..CLASSES).filter(|&c| c != first && t.mask & (1 << c) != 0).map(|c| (counts[c], pair(c))).collect();
        rest.extend((0..CLASSES).filter(|&c| twice & (1 << c) != 0).map(|c| (doubles[c], [idx.bitmap(BM_DOUBLE + c); 2])));
        rest.sort_by_key(|x| x.0);
        let is_ext = t.axis == Axis::Extension;
        TokenBits {
            seq: t.text.iter().map(|&b| pair(class(b))).collect(),
            classes: core::iter::once(pair(first)).chain(rest.into_iter().map(|x| x.1)).collect(),
            start: (t.start != 0 && !is_ext).then(|| idx.bitmap(BM_START + crate::name_index::start_hash(t.text[0]))),
            dot: is_ext.then(|| pair(class(b'.'))),
            typo_ext: t.start != 0 && is_ext,
            other: pair(crate::name_index::CLASS_NON_ASCII),
        }
    }

    /// For the 64 names of word `w`: (can match: `Token::fits`, narrowed by
    /// the halves test; can match cleanly; may match with a typo). Cleanly,
    /// the name has every class of the token (twice where the token has it
    /// twice), and some prefix of the token fits the classes of its first
    /// half and the rest its second half. With a typo, a word starts with
    /// the first letter, at most one other of these is missing (an edit
    /// costs the name one char of the token at most), and so does the
    /// halves test with one char after the first left out: every edit
    /// `typo_score` takes leaves the rest of the token in order in the name.
    #[inline]
    fn word(&self, w: usize) -> (u64, u64, u64) {
        self.words::<1>(w)[0]
    }

    /// `word` for words `w..w + B`, side by side: a pair fills a NEON
    /// register, and the loops exit when both words are done, which is
    /// easier to predict than each word on its own.
    #[inline(always)]
    fn words<const B: usize>(&self, w: usize) -> [(u64, u64, u64); B] {
        let has = |b: &[&[u64]; 2], j: usize| b[0][w + j] | b[1][w + j];
        let none = |x: &[[u64; B]]| x.iter().flatten().fold(0, |a, &b| a | b) == 0;
        let mut all: [u64; B] = core::array::from_fn(|j| has(&self.classes[0], j));
        if let Some(d) = &self.dot {
            for (j, a) in all.iter_mut().enumerate() {
                *a &= has(d, j);
            }
        }
        let typo: [u64; B] = if self.typo_ext { all } else { self.start.map_or([0; B], |s| core::array::from_fn(|j| s[w + j] & all[j])) };
        // Names with every class so far, and typo-able ones missing one.
        let mut one = [0; B];
        for b in &self.classes[1..] {
            if none(&[all, one]) {
                break;
            }
            for j in 0..B {
                let x = has(b, j);
                one[j] = (one[j] & x) | (all[j] & !x & typo[j]);
                all[j] &= x;
            }
        }
        // Names whose first half holds the token so far, and names where
        // the token so far ends in the second half; then the same with one
        // char left out, for typo-able names.
        let (mut first, mut second): ([u64; B], _) = (core::array::from_fn(|j| all[j] | one[j]), [0; B]);
        let (mut first1, mut second1) = ([0; B], [0; B]);
        for (k, b) in self.seq.iter().enumerate() {
            if none(&[first, second, first1, second1]) {
                break;
            }
            let skip = if k > 0 { !0 } else { 0 };
            for j in 0..B {
                let (b0, b1) = (b[0][w + j], b[1][w + j]);
                second1[j] = ((second1[j] | first1[j]) & b1) | (second[j] & typo[j] & skip);
                first1[j] = (first1[j] & b0) | (first[j] & typo[j] & skip);
                second[j] = (second[j] | first[j]) & b1;
                first[j] &= b0;
            }
        }
        core::array::from_fn(|j| {
            let clean = (first[j] | second[j]) & all[j];
            let typo = (first[j] | second[j] | first1[j] | second1[j]) & typo[j];
            // A name with a byte at U+0080 or above may be read in another
            // script (see `readings`), and the class filter cannot see
            // that: it knows the name's bytes, not its readings. So those
            // names are candidates as well, and the scorer tries the
            // shadow (`shadow_score`).
            let other = self.dot.as_ref().map_or_else(|| has(&self.other, j), |d| has(&self.other, j) & has(d, j));
            (clean | typo | other, clean, typo)
        })
    }
}

/// What `score_names` holds for name `k`, scored on its own (from its mask
/// rather than the bitmaps): the same hit, or None for the same names.
fn name_hit(idx: &Index, q: &Query, pos: &[&Token], neg: &[&Token], re: Option<&crate::regex::Regex>, k: u32) -> Option<NameHit> {
    // The name is folded for the comparison: the index keeps the bytes the
    // filesystem gave, and a query typed in NFC has to match one stored in
    // NFD.
    let (name, wide) = idx.uname_folded(k);
    let m = crate::name_index::mask_of(&name);
    let mut h = NameHit { score: 0, bits: 0, flags: name_flags(&name), best: [0; 4] };
    // Whether every token is on the name axis: then this is the scoring it
    // always was, and it costs the same.
    let plain = pos.iter().all(|t| t.axis == Axis::Name) && neg.iter().all(|t| t.axis == Axis::Name);
    // Whether the name answered one of the name-axis tokens.
    //
    // A query that asks for a name is answered by a name: the name has to
    // carry one of the name-axis tokens, even when a folder above carries
    // the others. The overlay says the same (see `match_path`), and the two
    // paths have to agree on it -- a query that names a file is not a query
    // that names a folder with that file under it.
    let mut got_name = false;
    if plain {
        for (t, tok) in pos.iter().enumerate() {
            if tok.fits(m)
                && let Some(s) = token_score(&name, wide, m, tok)
            {
                h.bits |= 1 << t;
                // Every positive token is a name token here.
                got_name = true;
                let s16 = s.clamp(i16::MIN as i32, i16::MAX as i32) as i16;
                h.score = h.score.saturating_add(s16);
                if t < 4 {
                    h.best[t] = s16.max(0);
                }
            }
        }
    } else {
        for (t, tok) in pos.iter().enumerate() {
            // The token says which part of the path it matches on (parity row
            // Q.1). The extension is at the name's end and the attribute is
            // the entry's kind byte, so neither is the name -- and the
            // prefilter, which is about the name's start, says nothing about
            // either.
            let scored = match tok.axis {
                Axis::Name => tok.fits(m).then(|| token_score(&name, wide, m, tok)).flatten(),
                Axis::Extension => ext_of(&name).and_then(|e| token_score(e, None, !0, tok)),
                // A folder token is matched by a folder of the path, and the
                // memo carries it from the folder's name: the name's own hit
                // carries the bit so the memo can pick it up, and the score is
                // the folder's.
                Axis::Folder => tok.fits(m).then(|| token_score(&name, wide, m, tok)).flatten(),
                Axis::Attribute => None,
            };
            if let Some(s) = scored {
                h.bits |= 1 << t;
                got_name |= tok.axis == Axis::Name;
                let s16 = s.clamp(i16::MIN as i32, i16::MAX as i32) as i16;
                // A folder token's score is the folder's, and the scan adds it:
                // counting it here would count it twice.
                if tok.axis != Axis::Folder {
                    h.score = h.score.saturating_add(s16);
                }
                if t < 4 {
                    h.best[t] = s16.max(0);
                }
            }
        }
    }
    // A negated extension token is about the extension; every other negated
    // token is about the name.
    //
    // A negated FOLDER token is about neither: it is about the folders of
    // the path, so it rejects what is under a folder it matches, and the
    // memo is what carries that. The name is asked anyway -- it is how a
    // directory learns that its own name matches -- but the answer does
    // not reject the entry, because an entry's name is not a folder of
    // its path.
    let mut neg_name = false;
    let mut neg_dir = false;
    for t in neg {
        match t.axis {
            Axis::Name => {
                let hit = token_matches(&name, t);
                neg_name |= hit;
                neg_dir |= hit;
            }
            Axis::Extension => neg_name |= ext_of(&name).is_some_and(|e| token_matches(e, t)),
            Axis::Folder => neg_dir |= token_matches(&name, t),
            Axis::Attribute => {}
        }
    }
    if neg_name {
        h.flags |= NF_NEG;
    }
    if neg_dir {
        h.flags |= NF_NEG_DIR;
    }
    // Whether the query asks anything of the name: the name-axis tokens
    // and the extension ones (the extension is part of the name). A query
    // with no such token asks nothing, and every name is a candidate --
    // the scan decides.
    let name_asks = pos.iter().any(|t| matches!(t.axis, Axis::Name | Axis::Extension));
    // And whether it asks for a name: a name-axis token has to be answered
    // by the name itself.
    let name_pos = pos.iter().any(|t| t.axis == Axis::Name);
    let ext_mask: u8 = pos.iter().enumerate().fold(0, |acc, (t, tok)| if tok.axis == Axis::Extension { acc | (1 << t) } else { acc });
    let ok = (pos.is_empty() || !name_asks || h.bits != 0)
        && (!name_pos || got_name)
        && h.bits & ext_mask == ext_mask
        && h.flags & NF_NEG == 0
        && (q.exts.is_empty() || ext_ok(&name, &q.exts))
        && re.is_none_or(|re| re.is_match_bytes(&name));
    if ok {
        h.flags |= NF_OK;
    }
    (ok || h.bits != 0 || h.flags & (NF_NEG | NF_NEG_DIR) != 0).then_some(h)
}

/// Lowercased literals one of which starts every match of `re` (so a name
/// it matches contains it), if the regex has a short list of them.
fn re_literals(re: &crate::regex::Regex) -> Option<Vec<Vec<u8>>> {
    crate::regex::prefix_literals(re.pattern())
}

/// Lowercased literals one of which ends every path `re` matches, if it is
/// anchored at the end ("swift$") and they hold no '/': then a match's own
/// name, the path's last part, ends with one of them.
fn path_suffixes(re: &crate::regex::Regex) -> Option<Vec<Vec<u8>>> {
    crate::regex::suffix_literals(re.pattern())
}

/// Does `name` end with `s` (lowercase), ignoring ASCII case?
fn ends_with_fold(name: &[u8], s: &[u8]) -> bool {
    name.len() >= s.len() && name[name.len() - s.len()..].iter().zip(s).all(|(&a, &b)| fold(a) == b)
}

/// Fuzzy words this long forgive one typo (see `typo_score`).
#[derive(Clone)]
pub struct Hit {
    /// The key it was ranked by: the tier, the score, and the tie-break.
    ///
    /// The order of the results is the order of these keys (see
    /// [`crate::rank`]), so the score alone is not what decides a result.
    pub key: RankKey,
    /// Base entry, or u32::MAX for an overlay entry (then `over` is its path).
    pub idx: u32,
    /// The last change, for an overlay entry.
    ///
    /// An indexed entry's mtime is read from the index when the query
    /// sorts by it; only the overlay carries one here, because the index's
    /// entry is what the search reads it from.
    pub mtime: u32,
    pub over: Option<Vec<u8>>,
}

impl Hit {
    /// The score, in whole points.
    ///
    /// The key carries the score in a 24-bit field, so this is the score
    /// the ranking used, minus the tier that outranks it.
    pub fn score(&self) -> i32 {
        self.key.points()
    }
}

/// What makes a hit the entry it is, so a merge of `|` alternatives can tell
/// "the same entry again" from "a different entry".
///
/// `Hit::idx` alone is not that. [`search_overlay`] stamps `idx == u32::MAX`
/// on every entry it returns -- it is the sentinel for "not a base entry" --
/// and keeps the real identity in `over`. A merge keyed on `idx` therefore
/// treats every entry added since the last compaction as one entry and drops
/// the rest, which is a silent wrong answer rather than a loud one.
///
/// Owned (not a borrow of the hit) so a count can hold it while the hits it
/// came from are dropped.
#[derive(PartialEq, Eq, PartialOrd, Ord, Hash)]
enum HitId {
    /// A base entry, named by its index.
    Base(u32),
    /// An overlay entry, named by its path.
    Over(Vec<u8>),
}

impl Hit {
    fn id(&self) -> HitId {
        match &self.over {
            Some(p) => HitId::Over(p.clone()),
            None => HitId::Base(self.idx),
        }
    }
}

/// Collapse hits from several alternatives into one ranked list: an entry
/// matched twice keeps the better of the two scores and appears once.
///
/// The two ways to get this wrong are both silent. Key the merge on `idx` and
/// distinct overlay entries disappear (see [`HitId`]); find the match by
/// scanning the hits kept so far and the loop is O(n^2) over up to `limit`
/// hits, which is hundreds of milliseconds on every keystroke of an
/// interactive search. So: a sort by identity and a scan over the runs, which
/// is O(n log n) and asks for no table at all. The merge is one-shot -- the
/// hits are in hand and the answer is a list -- so a table would be a table
/// built to be thrown away.
///
/// The order is score descending, then base entries by index before overlay
/// entries, then overlay entries by path. That is the same total order the
/// callers used to get from `score desc, idx asc` plus a stable sort -- and
/// it matters that it is *total*: a map iteration order is not, and overlay
/// hits share `idx`, so two of them tying on score would otherwise come out
/// in a different order on every run.
pub fn merge_hits(all: Vec<Hit>, limit: usize) -> Vec<Hit> {
    let mut all = all;
    // The hits by identity, so the hits of one entry are together.
    all.sort_by(id_cmp);
    let mut hits: Vec<Hit> = Vec::with_capacity(all.len());
    for h in all {
        let same = match hits.last() {
            Some(last) => id_cmp(last, &h) == core::cmp::Ordering::Equal,
            None => false,
        };
        if same {
            // The same entry again: the better of the two scores wins, and
            // the first of them wins a tie.
            let last = hits.last_mut().expect("there is one");
            if last.key < h.key {
                *last = h;
            }
        } else {
            hits.push(h);
        }
    }
    hits.sort_by(|a, b| {
        b.key.cmp(&a.key).then_with(|| match (&a.over, &b.over) {
            (None, None) => a.idx.cmp(&b.idx),
            (Some(x), Some(y)) => x.cmp(y),
            (None, Some(_)) => core::cmp::Ordering::Less,
            (Some(_), None) => core::cmp::Ordering::Greater,
        })
    });
    hits.truncate(limit);
    hits
}

/// The order of two hits by identity.
///
/// The same identity as [`Hit::id`], compared without owning it: a merge
/// sorts the hits by this rather than copying a path per hit.
fn id_cmp(a: &Hit, b: &Hit) -> core::cmp::Ordering {
    match (&a.over, &b.over) {
        (None, None) => a.idx.cmp(&b.idx),
        (Some(x), Some(y)) => x.cmp(y),
        (None, Some(_)) => core::cmp::Ordering::Less,
        (Some(_), None) => core::cmp::Ordering::Greater,
    }
}
struct TopK {
    k: usize,
    buf: Vec<RankKey>,
    /// Keys at or below this cannot get in.
    floor: RankKey,
}

/// The key of an entry: the tier it inherits from its directory, its score
/// and its place in the order.
///
/// The tier is the point of the key: a priority that is only a term added
/// to the score loses to a better name, and the ranking corpus
/// (`tests/ranking.rs`) says the priority is what the user meant. The name
/// and attribute tiers are `Normal` until the rules of the next phase set
/// them.
#[inline(always)]
fn rank_key(tier: Tier, score: i32, entry: u32) -> RankKey {
    RankKey::new(tier, Tier::Normal, Tier::Normal, Score::from_points(score), entry)
}

/// The key of an entry whose tier rides in its kind byte: the path's tier
/// goes into the key's tier field without being unpacked and repacked.
#[inline(always)]
fn entry_key(kind: u8, score: i32, entry: u32) -> RankKey {
    RankKey::for_entry(kind, Score::from_points(score), entry)
}

impl TopK {
    fn new(k: usize, floor: RankKey) -> TopK {
        TopK { k, buf: Vec::new(), floor: if k == 0 { RankKey::from_bits(u64::MAX) } else { floor } }
    }
    #[inline]
    fn push(&mut self, key: RankKey) {
        if key > self.floor {
            self.buf.push(key);
            if self.buf.len() >= (2 * self.k).max(64) {
                self.cut();
            }
        }
    }
    /// Keep the k best; the k-th becomes the floor.
    fn cut(&mut self) {
        if self.buf.len() > self.k {
            self.buf.select_nth_unstable_by(self.k - 1, |a, b| b.cmp(a));
            self.buf.truncate(self.k);
            self.floor = self.buf[self.k - 1];
        }
    }
}

/// `f(state, i)` for each `i` in `0..n`, results in order, on the pool: the
/// calling thread starts at once, taking items in order from a shared
/// counter; one helper is queued, and each helper once running queues the
/// next before it takes items too. Threads asleep wake one after another off
/// the caller's path, and a helper that runs after the items are gone (the
/// caller runs any still queued when it is done) returns at once: no one
/// waits for a thread to wake, and no thread sits on a share of the work
/// while it does. `state` is each thread's scratch.
///
/// The pool is `rgy-par`'s: the same shape, the same width, and a width
/// the user chooses. What this adds is the worker's setup -- the platform's
/// interactive priority, once per worker, before it takes any index.
fn par_each<S: Send, T: Send>(n: usize, state: impl Fn() -> S + Sync, f: impl Fn(&mut S, usize) -> T + Sync) -> Vec<T> {
    rgy_par::each_index_prepared(n, crate::scan_priority::apply, state, f)
}

/// Top `k` over `0..n` items with keys above `floor`: a few contiguous
/// pieces per thread, each with its own heap, then one selection over their
/// survivors (merging heaps pairwise costs more than the scan when `k` is
/// large). Up to INLINE_VISITS entries to `visit` in all take less time
/// than waking threads.
fn top_k(n: usize, k: usize, visits: usize, floor: RankKey, visit: impl Fn(core::ops::Range<usize>, &mut TopK) + Sync) -> Vec<Hit> {
    let piece = |r: core::ops::Range<usize>| {
        let mut top = TopK::new(k, floor);
        visit(r, &mut top);
        top.cut();
        top
    };
    let tops: Vec<TopK> = if visits <= INLINE_VISITS {
        vec![piece(0..n)]
    } else {
        let pieces = (rgy_par::workers() * 4).min(n.max(1));
        let step = n.div_ceil(pieces).max(1);
        par_each(pieces, || (), |_, p| piece((p * step).min(n)..((p + 1) * step).min(n)))
    };
    // A full piece's k-th best already bounds the overall k-th from below.
    let floor = tops.iter().filter(|t| t.buf.len() == k).map(|t| t.floor).max().unwrap_or_default();
    let mut keys: Vec<RankKey> = tops.into_iter().flat_map(|t| t.buf).filter(|&x| x >= floor).collect();
    if keys.len() > k {
        if k == 0 {
            return Vec::new();
        }
        keys.select_nth_unstable_by(k - 1, |a, b| b.cmp(a));
        keys.truncate(k);
    }
    keys.into_iter().map(|key| Hit { key, idx: key.entry(), mtime: 0, over: None }).collect()
}

/// Below this many entries carrying a matching name, a `driven` attempt
/// visits just those entries (via the name -> entries list) and gives up
/// past it. A work budget, not a plan choice -- see
/// [`SELECTIVE_VISIT_RATIO`] for the latter.
const SELECTIVE: usize = 60_000;

/// How much more one visit costs `selective` than `full`, which is what
/// decides between them.
///
/// `selective` visits only the entries carrying a matching name and `full`
/// visits every entry, so `selective` does less work -- and was measured
/// losing anyway, because each of its visits costs far more. It walks a
/// name -> entries list and takes a HashMap per candidate; `full` makes one
/// linear pass over arrays. On the 200k-name bench corpus: about 18 ns a
/// candidate against 0.3 ns an entry, so a 60x ratio.
///
/// The crossover follows from it. `selective` wins while
/// `candidates * 60 < entries` -- about 3.3k of 200k there. The rule this
/// replaces was an absolute `ok_entries <= 60_000`, which chose `selective`
/// for every row the benchmark measures and lost 5-12x on all but the
/// narrowest:
///
/// ```text
/// report   29k candidates   575 us (selective) vs  60 us (full)   9.7x
/// mod      57k candidates   873 us (selective) vs  74 us (full)  11.8x
/// ```
///
/// while the one query it exists for still wins, and by a lot:
///
/// ```text
/// 'report_00000.pdf   1 candidate   0.4 us vs 63 us   157x
/// ```
///
/// So the plan is not wrong; the cut was. Narrow stays instant, broad stops
/// paying for a list walk it does not need.
const SELECTIVE_VISIT_RATIO: u64 = 60;
/// Bitset words (64 name ids each) per chunk of a name table: the unit of
/// parallel scoring and of hit storage (so a chunk's rank fits a u16).
const CHUNK_WORDS: usize = 64;
/// Restricted scoring with at most this many words to look at runs on the
/// calling thread.
const INLINE_WORDS: usize = 4096;
/// Entry scoring with at most this many entries to visit runs on the
/// calling thread.
const INLINE_VISITS: usize = 4096;
/// One-token searches expected to match this many names go `ranked`;
/// below BORDERLINE they do not, in between the bitmaps tell (`rankable`).
const RANKED_MIN: f64 = 20_000.0;
const BORDERLINE: f64 = 1_000.0;
/// `driven` tries a token expected to match at most this many names, the
/// rarest first, DRIVEN_TRIES of them at most.
const DRIVEN_NAMES: f64 = 20_000.0;
const DRIVEN_TRIES: usize = 2;
/// Cheap bounds `ranked` tells apart: BINS of them from -BIN0.
const BINS: usize = 1024;
/// The tiers the bins are stacked in: the tier first, then the score.
///
/// The key a bound belongs to starts with the tier, so the bins have to be
/// tier-major: a name whose folders are all demoted cannot beat a name
/// whose folder is not, whatever their scores say. A bin space that only
/// held the score would drop the second name for the first one's score.
const TIER_BINS: usize = 6 * BINS;
const BIN0: i32 = 256;
/// `ranked` scores the names best bound first in batches, the first about
/// this many names, then four times as many each time up to LAST_BATCH;
/// batches up to INLINE_NAMES run on the calling thread.
const FIRST_BATCH: usize = 64;
const LAST_BATCH: usize = 4096;
const INLINE_NAMES: usize = 512;
/// Overlay candidates a thread scores at a time (all of them on the calling
/// thread if no more).
const OVERLAY_CHUNK: usize = 256;

pub struct Searcher<'a> {
    pub live: &'a Live,
}

/// Which execution path a search actually took.
///
/// `search_base` picks between these by trying one and *bailing out* when a
/// precondition does not hold — `ranked` guesses, `driven` tries, and failing
/// both it falls through to `selective` or `full`. None of that is recorded, so
/// a slow query and a fast one look identical from outside. This is the
/// missing observation, not a new strategy: the paths are exactly the ones
/// already there.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Plan {
    /// A filter failed closed (`type:banana`); nothing is searched.
    FailClosed,
    /// The query's scope selects no entries.
    Empty,
    /// fzf-v1 ranked scorer over class-bitmap candidates.
    Ranked { tentative: bool },
    /// Bitmap intersection for multi-token or negated queries.
    Driven,
    /// Few candidates: score only entries whose names matched.
    Selective,
    /// Many candidates: one pass over every entry.
    Full,
}

impl core::fmt::Display for Plan {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Plan::FailClosed => write!(f, "fail-closed"),
            Plan::Empty => write!(f, "empty-scope"),
            Plan::Ranked { tentative: false } => write!(f, "ranked"),
            Plan::Ranked { tentative: true } => write!(f, "ranked (tentative)"),
            Plan::Driven => write!(f, "driven"),
            Plan::Selective => write!(f, "selective"),
            Plan::Full => write!(f, "full"),
        }
    }
}

// The plan the last search took, and the strategies it declined.
//
// This is the `--explain` surface: a diagnostic, not part of the search.
// It is thread-local because the decision points are all on the calling
// thread, even though the scan itself fans out.
//
// The `std` build: thread-local storage is what keeps the plan on the
// thread that decided it.
#[cfg(feature = "std")]
std::thread_local! {
    /// The plan taken by the most recent search on this thread. Thread-local
    /// rather than a field on `Searcher` so no call site has to change, and so
    /// `search(&self)` keeps taking `&self` — every decision point is on the
    /// calling thread even though the scan itself fans out.
    static LAST_PLAN: core::cell::Cell<Option<Plan>> = const { core::cell::Cell::new(None) };
    /// Strategies the query *reached for* and then declined, in order.
    ///
    /// Without this, a fast path that is guessed and rejected looks identical
    /// to one that never applied — and "never applied" and "always rejected"
    /// are very different findings about the code.
    static ATTEMPTS: core::cell::RefCell<Vec<&'static str>> = const { core::cell::RefCell::new(Vec::new()) };
}

#[cfg(feature = "std")]
fn note_plan(p: Plan) {
    LAST_PLAN.with(|c| c.set(Some(p)));
}

#[cfg(feature = "std")]
fn note_attempt(name: &'static str) {
    ATTEMPTS.with(|a| a.borrow_mut().push(name));
}

#[cfg(feature = "std")]
fn attempts_reset() {
    ATTEMPTS.with(|a| a.borrow_mut().clear());
}

/// Without thread-local storage there is nowhere for the plan to live, and
/// the diagnostic is not worth a lock: the search itself does not change.
#[cfg(not(feature = "std"))]
fn note_plan(_p: Plan) {}

#[cfg(not(feature = "std"))]
fn note_attempt(_name: &'static str) {}

#[cfg(not(feature = "std"))]
fn attempts_reset() {}

impl Searcher<'_> {
    /// The plan the most recent search **on this thread** took.
    ///
    /// `None` before the first search. Reads the same thread-local the
    /// decision points write, so it reports the path that actually returned —
    /// a `ranked` that bailed out and fell through shows up as what ran, not
    /// as what was tried.
    ///
    /// The `std` build: the plan lives in thread-local storage.
    #[cfg(feature = "std")]
    pub fn last_plan() -> Option<Plan> {
        LAST_PLAN.with(core::cell::Cell::get)
    }

    /// Without thread-local storage there is no plan to report.
    #[cfg(not(feature = "std"))]
    pub fn last_plan() -> Option<Plan> {
        None
    }

    /// Strategies the last search tried and declined, in order.
    ///
    /// A non-empty entry for a path `last_plan` never reports is the tell:
    /// the path is reached, its precondition holds, and it still declines.
    ///
    /// The `std` build: the attempts live in thread-local storage.
    #[cfg(feature = "std")]
    pub fn rejected_attempts() -> Vec<&'static str> {
        ATTEMPTS.with(|a| a.borrow().clone())
    }

    /// Without thread-local storage there are no attempts to report.
    #[cfg(not(feature = "std"))]
    pub fn rejected_attempts() -> Vec<&'static str> {
        Vec::new()
    }
}

impl Searcher<'_> {
    /// Entry range to scan, from the `in:` scope.
    pub fn scope_range(&self, q: &Query) -> Option<(usize, usize)> {
        let idx = &self.live.base;
        let Some(scope) = &q.scope else { return Some((1, idx.n)) };
        let e = idx.lookup(scope)?;
        let d = idx.dir_of(e)? as usize;
        Some((idx.dir_start()[d] as usize, idx.dir_end()[d] as usize))
    }

    pub fn search(&self, q: &Query) -> Vec<Hit> {
        attempts_reset();
        // Fail closed at the door. Every fast path below must agree with
        // match_path's `never` answer; a unit test on match_path alone let
        // `walk type:banana` leak through `ranked` once (the CLI returned
        // hits and exit 0 where zen-rs promises nothing + a warning).
        if q.never {
            note_plan(Plan::FailClosed);
            return Vec::new();
        }
        let mut hits = self.search_base(q);
        hits.extend(self.search_overlay(q));
        self.sort_hits(&mut hits, q.sort);
        hits.truncate(q.limit);
        hits
    }

    /// The hits of `q`, re-ranked from a set of entries a previous query
    /// matched.
    ///
    /// The entries are `(entry index, name id)`: the name id is what the
    /// score comes from, and the name table already holds it. The walk
    /// this replaces is the one over every entry in the scope; here only
    /// the entries that matched are visited, and each is re-scored against
    /// the new names.
    ///
    /// The answer is the same a fresh search gives, and that is the point:
    /// the caller may only use this when the new query can match no more
    /// entries than the old one, so the old match set is a superset of the
    /// new one. The session decides that; this only does the work.
    pub(crate) fn rerank_entries(&self, q: &Query, entries: &[(u32, u32)]) -> Vec<Hit> {
        let pos: Vec<&Token> = q.tokens.iter().filter(|t| !t.negate).collect();
        let neg: Vec<&Token> = q.tokens.iter().filter(|t| t.negate).collect();
        let (lo, hi) = self.scope_range(q).unwrap_or((1, self.live.base.n));
        // The names are scored as a fresh search scores them: the table
        // does the narrowing itself.
        let scored = self.names(q, &pos, &neg, lo, hi);
        let axes = q.axes();
        let s = Scan {
            q,
            live: self.live,
            names: &scored.names,
            npos: pos.len(),
            // The folder tokens need the memo: a folder of the path is what
            // satisfies them.
            need_dirs: axes.name.count_ones() > 1 || axes.folder != 0 || neg.iter().any(|t| matches!(t.axis, Axis::Name | Axis::Folder)),
            all: all_mask(pos.len(), &axes),
            axes,
            now: now_secs(),
        };
        let dense = scored.names.all.then(|| scored.names.dense(&self.live.base));
        // The dir memo is built per candidate (the same way the `selective`
        // plan does), so the re-rank does not build the whole tree's memo.
        // The entries are re-scored in parallel, the way the scan scores
        // them: a sequential loop over the candidates would make the
        // re-rank slower than the scan it replaces, on a machine with more
        // than one core.
        let mut hits = top_k(entries.len(), q.limit, entries.len(), RankKey::default(), |r, top| {
            let (mut pbuf, mut memo) = (Vec::new(), Map::<u32, DirMemo>::default());
            let re = q.path_re.clone();
            for &(i, name) in &entries[r] {
                let i = i as usize;
                if i < lo || i >= hi {
                    continue;
                }
                // The entry's name must still match: the query narrowed, so
                // some of the old matches no longer do.
                let nh = match dense {
                    Some(d) => {
                        let s = d[name as usize];
                        NameHit { score: s.score, bits: s.bits, flags: s.flags, best: [0; 4] }
                    }
                    None => {
                        let Some(nh) = scored.names.get(name) else { continue };
                        nh
                    }
                };
                if nh.flags & NF_OK == 0 {
                    continue;
                }
                let m = if s.need_dirs { s.memo_of(self.live.base.parent()[i], &mut memo, &mut |k| scored.names.get(k)) } else { DirMemo::default() };
                if !s.axes.plain && !s.axes_ok(i, &nh, &m) {
                    continue;
                }
                if let Some(k) = s.score(i, nh, m, top.floor, &mut pbuf, re.as_ref()) {
                    top.push(k);
                }
            }
        });
        // The overlay is not in the index, so it is searched every time:
        // an entry added since the last compaction has no index to re-rank.
        hits.extend(self.search_overlay(q));
        self.sort_hits(&mut hits, q.sort);
        hits.truncate(q.limit);
        hits
    }

    /// The hits of `q`, split into the index's and the overlay's.
    ///
    /// The split is what a session needs: the overlay's entries have no
    /// index to re-rank, so the session keeps only the index's -- and the
    /// count of the two together is what says whether the index's set was
    /// complete (a search that hit its cap found more than it kept).
    pub(crate) fn search_split(&self, q: &Query) -> (Vec<Hit>, Vec<Hit>) {
        let base = self.search_base(q);
        let over = self.search_overlay(q);
        (base, over)
    }

    /// The order the hits come in, for the query's `sort:`.
    ///
    /// The choice orders the hits the cap kept; it does not pick them. The
    /// cap still takes the best by score, so `sort:mtime` is "the newest of
    /// the best matches", not "the newest matches".
    pub(crate) fn sort_hits_public(&self, hits: &mut [Hit], sort: SortBy) {
        self.sort_hits(hits, sort);
    }

    fn sort_hits(&self, hits: &mut [Hit], sort: SortBy) {
        let idx = &self.live.base;
        match sort {
            SortBy::Score => hits.sort_by(|a, b| b.key.cmp(&a.key).then(a.idx.cmp(&b.idx))),
            SortBy::Name => hits.sort_by(|a, b| {
                let an = match &a.over {
                    Some(p) => name_of(p),
                    None => idx.name(a.idx as usize),
                };
                let bn = match &b.over {
                    Some(p) => name_of(p),
                    None => idx.name(b.idx as usize),
                };
                an.cmp(bn).then(a.idx.cmp(&b.idx))
            }),
            SortBy::MTime => hits.sort_by(|a, b| {
                let am = match &a.over {
                    Some(_) => a.mtime,
                    None => idx.mtime()[a.idx as usize],
                };
                let bm = match &b.over {
                    Some(_) => b.mtime,
                    None => idx.mtime()[b.idx as usize],
                };
                bm.cmp(&am).then(a.idx.cmp(&b.idx))
            }),
            SortBy::None => hits.sort_by_key(|h| h.idx),
        }
    }

    /// The query with its result cap lifted.
    ///
    /// `search_base` truncates *internally* through `TopK`, so raising the
    /// limit is what actually makes a result complete. Skipping only the
    /// outer `truncate` still returned exactly `q.limit` hits — this helper
    /// exists so `count` and `search_uncapped` cannot drift on that detail.
    fn uncapped(q: &Query) -> Query {
        let mut c = q.clone();
        // Room for the `(2 * k)` cut threshold's multiply.
        c.limit = usize::MAX / 2;
        c
    }

    /// Every match for the query, ranked, **ignoring `q.limit`**.
    ///
    /// The count path needs this: a caller that unions the results of several
    /// `|` alternatives must see each alternative's full match set, or a hit
    /// that both alternatives match would be undercounted.
    pub fn search_uncapped(&self, q: &Query) -> Vec<Hit> {
        attempts_reset();
        if q.never {
            note_plan(Plan::FailClosed);
            return Vec::new();
        }
        let q = &Self::uncapped(q);
        let mut hits = self.search_base(q);
        hits.extend(self.search_overlay(q));
        self.sort_hits(&mut hits, q.sort);
        hits
    }

    /// How many entries match, without ranking them.
    ///
    /// Deliberately *not* a parallel implementation. `search_base` and
    /// `search_overlay` are reused verbatim, so the count can never disagree
    /// with `search`; only the final sort and the caller's per-hit path
    /// reconstruction are skipped. Writing a separate "count only" path
    /// through `ranked`/`drive`/`selective`/`full` would have saved more, but
    /// four match logics drifting apart is a silent-wrong-answer bug waiting
    /// to happen — and that costs far more than the sort.
    ///
    /// This is the single-alternative count. A caller unioning several `|`
    /// alternatives must dedup across them (an entry both match counts once),
    /// which is what `search_uncapped` plus a set is for.
    pub fn count(&self, q: &Query) -> usize {
        // Fail closed exactly as `search` does.
        if q.never {
            return 0;
        }
        let q = &Self::uncapped(q);
        self.search_base(q).len() + self.search_overlay(q).len()
    }

    /// Score every `|` alternative and merge the results into one ranked
    /// list: an entry matched by more than one of them appears once, keeping
    /// the best score any of them gave it.
    ///
    /// Both `searchctl` and the TUI need exactly this, so it lives here
    /// rather than being written twice -- it was written twice, and both
    /// copies keyed the merge on `Hit::idx`. That is not an entry's identity:
    /// `search_overlay` gives `idx == u32::MAX` to *every* entry it returns,
    /// with the real identity in `over`, so the two copies collapsed all of
    /// the entries added since the last compaction into one and silently
    /// dropped the rest. The merge keys on [`Hit::id`] instead.
    pub fn search_alternatives(&self, alts: &[Query], limit: usize) -> Vec<Hit> {
        let mut all = Vec::new();
        for alt in alts {
            let mut q = alt.clone();
            q.limit = limit;
            all.extend(self.search(&q));
        }
        merge_hits(all, limit)
    }

    /// How many distinct entries the `|` alternatives match between them.
    ///
    /// Same identity as [`Self::search_alternatives`], so a count cannot
    /// disagree with the list it counts. Keyed on `Hit::idx` this returns 1
    /// for any query matching only entries added since the last compaction.
    pub fn count_alternatives(&self, alts: &[Query]) -> usize {
        // The identities of the hits, sorted: the count is the number of
        // runs. A set would be a table built to be thrown away, and the
        // count is one-shot.
        let mut seen: Vec<HitId> = Vec::new();
        for alt in alts {
            for h in self.search_uncapped(alt) {
                seen.push(h.id());
            }
        }
        seen.sort_unstable();
        seen.dedup();
        seen.len()
    }

    fn search_base(&self, q: &Query) -> Vec<Hit> {
        // Every branch below records the plan it commits to. Only the path
        // that actually *returns* notes it, so a `ranked` that bails out is
        // not reported as the plan that ran — it is recorded as a rejection.
        let Some((lo, hi)) = self.scope_range(q) else {
            note_plan(Plan::Empty);
            return Vec::new();
        };
        if let Some((t, tentative)) = self.rankable(q, lo, hi) {
            let scope = (q.scope.is_some() && hi - lo <= self.live.base.u / 4).then(|| self.scope_names(lo, hi));
            match self.ranked(q, t, lo, hi, scope.as_deref(), tentative) {
                Some(hits) => {
                    note_plan(Plan::Ranked { tentative });
                    return hits;
                }
                None => note_attempt("ranked"),
            }
        }
        let pos: Vec<&Token> = q.tokens.iter().filter(|t| !t.negate).collect();
        let neg: Vec<&Token> = q.tokens.iter().filter(|t| t.negate).collect();
        if pos.len() > 1 || !neg.is_empty() {
            match self.driven(q, &pos, &neg, lo, hi) {
                Some(hits) => {
                    note_plan(Plan::Driven);
                    return hits;
                }
                None => note_attempt("driven"),
            }
        }
        // Step 1: every name-only predicate, once per distinct name (~2M)
        // rather than once per entry (~7.5M); reused while you type.
        let scored = self.names(q, &pos, &neg, lo, hi);
        let axes = q.axes();
        let s = Scan {
            q,
            live: self.live,
            names: &scored.names,
            npos: pos.len(),
            need_dirs: axes.name.count_ones() > 1 || axes.folder != 0 || neg.iter().any(|t| matches!(t.axis, Axis::Name | Axis::Folder)),
            all: all_mask(pos.len(), &axes),
            axes,
            now: now_secs(),
        };
        // Step 2: score entries. Few candidates: just the entries carrying a
        // matching name. Many: one sequential pass over every entry.
        let fast = !scored.names.all && !FULL_PASS.load(core::sync::atomic::Ordering::Relaxed);
        if fast && (scored.names.ok_entries as u64) * SELECTIVE_VISIT_RATIO <= (hi - lo) as u64 {
            note_plan(Plan::Selective);
            return s.selective(lo, hi);
        }

        note_plan(Plan::Full);
        let memo = if s.need_dirs { Some(scored.memo.get_or_init(|| self.dir_tokens(&scored.names))) } else { None };
        s.full(lo, hi, memo.map(|m| m.as_slice()))
    }

    /// The query's token if `ranked` can answer it: one fuzzy token (and any
    /// negated ones), no filter on the entries themselves, and broad enough
    /// to be worth it, going by how many names have each of its classes.
    /// That guess misses words whose letters go together ("delivery": 1,349
    /// names guessed, 25k candidates): an unscoped word (letters only; with
    /// a dot, digit or delimiter a token names a file) it puts between
    /// BORDERLINE and RANKED_MIN names gets its candidates counted in every
    /// 64th bitmap word, and if those are many, a tentative try (true).
    fn rankable<'q>(&self, q: &'q Query, lo: usize, hi: usize) -> Option<(&'q Token, bool)> {
        let mut pos = q.tokens.iter().filter(|t| !t.negate);
        let (Some(t), None) = (pos.next(), pos.next()) else { return None };
        if t.mode != Mode::Fuzzy
            || !q.tokens.iter().all(|t| t.axis == Axis::Name)
            || q.kind.is_some()
            || q.link
            || q.size_set
            || q.mtime != (0, u32::MAX)
            || q.path_re.is_some()
            || !q.globs.is_empty()
            || q.limit == 0
        {
            return None;
        }
        // Typing on from a table (the last query was narrow): narrowing it
        // is cheaper.
        let scope = (q.scope.is_some() && hi - lo <= self.live.base.u / 4).then_some((lo, hi));
        let key = NameKey::of(q, scope);
        if self.live.names_cache.last.lock().unwrap().as_ref().is_some_and(|p| key.narrows(&p.key)) {
            return None;
        }
        let (idx, u) = (&self.live.base, self.live.base.u as f64);
        let counts = idx.class_counts();
        let est = (0..CLASSES).filter(|&c| t.mask & (1 << c) != 0).fold(u, |e, c| e * counts[c] as f64 / u);
        // `ranked` is off by default, and the reason is a measurement rather
        // than a judgement call. It is a bounded top-k meant to win on broad
        // one-token searches; measured on `name_bench`'s 200k corpus against
        // the `full` plan it replaces, it loses on both rows it is chosen
        // for:
        //
        //     file   5,632 us (ranked)  vs    91 us (full)   62x slower
        //     f1     4,547 us (ranked)  vs   137 us (full)   33x slower
        //
        // Same match counts and the same results. On real trees it does not
        // even get that far: it bails out (`rejected-after: ranked`) and
        // `full` runs anyway, so it costs a failed attempt every time.
        //
        // The design is sound for a shape the bench does not exercise -- a
        // broad query whose top `limit` comes from one or two dominant names,
        // where the floor jumps at once and prunes the rest. `rankable`
        // selects on breadth (`RANKED_MIN`), which is the wrong signal: it
        // fires exactly when many names score alike and pruning cannot pay.
        //
        // So the gate is opt-in, and the thresholds below document what the
        // design intended. `RGY_FORCE_RANKED=1` turns it back on to study it.
        // No `note_attempt` here: this is not a path reached and rejected, it
        // is one deliberately not considered, and saying otherwise on every
        // `--explain` would be noise.
        //
        // The `std` build: the environment is where the switch lives.
        #[cfg(feature = "std")]
        std::env::var_os("RGY_FORCE_RANKED")?;
        if est >= RANKED_MIN {
            return Some((t, false));
        }
        if est < BORDERLINE || q.scope.is_some() || !t.text.iter().all(u8::is_ascii_alphabetic) {
            // `rankable` never even guessed, which is a different finding from
            // a guess that `ranked` rejected: this path is not being reached.
            note_attempt("rankable");
            return None;
        }
        let tb = TokenBits::new(idx, t);
        let found = (0..idx.words).step_by(64).map(|w| tb.word(w).0.count_ones() as f64).sum::<f64>() * 64.0;
        if found < RANKED_MIN {
            note_attempt("rankable-sampled");
            return None;
        }
        Some((t, true))
    }

    /// A broad one-token search ("a", "de"): its top `limit` comes from a few
    /// dozen of the million names it matches. Names are scored in batches,
    /// in falling order of a bound on their entries' scores read without the
    /// name (`index::NameInfo`): 24 per char + 8, the placement bonus its
    /// length and stem leave possible if it starts with the token's first
    /// byte, the best location prior among its folders and the best rank
    /// tweak. Each batch's entries are visited best prior first, until no
    /// name left can beat the limit-th entry found. Names that cannot start
    /// with the token come from a list of the highest keys, or, if the floor
    /// ends up below it, from a pass over them all.
    /// None if fewer than `limit` hits turn up among the few thousand names
    /// with the best bounds: then a name table, which typing can narrow from,
    /// costs about the same.
    fn ranked(&self, q: &Query, t: &Token, lo: usize, hi: usize, within: Option<&NameSet>, tentative: bool) -> Option<Vec<Hit>> {
        let idx = &self.live.base;
        let (prior, info, ne_off, ne) = (idx.name_prior(), idx.name_info(), idx.name_ents_off(), idx.name_ents());
        let tb = TokenBits::new(idx, t);
        let start = idx.bitmap(BM_START + crate::name_index::start_hash(t.text[0]));
        let space = char_bit(b' ').trailing_zeros() as usize;
        let space = [idx.bitmap(BM_FIRST + space), idx.bitmap(BM_SECOND + space)];
        let tweak = |flags: u8| 10 + if flags & NF_APP != 0 { 25 } else { 0 } - if flags & NF_DOT != 0 { 8 } else { 0 };
        // The bin a bound falls in: the tier first, then the score.
        //
        // The tier is the point. The key a bound belongs to starts with the
        // tier, so a name whose folders are all demoted cannot beat a name
        // whose folder is not, whatever their scores say -- and a bin space
        // that only held the score would drop the second name for the
        // first one's score.
        let bin_of = |tier: Tier, bound: i32| tier.bits() as usize * BINS + (bound + BIN0).clamp(0, BINS as i32 - 1) as usize;
        let neg: Vec<&Token> = q.tokens.iter().filter(|t| t.negate).collect();
        let scoped = within.is_some();
        let within = |w: usize| within.map_or(!0, |s| s.bits[w]);
        // Name k, given its word's `TokenBits::word`: its hit and the bound
        // on its entries.
        let score = |k: u32, (_, clean, typo): (u64, u64, u64), re: Option<&crate::regex::Regex>| -> Option<Cand> {
            let (w, i) = (k as usize / 64, k % 64);
            // The name is folded: a query typed in NFC has to match a name
            // the filesystem stored in NFD.
            let (name, wide) = idx.uname_folded(k);
            let spaced = (space[0][w] | space[1][w]) >> i & 1 != 0;
            let s = token_score(&name, wide, t.known_mask(clean >> i & 1 != 0, typo >> i & 1 != 0, spaced), t)?;
            if !(q.exts.is_empty() || ext_ok(&name, &q.exts))
                || re.is_some_and(|re| !re.is_match_bytes(&name))
                || neg.iter().any(|t| token_matches(&name, t))
            {
                return None;
            }
            let score = s.clamp(i16::MIN as i32, i16::MAX as i32) as i16;
            let flags = name_flags(&name) | NF_OK;
            let p = prior[k as usize] as i32;
            Some(Cand { bound: score as i32 + p + tweak(flags), tier: Tier::from_prior(p), k, score, flags })
        };
        // The cheap bound: 24 per char + 8 (16 less if the token starts with
        // a delimiter, which takes no boundary bonus), plus the placement
        // bonus its length and stem leave possible if the name starts with
        // the token's first byte, plus its key.
        let (m, t0) = (t.text.len() as i32, t.text[0]);
        let base = 24 * m + 8 - if class(t0) == Class::Delim { 16 } else { 0 };
        let cheap = |x: crate::name_index::NameInfo| {
            base + x.key as i32
                + match x.head == t0 {
                    false => 0,
                    true if x.len as i32 == m => 100,
                    true if x.stem as i32 == m => 80,
                    true => 30,
                }
        };
        let scan = Scan {
            q,
            live: self.live,
            names: &NameTable::new(Vec::new(), Vec::new(), Vec::new(), 0, false),
            npos: 1,
            need_dirs: false,
            // The token of this path is a name token (see `rankable`), so
            // there is no axis to honour.
            axes: Axes::default(),
            all: 1,
            now: now_secs(),
        };
        let entries = |k: u32| &ne[ne_off[k as usize] as usize..ne_off[k as usize + 1] as usize];
        let (parent, de, dp, en) = (idx.parent(), idx.dir_entry(), idx.dir_parent(), idx.ent_name());
        // Whether a folder on the path to dir `d` has a negated token's name.
        let negated = |d: u32, cache: &mut Map<u32, bool>| {
            let mut chain = Vec::new();
            let mut k = d;
            let mut out = loop {
                if k == 0 {
                    break false;
                }
                if let Some(&b) = cache.get(&k) {
                    break b;
                }
                chain.push(k);
                k = dp[k as usize];
            };
            for &k in chain.iter().rev() {
                let (name, _) = idx.uname_folded(en[de[k as usize] as usize]);
                out = out || neg.iter().any(|t| token_matches(&name, t));
                cache.insert(k, out);
            }
            out
        };
        // The entries of `names` that beat `floor` (a key). A name's run
        // stops once `limit` of its entries are in: that many at most count
        // as work.
        let visit = |names: &[Cand], floor: RankKey| {
            let visits = names.iter().map(|x| entries(x.k).len().min(q.limit)).sum();
            top_k(names.len(), q.limit, visits, floor, |r, top| {
                let (mut pbuf, mut cache) = (Vec::new(), Map::<u32, bool>::default());
                for c in &names[r] {
                    let nh = NameHit { score: c.score, bits: 1, flags: c.flags, best: [0; 4] };
                    for &e in entries(c.k).iter().filter(|&&e| (lo..hi).contains(&(e as usize))) {
                        // Entries come best prior first: the rest cannot make it either.
                        let p = idx.rank_adj()[e as usize] as i32;
                        if entry_key(idx.kind()[e as usize], c.score as i32 + p + tweak(c.flags), e) <= top.floor {
                            break;
                        }
                        if !neg.is_empty() && negated(parent[e as usize], &mut cache) {
                            continue;
                        }
                        if let Some(key) = scan.score(e as usize, nh, DirMemo::default(), top.floor, &mut pbuf, None) {
                            top.push(key);
                        }
                    }
                }
            })
        };
        // The best `limit` hits so far, the limit-th one's bin (i32::MIN if
        // fewer) and key (the default if fewer).
        let best = |mut hits: Vec<Hit>| {
            hits.sort_by(|a, b| b.key.cmp(&a.key).then(a.idx.cmp(&b.idx)));
            hits.truncate(q.limit);
            let last = hits.get(q.limit - 1).map(|h| (bin_of(h.key.path_tier(), h.key.points()) as i32, h.key));
            let (floor, fkey) = last.unwrap_or((i32::MIN, RankKey::default()));
            (hits, floor, fkey)
        };
        // The names that can start with the token (the only ones that can
        // get a placement bonus), best cheap bound first, with how many fall
        // in each bin.
        //
        // A name's bin is its best folder's tier -- the best tier any of its
        // entries is in -- and its cheap bound.
        let bin = |bound: i32, k: u32| bin_of(Tier::from_prior(prior[k as usize] as i32), bound);
        let sorted = |got: Vec<(i32, u32)>, h: Vec<u32>| {
            let mut at = vec![0u32; TIER_BINS];
            let mut acc = 0;
            for b in (0..TIER_BINS).rev() {
                at[b] = acc;
                acc += h[b];
            }
            let mut sorted = vec![(0, 0); got.len()];
            for x in got {
                let b = bin(x.0, x.1);
                sorted[at[b] as usize] = x;
                at[b] += 1;
            }
            (sorted, h)
        };
        let by_key = idx.by_key();
        // How far a run of batches got: the best hits so far, the limit-th
        // one's bin and key, the bin from which on every name is scored,
        // the next batch's size and how much of `by_key.list` is done.
        struct Run {
            hits: Vec<Hit>,
            floor: i32,
            fkey: RankKey,
            top: usize,
            want: usize,
            listed: usize,
        }
        // Names with their cheap bounds, best first, and how many per bin.
        type Part = (Vec<(i32, u32)>, Vec<u32>);
        // The bin a name of `by_key.list` falls in: its key's bound, and the
        // highest tier a name with that key can be in.
        //
        // A name's key is its prior plus its tweak less its length
        // penalty, so its prior is at most the key plus 34 -- and the tier
        // is what the prior says. The tier that goes with the key is
        // therefore an upper bound, and the bound is what a walk needs: the
        // name is looked at no later than it deserves. It also keeps the
        // bins monotone in the key, which is the order the list is in.
        let list_bin = |key: i32| bin_of(Tier::from_prior(key + 34), base + key);
        // Score them best bin first, a batch at a time, until no name left
        // can beat the limit-th entry found. With `complete` false `parts`
        // holds only some of them, the rest bounded by `cover + 29` over
        // base: unless that settles the search, how far it got.
        let run = |parts: Vec<Part>, complete: bool, r: Run| -> Result<Option<Vec<Hit>>, Run> {
            let Run { mut hits, mut floor, mut fkey, mut top, mut want, mut listed } = r;
            let mut hist = vec![0usize; TIER_BINS];
            for (_, h) in &parts {
                hist.iter_mut().zip(h).for_each(|(a, &b)| *a += b as usize);
            }
            // Names that cannot start with the token get no placement bonus:
            // base + key bounds them. The highest keys come in order, the
            // rest only matter if the floor ends up below them.
            //
            // The bins are tier-major, and the counts by key cannot say
            // which tier a name is in, so the histogram is built from the
            // list itself: it holds every name at or above the cover, and
            // it is bounded.
            for &(key, _) in &by_key.list {
                hist[list_bin(key as i32)] += 1;
            }
            let mut taken: Vec<usize> = parts.iter().map(|(items, _)| items.iter().take_while(|x| bin(x.0, x.1) >= top).count()).collect();
            let done = |floor: i32, top: usize| floor != i32::MIN && (top == 0 || floor > top as i32 - 1);
            // The cheapest bin a name left can be in: the cover (the lowest
            // name key) over base, in the lowest tier there is. The floor
            // has to be above it before no name left can settle the search.
            let cheapest = bin_of(Tier::Ignored, base + by_key.cover) as i32;
            // Without all of them, names bounded by `cover + 29` over base
            // cannot settle it.
            let least = if complete { 0 } else { bin_of(Tier::Ignored, base + by_key.cover + 30) };
            while want <= LAST_BATCH && !done(floor, top) && top > least {
                // The floor as a bin. i32::MIN means no hit yet, and then
                // every name is worth looking at.
                let fbin = if floor == i32::MIN { 0 } else { floor as usize };
                let low = if floor == i32::MIN { 0 } else { floor as usize }.max(least);
                let (mut cut, mut n) = (top, 0);
                while cut > low && n < want {
                    cut -= 1;
                    n += hist[cut];
                }
                let ends: Vec<usize> =
                    parts.iter().zip(&taken).map(|((items, _), &a)| a + items[a..].iter().take_while(|x| bin(x.0, x.1) >= cut).count()).collect();
                let lend = listed + by_key.list[listed..].iter().take_while(|x| list_bin(x.0 as i32) >= cut).count();
                let one = |p: usize| {
                    let re = q.name_re.clone();
                    let items = &parts[p].0[taken[p]..ends[p]];
                    items
                        .iter()
                        .filter_map(|&(_, k)| score(k, tb.word(k as usize / 64), re.as_ref()).filter(|c| bin_of(c.tier, c.bound) >= fbin))
                        .collect::<Vec<_>>()
                };
                let mut batch: Vec<Cand> = if n <= INLINE_NAMES {
                    (0..parts.len()).flat_map(one).collect()
                } else {
                    par_each(parts.len(), || (), |_, p| one(p)).into_iter().flatten().collect()
                };
                let re = q.name_re.clone();
                batch.extend(by_key.list[listed..lend].iter().filter_map(|&(_, k)| {
                    let (w, i) = (k as usize / 64, k % 64);
                    if info[k as usize].head == t0 || within(w) >> i & 1 == 0 {
                        return None;
                    }
                    let x = tb.word(w);
                    (x.0 >> i & 1 != 0).then(|| score(k, x, re.as_ref())).flatten().filter(|c| bin_of(c.tier, c.bound) >= fbin)
                }));
                (taken, listed) = (ends, lend);
                (hits, floor, fkey) = best(hits.into_iter().chain(visit(&batch, fkey)).collect());
                top = cut;
                want *= 4;
                // A tentative try that one batch does not settle has few good
                // matches: a name table costs no more and typing can narrow
                // it. So does any try whose first two batches turn up a
                // handful of hits.
                if tentative && !(done(floor, top) && floor >= cheapest) || hits.len() < q.limit / 4 && want > FIRST_BATCH * 4 {
                    if !complete {
                        break;
                    }
                    return Ok(None);
                }
            }
            // Then, if the floor is low, every other name that can still
            // beat it, in one pass.
            if done(floor, top) && floor >= cheapest && floor >= least as i32 {
                return Ok(Some(hits));
            }
            if !complete {
                return Err(Run { hits, floor, fkey, top, want, listed });
            }
            if floor == i32::MIN {
                return Ok(None);
            }
            // The floor as a bin, for the pass over what is left.
            let fbin = if floor == i32::MIN { 0 } else { floor as usize };
            let scored = |k: usize| {
                let x = info[k];
                let placed = x.head == t0;
                bin(cheap(x), k as u32) >= top && (placed || x.key as i32 >= by_key.cover)
            };
            let rest = par_each(
                idx.words.div_ceil(CHUNK_WORDS),
                || q.name_re.clone(),
                |re, c| {
                    let mut out = Vec::new();
                    for w in c * CHUNK_WORDS..((c + 1) * CHUNK_WORDS).min(idx.words) {
                        let mut m = within(w);
                        if m == 0 {
                            continue;
                        }
                        let x = tb.word(w);
                        m &= x.0;
                        while m != 0 {
                            let k = w * 64 + m.trailing_zeros() as usize;
                            m &= m - 1;
                            if bin_of(Tier::from_prior(prior[k] as i32), cheap(info[k])) >= fbin
                                && !scored(k)
                                && let Some(c) = score(k as u32, x, re.as_ref()).filter(|c| bin_of(c.tier, c.bound) >= fbin)
                            {
                                out.push(c);
                            }
                        }
                    }
                    out
                },
            )
            .into_iter()
            .flatten()
            .collect::<Vec<_>>();
            Ok(Some(best(hits.into_iter().chain(visit(&rest, fkey)).collect()).0))
        };
        let mut from = Run { hits: Vec::new(), floor: i32::MIN, fkey: RankKey::default(), top: TIER_BINS, want: FIRST_BATCH, listed: 0 };
        // A short token first tries the names starting with it that it can
        // find without a pass over every word: those with the highest keys,
        // and those it can match whole (`index::ByKey`). Any other such name
        // has no bonus above 30 and a key below `cover`. If that does not
        // settle it, the pass goes on from there.
        if t.text.len() <= crate::name_index::SHORT && !scoped {
            let m = t.text.len();
            let whole = |k: &u32| {
                let x = info[*k as usize];
                (x.len as usize == m || x.stem as usize == m) && (x.key as i32) < by_key.cover
            };
            let (mut got, mut h) = (Vec::new(), vec![0u32; TIER_BINS]);
            for &k in by_key.heads[t0 as usize].iter().chain(by_key.short[t0 as usize].iter().filter(|k| whole(k))) {
                if tb.word(k as usize / 64).0 >> (k % 64) & 1 != 0 {
                    let c = cheap(info[k as usize]);
                    h[bin_of(Tier::from_prior(prior[k as usize] as i32), c)] += 1;
                    got.push((c, k));
                }
            }
            match run(vec![sorted(got, h)], false, from) {
                Ok(found) => return found,
                Err(r) => from = r,
            }
        }
        // Otherwise all of them, from a pass over the words in pieces.
        let pieces = if idx.words <= INLINE_WORDS { 1 } else { rgy_par::workers() * 2 };
        let step = idx.words.div_ceil(pieces);
        let piece = |p: usize| {
            let (mut got, mut h) = (Vec::new(), vec![0u32; TIER_BINS]);
            for (w, &s) in start.iter().enumerate().take(((p + 1) * step).min(idx.words)).skip(p * step) {
                let mut x = s & within(w);
                if x == 0 {
                    continue;
                }
                x &= tb.word(w).0;
                while x != 0 {
                    let k = w * 64 + x.trailing_zeros() as usize;
                    x &= x - 1;
                    if info[k].head == t0 {
                        let c = cheap(info[k]);
                        h[bin_of(Tier::from_prior(prior[k] as i32), c)] += 1;
                        got.push((c, k as u32));
                    }
                }
            }
            sorted(got, h)
        };
        let parts = if pieces == 1 { vec![piece(0)] } else { par_each(pieces, || (), |_, p| piece(p)) };
        run(parts, true, from).ok().flatten()
    }

    /// Several tokens: a hit has each in its own name or in a folder above
    /// it, so the names the rarest token matches and the subtrees of folders
    /// so named hold every hit. When those are few, they are all that gets
    /// looked at: their names and folders are scored as they come up, not
    /// all two million names up front. None if they are many.
    fn driven(&self, q: &Query, pos: &[&Token], neg: &[&Token], lo: usize, hi: usize) -> Option<Vec<Hit>> {
        let idx = &self.live.base;
        let (counts, u) = (idx.class_counts(), idx.u as f64);
        let est = |t: &Token| (0..CLASSES).filter(|&c| t.mask & (1 << c) != 0).fold(u, |e, c| e * counts[c] as f64 / u);
        // Rarest first. A rare name can still hold too much (a big folder:
        // "developer" is ~/Developer); found out early, the next one gets a
        // try.
        //
        // Only a token that is matched against the NAME may drive: the
        // driver's prefilter says which names hold the token, and a token
        // on the extension is at the name's end and a token on the
        // attribute is not a name at all -- so driving on one of those
        // selects the wrong names (or none), and the query answers
        // something the other token would not. The folder axis is a name
        // too: a folder's own name is in the table, and its subtree comes
        // with it.
        let mut order: Vec<usize> =
            (0..pos.len()).filter(|&t| matches!(pos[t].axis, Axis::Name | Axis::Folder) && est(pos[t]) <= DRIVEN_NAMES).collect();
        order.sort_by(|&a, &b| est(pos[a]).total_cmp(&est(pos[b])));
        for t in order.into_iter().take(DRIVEN_TRIES) {
            match self.drive(q, pos, neg, lo, hi, t) {
                Ok(hits) => return Some(hits),
                Err(true) => continue,
                Err(false) => return None,
            }
        }
        None
    }

    /// `driven` from token `t`'s names; Err(true) if it gave up within the
    /// first sixteenth of the names.
    fn drive(&self, q: &Query, pos: &[&Token], neg: &[&Token], lo: usize, hi: usize, t: usize) -> Result<Vec<Hit>, bool> {
        let idx = &self.live.base;
        let tb = TokenBits::new(idx, pos[t]);
        let space = char_bit(b' ').trailing_zeros() as usize;
        let space = [idx.bitmap(BM_FIRST + space), idx.bitmap(BM_SECOND + space)];
        let (ne_off, ne, kind, en, parent) = (idx.name_ents_off(), idx.name_ents(), idx.kind(), idx.ent_name(), idx.parent());
        // The names' entries, and the subtrees of those that are folders
        // (from above `lo` too: the scope's own ancestors). Past SELECTIVE
        // entries in all, give up, or as soon as the chunks so far (handed
        // out in order; a sixteenth at least) say twice that many are coming.
        use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering::Relaxed};
        let chunks = idx.words.div_ceil(CHUNK_WORDS);
        let (seen, quit, early) = (AtomicUsize::new(0), AtomicBool::new(false), AtomicBool::new(false));
        let over = || quit.load(Relaxed);
        let spaced = |w: usize, i: u32| (space[0][w] | space[1][w]) >> i & 1 != 0;
        let found = par_each(
            chunks,
            || (),
            |_, c| {
                let (mut ents, mut trees) = (Vec::new(), Vec::new());
                for w in c * CHUNK_WORDS..((c + 1) * CHUNK_WORDS).min(idx.words) {
                    if over() {
                        break;
                    }
                    let (fits, clean, typo) = tb.word(w);
                    let mut m = fits;
                    while m != 0 {
                        let i = m.trailing_zeros();
                        m &= m - 1;
                        let k = w * 64 + i as usize;
                        let known = pos[t].known_mask(clean >> i & 1 != 0, typo >> i & 1 != 0, spaced(w, i));
                        let (name, wide) = idx.uname_folded(k as u32);
                        if token_score(&name, wide, known, pos[t]).is_none() {
                            continue;
                        }
                        let mut n = 0;
                        for &e in &ne[ne_off[k] as usize..ne_off[k + 1] as usize] {
                            if n > SELECTIVE {
                                break;
                            }
                            if (lo..hi).contains(&(e as usize)) {
                                ents.push(e);
                                n += 1;
                            }
                            if kind[e as usize] & 3 == KIND_DIR
                                && let Some(d) = idx.dir_of(e)
                            {
                                let (a, b) = (idx.dir_start()[d as usize].max(lo as u32), idx.dir_end()[d as usize].min(hi as u32));
                                if a < b {
                                    trees.push((a, b));
                                    n += (b - a) as usize;
                                }
                            }
                        }
                        if seen.fetch_add(n, Relaxed) + n > SELECTIVE {
                            quit.store(true, Relaxed);
                            early.fetch_or(c < chunks / 16, Relaxed);
                        }
                    }
                }
                if c >= chunks / 16 && seen.load(Relaxed) * chunks / (c + 1) > 2 * SELECTIVE {
                    quit.store(true, Relaxed);
                }
                (ents, trees)
            },
        );
        let (mut ents, mut trees): (Vec<u32>, Vec<(u32, u32)>) = (Vec::new(), Vec::new());
        for (e, t) in found {
            ents.extend(e);
            trees.extend(t);
        }
        if over() {
            return Err(early.load(Relaxed));
        }
        trees.sort_unstable();
        let mut merged: Vec<(u32, u32)> = Vec::new();
        for (a, b) in trees {
            match merged.last_mut() {
                Some(l) if a <= l.1 => l.1 = l.1.max(b),
                _ => merged.push((a, b)),
            }
        }
        // In those subtrees, entries whose name has the token are in `ents`.
        let first = ents.len();
        if first + merged.iter().map(|&(a, b)| (b - a) as usize).sum::<usize>() > SELECTIVE {
            return Err(false);
        }
        ents.extend(merged.iter().flat_map(|&(a, b)| a..b));
        if ents.len() <= INLINE_VISITS {
            // Few (one thread's worth): names and folders scored as they
            // come up.
            let axes = q.axes();
            let scan = Scan {
                q,
                live: self.live,
                names: &NameTable::new(Vec::new(), Vec::new(), Vec::new(), 0, false),
                npos: pos.len(),
                need_dirs: true,
                all: all_mask(pos.len(), &axes),
                axes,
                now: now_secs(),
            };
            return Ok(top_k(ents.len(), q.limit, ents.len(), RankKey::default(), |r, top| {
                let (mut pbuf, mut memo) = (Vec::new(), Map::<u32, DirMemo>::default());
                let mut hits = Map::<u32, Option<NameHit>>::default();
                let re = q.name_re.clone();
                let mut hit = |k: u32| *hits.get_or_insert_with(k, || name_hit(idx, q, pos, neg, re.as_ref(), k));
                for j in r {
                    let i = ents[j] as usize;
                    let Some(nh) = hit(en[i]).filter(|h| h.flags & NF_OK != 0 && (j < first || h.bits >> t & 1 == 0)) else { continue };
                    let m = scan.memo_of(parent[i], &mut memo, &mut hit);
                    // The axes are the entry's own question -- its kind, its
                    // extension, the folder it is under -- and the scan that
                    // walks the table asks it per entry. The driven scan
                    // asked it too: it does not walk the table, and without
                    // the question a folder's extension satisfied the
                    // extension axis of the files under it.
                    if !scan.axes.plain && !scan.axes_ok(i, &nh, &m) {
                        continue;
                    }
                    if let Some(key) = scan.score(i, nh, m, top.floor, &mut pbuf, q.path_re.as_ref()) {
                        top.push(key);
                    }
                }
            }));
        }
        // Many: score the names of those entries and of the folders above
        // them once, in one table, rather than per thread as they come up.
        let (de, dp) = (idx.dir_entry(), idx.dir_parent());
        let (mut bits, mut seen_dir) = (vec![0u64; idx.words], vec![0u64; idx.d.div_ceil(64)]);
        for &e in &ents {
            let k = en[e as usize];
            bits[k as usize >> 6] |= 1 << (k & 63);
            let mut d = parent[e as usize];
            while d != 0 && seen_dir[d as usize >> 6] >> (d & 63) & 1 == 0 {
                seen_dir[d as usize >> 6] |= 1 << (d & 63);
                let k = en[de[d as usize] as usize];
                bits[k as usize >> 6] |= 1 << (k & 63);
                d = dp[d as usize];
            }
        }
        let names = self.score_names(q, pos, neg, None, Some(&NameSet { busy: true, ..NameSet::new(bits) }));
        let axes = q.axes();
        let scan =
            Scan { q, live: self.live, names: &names, npos: pos.len(), need_dirs: true, all: all_mask(pos.len(), &axes), axes, now: now_secs() };
        Ok(top_k(ents.len(), q.limit, ents.len(), RankKey::default(), |r, top| {
            let (mut pbuf, mut memo) = (Vec::new(), Map::<u32, DirMemo>::default());
            let path_re = q.path_re.clone();
            for j in r {
                let i = ents[j] as usize;
                let Some(nh) = names.get(en[i]).filter(|h| h.flags & NF_OK != 0 && (j < first || h.bits >> t & 1 == 0)) else { continue };
                let m = scan.memo_of(parent[i], &mut memo, &mut |k| names.get(k));
                // The axes are the entry's own question, and the driven scan
                // asks it per entry too (see the other branch).
                if !scan.axes.plain && !scan.axes_ok(i, &nh, &m) {
                    continue;
                }
                if let Some(key) = scan.score(i, nh, m, top.floor, &mut pbuf, path_re.as_ref()) {
                    top.push(key);
                }
            }
        }))
    }

    /// The name table for this query: cached for a repeat of the last one
    /// (the second, longer page of results), narrowed from the last one when
    /// this query only extends it (typing), else scored from scratch.
    fn names(&self, q: &Query, pos: &[&Token], neg: &[&Token], lo: usize, hi: usize) -> alloc::sync::Arc<Scored> {
        // A small `in:` scope scores just the names found in it.
        let scope = (q.scope.is_some() && hi - lo <= self.live.base.u / 4).then_some((lo, hi));
        let key = NameKey::of(q, scope);
        let prev = self.live.names_cache.last.lock().unwrap().clone();
        if let Some(p) = &prev
            && p.key == key
        {
            return p.clone();
        }
        let from = prev.as_ref().filter(|p| key.narrows(&p.key)).map(|p| &p.names);
        let within = scope.filter(|_| from.is_none()).map(|(lo, hi)| self.scope_names(lo, hi));
        let scored = alloc::sync::Arc::new(Scored {
            at: now_millis(),
            key,
            names: self.score_names(q, pos, neg, from, within.as_deref()),
            memo: crate::sync::OnceCell::new(),
        });
        *self.live.names_cache.last.lock().unwrap() = Some(scored.clone());
        scored
    }

    /// Bitset of the names of entries `lo..hi` (a folder's subtree) and of
    /// the folder and its ancestors, whose names folder tokens match. The
    /// last one is kept: searches in a folder tend to come in a row.
    fn scope_names(&self, lo: usize, hi: usize) -> alloc::sync::Arc<NameSet> {
        let mut cache = self.live.names_cache.scope.lock().unwrap();
        if let Some((r, set)) = &*cache
            && *r == (lo, hi)
        {
            return set.clone();
        }
        let idx = &self.live.base;
        let (en, de, dp) = (idx.ent_name(), idx.dir_entry(), idx.dir_parent());
        let mut bits = vec![0u64; idx.words];
        let mut set = |k: u32| bits[k as usize >> 6] |= 1 << (k & 63);
        for &k in &en[lo..hi] {
            set(k);
        }
        let mut d = if lo < hi { idx.parent()[lo] } else { 0 };
        while d != 0 {
            set(en[de[d as usize] as usize]);
            d = dp[d as usize];
        }
        let set = alloc::sync::Arc::new(NameSet::new(bits));
        *cache = Some(((lo, hi), set.clone()));
        set
    }

    /// Score distinct names against the query's name-only predicates: every
    /// name, or only the ones `from` matched and `within` holds.
    fn score_names(&self, q: &Query, pos: &[&Token], neg: &[&Token], from: Option<&NameTable>, within: Option<&NameSet>) -> NameTable {
        let idx = &self.live.base;
        // A `path:` anchored at the end says how a match's own name ends.
        let tails = q.path_re.as_ref().and_then(path_suffixes);
        let any_non_attr = pos.iter().chain(neg).any(|t| t.axis != Axis::Attribute);
        if !any_non_attr && q.exts.is_empty() && q.name_re.is_none() && tails.is_none() {
            // Every name passes with score 0; only its flags differ.
            return NameTable::new(Vec::new(), Vec::new(), Vec::new(), idx.n, true);
        }
        let ne_off = idx.name_ents_off();
        // One token, no negation and an `ext:`: no folder memo reads the
        // table, so only names with an allowed extension matter.
        let by_ext = pos.len() == 1 && neg.is_empty() && !q.exts.is_empty();
        let toks: Vec<TokenBits> = pos.iter().chain(neg).map(|t| TokenBits::new(idx, t)).collect();
        // The name-axis and extension-axis tokens, and the folder-axis and
        // negated tokens: the candidates are the names that could match one
        // of them.
        //
        // They are LISTS, not bitmasks. The parser caps the positive tokens
        // at eight, but it does not cap the negated ones -- and a bitmask
        // of them overflows at the thirty-third: a panic in a debug build,
        // and a wrong bit in a release one.
        let pos_name_ext_idx: Vec<usize> =
            pos.iter().enumerate().filter(|(_, tok)| matches!(tok.axis, Axis::Name | Axis::Extension)).map(|(t, _)| t).collect();
        let folder_idx: Vec<usize> = pos.iter().enumerate().filter(|(_, tok)| tok.axis == Axis::Folder).map(|(t, _)| t).collect();
        let neg_check_idx: Vec<usize> = neg.iter().enumerate().filter(|(_, tok)| tok.axis != Axis::Attribute).map(|(t, _)| pos.len() + t).collect();
        // Whether the query asks anything of the name: the name-axis
        // tokens and the extension ones (the extension is part of the
        // name). A query with no such token asks nothing, and every name
        // is a candidate -- the scan decides.
        let name_asks = !pos_name_ext_idx.is_empty();
        // And whether it asks for a name: a name-axis token has to be answered
        // by the name itself.
        let name_pos = pos.iter().any(|t| t.axis == Axis::Name);
        let ext_mask: u8 = pos.iter().enumerate().fold(0, |acc, (t, tok)| if tok.axis == Axis::Extension { acc | (1 << t) } else { acc });
        // Whether every token is on the name axis: then the scoring is the
        // scoring it always was, and it costs the same.
        let plain = name_asks && pos.iter().all(|t| t.axis == Axis::Name) && neg.iter().all(|t| t.axis == Axis::Name);
        let space = char_bit(b' ').trailing_zeros() as usize;
        let space = [idx.bitmap(BM_FIRST + space), idx.bitmap(BM_SECOND + space)];
        // Name filters as bitmap prefilters: a name passing `ext:` contains
        // ".ext"; one passing `re:` contains a literal every match starts with.
        let filter = |lits: Vec<Vec<u8>>| lits.into_iter().map(Token::literal).collect::<Vec<_>>();
        let re_toks = q.name_re.as_ref().and_then(re_literals).map(filter);
        let re_bits = re_toks.as_ref().map(|t| t.iter().map(|t| TokenBits::new(idx, t)).collect::<Vec<_>>());
        // `ext:` as the extension slots it allows (see `Index::name_ext`):
        // only a name whose extension is not in the table needs reading.
        let ext_slots = idx.name_ext();
        let mut allow = [0u64; 4];
        for e in &q.exts {
            let s = idx.ext_slot(e) as usize;
            allow[s / 64] |= 1 << (s % 64);
        }
        let ext_ok_k = |k: usize, name: &[u8]| {
            let s = ext_slots[k] as usize;
            allow[s / 64] >> (s % 64) & 1 != 0 && (s != 255 || ext_ok(name, &q.exts))
        };
        // The names of word w whose slot the filter allows.
        let ext_word = |w: usize| -> u64 {
            if q.exts.is_empty() {
                return !0;
            }
            let slots = &ext_slots[w * 64..(w * 64 + 64).min(idx.u)];
            slots.iter().enumerate().fold(0, |m, (i, &s)| m | (allow[s as usize / 64] >> (s % 64) & 1) << i)
        };
        let (dot, app) = (idx.bitmap(crate::name_index::BM_DOT), idx.bitmap(crate::name_index::BM_APP));
        // Name k is bit i of lane j of `per` (each token's
        // `TokenBits::words`). `re` is the task's own copy of `name_re`:
        // sharing one regex's cache pool across threads costs more than the
        // match.
        let score_one = |k: usize,
                         i: u32,
                         per: &[Pair],
                         j: usize,
                         spaced: bool,
                         tok_fits: bool,
                         re: Option<&crate::regex::Regex>,
                         ok_entries: &mut usize|
         -> Option<NameHit> {
            let (w, bit) = (k / 64, |x: u64| x >> i & 1 != 0);
            let flags = (if bit(dot[w]) { NF_DOT } else { 0 }) | (if bit(app[w]) { NF_APP } else { 0 });
            let mut h = NameHit { score: 0, bits: 0, flags, best: [0; 4] };
            if !tok_fits && (q.exts.is_empty() || ext_slots[k] != 255) && tails.is_none() && re.is_none() {
                if !name_asks && (q.exts.is_empty() || allow[ext_slots[k] as usize / 64] >> (ext_slots[k] % 64) & 1 != 0) {
                    h.flags |= NF_OK;
                    *ok_entries += (ne_off[k + 1] - ne_off[k]) as usize;
                    return Some(h);
                }
                return None;
            }
            // The name is folded: a query typed in NFC has to match a
            // name the filesystem stored in NFD.
            let (name, wide) = idx.uname_folded(k as u32);
            // Whether the name answered one of the name-axis tokens.
            //
            // A query that asks for a name is answered by a name: the
            // name has to carry one of the name-axis tokens, even when
            // a folder above carries the others. The overlay says the
            // same (see `match_path`), and the two paths have to agree
            // on it -- a query that names a file is not a query that
            // names a folder with that file under it.
            let mut got_name = false;
            if plain {
                // Every token is on the name axis: this is the scoring it
                // always was, and it costs the same. The axis is read per
                // token, and a query that does not use the axes pays
                // nothing for the privilege -- and the name table is
                // built once per search, over every name in the index.
                for (t, tok) in pos.iter().enumerate() {
                    let (fits, clean, typo) = per[t].0[j];
                    if bit(fits)
                        && let Some(s) = token_score(&name, wide, tok.known_mask(bit(clean), bit(typo), spaced), tok)
                    {
                        h.bits |= 1 << t;
                        // Every positive token is a name token here.
                        got_name = true;
                        let s16 = s.clamp(i16::MIN as i32, i16::MAX as i32) as i16;
                        h.score = h.score.saturating_add(s16);
                        if t < 4 {
                            h.best[t] = s16.max(0);
                        }
                    }
                }
            } else {
                for (t, tok) in pos.iter().enumerate() {
                    let (fits, clean, typo) = per[t].0[j];
                    if !bit(fits) {
                        continue;
                    }
                    // The token says which part of the path it matches on
                    //.
                    let scored = match tok.axis {
                        Axis::Name => token_score(&name, wide, tok.known_mask(bit(clean), bit(typo), spaced), tok),
                        // The extension is part of the name, so the name's own
                        // hit carries it and the scan asks the name for it.
                        Axis::Extension => {
                            let mask = tok.known_mask(bit(clean), bit(typo), spaced);
                            // The name's own prefilter is a superset of the
                            // extension's, so it never skips work that would
                            // have succeeded.
                            ext_of(&name).and_then(|e| token_score(e, None, mask, tok))
                        }
                        // A folder token is matched by a folder of the path,
                        // and the memo carries it from the folder's name: the
                        // name's own hit carries the bit so the memo can pick
                        // it up, and the score is the folder's.
                        Axis::Folder => token_score(&name, wide, tok.known_mask(bit(clean), bit(typo), spaced), tok),
                        // An attribute is the entry's kind byte: nothing the
                        // name can say.
                        Axis::Attribute => None,
                    };
                    if let Some(s) = scored {
                        h.bits |= 1 << t;
                        got_name |= tok.axis == Axis::Name;
                        let s16 = s.clamp(i16::MIN as i32, i16::MAX as i32) as i16;
                        // A folder token's score is the folder's, and the scan
                        // adds it: counting it here would count it twice.
                        if tok.axis != Axis::Folder {
                            h.score = h.score.saturating_add(s16);
                        }
                        if t < 4 {
                            h.best[t] = s16.max(0);
                        }
                    }
                }
            }
            // A negated extension token is about the extension; every
            // other negated token is about the name. A negated FOLDER
            // token is about the path's folders: it poisons the memo
            // of the directory whose name it matches, and it does not
            // reject the entry whose own name it matches.
            let mut neg_name = false;
            let mut neg_dir = false;
            for (t, p) in neg.iter().zip(&per[pos.len()..]) {
                if !bit(p.0[j].0) {
                    continue;
                }
                match t.axis {
                    Axis::Name => {
                        let hit = token_matches(&name, t);
                        neg_name |= hit;
                        neg_dir |= hit;
                    }
                    Axis::Extension => neg_name |= ext_of(&name).is_some_and(|e| token_matches(e, t)),
                    Axis::Folder => neg_dir |= token_matches(&name, t),
                    Axis::Attribute => {}
                }
            }
            if neg_name {
                h.flags |= NF_NEG;
            }
            if neg_dir {
                h.flags |= NF_NEG_DIR;
            }
            // As a file match it must hit a token and pass name filters;
            // as a folder on someone's path, the raw token bits matter.
            //
            // A query whose tokens are all on other axes -- an
            // attribute, a folder -- asks nothing of the name, so every
            // name is a candidate and the scan decides.
            //
            // And a query that asks for a name is answered by a name:
            // the name has to carry one of the name-axis tokens, even
            // when a folder above carries the others (see `name_hit`).
            let ok = (pos.is_empty() || !name_asks || h.bits != 0)
                && (!name_pos || got_name)
                && h.bits & ext_mask == ext_mask
                && h.flags & NF_NEG == 0
                && (q.exts.is_empty() || ext_ok_k(k, &name))
                && tails.as_ref().is_none_or(|t| t.iter().any(|t| ends_with_fold(&name, t)))
                && re.is_none_or(|re| re.is_match_bytes(&name));
            if ok {
                h.flags |= NF_OK;
                *ok_entries += (ne_off[k + 1] - ne_off[k]) as usize;
            }
            (ok || h.bits != 0 || h.flags & (NF_NEG | NF_NEG_DIR) != 0).then_some(h)
        };
        // One chunk of name ids: its words of the bitset and rank, and its
        // hits in id order (into a spare buffer). Words with candidates go
        // in pairs where they can (see `TokenBits::words`). `per` is the
        // task's scratch.
        let chunk = |c: usize, bits: &mut [u64], rank: &mut [u16], mut hits: Vec<NameHit>, per: &mut [Pair]| {
            let mut ok = 0;
            let (lo, n) = (c * CHUNK_WORDS, bits.len());
            // The chunk's words with candidates, as a bitset.
            let mut todo: Todo = core::array::from_fn(|g| 1u64.checked_shl(n.saturating_sub(g * 64) as u32).map_or(!0, |b| b - 1));
            if let Some(f) = from {
                let words = if f.hits[c].is_empty() { Todo::default() } else { nonzero(&f.bits[lo..lo + n]) };
                todo = core::array::from_fn(|g| todo[g] & words[g]);
            }
            if let Some(s) = within {
                todo = core::array::from_fn(|g| todo[g] & s.todo[c][g]);
            }
            if todo == Todo::default() {
                return (hits, ok);
            }
            let re = q.name_re.clone();
            let cand_of = |w: usize| from.map_or(!0, |f| f.bits[w]) & within.map_or(!0, |s| s.bits[w]);
            let has = |todo: &[u64], wi: usize| wi < n && todo[wi / 64] >> (wi % 64) & 1 != 0;
            for g in 0..todo.len() {
                while todo[g] != 0 {
                    let wi = g * 64 + todo[g].trailing_zeros() as usize;
                    todo[g] &= todo[g] - 1;
                    let pair = has(&todo, wi + 1);
                    if pair {
                        todo[(wi + 1) / 64] &= !(1 << ((wi + 1) % 64));
                    }
                    let w = lo + wi;
                    for (p, tb) in per.iter_mut().zip(&toks) {
                        p.0 = if pair { tb.words::<2>(w) } else { [tb.word(w), (0, 0, 0)] };
                    }
                    for j in 0..1 + pair as usize {
                        // A word's rank counts the chunk's hits before it
                        // (read only for words with a hit).
                        let (wi, w) = (wi + j, w + j);
                        rank[wi] = hits.len() as u16;
                        let mut tok_any = 0;
                        for &i in &pos_name_ext_idx {
                            tok_any |= per[i].0[j].0;
                        }
                        for &i in &folder_idx {
                            tok_any |= per[i].0[j].0;
                        }
                        for &i in &neg_check_idx {
                            tok_any |= per[i].0[j].0;
                        }
                        let mut any = tok_any;
                        // With no name-axis or extension-axis token every
                        // name is a candidate, unless a name filter says
                        // which can pass.
                        if !name_asks {
                            let re = re_bits.as_ref().map_or(!0, |f| f.iter().fold(0, |a, t| a | t.word(w).0));
                            any |= !0 >> (64 - (idx.u - w * 64).min(64)) & ext_word(w) & re;
                        } else if by_ext && any != 0 {
                            any &= ext_word(w);
                        }
                        let mut cand = cand_of(w) & any;
                        while cand != 0 {
                            let i = cand.trailing_zeros();
                            cand &= cand - 1;
                            let spaced = (space[0][w] | space[1][w]) >> i & 1 != 0;
                            let tok_fits = tok_any >> i & 1 != 0;
                            if let Some(h) = score_one(w * 64 + i as usize, i, per, j, spaced, tok_fits, re.as_ref(), &mut ok) {
                                hits.push(h);
                                bits[wi] |= 1 << i;
                            }
                        }
                    }
                }
            }
            (hits, ok)
        };
        // A spare pair from the last table (its bits cleared; ranks are only
        // read where a bit is set), or fresh ones.
        let spare_bits = BITS_POOL.lock().unwrap().pop().filter(|(b, _)| b.len() == idx.words);
        let (mut bits, mut rank) = spare_bits.unwrap_or_else(|| (vec![0; idx.words], vec![0; idx.words]));
        // A few thousand words to look at (a small index, a scope, a
        // narrowed table) take less time than waking threads.
        let few = from.map_or(within.map_or(idx.words, |s| if s.busy { s.names } else { s.words }), |f| f.len) <= INLINE_WORDS;
        let mut spare = core::mem::take(&mut *HIT_POOL.lock().unwrap());
        spare.resize_with(idx.words.div_ceil(CHUNK_WORDS), Vec::new);
        let scratch = || vec![Line([(0, 0, 0); 2]); toks.len()];
        let chunks = bits.chunks_mut(CHUNK_WORDS).zip(rank.chunks_mut(CHUNK_WORDS)).zip(spare);
        let out: Vec<(Vec<NameHit>, usize)> = if few {
            let mut per = scratch();
            chunks.enumerate().map(|(c, ((b, r), h))| chunk(c, b, r, h, &mut per)).collect()
        } else {
            // The chunks are handed to the workers by value: each one owns
            // its piece of the bitmaps, and the pool takes them off the list
            // one at a time.
            rgy_par::each_owned(chunks.collect::<Vec<_>>(), scratch, |per, c, ((b, r), h)| chunk(c, b, r, h, per))
        };
        let ok_entries = out.iter().map(|c| c.1).sum();
        let hits: Vec<Vec<NameHit>> = out.into_iter().map(|c| c.0).collect();
        NameTable::new(bits, rank, hits, ok_entries, false)
    }

    /// The overlay (entries added since the last compaction) has no dir
    /// memo: each candidate's path components stand in for it. Its top
    /// `limit` is all the merge can use.
    fn search_overlay(&self, q: &Query) -> Vec<Hit> {
        let now = now_secs();
        let pos: Vec<&Token> = q.tokens.iter().filter(|t| !t.negate && t.axis == Axis::Name).collect();
        // A hit with name-axis tokens needs some positive name token in its own name.
        let flat = &self.live.flat;
        let cands: Vec<&(Vec<u8>, crate::live::OEnt)> = flat
            .masks
            .iter()
            .zip(&flat.items)
            .filter(|&(&m, _)| m != 0 && (pos.is_empty() || pos.iter().any(|t| t.fits(m))))
            .map(|(_, item)| item)
            .collect();
        if cands.is_empty() {
            return Vec::new();
        }
        // Overlay entries cluster in a few busy folders: match each folder's
        // components once per folder, not per entry.
        let score = |items: &[&(Vec<u8>, crate::live::OEnt)]| {
            let mut memo = Map::<&[u8], DirMatch>::default();
            let mut out = Vec::new();
            for &&(ref path, o) in items {
                let cut = path.iter().rposition(|&b| b == b'/').unwrap_or(0);
                let dir = &path[..cut];
                if let Some(score) = q.match_path_with(path, o.kind, o.size, o.mtime, |_| *memo.get_or_insert_with(dir, || q.dir_match(dir))) {
                    let prior = o.prior as i32;
                    let score = score + prior + rank_tweaks(name_flags(&path[cut + 1..]), o.kind, o.mtime, now);
                    // The overlay has no entry in the index, so its key has
                    // no entry to tie-break on: it loses the tie to a base
                    // entry, the way the merge has always ordered them.
                    out.push(Hit {
                        key: rank_key(Tier::from_prior(prior), score, RankKey::NO_ENTRY),
                        idx: u32::MAX,
                        mtime: o.mtime,
                        over: Some(path.clone()),
                    });
                }
            }
            out
        };
        let mut hits = if cands.len() <= OVERLAY_CHUNK {
            score(&cands)
        } else {
            let n = cands.len().div_ceil(OVERLAY_CHUNK);
            par_each(
                n,
                || (),
                |_, ci| {
                    let start = ci * OVERLAY_CHUNK;
                    let end = (start + OVERLAY_CHUNK).min(cands.len());
                    score(&cands[start..end])
                },
            )
            .into_iter()
            .flatten()
            .collect()
        };
        // Best first, equal keys by path (the overlay map's order).
        hits.sort_by(|a, b| b.key.cmp(&a.key).then_with(|| a.over.cmp(&b.over)));
        hits.truncate(q.limit);
        hits
    }

    /// For each dir: which tokens its name or an ancestor's matches, with the
    /// best score per token (first 4); bits == u32::MAX if a negated token does.
    fn dir_tokens(&self, names: &NameTable) -> Vec<DirMemo> {
        let idx = &self.live.base;
        let de = idx.dir_entry();
        let en = idx.ent_name();
        // Each dir's own name, written in parallel into a spare buffer.
        let mut out = MEMO_POOL.lock().unwrap().pop().unwrap_or_default();
        rgy_par::map_collect_into(idx.d, 1 << 12, |k| if k > 0 { DirMemo::own(names.get(en[de[k] as usize])) } else { DirMemo::default() }, &mut out);
        // Fold ancestors in, parents first. A dir's descendants are one
        // contiguous id range, so the children of huge dirs go one by one,
        // then every small subtree in parallel.
        let dp = idx.dir_parent();
        let plan = idx.memo_plan();
        for &k in &plan.upper {
            out[k as usize] = out[k as usize].under(out[dp[k as usize] as usize]);
        }
        let roots: Vec<(u32, DirMemo)> = plan.chunks.iter().map(|(c, _)| (*c, out[*c as usize])).collect();
        let mut slices = Vec::with_capacity(plan.chunks.len());
        let mut rest: &mut [DirMemo] = &mut out;
        let mut at = 0usize;
        for (_, r) in &plan.chunks {
            let (_, tail) = core::mem::take(&mut rest).split_at_mut(r.start as usize - at);
            let (mine, tail) = tail.split_at_mut((r.end - r.start) as usize);
            slices.push(mine);
            rest = tail;
            at = r.end as usize;
        }
        let work = slices.into_iter().zip(&plan.chunks).zip(roots).collect::<Vec<_>>();
        rgy_par::for_each_owned(work, |_, ((slice, (_, r)), (c, root))| {
            let a = r.start as usize;
            for k in a..r.end as usize {
                let p = dp[k];
                let pm = if p == c { root } else { slice[p as usize - a] };
                slice[k - a] = slice[k - a].under(pm);
            }
        });
        out
    }
}

/// Debug switch: always take the full pass (for checking the selective one).
pub static FULL_PASS: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

/// One query's entry scoring, shared by the two strategies.
struct Scan<'a> {
    q: &'a Query,
    live: &'a Live,
    names: &'a NameTable,
    npos: usize,
    need_dirs: bool,
    /// The axes the tokens match on.
    axes: Axes<'a>,
    /// The bits the name table's bitmap and the memo can carry.
    ///
    /// Every positive token but the attribute ones: an attribute is the
    /// entry's kind byte, and no bit can carry it.
    all: u32,
    now: u32,
}

impl Scan<'_> {
    /// Whether the entry answers the axes the query asks about.
    ///
    /// The attribute axis is the entry's kind byte, and the byte is read
    /// anyway. The extension axis is the name's own hit. The folder axis
    /// is a folder of the path: a name that matches the folder token is
    /// not enough -- the entry has to be under a folder with that name,
    /// and the memo is what says so.
    ///
    /// It is a question about the entry, asked once per candidate: the
    /// scan is inlined into the loop, and a query that does not use the
    /// axes does not pay for them.
    #[inline(always)]
    fn axes_ok(&self, i: usize, nh: &NameHit, memo: &DirMemo) -> bool {
        if !self.axes.attrs_ok(self.live.base.kind()[i]) {
            return false;
        }
        // The extension axis: the name's own hit carries it.
        if nh.bits as u32 & self.axes.ext != self.axes.ext {
            return false;
        }
        // The folder axis: a folder of the path, and not the name.
        memo.bits & self.axes.folder == self.axes.folder
    }

    /// Score entry `i` whose name scored `nh`, given its parent's memo.
    /// `None` if it fails a filter or cannot beat `floor`.
    #[inline(always)]
    /// `re` is the caller's own copy of `path_re` (see `score_names`).
    fn score(&self, i: usize, nh: NameHit, memo: DirMemo, floor: RankKey, pbuf: &mut Vec<u8>, re: Option<&crate::regex::Regex>) -> Option<RankKey> {
        let idx = &self.live.base;
        let q = self.q;
        let k = idx.kind()[i];
        if !q.kind_ok(k) {
            return None;
        }
        // `type:link` is orthogonal to kind (same rule as match_path_with).
        if q.link && k & 3 != KIND_LINK {
            return None;
        }
        if q.size_set {
            if k & 3 == KIND_DIR {
                return None;
            }
            let sz = crate::name_index::dec_size(idx.size_raw()[i]);
            if sz < q.size.0 || sz > q.size.1 {
                return None;
            }
        }
        let mtime = idx.mtime()[i];
        if q.mtime != (0, u32::MAX) && (mtime < q.mtime.0 || mtime > q.mtime.1) {
            return None;
        }
        let mut score = nh.score as i32;
        if self.need_dirs {
            let all = self.all;
            // Who may answer which token.
            //
            // The name is the entry's own name: it answers the name-axis
            // tokens and the extension-axis ones (the extension is part of
            // the name), and it does not answer the folder axis -- a
            // folder's own name is not a folder of its own path.
            //
            // The memo is a folder of the path, by its own name. It answers
            // the folder-axis tokens -- a folder of the path is what a
            // folder token asks for -- and the name-axis ones too: a bare
            // word has always meant the file's name *or* a folder above it,
            // and the overlay says the same, so the two paths have to agree
            // on it. It does not answer the extension axis: the extension is
            // the name's own, so a folder's extension cannot satisfy the
            // extension of the file under it.
            let covered = (nh.bits as u32 & all & !self.axes.folder) | (memo.bits & (self.axes.folder | self.axes.name));
            if memo.bits == u32::MAX || covered & all != all {
                return None;
            }
            for t in 0..self.npos {
                let bit = 1u32 << t;
                if (self.axes.folder & bit != 0) || (self.axes.name & bit != 0 && (nh.bits as u32) & bit == 0) {
                    // Matched by a folder on the path instead.
                    score += memo.best.get(t).map_or(6, |&b| b as i32 * 3 / 4);
                }
            }
        }
        if self.live.is_dead(i as u32) {
            return None;
        }
        // The prior the entry inherits from its directory travels with the
        // entry (see `Index::rank_adj`), so the hot loop reads one byte
        // instead of the entry's parent and then that parent's prior.
        //
        // The prior is also what says which *tier* the entry is in, and the
        // tier is in the high bits of the key: a priority that is only a
        // term added to the score loses to a better name, and the corpus
        // (`tests/ranking.rs`) says the priority is what the user meant.
        // The tier rides in the kind byte, which is read anyway.
        let prior = idx.rank_adj()[i] as i32;
        score += prior + rank_tweaks(nh.flags, k, mtime, self.now);
        let key = entry_key(k, score, i as u32);
        if key <= floor {
            return None;
        }
        if q.path_re.is_some() || !q.globs.is_empty() {
            // Whole-path predicates share one path build (match_path_with
            // applies the same `re:` + glob rules; this entry path used to
            // skip the globs entirely, so `*walk.rs` returned every name).
            idx.path(i, pbuf);
            // The path is folded for the comparison: the path that is
            // displayed and opened is the original one (the fold rule, rule 2).
            let folded = fold::fold(pbuf);
            let path = &folded[..];
            if let Some(re) = re
                && !re.is_match_bytes(path)
            {
                return None;
            }
            for (neg, pat) in &q.globs {
                if *neg == glob_match(pat, path) {
                    return None;
                }
            }
        }
        Some(key)
    }

    /// One sequential pass over every entry in `lo..hi`.
    fn full(&self, lo: usize, hi: usize, memo: Option<&[DirMemo]>) -> Vec<Hit> {
        let idx = &self.live.base;
        let (ent_name, parent, zones) = (idx.ent_name(), idx.parent(), idx.zones());
        let dense = self.names.dense(idx);
        let q = self.q;
        // Runs of entries none of which passes the size/mtime/kind filters
        // are skipped whole.
        let kinds = self.axes.kinds
            & if q.link { 1 << KIND_LINK } else { ALL_KINDS }
            & if q.size_set { !(1 << KIND_DIR) } else { ALL_KINDS }
            & match q.kind {
                Some(k) if q.apps => (1 << k) | (1 << KIND_LINK),
                Some(k) => 1 << k,
                None => ALL_KINDS,
            };
        let filtered = kinds != ALL_KINDS || q.size != (0, u64::MAX) || q.mtime != (0, u32::MAX);
        let zone_ok = |z: &Zone| {
            (z.kinds as u8) & kinds != 0
                && crate::name_index::dec_size(z.max_size) >= q.size.0
                && z.max_mtime >= q.mtime.0
                && z.min_mtime <= q.mtime.1
        };
        top_k(hi - lo, q.limit, hi - lo, RankKey::default(), |r, top| {
            let (mut pbuf, re) = (Vec::new(), q.path_re.clone());
            let (mut a, b) = (lo + r.start, lo + r.end);
            while a < b {
                let end = ((a / ZONE + 1) * ZONE).min(b);
                if filtered && !zone_ok(&zones[a / ZONE]) {
                    a = end;
                    continue;
                }
                for i in a..end {
                    // `dense[ent_name[i]]` is entry -> name -> Short: two
                    // dependent loads per entry in this sequential pass.
                    // Item 3 measured killing this indirection and it loses
                    // both ways (benchmark/baseline-names.md, "item 3"): the
                    // per-entry `Short` view -- `dense[ent_name[i]]` composed
                    // once per query -- ran the 5M rows 2.8-5.9x SLOWER,
                    // because the gather is hidden under `score()`'s work in
                    // this stream while a separate composition pass pays each
                    // gather at full latency; it also wants ~20 MB more
                    // resident at 5M (a `Short` is 4 bytes). Walking names
                    // instead removes the gather but replaces it with
                    // name -> entries list walks. The indirection stays: the
                    // 5M rows are bound by match volume, not by this load.
                    let s = dense[ent_name[i] as usize];
                    if s.flags & NF_OK == 0 {
                        continue;
                    }
                    let nh = NameHit { score: s.score, bits: s.bits, flags: s.flags, best: [0; 4] };
                    let m = memo.map_or(DirMemo::default(), |m| m[parent[i] as usize]);
                    if !self.axes.plain && !self.axes_ok(i, &nh, &m) {
                        continue;
                    }
                    if let Some(k) = self.score(i, nh, m, top.floor, &mut pbuf, re.as_ref()) {
                        top.push(k);
                    }
                }
                a = end;
            }
        })
    }

    /// Visit only the entries carrying a matching name; folder tokens are
    /// checked by walking each candidate's ancestors (memoized per piece).
    fn selective(&self, lo: usize, hi: usize) -> Vec<Hit> {
        let idx = &self.live.base;
        let (ne_off, ne, parent) = (idx.name_ents_off(), idx.name_ents(), idx.parent());
        let ok: Vec<(u32, NameHit)> = self.names.iter().filter(|(_, h)| h.flags & NF_OK != 0).collect();
        top_k(ok.len(), self.q.limit, self.names.ok_entries, RankKey::default(), |r, top| {
            let (mut pbuf, mut memo) = (Vec::new(), Map::<u32, DirMemo>::default());
            let re = self.q.path_re.clone();
            for &(id, nh) in &ok[r] {
                for &e in &ne[ne_off[id as usize] as usize..ne_off[id as usize + 1] as usize] {
                    let i = e as usize;
                    if i < lo || i >= hi {
                        continue;
                    }
                    let m = if self.need_dirs { self.memo_of(parent[i], &mut memo, &mut |k| self.names.get(k)) } else { DirMemo::default() };
                    if !self.axes.plain && !self.axes_ok(i, &nh, &m) {
                        continue;
                    }
                    if let Some(k) = self.score(i, nh, m, top.floor, &mut pbuf, re.as_ref()) {
                        top.push(k);
                    }
                }
            }
        })
    }

    /// The dir memo of `d` (see `dir_tokens`), from its ancestor chain.
    fn memo_of(&self, d: u32, cache: &mut Map<u32, DirMemo>, hit: &mut impl FnMut(u32) -> Option<NameHit>) -> DirMemo {
        let idx = &self.live.base;
        let (de, en, dp) = (idx.dir_entry(), idx.ent_name(), idx.dir_parent());
        let mut chain = Vec::new();
        let mut k = d;
        let mut acc = loop {
            if k == 0 {
                break DirMemo::default();
            }
            if let Some(&m) = cache.get(&k) {
                break m;
            }
            chain.push(k);
            k = dp[k as usize];
        };
        for &k in chain.iter().rev() {
            acc = DirMemo::own(hit(en[de[k as usize] as usize])).under(acc);
            cache.insert(k, acc);
        }
        acc
    }
}

/// The dir memo is ~13 MB; reusing it saves a page-fault storm per query.
static MEMO_POOL: crate::sync::Mutex<Vec<Vec<DirMemo>>> = crate::sync::Mutex::new(Vec::new());

/// What the name table depends on: two queries with the same key score
/// every name the same.
#[derive(PartialEq)]
pub struct NameKey {
    /// The tokens, with the axis each one matches on.
    ///
    /// The axis is part of it: the same word on another axis scores the
    /// name differently -- `ext:^txt` is the extension, `txt` is the name
    /// -- and a table built for one is not the table for the other.
    tokens: Vec<(Vec<u8>, Mode, bool, Axis)>,
    exts: Vec<Vec<u8>>,
    name_re: Option<String>,
    /// The entry range whose names alone were scored, if not all.
    scope: Option<(usize, usize)>,
}

impl NameKey {
    /// The key of `q`, with the scope whose names alone were scored.
    pub fn of(q: &Query, scope: Option<(usize, usize)>) -> NameKey {
        NameKey {
            tokens: q.tokens.iter().map(|t| (t.text.clone(), t.mode, t.negate, t.axis)).collect(),
            exts: q.exts.clone(),
            name_re: q.name_re.as_ref().map(|r| r.pattern().to_string()),
            scope,
        }
    }

    /// Can only match names `prev` matched: same filters, same tokens, each
    /// positive token the same or longer in a way that only narrows it.
    pub fn narrows(&self, prev: &NameKey) -> bool {
        self.exts == prev.exts
            && self.name_re == prev.name_re
            && self.scope == prev.scope
            && self.tokens.len() == prev.tokens.len()
            && self.tokens.iter().any(|t| !t.2)
            && self.tokens.iter().zip(&prev.tokens).all(|(a, b)| {
                a.1 == b.1
                    && a.2 == b.2
                    // The same word on another axis is another question.
                    && a.3 == b.3
                    && if a.2 {
                        a.0 == b.0
                    } else if a.1 == Mode::Suffix {
                        a.0.ends_with(&b.0)
                    } else {
                        // Gaining a typo widens the match: score afresh.
                        a.0.starts_with(&b.0) && takes_typos(&a.0, a.1) == takes_typos(&b.0, b.1)
                    }
            })
    }
}

/// The last query's name table (and dir memo, built on first use).
pub struct Scored {
    /// The wall-clock millisecond the table was scored at.
    #[cfg_attr(not(feature = "std"), allow(dead_code))]
    at: u64,
    key: NameKey,
    names: NameTable,
    memo: crate::sync::OnceCell<Vec<DirMemo>>,
}

impl Drop for Scored {
    fn drop(&mut self) {
        if let Some(v) = self.memo.take() {
            let mut pool = MEMO_POOL.lock().unwrap();
            if pool.is_empty() {
                pool.push(v);
            }
        }
    }
}

/// Lives with the index it was scored against (`Live`).
#[derive(Default)]
pub struct NameCache {
    last: crate::sync::Mutex<Option<alloc::sync::Arc<Scored>>>,
    /// The last `in:` scope's names (see `Searcher::scope_names`).
    #[allow(clippy::type_complexity)]
    scope: crate::sync::Mutex<Option<((usize, usize), alloc::sync::Arc<NameSet>)>>,
}

impl NameCache {
    /// Drop the cached table and spare buffers once searching has stopped:
    /// tens of MB after a broad query, worth keeping only while typing.
    ///
    /// The std build: the wall clock says whether it has stopped.
    #[cfg(feature = "std")]
    pub fn trim_if_idle(&self, idle: core::time::Duration) {
        let mut g = self.last.lock().unwrap();
        // `>=`, not `>`: "idle for at least this long" is what a zero
        // threshold asks for, and a millisecond clock cannot promise more
        // precision than that.
        if g.as_ref().is_some_and(|s| now_millis().saturating_sub(s.at) >= idle.as_millis() as u64) {
            *g = None;
            drop(g);
            *self.scope.lock().unwrap() = None;
            trim_pools();
        }
    }

    /// Without a clock there is no "idle" to decide, so the cache stays.
    ///
    /// The `no_std` path: nothing here can tell how long ago the table was
    /// scored, and a table that is never dropped is a bounded cost (one
    /// table, one memo) rather than a leak.
    #[cfg(not(feature = "std"))]
    pub fn trim_if_idle(&self, _idle: core::time::Duration) {}
}

#[derive(Clone, Copy, Default)]
pub struct DirMemo {
    bits: u32,
    best: [i16; 4],
}

impl DirMemo {
    /// A dir's own name's contribution.
    #[inline]
    fn own(hit: Option<NameHit>) -> DirMemo {
        match hit {
            // A negated name or folder token that matches this dir's name
            // rejects what is under it.
            Some(h) if h.flags & NF_NEG_DIR != 0 => DirMemo { bits: u32::MAX, best: [0; 4] },
            Some(h) => DirMemo { bits: h.bits as u32, best: h.best },
            None => DirMemo::default(),
        }
    }

    /// This dir's memo with its parent's folded in.
    #[inline]
    fn under(self, p: DirMemo) -> DirMemo {
        if p.bits == u32::MAX || self.bits == u32::MAX {
            return DirMemo { bits: u32::MAX, best: self.best };
        }
        let mut best = self.best;
        for (b, &q) in best.iter_mut().zip(&p.best) {
            *b = (*b).max(q);
        }
        DirMemo { bits: self.bits | p.bits, best }
    }
}

/// Spare hit buffers, one per name table chunk (tens of MB after a broad
/// query), so the next broad query doesn't page-fault fresh ones in.
static HIT_POOL: crate::sync::Mutex<Vec<Vec<NameHit>>> = crate::sync::Mutex::new(Vec::new());

/// Free the spare buffers searches keep for speed (after a quiet spell).
pub fn trim_pools() {
    BITS_POOL.lock().unwrap().clear();
    HIT_POOL.lock().unwrap().clear();
    DENSE_POOL.lock().unwrap().clear();
    MEMO_POOL.lock().unwrap().clear();
}

/// A spare name table bitset and rank (344 KB on this disk): allocating
/// and freeing them each search cost page faults and madvise.
#[allow(clippy::type_complexity)]
static BITS_POOL: crate::sync::Mutex<Vec<(Vec<u64>, Vec<u16>)>> = crate::sync::Mutex::new(Vec::new());

/// Spare `NameTable::dense` views (8.9 MB on this disk).
static DENSE_POOL: crate::sync::Mutex<Vec<Vec<Short>>> = crate::sync::Mutex::new(Vec::new());

impl Drop for NameTable {
    fn drop(&mut self) {
        // Only the chunks holding hits have bits to clear.
        if !self.bits.is_empty() {
            for (chunk, hits) in self.bits.chunks_mut(CHUNK_WORDS).zip(&self.hits) {
                if !hits.is_empty() {
                    chunk.fill(0);
                }
            }
            let mut pool = BITS_POOL.lock().unwrap();
            if pool.is_empty() {
                pool.push((core::mem::take(&mut self.bits), core::mem::take(&mut self.rank)));
            }
        }
        if let Some(d) = self.dense.take() {
            let mut pool = DENSE_POOL.lock().unwrap();
            if pool.is_empty() {
                pool.push(d);
            }
        }
        let mut pool = HIT_POOL.lock().unwrap();
        if pool.is_empty() {
            *pool = core::mem::take(&mut self.hits);
            pool.iter_mut().for_each(Vec::clear);
        }
    }
}

/// Scored names: which ones (`bits`), and their hits stored per chunk of
/// CHUNK_WORDS words in id order, `rank[w]` counting the chunk's hits
/// before word w. Lookups are O(1) and the table is as small as its hits.
struct NameTable {
    bits: Vec<u64>,
    rank: Vec<u16>,
    hits: Vec<Vec<NameHit>>,
    len: usize,
    /// Entries carrying a name that passes as a match (NF_OK).
    ok_entries: usize,
    /// Every name, unscored (no token or name filter): only `dense` works.
    all: bool,
    /// For a pass over every entry: each name's `Short` (zero if absent), a
    /// direct index with a quarter of a hit's bytes.
    dense: crate::sync::OnceCell<Vec<Short>>,
}

/// What an entry's score needs of its name's hit.
#[derive(Clone, Copy, Default)]
struct Short {
    score: i16,
    bits: u8,
    flags: u8,
}

/// `n` default `Short`s, from the allocator's zeroed memory.
fn zeroed(n: usize) -> Vec<Short> {
    let layout = alloc::alloc::Layout::array::<Short>(n).expect("dense view size");
    if layout.size() == 0 {
        return Vec::new();
    }
    // Safety: allocated with Short's layout for `n` of them; a Short is
    // plain integers, and all zero bytes is its default.
    unsafe {
        let p = alloc::alloc::alloc_zeroed(layout) as *mut Short;
        if p.is_null() {
            alloc::alloc::handle_alloc_error(layout);
        }
        Vec::from_raw_parts(p, n, n)
    }
}

impl NameTable {
    fn new(bits: Vec<u64>, rank: Vec<u16>, hits: Vec<Vec<NameHit>>, ok_entries: usize, all: bool) -> NameTable {
        let len = hits.iter().map(Vec::len).sum();
        NameTable { bits, rank, hits, len, ok_entries, all, dense: crate::sync::OnceCell::new() }
    }

    /// The `dense` view, built on first use.
    fn dense(&self, idx: &Index) -> &[Short] {
        self.dense.get_or_init(|| {
            let spare = DENSE_POOL.lock().unwrap().pop().filter(|d| d.len() == idx.words * 64);
            // A fresh one comes zeroed (all `Short::default()`) from the
            // allocator, which maps zero pages in without writing them.
            let fresh = spare.is_none();
            let mut d = spare.unwrap_or_else(|| zeroed(idx.words * 64));
            let flags = [idx.bitmap(crate::name_index::BM_DOT), idx.bitmap(crate::name_index::BM_APP)];
            rgy_par::for_each_owned(rgy_par::split_mut(&mut d, CHUNK_WORDS * 64), |c, out| {
                if !fresh {
                    out.fill(Short::default());
                }
                let base = c * CHUNK_WORDS * 64;
                if self.all {
                    for (j, o) in out.iter_mut().enumerate() {
                        let (w, i) = ((base + j) / 64, j % 64);
                        let f = |b: &[u64], nf: u8| if b[w] >> i & 1 != 0 { nf } else { 0 };
                        o.flags = NF_OK | f(flags[0], NF_DOT) | f(flags[1], NF_APP);
                    }
                    return;
                }
                let hits = self.hits[c].iter();
                let words = self.bits[c * CHUNK_WORDS..].iter().take(CHUNK_WORDS);
                let ids = words.enumerate().flat_map(|(wi, &b)| {
                    let mut b = b;
                    core::iter::from_fn(move || {
                        (b != 0).then(|| {
                            let j = wi * 64 + b.trailing_zeros() as usize;
                            b &= b - 1;
                            j
                        })
                    })
                });
                for (j, h) in ids.zip(hits) {
                    out[j] = Short { score: h.score, bits: h.bits, flags: h.flags };
                }
            });
            d
        })
    }

    #[inline(always)]
    fn get(&self, id: u32) -> Option<NameHit> {
        if self.all {
            return Some(NameHit { score: 0, bits: 0, flags: NF_OK, best: [0; 4] });
        }
        let (w, i) = (id as usize >> 6, id & 63);
        let b = self.bits[w];
        if b >> i & 1 == 0 {
            return None;
        }
        let r = self.rank[w] as usize + (b & ((1 << i) - 1)).count_ones() as usize;
        Some(self.hits[w / CHUNK_WORDS][r])
    }

    /// Every name in the table, ascending.
    fn iter(&self) -> impl Iterator<Item = (u32, NameHit)> + '_ {
        self.hits.iter().enumerate().filter(|(_, hits)| !hits.is_empty()).flat_map(move |(c, hits)| {
            let words = self.bits[c * CHUNK_WORDS..].iter().take(CHUNK_WORDS);
            let ids = words.enumerate().flat_map(move |(wi, &b)| {
                let mut b = b;
                let base = ((c * CHUNK_WORDS + wi) * 64) as u32;
                core::iter::from_fn(move || {
                    (b != 0).then(|| {
                        let id = base + b.trailing_zeros();
                        b &= b - 1;
                        id
                    })
                })
            });
            ids.zip(hits.iter().copied())
        })
    }
}

/// A set of name ids, and which words of each chunk are not empty.
struct NameSet {
    bits: Vec<u64>,
    todo: Vec<Todo>,
    /// Non-zero words, and names.
    words: usize,
    names: usize,
    /// Most of its names will need scoring (not just their words'
    /// bitmaps): its names, not its words, say whether threads pay.
    busy: bool,
}

impl NameSet {
    fn new(bits: Vec<u64>) -> NameSet {
        let todo = bits.chunks(CHUNK_WORDS).map(nonzero).collect();
        let words = bits.iter().filter(|&&b| b != 0).count();
        let names = bits.iter().map(|b| b.count_ones() as usize).sum();
        NameSet { bits, todo, words, names, busy: false }
    }
}

/// One bit per word of a chunk (CHUNK_WORDS of them).
type Todo = [u64; CHUNK_WORDS / 64];

/// Which of a chunk's words are not zero.
fn nonzero(words: &[u64]) -> Todo {
    let mut out = Todo::default();
    for (o, g) in out.iter_mut().zip(words.chunks(64)) {
        *o = g.iter().enumerate().fold(0, |m, (b, &x)| m | ((x != 0) as u64) << b);
    }
    out
}

#[derive(Clone, Copy)]
struct NameHit {
    score: i16,
    /// Which positive tokens the name matched.
    bits: u8,
    flags: u8,
    /// Per-token score, first 4 tokens (for the folder memo).
    best: [i16; 4],
}

const NF_OK: u8 = 1;
const NF_DOT: u8 = 2;
const NF_NEG: u8 = 8;
const NF_APP: u8 = 4;
/// A negated FOLDER token matched this name.
///
/// The folder axis is about the path's folders, so a negated folder token
/// rejects what is under a folder it matches -- and the memo is what
/// carries that. The entry's own name is not a folder of its path, so
/// this flag does not reject the entry: it poisons the memo of the
/// directory whose name it is.
const NF_NEG_DIR: u8 = 16;

fn name_flags(name: &[u8]) -> u8 {
    let mut f = 0;
    if name.first() == Some(&b'.') {
        f |= NF_DOT;
    }
    if name.ends_with(b".app") {
        f |= NF_APP;
    }
    f
}

/// Small per-entry nudges on top of match quality and the location prior.
#[inline]
fn rank_tweaks(flags: u8, kind: u8, mtime: u32, now: u32) -> i32 {
    let mut s = 0;
    if flags & NF_DOT != 0 {
        s -= 8;
    }
    if kind & FLAG_HIDDEN != 0 {
        s -= 8;
    }
    // Apps are dirs, or symlinks into the cryptex (/Applications/Safari.app).
    if matches!(kind & 3, KIND_DIR | KIND_LINK) && flags & NF_APP != 0 {
        s += 25;
    }
    let age = now.saturating_sub(mtime);
    s += match age {
        0..=86_400 => 10,
        86_401..=604_800 => 7,
        604_801..=2_592_000 => 4,
        2_592_001..=31_536_000 => 1,
        _ => 0,
    };
    s
}

#[cfg(test)]
mod sse2_wide_tests {
    use super::*;

    /// The wide scorer must agree with the scalar matcher exactly: same
    /// score when a match exists, same rejection when it does not. This is
    /// the gate that lets the index's 64-byte windows drive scoring on
    /// x86_64 (and NEON on aarch64) without a second ranker to maintain.
    #[test]
    fn masked_wide_scorer_matches_the_scalar_matcher() {
        let names: &[&[u8]] = &[
            b"main.rs",
            b"Report-2024.pdf",
            b"README",
            b"a",
            b"xyzzy",
            b"Test_Document_v2.txt",
            b"lowerUPPER123",
            b"..hidden",
            b"some_file-with.many.dots.rs",
            b"CAPS",
            b"abcdefghijklmnopqrstuvwxyz",
            b"The Quick Brown Fox",
            b"zzzz",
            b"aaa",
            b"ababab",
        ];
        let queries: &[&[u8]] = &[
            b"main", b"m", b"rep", b"r24", b"zz", b"aa", b"abab", b"qbf", b"tf", b"doc", b"xyzzy", b"rs", b"tqv", b"e", b"0", b"...", b"caps", b"z",
            b"qu", b"fox",
        ];
        for name in names {
            for q in queries {
                let sse = fuzzy_score(name, q);
                let scalar = fuzzy_scalar::<NoMatch>(name, q, 100, &mut NoMatch);
                assert_eq!(sse, scalar, "name={name:?} q={q:?}");
            }
        }
        // Longer-than-window names take the scalar path by construction;
        // pin the window boundary itself.
        let long = b"a".repeat(65);
        assert_eq!(fuzzy_score(&long, b"a"), fuzzy_scalar::<NoMatch>(&long, b"a", 100, &mut NoMatch));
    }
}
