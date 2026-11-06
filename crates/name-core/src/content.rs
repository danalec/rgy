// SPDX-License-Identifier: Apache-2.0

//! Content search over the live name index, confirmed on rgx-core's byte
//! engine.
//!
//! This module is the replacement for the persisted trigram index. It keeps
//! the same public surface the CLIs, the daemon and the adapter registry use,
//! but it holds no on-disk index of its own: the candidate paths come from a
//! name search over [`crate::live::Live`], and every candidate is confirmed
//! against the bytes on disk right now. The pack store in `rgx-core`'s
//! `tv_runtime` is the accelerator for large trees; this path is the
//! correctness oracle, and it is the only path when no pack has been built.
//!
//! There is a reason the index is gone rather than rewritten. A second index
//! means a second thing to keep in sync with the tree, a second on-disk
//! format, and a second answer to "is this file's content current?" — the
//! bytes on disk answer that question once, and this module asks them.
//!
//! The old `Segment` / `Docs` / `Content` lifecycle is kept as an inert
//! shell so the daemon's content thread and the CLI's index probe keep
//! compiling unchanged; it writes no files and rules nothing out. The
//! daemon's `spawn_content` loop therefore waits on the change bell instead
//! of building segments, and every content answer goes through
//! [`verify_paths_with`].

use alloc::boxed::Box;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

use crate::live::Live;
use crate::name_query::{GrepMode, Query, Searcher};
use crate::placeholder;
use rgx_core::regex::ByteRegex;

/// The largest file the content path will confirm. Four times this is the
/// cap on bytes read, the same pair the adapter reader uses.
pub const MAX_FILE: u64 = 1 << 20;

/// Why a candidate was left out of the answer.
pub const SKIP_UNOPENABLE: &str = "it could not be opened; it is locked, unreadable, or gone";
pub const SKIP_NOT_A_FILE: &str = "it is not a regular file; it is a folder, a pipe, or a device";
pub const SKIP_READ_FAILED: &str = "the read failed; the file could not be read to the end";

/// How many candidates one result is worth scanning.
const CANDIDATES_PER_FILE: usize = 16;
/// The hard ceiling on candidates, so ten results never name the whole tree.
const MAX_CANDIDATES: usize = 200_000;

/// The declaring keywords a `sym:` pattern looks for.
const DEFINES: &str = "fn|func|function|def|class|struct|enum|trait|interface|type|typealias|impl|let|const|var|val|static|module|mod|protocol|extension|macro_rules!|define|typedef|union|object|record|namespace|actor";

/// Escape every regex metacharacter in a literal.
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

/// Every match start in `hay`, leftmost-first, non-overlapping.
fn starts(re: &ByteRegex, hay: &[u8]) -> Vec<usize> {
    let mut out = Vec::new();
    let mut at = 0;
    while let Some(m) = re.search(hay, at) {
        out.push(m.start);
        at = if m.end > m.start { m.end } else { m.start + 1 };
    }
    out
}

/// Turn byte offsets into (1-based line number, trimmed line text) pairs.
///
/// One line is reported once however many times the pattern matches in it,
/// and `max_per_file` caps the per-file cost.
fn lines_at(g: &Grep, buf: &[u8], starts: impl Iterator<Item = usize>) -> Vec<(usize, String)> {
    let mut out = Vec::new();
    let mut last_line_start = usize::MAX;
    for start in starts {
        let line_no = 1 + rgx_core::memchr::memchr_iter(b'\n', &buf[..start]).count();
        let ls = rgx_core::memchr::memrchr(b'\n', &buf[..start]).map_or(0, |p| p + 1);
        if ls == last_line_start {
            continue;
        }
        last_line_start = ls;
        let le = rgx_core::memchr::memchr(b'\n', &buf[start..]).map_or(buf.len(), |p| start + p);
        let text = String::from_utf8_lossy(&buf[ls..le.min(ls + 400)]).trim_end().to_string();
        out.push((line_no, text));
        if out.len() >= g.max_per_file {
            break;
        }
    }
    out
}

/// A path is bytes. On unix the OS takes them as they are; on Windows the
/// lossy UTF-8 spelling is the one the console uses too (see
/// `rgy_engine::console::spell`).
fn path_of(bytes: &[u8]) -> Option<PathBuf> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        Some(PathBuf::from(std::ffi::OsStr::from_bytes(bytes)))
    }
    #[cfg(windows)]
    {
        Some(PathBuf::from(String::from_utf8_lossy(bytes).into_owned()))
    }
    #[cfg(not(any(unix, windows)))]
    {
        Some(PathBuf::from(std::str::from_utf8(bytes).ok()?))
    }
}

/// Open a regular file, refusing cloud placeholders and non-regular entries.
/// A placeholder is not refused with an error: reading it would download the
/// user's file, and a search must never do that without being asked.
pub fn open_regular(path: &[u8]) -> Option<std::fs::File> {
    if placeholder::is_placeholder_path(path) {
        return None;
    }
    let f = std::fs::File::open(path_of(path)?).ok()?;
    if !f.metadata().ok()?.is_file() {
        return None;
    }
    Some(f)
}

/// The compiled content pattern.
pub struct Grep {
    pub pattern: String,
    pub mode: GrepMode,
    pub max_per_file: usize,
    /// Stop reading candidates after this long (None = read them all).
    pub budget: Option<std::time::Duration>,
    re: Box<ByteRegex>,
}

impl Grep {
    /// Compile `pattern` on the tree's one regex engine.
    ///
    /// Smart case: a pattern with no upper-case letter is case-insensitive.
    /// A `sym:` pattern becomes a definition regex; unknown syntax that the
    /// byte engine refuses is refused here, with the pattern named, rather
    /// than quietly matching nothing.
    pub fn new(pattern: &str, mode: GrepMode) -> Result<Grep, String> {
        let smart_ci = !pattern.chars().any(char::is_uppercase);
        let src = match mode {
            GrepMode::Literal => re_escape(pattern),
            GrepMode::Regex => pattern.to_string(),
            GrepMode::Symbol => format!(r"(?-u:\b)(?:{DEFINES})(?:<[^>\n]*>)?[ \t*&]+(?:mut[ \t]+)?{}(?-u:\b)", re_escape(pattern)),
        };
        let case_insensitive = smart_ci && mode != GrepMode::Symbol;
        let re =
            ByteRegex::new_multiline(src.as_bytes(), case_insensitive, false).map(Box::new).map_err(|e| format!("the pattern `{pattern}`: {e}"))?;
        Ok(Grep { pattern: pattern.to_string(), mode, max_per_file: 5, budget: Some(std::time::Duration::from_millis(250)), re })
    }

    /// The matching lines of a buffer the caller already has — a plain
    /// file's bytes or an adapter's rendered text.
    pub fn lines_in(&self, buf: &[u8]) -> Vec<(usize, String)> {
        lines_at(self, buf, starts(&self.re, buf).into_iter())
    }
}

/// The answer to a content search.
#[derive(Default)]
pub struct GrepResult {
    pub files: Vec<FileMatches>,
    pub candidates: usize,
    pub read: usize,
    /// False if the time budget ran out before every candidate was read.
    pub complete: bool,
    pub skipped: Vec<Skipped>,
    pub error: Option<String>,
}

/// A candidate the search could not confirm, and why.
pub struct Skipped {
    pub path: Vec<u8>,
    pub reason: &'static str,
}

/// One file that matched, with its matching lines.
pub struct FileMatches {
    pub path: Vec<u8>,
    pub lines: Vec<(usize, String)>,
}

/// The bytes an adapter rendered for one path.
pub struct Rendered {
    pub path: Vec<u8>,
    pub bytes: Vec<u8>,
}

/// The adapter seam: a file an adapter claims is read through it, so a
/// PDF's words and an archive member's content are searched like any other
/// file's bytes. An empty vector means no adapter claims the file.
pub trait AdapterReader: Send + Sync {
    fn rendered(&self, path: &[u8]) -> Vec<Rendered>;
}

/// How much of a path a query rules out. Retained as a public type for the
/// API the plan used to expose; with no index there is no narrowing plan.
#[derive(Debug, Clone)]
pub enum TQ {
    All,
    Tri(u32),
    Pair(u32),
    And(Vec<TQ>),
    Or(Vec<TQ>),
}

/// The world a content search runs in.
pub struct Scope<'a> {
    pub root: &'a [u8],
    pub live: &'a Live,
    pub home: &'a [u8],
    pub index: Option<&'a Content>,
    pub adapter: Option<&'a dyn AdapterReader>,
}

/// Confirm `paths` against the bytes on disk.
///
/// Each candidate is opened, capped at `MAX_FILE * 4` bytes, and matched with
/// [`Grep::lines_in`]. An adapter that claims the path supplies its rendered
/// bytes instead. The `limit` caps files returned, and `budget` caps the
/// wall time; a search that stopped early says so with `complete = false`.
pub fn verify_paths_with(g: &Grep, paths: &[impl AsRef<[u8]> + Sync], limit: usize, adapter: Option<&dyn AdapterReader>) -> GrepResult {
    let mut r = GrepResult { candidates: paths.len(), complete: true, ..Default::default() };
    if limit == 0 {
        return r;
    }
    let began = std::time::Instant::now();
    for p in paths {
        if r.files.len() >= limit {
            break;
        }
        if let Some(b) = g.budget
            && began.elapsed() >= b
        {
            r.complete = false;
            break;
        }
        let path = p.as_ref();
        if placeholder::is_placeholder_path(path) {
            r.skipped.push(Skipped { path: path.to_vec(), reason: placeholder::PLACEHOLDER_REASON });
            continue;
        }

        let rendered = adapter.map(|a| a.rendered(path)).unwrap_or_default();
        let buf: Vec<u8> = if rendered.is_empty() {
            let Some(file) = open_regular(path) else {
                r.skipped.push(Skipped { path: path.to_vec(), reason: SKIP_UNOPENABLE });
                continue;
            };
            use std::io::Read;
            let mut bytes = Vec::new();
            match file.take(MAX_FILE * 4).read_to_end(&mut bytes) {
                Ok(_) => {}
                Err(_) => {
                    r.skipped.push(Skipped { path: path.to_vec(), reason: SKIP_READ_FAILED });
                    continue;
                }
            }
            bytes
        } else {
            rendered.iter().flat_map(|d| d.bytes.iter().copied()).collect()
        };
        r.read += 1;

        let lines = g.lines_in(&buf);
        if !lines.is_empty() {
            r.files.push(FileMatches { path: path.to_vec(), lines });
        }
    }
    r
}

/// [`verify_paths_with`] with no adapter.
pub fn verify_paths(g: &Grep, paths: &[impl AsRef<[u8]> + Sync], limit: usize) -> GrepResult {
    verify_paths_with(g, paths, limit, None)
}

/// [`verify_paths`] plus the skip notes a caller wants on stderr.
pub fn verify(g: &Grep, paths: &[impl AsRef<[u8]> + Sync], limit: usize) -> GrepResult {
    let r = verify_paths(g, paths, limit);
    let mut err = std::io::stderr().lock();
    for s in &r.skipped {
        use std::io::Write;
        let _ = err.write_all(format!("{}: {}\n", String::from_utf8_lossy(&s.path), s.reason).as_bytes());
    }
    r
}

/// Join an index-relative path onto the root the way the tree joins paths.
fn openable(root: &[u8], rel: &[u8]) -> Vec<u8> {
    let rel = rel.strip_prefix(b"/").unwrap_or(rel);
    let mut full: Vec<u8> = root.iter().map(|&b| if b == b'\\' { b'/' } else { b }).collect();
    if !full.is_empty() && !full.ends_with(b"/") && !rel.is_empty() {
        full.push(b'/');
    }
    full.extend_from_slice(rel);
    full
}

/// The candidate files a content query might match, newest first.
///
/// This is a name search with the filters still applied: `grep:` on its own
/// names every file, and `grep:foo ext:pdf` names only PDFs. The limit is
/// expanded because most candidates will not carry a match.
pub fn scan_paths(root: &[u8], live: &Live, mut q: Query) -> Vec<Vec<u8>> {
    q.kind = Some(rgy_discovery::walk::KIND_FILE);
    q.limit = q.limit.max(1).saturating_mul(CANDIDATES_PER_FILE).min(MAX_CANDIDATES);
    q.size = (q.size.0, q.size.1.min(MAX_FILE));
    let mut files: Vec<(Vec<u8>, u32)> = Searcher { live }
        .search(&q)
        .into_iter()
        .map(|h| match h.over {
            Some(path) => (path, h.mtime),
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

/// Grep `g` over the files `q` selects under `root`.
pub fn answer(g: &Grep, q: &Query, root: &[u8], live: &Live, home: &[u8], index: Option<&Content>, limit: usize) -> GrepResult {
    answer_with(g, q, Scope { root, live, home, index, adapter: None }, limit)
}

/// [`answer`] with the full scope, including an adapter reader.
pub fn answer_with(g: &Grep, q: &Query, scope: Scope<'_>, limit: usize) -> GrepResult {
    let _ = scope.index; // no persisted index: the direct path is the answer
    let paths = scan_paths(scope.root, scope.live, q.clone_for_scan());
    let mut r = verify_paths_with(g, &paths, limit, scope.adapter);
    if !r.complete {
        r.error = Some("the content search ran out of its time budget".to_string());
    }
    r
}

/// The plan the daemon asks for: which paths the index can vouch for, and
/// which must be read directly. With no index, nothing is covered and every
/// candidate is read — the union is still the exact answer.
pub fn plan(g: &Grep, q: &Query, root: &[u8], live: &Live, _home: &[u8], _index: Option<&Content>) -> (HashSet<Vec<u8>>, Vec<Vec<u8>>, GrepResult) {
    let _ = g;
    let bare = scan_paths(root, live, q.clone_for_scan());
    let r = GrepResult { candidates: bare.len(), complete: true, ..Default::default() };
    (HashSet::new(), bare, r)
}

/// The docs a directory (and its tree) would index. With no index there is
/// nothing to build, so the daemon's content thread waits on the tree
/// changing instead of walking it on a timer.
#[derive(Default)]
pub struct Docs {
    paths: Vec<Vec<u8>>,
}

impl Docs {
    pub fn len(&self) -> usize {
        self.paths.len()
    }
    pub fn is_empty(&self) -> bool {
        self.paths.is_empty()
    }
    pub fn batches(&self) -> Vec<std::ops::Range<usize>> {
        if self.paths.is_empty() { Vec::new() } else { core::iter::once(0..self.paths.len()).collect() }
    }
}

/// A persisted segment. The replacement keeps no segments; the type stays
/// so the daemon's lifecycle compiles unchanged.
#[derive(Clone, Default)]
pub struct Segment {
    pub id: u64,
    pub ndocs: usize,
    pub live_docs: usize,
}

impl Segment {
    pub fn path(&self, _d: u32) -> &[u8] {
        b""
    }
    pub fn is_dead(&self, _d: u32) -> bool {
        false
    }
}

/// The inert content index. `open` and `open_shared` never read or write a
/// file; `segs` is always empty, so the CLI's index probe declines it and
/// the daemon answers through the direct path.
pub struct Content {
    pub dir: PathBuf,
    pub segs: Vec<Segment>,
    next_id: u64,
    _seen: BTreeMap<Vec<u8>, u32>,
}

impl Content {
    pub fn open_shared(dir: PathBuf) -> Content {
        Content::open(dir)
    }
    pub fn open(dir: PathBuf) -> Content {
        Content { dir, segs: Vec::new(), next_id: 0, _seen: BTreeMap::new() }
    }
    pub fn docs(&self) -> usize {
        self.segs.iter().map(|s| s.live_docs).sum()
    }
    pub fn bytes(&self) -> usize {
        0
    }
    pub fn diff(&mut self, _wants: Vec<(Vec<u8>, bool, Docs)>) -> Docs {
        Docs::default()
    }
    pub fn changed_dirs(&self, _since: u32) -> Vec<Vec<u8>> {
        Vec::new()
    }
    pub fn alloc_id(&mut self) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }
    pub fn push(&mut self, seg: Segment) {
        self.segs.push(seg);
    }
    pub fn sync(&mut self, _live: &Live, _home: &[u8], _dirs: &[Vec<u8>], _trees: &[Vec<u8>], _root: &[u8]) -> usize {
        0
    }
    pub fn merge_plan(&self) -> Option<Vec<u64>> {
        None
    }
    pub fn segments(&self, ids: &[u64]) -> Vec<&Segment> {
        self.segs.iter().filter(|s| ids.contains(&s.id)).collect()
    }
    pub fn replace(&mut self, ids: &[u64], seg: Segment) {
        self.segs.retain(|s| !ids.contains(&s.id));
        self.segs.push(seg);
    }
    pub fn search(&self, _g: &Grep, _filt: &Query) -> GrepResult {
        GrepResult::default()
    }
    pub fn covers(&self, _root: &[u8], _path: &[u8], _home: &[u8], _q: &Query) -> bool {
        false
    }
}

/// No segment is ever built: the direct path is the only index.
pub fn build_segment(_dir: &Path, _id: u64, _docs: &Docs, _range: std::ops::Range<usize>) -> Option<Segment> {
    None
}
pub fn merge(_dir: &Path, _id: u64, _segs: &[&Segment]) -> Option<Segment> {
    None
}

/// A path the content path would have indexed. Retained for API parity; the
/// replacement indexes nothing.
pub fn eligible(path: &[u8], size: u64, _home: &[u8]) -> bool {
    size <= MAX_FILE && !placeholder::is_placeholder_path(path)
}
pub fn in_scope(path: &[u8], home: &[u8]) -> bool {
    home.is_empty() || path.starts_with(home)
}
pub fn wanted(_live: &Live, _home: &[u8], _dir: &[u8], _recursive: bool, _root: &[u8]) -> Docs {
    Docs::default()
}
pub fn wants(_live: &Live, _home: &[u8], _dirs: &[Vec<u8>], _trees: &[Vec<u8>], _root: &[u8]) -> Vec<(Vec<u8>, bool, Docs)> {
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TempDir {
        path: PathBuf,
    }
    impl TempDir {
        fn new() -> TempDir {
            let mut p = std::env::temp_dir();
            p.push(format!(
                "rgy-content-{}-{}",
                std::process::id(),
                std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
            ));
            std::fs::create_dir_all(&p).unwrap();
            TempDir { path: p }
        }
        fn file(&self, name: &str, bytes: &[u8]) -> Vec<u8> {
            let p = self.path.join(name);
            let b = p.to_string_lossy().into_owned().into_bytes();
            std::fs::write(&p, bytes).unwrap();
            b
        }
    }
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }

    #[test]
    fn a_literal_finds_the_lines_it_matches() {
        let g = Grep::new("needle", GrepMode::Literal).unwrap();
        let lines = g.lines_in(b"one\na needle here\ntwo\nneedle again\n");
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].0, 2);
        assert!(lines[0].1.contains("needle"));
    }

    #[test]
    fn smart_case_is_case_insensitive_without_capitals() {
        let g = Grep::new("needle", GrepMode::Literal).unwrap();
        assert_eq!(g.lines_in(b"NEEDLE").len(), 1);
        let g = Grep::new("Needle", GrepMode::Literal).unwrap();
        assert_eq!(g.lines_in(b"NEEDLE").len(), 0);
    }

    #[test]
    fn a_refused_pattern_is_refused_by_the_engine() {
        assert!(Grep::new(r"\p{L}+", GrepMode::Regex).is_err());
        assert!(Grep::new(r"foo(?=bar)", GrepMode::Regex).is_err());
    }

    #[test]
    fn a_symbol_pattern_matches_a_declaration() {
        let g = Grep::new("widget", GrepMode::Symbol).unwrap();
        assert_eq!(g.lines_in(b"fn widget() -> u32 { 1 }").len(), 1);
        assert_eq!(g.lines_in(b"let x = widget;").len(), 0);
    }

    #[test]
    fn verify_reads_real_files_and_skips_missing_ones() {
        let d = TempDir::new();
        let a = d.file("a.txt", b"alpha\nbeta\n");
        let b = d.file("b.txt", b"nothing here\n");
        let missing = d.path.join("gone.txt").to_string_lossy().into_owned().into_bytes();
        let g = Grep::new("alpha", GrepMode::Literal).unwrap();
        let r = verify_paths_with(&g, &[a, b, missing], 10, None);
        assert_eq!(r.files.len(), 1);
        assert_eq!(r.files[0].lines[0].0, 1);
        assert_eq!(r.skipped.len(), 1);
        assert_eq!(r.skipped[0].reason, SKIP_UNOPENABLE);
        assert!(r.complete);
    }

    #[test]
    fn the_limit_caps_the_answer() {
        let d = TempDir::new();
        let paths: Vec<Vec<u8>> = (0..4).map(|i| d.file(&format!("f{i}.txt"), b"hit\n")).collect();
        let g = Grep::new("hit", GrepMode::Literal).unwrap();
        let r = verify_paths_with(&g, &paths, 2, None);
        assert_eq!(r.files.len(), 2);
    }

    #[test]
    fn an_adapter_supplies_the_bytes() {
        struct One;
        impl AdapterReader for One {
            fn rendered(&self, path: &[u8]) -> Vec<Rendered> {
                vec![Rendered { path: path.to_vec(), bytes: b"rendered marker\n".to_vec() }]
            }
        }
        let d = TempDir::new();
        let p = d.file("a.bin", b"plain bytes\n");
        let g = Grep::new("rendered", GrepMode::Literal).unwrap();
        assert!(verify_paths_with(&g, core::slice::from_ref(&p), 10, None).files.is_empty());
        assert_eq!(verify_paths_with(&g, &[p], 10, Some(&One)).files.len(), 1);
    }

    #[test]
    fn no_segments_are_ever_built() {
        let d = TempDir::new();
        assert!(build_segment(&d.path, 1, &Docs::default(), 0..0).is_none());
        assert!(merge(&d.path, 2, &[]).is_none());
        let mut c = Content::open(d.path.join("content"));
        assert!(c.segs.is_empty());
        c.push(Segment { id: 1, ndocs: 0, live_docs: 0 });
        assert_eq!(c.docs(), 0);
    }
}
