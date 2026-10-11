//! In-memory path index built from MFT file references.
//!
//! A USN record identifies a file by reference number and names its parent, but
//! never spells out a full path. Rebuilding paths therefore needs two passes:
//! collect every `(frn, parent_frn, name)` edge first, then resolve edges into
//! full paths with memoised recursion and cycle detection.

// The USN change vocabulary lives in `discovery` (one type, one home);
// aliased as `ntfs` so the long-standing `ntfs::…` call sites read unchanged.
use crate::regex::Regex;
use discovery as ntfs;
use std::collections::HashMap;
use std::io::{Read, Write};

/// Magic header bytes for the Everything-Rs Index v2 on-disk format ("ERI2").
pub const ERI_MAGIC: [u8; 4] = *b"ERI2";
/// Format version for the ERI on-disk index format.
pub const ERI_VERSION: u32 = 2;

/// Magic header bytes for the single-root v1 format ("ERI1"). Still read so an
/// index that predates the root table keeps working — rescanning is minutes of
/// elevated I/O and the file is a real artifact worth keeping. Writes stay
/// ERI2; only `read_from` accepts both.
pub const ERI1_MAGIC: [u8; 4] = *b"ERI1";
/// Version carried by the legacy single-root stream.
pub const ERI1_VERSION: u32 = 1;

/// Root of the volume; file reference 5 is the NTFS root directory.
pub const ROOT_FRN: u64 = 5;

/// Combine a volume ordinal with a raw file reference into the key used
/// everywhere inside [`Index`].
///
/// File reference numbers are unique only within a volume: two volumes both have
/// root at 5, and the same FRN can name unrelated files on each. The high 16
/// bits carry the volume ordinal so the keys cannot collide. Nothing outside the
/// indexer needs to know the split — it passes these keys back to the volume as
/// raw FRNs via [`volume_frn`].
pub fn volume_key(ordinal: u16, frn: u64) -> u64 {
    ((ordinal as u64) << 48) | (frn & 0x0000_FFFF_FFFF_FFFF)
}

/// The raw file reference inside a [`volume_key`], for talking to the volume.
pub fn volume_frn(key: u64) -> u64 {
    key & 0x0000_FFFF_FFFF_FFFF
}

/// The volume ordinal folded into a [`volume_key`].
pub fn volume_ordinal(key: u64) -> u16 {
    (key >> 48) as u16
}

/// The key of one volume's root directory.
pub fn volume_root_key(ordinal: u16) -> u64 {
    volume_key(ordinal, ROOT_FRN)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeKind {
    File,
    Directory,
}

/// Everything the interactive search needs about one entry beyond its path.
///
/// Grouped into one accessor because the UI reads all three per candidate; three
/// separate hash lookups per entry over millions of candidates is the
/// difference between a keystroke and a visible stall.
#[derive(Debug, Clone, Copy)]
pub struct EntryMeta {
    pub is_dir: bool,
    pub size: u64,
    /// Raw `FILE_ATTRIBUTE_*` bits; `type:link` tests
    /// [`ntfs::FILE_ATTRIBUTE_REPARSE_POINT`] against these.
    pub attrs: u32,
}

/// One entry as the interactive search sees it.
#[derive(Debug, Clone, Copy)]
pub struct Entry<'a> {
    pub name: &'a str,
    pub path: &'a str,
    pub is_dir: bool,
    pub size: u64,
    pub attrs: u32,
}

/// Packed per-entry record; see [`SearchTable`].
#[derive(Clone, Copy)]
struct SearchRow {
    name_start: u32,
    name_len: u16,
    path_start: u32,
    path_len: u16,
    is_dir: bool,
    size: u64,
    attrs: u32,
}

/// Flat, path-ordered projection of the index for interactive search.
///
/// Interactive search touches every entry on every keystroke. Reading each one
/// back out of the six parallel hash maps behind [`Index`] costs more in random
/// access than the matching itself — measured at roughly 180 ns per entry before
/// a single character is compared — so the UI scans this instead: one sequential
/// pass over a packed row array, with every string in a single buffer.
///
/// Rows are ordered by path, so a row index doubles as the tie-break for equal
/// match scores and the default view is alphabetical without a sort per query.
// `len` is the only cardinality the UI needs; `is_empty` would be untested
// API surface on transported code (kept as-is on purpose).
#[allow(clippy::len_without_is_empty)]
pub struct SearchTable {
    text: String,
    rows: Vec<SearchRow>,
}

impl SearchTable {
    /// Build the scan table. One pass, plus a sort to establish path order.
    ///
    /// Text offsets are `u32`, which caps the buffer at 4 GiB — far beyond any
    /// real volume, and checked rather than silently truncated.
    pub fn build(index: &Index) -> Self {
        // Canonical paths plus aliases, so a file is findable by its 8.3 short
        // name or a hard-link name as well as its Win32 name. The root is the
        // synthetic seed and carries no searchable name.
        let mut entries: Vec<(u64, &str)> = index.iter_all_paths().filter(|(frn, _)| volume_frn(*frn) != ROOT_FRN).collect();
        entries.sort_unstable_by(|a, b| a.1.cmp(b.1));

        let capacity: usize = entries.iter().map(|(_, p)| p.len() + 24).sum();
        let mut text = String::with_capacity(capacity);
        let mut rows = Vec::with_capacity(entries.len());

        for (frn, path) in entries {
            let meta = index.meta_of(frn);

            // The name is the path's own last component: for an alias that is
            // the alias name, not the canonical one hanging off the same FRN.
            let name = path.rsplit('\\').next().unwrap_or("");

            let name_start = text.len();
            text.push_str(name);
            let name_len = text.len() - name_start;

            let path_start = text.len();
            text.push_str(path);
            let path_len = text.len() - path_start;

            let (name_start, name_len, path_start, path_len) = (
                u32::try_from(name_start).expect("search table text exceeds 4 GiB"),
                u16::try_from(name_len).expect("file name exceeds 64 KiB"),
                u32::try_from(path_start).expect("search table text exceeds 4 GiB"),
                u16::try_from(path_len).expect("path exceeds 64 KiB"),
            );

            rows.push(SearchRow { name_start, name_len, path_start, path_len, is_dir: meta.is_dir, size: meta.size, attrs: meta.attrs });
        }

        SearchTable { text, rows }
    }

    pub fn len(&self) -> usize {
        self.rows.len()
    }

    /// The entry at `row`, in path order.
    pub fn get(&self, row: usize) -> Entry<'_> {
        let r = self.rows[row];
        Entry {
            name: slice(&self.text, r.name_start, r.name_len),
            path: slice(&self.text, r.path_start, r.path_len),
            is_dir: r.is_dir,
            size: r.size,
            attrs: r.attrs,
        }
    }

    /// Bytes held by the table. Only the interactive scan benchmark reports it;
    /// nothing at runtime needs the figure.
    #[allow(dead_code)]
    pub fn bytes(&self) -> usize {
        self.text.len() + self.rows.len() * std::mem::size_of::<SearchRow>()
    }
}

fn slice(text: &str, start: u32, len: u16) -> &str {
    &text[start as usize..start as usize + len as usize]
}

/// One unresolved edge from a file record to its parent.
#[derive(Debug, Clone)]
struct Edge {
    parent_frn: u64,
    name: String,
    kind: NodeKind,
    size: u64,
    attributes: u32,
}

#[derive(Debug, Default)]
pub struct Index {
    /// Full path by file reference number.
    paths: HashMap<u64, String>,
    /// Parent reference by file reference number, for rename handling.
    parents: HashMap<u64, u64>,
    /// Names by file reference number.
    names: HashMap<u64, String>,
    /// Directory flag by file reference number.
    is_dir: HashMap<u64, bool>,
    sizes: HashMap<u64, u64>,
    attributes: HashMap<u64, u32>,
    /// Non-canonical names for a record: hard-link aliases and the DOS 8.3
    /// short name that sits alongside the Win32 name. The canonical path lives
    /// in `paths`; these are additional searchable paths for the same record.
    aliases: HashMap<u64, Vec<String>>,
}

impl Index {
    /// `root` is the volume's DOS-style root (`C:\`, `D:\`). It becomes the path
    /// of that volume's root FRN, so every resolved path inherits the right
    /// volume prefix. Single-volume case; see [`Index::add_root`] for more.
    pub fn new(root: &str) -> Self {
        let mut idx = Index::default();
        idx.add_root(0, root);
        idx
    }

    /// Seed one volume's root directory.
    ///
    /// Each volume needs its own root because FRNs are only unique within a
    /// volume: without a per-volume root the second volume's records would
    /// resolve against the first volume's path prefix.
    pub fn add_root(&mut self, ordinal: u16, root: &str) {
        let key = volume_root_key(ordinal);
        self.paths.insert(key, root.to_string());
        self.parents.insert(key, key);
        self.names.insert(key, String::new());
        self.is_dir.insert(key, true);
    }

    #[allow(dead_code)]
    pub fn len(&self) -> usize {
        self.paths.len()
    }

    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.paths.is_empty()
    }

    /// Indexed entries, excluding every volume's synthetic root.
    ///
    /// `len` counts the roots because they are real entries in `paths`; a
    /// count shown to a user should not. Subtracting 1 instead of counting
    /// leaves one root in the total on a multi-volume index — the same
    /// single-root assumption that used to truncate the ERI root count.
    pub fn entry_count(&self) -> usize {
        self.paths.keys().filter(|k| volume_frn(**k) != ROOT_FRN).count()
    }

    #[allow(dead_code)]
    pub fn path_of(&self, frn: u64) -> Option<&str> {
        self.paths.get(&frn).map(|s| s.as_str())
    }

    #[allow(dead_code)]
    pub fn contains(&self, frn: u64) -> bool {
        self.paths.contains_key(&frn)
    }

    pub fn directory_count(&self) -> usize {
        self.is_dir.values().filter(|d| **d).count()
    }

    pub fn total_bytes(&self) -> u64 {
        self.sizes.iter().filter(|(frn, _)| !self.is_dir.get(frn).copied().unwrap_or(false)).map(|(_, s)| *s).sum()
    }

    pub fn iter(&self) -> impl Iterator<Item = (u64, &str)> {
        self.paths.iter().map(|(k, v)| (*k, v.as_str()))
    }

    /// Every searchable path: the canonical one plus every hard-link / DOS alias.
    ///
    /// A record with three names yields three paths, which is what a user
    /// expects when they know the file by its 8.3 short name.
    pub fn iter_all_paths(&self) -> impl Iterator<Item = (u64, &str)> {
        let extras = self.aliases.iter().flat_map(|(frn, list)| list.iter().map(move |p| (*frn, p.as_str())));
        self.iter().chain(extras)
    }

    /// Add a non-canonical path for `frn`.
    ///
    /// The caller has already resolved `path`; the index only records it.
    pub fn add_alias(&mut self, frn: u64, path: String) {
        self.aliases.entry(frn).or_default().push(path);
    }

    /// Number of alias paths recorded across all records.
    pub fn alias_count(&self) -> usize {
        self.aliases.values().map(Vec::len).sum()
    }

    /// Kind and size for one entry. Absent entries report file kind and zero size,
    /// which no filter can distinguish from a real empty file.
    ///
    /// The name is deliberately not carried here: an entry can be reached by
    /// several paths (canonical, hard link, DOS short name) and the searchable
    /// name is the one on the path being scanned.
    pub fn meta_of(&self, frn: u64) -> EntryMeta {
        EntryMeta {
            is_dir: self.is_dir.get(&frn).copied().unwrap_or(false),
            size: self.sizes.get(&frn).copied().unwrap_or(0),
            attrs: self.attributes.get(&frn).copied().unwrap_or(0),
        }
    }

    /// Record a resolved file: insert, or update in place if already present.
    ///
    /// `frn` and `parent_frn` are keys, not raw file references: the caller has
    /// already folded the volume ordinal into them. Masking here would strip it.
    pub fn insert(&mut self, frn: u64, parent_frn: u64, name: &str, kind: NodeKind, size: u64, attrs: u32) {
        if volume_frn(frn) == ROOT_FRN {
            return;
        }
        self.parents.insert(frn, parent_frn);
        self.names.insert(frn, name.to_string());
        self.is_dir.insert(frn, kind == NodeKind::Directory);
        self.sizes.insert(frn, size);
        self.attributes.insert(frn, attrs);
        if let Some(parent_path) = self.paths.get(&parent_frn) {
            self.paths.insert(frn, join(parent_path, name));
        }
    }

    pub fn remove(&mut self, frn: u64) -> Option<String> {
        self.parents.remove(&frn);
        self.names.remove(&frn);
        self.is_dir.remove(&frn);
        self.sizes.remove(&frn);
        self.attributes.remove(&frn);
        self.paths.remove(&frn)
    }

    /// Re-point a file at a new parent (rename).
    #[allow(dead_code)]
    pub fn reparent(&mut self, frn: u64, new_parent_frn: u64, new_name: &str) {
        self.parents.insert(frn, new_parent_frn);
        self.names.insert(frn, new_name.to_string());
        if let Some(parent_path) = self.paths.get(&new_parent_frn) {
            self.paths.insert(frn, join(parent_path, new_name));
        }
    }

    /// Apply one path-relevant USN record.
    ///
    /// `ordinal` is the volume this record came from; USN records carry raw file
    /// references and must be folded into the same key space the scanner used.
    ///
    /// Returns true when the index changed. Duplicate-source and
    /// duplicate-target reasons are ignored because the original entry survives
    /// under its own reference and removing it would corrupt the index.
    pub fn apply_usn(&mut self, rec: &ntfs::UsnRecord, ordinal: u16) -> bool {
        use ntfs::{
            USN_REASON_FILE_CREATE, USN_REASON_FILE_DELETE, USN_REASON_RENAME_DUPLICATE_SOURCE, USN_REASON_RENAME_DUPLICATE_TARGET,
            USN_REASON_RENAME_NEW_NAME, USN_REASON_RENAME_OLD_NAME,
        };

        if rec.reason & USN_REASON_RENAME_DUPLICATE_SOURCE != 0 || rec.reason & USN_REASON_RENAME_DUPLICATE_TARGET != 0 {
            return false;
        }

        if rec.reason & USN_REASON_FILE_DELETE != 0 || rec.reason & USN_REASON_RENAME_OLD_NAME != 0 {
            return self.remove(volume_key(ordinal, rec.file_reference)).is_some();
        }

        let kind = if rec.file_attributes & 0x10 != 0 { NodeKind::Directory } else { NodeKind::File };

        if rec.reason & USN_REASON_FILE_CREATE != 0 || rec.reason & USN_REASON_RENAME_NEW_NAME != 0 {
            let frn = volume_key(ordinal, rec.file_reference);
            let parent_frn = volume_key(ordinal, rec.parent_reference);
            let size = self.sizes.get(&frn).copied().unwrap_or(0);
            self.insert(frn, parent_frn, &rec.name, kind, size, rec.file_attributes);
            return true;
        }

        false
    }

    pub fn root(&self) -> &str {
        self.paths.get(&ROOT_FRN).map(|s| s.as_str()).unwrap_or("C:\\")
    }

    /// Serialize the entire resolved index to a binary stream using the ERI2
    /// format: one root table (every volume's root string, keyed by ordinal)
    /// followed by the record stream. Volume 0 keys are bit-identical to raw
    /// FRNs, but a multi-volume index carries its roots as `(ordinal, string)`
    /// pairs so volumes 1..n survive the round trip.
    pub fn write_to<W: Write>(&self, w: &mut W) -> std::io::Result<()> {
        w.write_all(&ERI_MAGIC)?;
        w.write_all(&ERI_VERSION.to_le_bytes())?;

        // Roots first: every key whose masked FRN is the synthetic root. Sort
        // by ordinal so the byte stream is deterministic regardless of hash
        // map iteration order.
        let mut roots: Vec<(u16, &str)> =
            self.paths.iter().filter(|(k, _)| volume_frn(**k) == ROOT_FRN).map(|(k, v)| (volume_ordinal(*k), v.as_str())).collect();
        roots.sort_unstable();
        w.write_all(&(roots.len() as u16).to_le_bytes())?;
        for (ordinal, root) in &roots {
            w.write_all(&ordinal.to_le_bytes())?;
            let root_bytes = root.as_bytes();
            w.write_all(&(root_bytes.len() as u16).to_le_bytes())?;
            w.write_all(root_bytes)?;
        }

        // Exclude every volume's synthetic root from the record stream
        let count = self.paths.keys().filter(|k| volume_frn(**k) != ROOT_FRN).count() as u64;
        w.write_all(&count.to_le_bytes())?;

        for (&frn, path) in &self.paths {
            if volume_frn(frn) == ROOT_FRN {
                continue;
            }
            let parent_frn = self.parents.get(&frn).copied().unwrap_or(ROOT_FRN);
            let size = self.sizes.get(&frn).copied().unwrap_or(0);
            let attrs = self.attributes.get(&frn).copied().unwrap_or(0);
            let is_dir = if self.is_dir.get(&frn).copied().unwrap_or(false) { 1u8 } else { 0u8 };
            let name = self.names.get(&frn).map(|s| s.as_str()).unwrap_or("");

            w.write_all(&frn.to_le_bytes())?;
            w.write_all(&parent_frn.to_le_bytes())?;
            w.write_all(&size.to_le_bytes())?;
            w.write_all(&attrs.to_le_bytes())?;
            w.write_all(&[is_dir])?;

            let name_bytes = name.as_bytes();
            w.write_all(&(name_bytes.len() as u16).to_le_bytes())?;
            w.write_all(name_bytes)?;

            let path_bytes = path.as_bytes();
            w.write_all(&(path_bytes.len() as u16).to_le_bytes())?;
            w.write_all(path_bytes)?;
        }

        w.flush()
    }

    /// Deserialize a resolved index from a binary stream in ERI2 format.
    pub fn read_from<R: Read>(r: &mut R) -> std::io::Result<Self> {
        let mut magic = [0u8; 4];
        r.read_exact(&mut magic)?;
        let legacy = if magic == ERI_MAGIC {
            false
        } else if magic == ERI1_MAGIC {
            true
        } else {
            return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "invalid index magic: not an ERI file"));
        };

        let mut ver_buf = [0u8; 4];
        r.read_exact(&mut ver_buf)?;
        let version = u32::from_le_bytes(ver_buf);
        let expected = if legacy { ERI1_VERSION } else { ERI_VERSION };
        if version != expected {
            return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, format!("unsupported index version: {version}")));
        }

        let mut index = Index::default();
        if legacy {
            // ERI1 predates the root table: exactly one root, and it is
            // implicitly volume 0. Keys in such a file already carry no
            // ordinal, so seeding ordinal 0 keeps every `volume_key` match.
            let mut root_len_buf = [0u8; 2];
            r.read_exact(&mut root_len_buf)?;
            let root_len = u16::from_le_bytes(root_len_buf) as usize;
            let mut root_bytes = vec![0u8; root_len];
            r.read_exact(&mut root_bytes)?;
            let root = String::from_utf8(root_bytes).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
            index.add_root(0, &root);
        } else {
            // Root table: every volume seeds its own root, or the second volume's
            // records resolve against the first volume's prefix.
            let mut root_count_buf = [0u8; 2];
            r.read_exact(&mut root_count_buf)?;
            let root_count = u16::from_le_bytes(root_count_buf);
            if root_count == 0 {
                return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "index has no volume roots"));
            }

            for _ in 0..root_count {
                let mut ordinal_buf = [0u8; 2];
                r.read_exact(&mut ordinal_buf)?;
                let ordinal = u16::from_le_bytes(ordinal_buf);

                let mut root_len_buf = [0u8; 2];
                r.read_exact(&mut root_len_buf)?;
                let root_len = u16::from_le_bytes(root_len_buf) as usize;
                let mut root_bytes = vec![0u8; root_len];
                r.read_exact(&mut root_bytes)?;
                let root = String::from_utf8(root_bytes).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
                index.add_root(ordinal, &root);
            }
        }

        let mut count_buf = [0u8; 8];
        r.read_exact(&mut count_buf)?;
        let count = u64::from_le_bytes(count_buf) as usize;

        // `count` is attacker- or corruption-controlled: a 21-byte file claiming
        // a trillion records would make an unbounded `reserve` abort the process
        // on allocation failure. Reserve a bounded hint and let `read_exact`
        // below fail with UnexpectedEof on a truncated stream, which surfaces as
        // a normal error. Legitimate indexes still grow amortised past the hint.
        let reserve_hint = count.min(1 << 20);
        index.paths.reserve(reserve_hint);
        index.parents.reserve(reserve_hint);
        index.names.reserve(reserve_hint);
        index.is_dir.reserve(reserve_hint);
        index.sizes.reserve(reserve_hint);
        index.attributes.reserve(reserve_hint);

        let mut u64_buf = [0u8; 8];
        let mut u32_buf = [0u8; 4];
        let mut u16_buf = [0u8; 2];
        let mut u8_buf = [0u8; 1];

        for _ in 0..count {
            r.read_exact(&mut u64_buf)?;
            let frn = u64::from_le_bytes(u64_buf);

            r.read_exact(&mut u64_buf)?;
            let parent_frn = u64::from_le_bytes(u64_buf);

            r.read_exact(&mut u64_buf)?;
            let size = u64::from_le_bytes(u64_buf);

            r.read_exact(&mut u32_buf)?;
            let attrs = u32::from_le_bytes(u32_buf);

            r.read_exact(&mut u8_buf)?;
            let is_dir = u8_buf[0] != 0;

            r.read_exact(&mut u16_buf)?;
            let name_len = u16::from_le_bytes(u16_buf) as usize;
            let mut name_bytes = vec![0u8; name_len];
            r.read_exact(&mut name_bytes)?;
            let name = String::from_utf8(name_bytes).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;

            r.read_exact(&mut u16_buf)?;
            let path_len = u16::from_le_bytes(u16_buf) as usize;
            let mut path_bytes = vec![0u8; path_len];
            r.read_exact(&mut path_bytes)?;
            let path = String::from_utf8(path_bytes).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;

            index.paths.insert(frn, path);
            index.parents.insert(frn, parent_frn);
            index.names.insert(frn, name);
            index.is_dir.insert(frn, is_dir);
            index.sizes.insert(frn, size);
            index.attributes.insert(frn, attrs);
        }

        Ok(index)
    }

    /// Search the index against a structured query.
    ///
    /// Scans every searchable path — canonical and alias — so a file is found
    /// under its 8.3 short name as well as its Win32 name.
    pub fn search(&self, query: &OrQuery) -> Vec<(u64, &str)> {
        let mut results = Vec::new();
        for (frn, path) in self.iter_all_paths() {
            if volume_frn(frn) == ROOT_FRN {
                continue;
            }
            let is_dir = self.is_dir.get(&frn).copied().unwrap_or(false);
            let size = self.sizes.get(&frn).copied().unwrap_or(0);
            let attrs = self.attributes.get(&frn).copied().unwrap_or(0);
            let name = path.rsplit('\\').next().unwrap_or("");

            if query.matches(path, name, is_dir, size, attrs) {
                results.push((frn, path));
            }
        }
        results
    }
}

/// Query expression carrying parsed filters and search terms.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Query {
    /// Bare substring terms to match (ANDed).
    pub terms: Vec<String>,
    /// Negative terms (must not appear in path).
    pub not_terms: Vec<String>,
    /// Extension filters (e.g. "rs", "txt", "md").
    pub extensions: Vec<String>,
    /// Directory substring filters (e.g. "src", "windows").
    pub dirs: Vec<String>,
    /// Minimum file size in bytes (inclusive).
    pub min_size: Option<u64>,
    /// Maximum file size in bytes (inclusive).
    pub max_size: Option<u64>,
    /// Node kind filter (file or directory).
    pub kind: Option<NodeKind>,
    /// `type:link`: the entry must be a reparse point (junction, symlink,
    /// mount point). Orthogonal to `kind` — a junction is both a directory
    /// and a link, so `type:dir type:link` is meaningful.
    pub link: bool,
    /// `re:pattern` terms (ANDed), matched against the full path.
    pub regexes: Vec<Regex>,
    /// `!re:pattern` terms: the path must NOT match.
    pub not_regexes: Vec<Regex>,
    /// Patterns that failed to compile. Such a pattern is treated as one that
    /// matches nothing; the messages exist so the CLI can say so instead of
    /// leaving the user with silent zero results.
    pub regex_errors: Vec<String>,
    /// `type:` tokens that name no known kind. The query then matches nothing
    /// and says so — an unknown type silently widening the query to "every
    /// path on the volume" is the worst possible answer to a typo.
    pub type_errors: Vec<String>,
}

impl Query {
    /// No terms and no filters: matches every path. An `|`-alternative that
    /// parses to this is dropped rather than treated as "match nothing".
    pub fn is_empty(&self) -> bool {
        self.terms.is_empty()
            && self.not_terms.is_empty()
            && self.extensions.is_empty()
            && self.dirs.is_empty()
            && self.min_size.is_none()
            && self.max_size.is_none()
            && self.kind.is_none()
            && !self.link
            && self.regexes.is_empty()
            && self.not_regexes.is_empty()
            && self.type_errors.is_empty()
    }

    /// Parse a query string with filter syntax (e.g. `ext:rs dir:src size:>100kb main !test`).
    pub fn parse(input: &str) -> Self {
        let mut q = Query::default();
        for token in input.split_whitespace() {
            if token.is_empty() {
                continue;
            }
            if let Some(rest) = token.strip_prefix('!').or_else(|| token.strip_prefix('-')) {
                if let Some(pat) = re_pattern_of(rest) {
                    push_regex(&mut q.not_regexes, &mut q.regex_errors, pat);
                } else if !rest.is_empty() {
                    q.not_terms.push(rest.to_lowercase());
                }
            } else if let Some(pat) = re_pattern_of(token) {
                push_regex(&mut q.regexes, &mut q.regex_errors, pat);
            } else if let Some(ext) = token.to_lowercase().strip_prefix("ext:") {
                let clean = ext.trim_start_matches('.');
                if !clean.is_empty() {
                    q.extensions.push(clean.to_string());
                }
            } else if let Some(dir) = token.to_lowercase().strip_prefix("dir:") {
                if !dir.is_empty() {
                    q.dirs.push(dir.to_string());
                }
            } else if let Some(kind_str) = token.to_lowercase().strip_prefix("type:") {
                match kind_str {
                    "file" => q.kind = Some(NodeKind::File),
                    "dir" | "directory" => q.kind = Some(NodeKind::Directory),
                    "link" => q.link = true,
                    // Media kinds are extension sets, so they also imply
                    // "a file": a directory can never carry an image extension.
                    other => match media_extensions(other) {
                        Some(exts) => {
                            q.kind = Some(NodeKind::File);
                            q.extensions.extend(exts.iter().map(|e| (*e).to_string()));
                        }
                        None => q.type_errors.push(format!("unknown type: {other}")),
                    },
                }
            } else if let Some(size_str) = token.to_lowercase().strip_prefix("size:") {
                parse_size_filter(size_str, &mut q.min_size, &mut q.max_size);
            } else {
                q.terms.push(token.to_lowercase());
            }
        }
        q
    }

    /// Check if a given file entry matches all criteria in the query.
    pub fn matches(&self, path: &str, name: &str, is_dir: bool, size: u64, attrs: u32) -> bool {
        self.matches_filters(path, name, is_dir, size, attrs) && self.matches_terms(path)
    }

    /// Every part of the query except the *positive* bare terms: kind, link,
    /// size bounds, extension and directory filters, and negations.
    ///
    /// The interactive UI fuzzy-scores the positive terms instead of
    /// substring-matching them, because a scattered match like `rc` for
    /// `report.txt` is exactly what fuzzy ranking exists to surface and a
    /// substring test would reject. It therefore runs this and lets the fuzzy
    /// pass own those terms. Negations stay here — `!test` is a constraint, not
    /// a ranking signal.
    pub fn matches_filters(&self, path: &str, name: &str, is_dir: bool, size: u64, attrs: u32) -> bool {
        // An unknown `type:` is a typo, not a wildcard. Failing closed keeps a
        // misspelling from turning into "every path on the volume".
        if !self.type_errors.is_empty() {
            return false;
        }

        if let Some(kind) = self.kind {
            match kind {
                NodeKind::Directory if !is_dir => return false,
                NodeKind::File if is_dir => return false,
                _ => {}
            }
        }

        // `type:link` is orthogonal to kind: a junction is a directory *and* a
        // reparse point, so `type:dir type:link` must find it.
        if self.link && attrs & ntfs::FILE_ATTRIBUTE_REPARSE_POINT == 0 {
            return false;
        }

        // Size is a file property; a directory's entry size is meaningless, so a
        // directory satisfies **no** size bound rather than all of them. Letting
        // a directory past because "its size is meaningless" made `size:>1gb`
        // return every directory on the volume plus the handful of large files.
        if self.min_size.is_some() || self.max_size.is_some() {
            if is_dir {
                return false;
            }
            if let Some(min) = self.min_size
                && size < min
            {
                return false;
            }
            if let Some(max) = self.max_size
                && size > max
            {
                return false;
            }
        }

        if !self.extensions.is_empty() {
            if is_dir {
                return false;
            }
            let ext = name.rsplit_once('.').map(|(_, e)| e.to_lowercase());
            let matched_ext = match ext {
                Some(ref e) => self.extensions.iter().any(|target| target == e),
                None => false,
            };
            if !matched_ext {
                return false;
            }
        }

        if !self.dirs.is_empty() {
            for d in &self.dirs {
                if !contains_ignore_case(path, d) {
                    return false;
                }
            }
        }

        // A negation is an explicit constraint the user typed, not a ranking
        // hint: `!test` must remove matches no matter who scores the positives.
        for not_term in &self.not_terms {
            if term_matches(path, not_term) {
                return false;
            }
        }

        // Regexes are constraints for the same reason: `re:` is something the
        // user typed precisely, and the fuzzy pass knows nothing about it.
        for r in &self.regexes {
            if !r.is_match(path) {
                return false;
            }
        }
        for r in &self.not_regexes {
            if r.is_match(path) {
                return false;
            }
        }

        true
    }

    /// Positive bare terms only. No match means the query did not ask for this
    /// path; the interactive UI replaces this with fuzzy scoring.
    fn matches_terms(&self, path: &str) -> bool {
        for term in &self.terms {
            if !term_matches(path, term) {
                return false;
            }
        }
        true
    }
}

/// The regex pattern inside a `re:` / `regex:` token, or `None` when the token
/// is not one. The prefix is matched case-insensitively; the pattern itself is
/// taken verbatim so class ranges like `[A-Z]` keep their case.
fn re_pattern_of(token: &str) -> Option<&str> {
    if token.get(..6).is_some_and(|p| p.eq_ignore_ascii_case("regex:")) {
        return Some(&token[6..]);
    }
    if token.get(..3).is_some_and(|p| p.eq_ignore_ascii_case("re:")) {
        return Some(&token[3..]);
    }
    None
}

/// Compile `pat` into `target`, failing closed on error.
///
/// An uncompilable pattern becomes [`Regex::never`] (matches nothing) and its
/// message is recorded: a positive `re:` then rejects everything rather than
/// silently admitting everything, and the CLI can explain why.
fn push_regex(target: &mut Vec<Regex>, errors: &mut Vec<String>, pat: &str) {
    if pat.is_empty() {
        return;
    }
    match Regex::new(pat) {
        Ok(r) => target.push(r),
        Err(e) => {
            target.push(Regex::never());
            errors.push(format!("re:{pat}: {e}"));
        }
    }
}

/// True when a `re:` / `regex:` prefix begins at byte offset `i`, counting a
/// leading `!` or `-`. Used by [`split_alternatives`] so a `|` inside a regex
/// pattern is not mistaken for a query-level OR.
fn re_prefix_at(input: &str, i: usize) -> bool {
    let rest = &input[i..];
    let rest = rest.strip_prefix('!').or_else(|| rest.strip_prefix('-')).unwrap_or(rest);
    re_pattern_of(rest).is_some()
}

/// True when the text after a glued `|` re-enters query syntax: another
/// `re:`/`ext:`/`dir:`/`type:`/`size:` token, a `!`/`-` negation, or simply
/// whitespace / end of input (a trailing `|` is query-OR with an empty side).
/// Anything else is the regex engine's own alternation.
fn starts_query_syntax(s: &str) -> bool {
    if s.is_empty() || s.starts_with(char::is_whitespace) {
        return true;
    }
    let t = s.strip_prefix('!').or_else(|| s.strip_prefix('-')).unwrap_or(s);
    re_pattern_of(t).is_some() || ["ext:", "dir:", "type:", "size:"].iter().any(|p| t.get(..p.len()).is_some_and(|x| x.eq_ignore_ascii_case(p)))
}

/// Split a query string on `|`, but not inside a `re:` / `regex:` pattern.
///
/// The regex engine has its own `|` alternation, so `re:(a|b)` and `re:a|b`
/// stay one alternative. A `|` still splits when it separates query sides —
/// `re:foo | re:bar`, `re:a\.rs$|re:b\.rs$`, or `ext:rs|ext:txt` exactly as
/// before.
fn split_alternatives(input: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut start = 0usize;
    let mut at_token_start = true;
    let mut in_re = false;
    let mut depth = 0i32;
    let mut prev_escape = false;
    for (i, ch) in input.char_indices() {
        if ch.is_whitespace() {
            in_re = false;
            depth = 0;
            prev_escape = false;
            at_token_start = true;
            continue;
        }
        if ch == '|' && !prev_escape {
            let regex_internal = in_re && depth > 0;
            let glued_regex = in_re && depth == 0 && !starts_query_syntax(&input[i + 1..]);
            if !regex_internal && !glued_regex {
                parts.push(&input[start..i]);
                start = i + ch.len_utf8();
                at_token_start = true;
                in_re = false;
                depth = 0;
                prev_escape = false;
                continue;
            }
        }
        if at_token_start {
            at_token_start = false;
            in_re = re_prefix_at(input, i);
            depth = 0;
        }
        if in_re && !prev_escape {
            match ch {
                '\\' => prev_escape = true,
                '(' => depth += 1,
                ')' if depth > 0 => depth -= 1,
                _ => {}
            }
        } else {
            prev_escape = false;
        }
    }
    parts.push(&input[start..]);
    parts
}

/// True when `pattern` matches `text`, honouring `*` (any run) and `?` (one
/// character) against the whole string, case-insensitively.
///
/// A pattern with no wildcard matches the whole string too, which is why callers
/// route bare terms through `contains_ignore_case` instead and only reach here
/// when the term actually carries a wildcard.
fn wildcard_match(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern.chars().flat_map(|c| c.to_lowercase()).collect();
    let t: Vec<char> = text.chars().flat_map(|c| c.to_lowercase()).collect();

    let (mut pi, mut ti) = (0usize, 0usize);
    // Position of the most recent `*` and the text index it is currently
    // consuming. Backtracking to it is what keeps `a*a*a*…b` linear in practice
    // rather than exponential.
    let mut star: Option<(usize, usize)> = None;

    while ti < t.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == t[ti]) {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some((pi, ti));
            pi += 1;
        } else if let Some((sp, st)) = star {
            pi = sp + 1;
            ti = st + 1;
            star = Some((sp, ti));
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }
    pi == p.len()
}

/// A term match: wildcard semantics when the term carries `*`/`?`, substring
/// otherwise. Both are case-insensitive because query terms are lowercased at
/// parse time and paths are compared without allocating.
fn term_matches(path: &str, term: &str) -> bool {
    if term.contains('*') || term.contains('?') { wildcard_match(term, path) } else { contains_ignore_case(path, term) }
}

/// An OR of [`Query`] alternatives, which is how a query string containing `|`
/// is evaluated.
///
/// `Query` stays an AND of terms and filters because that is what one alternative
/// is; this type is the `|`-separated layer above it. A query with no `|` has
/// exactly one alternative and behaves identically to the `Query` it wraps.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct OrQuery {
    pub alternatives: Vec<Query>,
}

impl OrQuery {
    /// Split on `|` and parse each side. `a | b` matches a path satisfying `a`
    /// or `b`; `!` and filters bind to their own side only. A `|` inside a
    /// `re:` token is the regex engine's own alternation and does not split —
    /// `re:(a|b)` stays one alternative.
    pub fn parse(input: &str) -> Self {
        let alternatives: Vec<Query> = split_alternatives(input).into_iter().map(Query::parse).filter(|q| !q.is_empty()).collect();
        if alternatives.is_empty() {
            // An empty query matches everything.
            return OrQuery { alternatives: vec![Query::default()] };
        }
        OrQuery { alternatives }
    }

    pub fn matches(&self, path: &str, name: &str, is_dir: bool, size: u64, attrs: u32) -> bool {
        self.alternatives.iter().any(|q| q.matches(path, name, is_dir, size, attrs))
    }

    /// Filters only, across every alternative: any alternative whose filters
    /// pass does. Used by the interactive UI, which fuzzy-scores the terms.
    pub fn matches_filters(&self, path: &str, name: &str, is_dir: bool, size: u64, attrs: u32) -> bool {
        self.alternatives.iter().any(|q| q.matches_filters(path, name, is_dir, size, attrs))
    }

    /// The term strings across every alternative, for fuzzy scoring.
    pub fn terms(&self) -> impl Iterator<Item = &str> {
        self.alternatives.iter().flat_map(|q| q.terms.iter().map(|s| s.as_str()))
    }
}

/// Case-insensitive substring test that does not allocate.
///
/// Query terms are lowercased once when parsed, and the interactive search runs
/// this against every indexed path on every keystroke, so the temporary
/// lowercased copy a naive `path.to_lowercase().contains(d)` needs is a real
/// cost. Paths are nearly always ASCII, which can be compared byte-wise.
fn contains_ignore_case(haystack: &str, needle: &str) -> bool {
    if needle.is_empty() {
        return true;
    }
    if haystack.is_ascii() && needle.is_ascii() {
        let (h, n) = (haystack.as_bytes(), needle.as_bytes());
        return h.windows(n.len()).any(|w| w.eq_ignore_ascii_case(n));
    }
    haystack.to_lowercase().contains(needle)
}

/// Extensions behind a media `type:` token. Kept as plain lists rather than a
/// dependency on a MIME database: the query layer only needs "is this file a
/// picture", and an explicit list is testable and offline.
///
/// `image` and `video` are the kinds the reference design demoed; `audio` and
/// `archive` are the obvious siblings so the family is not half-there.
fn media_extensions(kind: &str) -> Option<&'static [&'static str]> {
    match kind {
        "image" | "picture" | "img" => Some(&[
            "jpg", "jpeg", "png", "gif", "bmp", "tif", "tiff", "webp", "heic", "heif", "avif", "ico", "svg", "psd", "raw", "cr2", "nef", "arw", "dng",
        ]),
        "video" | "movie" | "film" => Some(&["mp4", "mkv", "avi", "mov", "wmv", "flv", "webm", "m4v", "mpg", "mpeg", "m2ts", "ts", "3gp", "ogv"]),
        "audio" | "sound" | "music" => Some(&["mp3", "wav", "flac", "aac", "m4a", "ogg", "wma", "opus", "aiff", "ape", "alac"]),
        "archive" | "zip" | "compressed" => Some(&["zip", "rar", "7z", "tar", "gz", "bz2", "xz", "zst", "lz", "lzma", "cab", "iso"]),
        _ => None,
    }
}

/// Parse a size expression such as `>100mb`, `<1gb`, `>=10k`, `<=500`, or `1024`.
fn parse_size_filter(s: &str, min_size: &mut Option<u64>, max_size: &mut Option<u64>) {
    let s = s.trim();
    if s.is_empty() {
        return;
    }
    if let Some(rest) = s.strip_prefix(">=") {
        if let Some(sz) = parse_size_str(rest) {
            *min_size = Some(sz);
        }
    } else if let Some(rest) = s.strip_prefix('>') {
        if let Some(sz) = parse_size_str(rest) {
            *min_size = Some(sz.saturating_add(1));
        }
    } else if let Some(rest) = s.strip_prefix("<=") {
        if let Some(sz) = parse_size_str(rest) {
            *max_size = Some(sz);
        }
    } else if let Some(rest) = s.strip_prefix('<') {
        if let Some(sz) = parse_size_str(rest) {
            *max_size = Some(sz.saturating_sub(1));
        }
    } else if let Some(rest) = s.strip_prefix('=') {
        if let Some(sz) = parse_size_str(rest) {
            *min_size = Some(sz);
            *max_size = Some(sz);
        }
    } else if let Some(sz) = parse_size_str(s) {
        *min_size = Some(sz);
        *max_size = Some(sz);
    }
}

/// Parse a human-readable size string (e.g. "100mb", "4k", "1.5gb", "1024").
pub fn parse_size_str(s: &str) -> Option<u64> {
    let s = s.trim().to_lowercase();
    if s.is_empty() {
        return None;
    }

    let (num_part, multiplier) = if s.ends_with("tb") || s.ends_with('t') {
        let end = if s.ends_with("tb") { s.len() - 2 } else { s.len() - 1 };
        (&s[..end], 1024u64 * 1024 * 1024 * 1024)
    } else if s.ends_with("gb") || s.ends_with('g') {
        let end = if s.ends_with("gb") { s.len() - 2 } else { s.len() - 1 };
        (&s[..end], 1024u64 * 1024 * 1024)
    } else if s.ends_with("mb") || s.ends_with('m') {
        let end = if s.ends_with("mb") { s.len() - 2 } else { s.len() - 1 };
        (&s[..end], 1024u64 * 1024)
    } else if s.ends_with("kb") || s.ends_with('k') {
        let end = if s.ends_with("kb") { s.len() - 2 } else { s.len() - 1 };
        (&s[..end], 1024u64)
    } else if s.ends_with('b') {
        (&s[..s.len() - 1], 1u64)
    } else {
        (s.as_str(), 1u64)
    };

    let val: f64 = num_part.trim().parse().ok()?;
    Some((val * multiplier as f64) as u64)
}

/// Resolves a batch of parent/name edges into full paths.
///
/// Iterative rather than recursive so that deep directory trees cannot blow the
/// stack, and tolerant of broken parents: an edge whose parent is missing is
/// recorded as unresolved rather than aborting the whole pass.
pub struct PathResolver {
    edges: HashMap<u64, Edge>,
    resolved: HashMap<u64, String>,
    /// Extra names for records that already have a canonical edge: hard links
    /// and the DOS 8.3 short name. Keyed by (frn, parent_frn) because an alias
    /// can live in a different directory from the canonical name.
    aliases: Vec<(u64, u64, String)>,
}

impl PathResolver {
    pub fn new() -> Self {
        PathResolver { edges: HashMap::new(), resolved: HashMap::new(), aliases: Vec::new() }
    }

    pub fn add(&mut self, frn: u64, parent_frn: u64, name: &str, kind: NodeKind, size: u64, attributes: u32) {
        // Keys are opaque here: the caller has already folded the volume ordinal
        // into the high bits, and masking again would strip it. Only the
        // synthetic root of any volume is skipped.
        if volume_frn(frn) == ROOT_FRN {
            return;
        }
        self.edges.insert(frn, Edge { parent_frn, name: name.to_string(), kind, size, attributes });
    }

    pub fn edge_count(&self) -> usize {
        self.edges.len()
    }

    /// Record a second name for a record that already has (or will have) a
    /// canonical edge. Resolved after the canonical pass, so the alias path is
    /// built from its own parent's already-resolved canonical path.
    pub fn add_alias(&mut self, frn: u64, parent_frn: u64, name: &str) {
        if volume_frn(frn) == ROOT_FRN {
            return;
        }
        self.aliases.push((frn, parent_frn, name.to_string()));
    }

    /// Walk every edge to a full path, memoising as it goes.
    pub fn resolve_into(mut self, index: &mut Index) -> ResolveStats {
        let mut stats = ResolveStats::default();
        let frns: Vec<u64> = self.edges.keys().copied().collect();

        let mut chain: Vec<u64> = Vec::new();
        let mut seen = std::collections::HashSet::new();

        for frn in frns {
            // Skip anything already known, whether from a previous seed or from
            // an ancestor chain walked earlier in this same pass. Without the
            // second check a deep tree re-walks its whole tail per node, which
            // is quadratic.
            if index.paths.contains_key(&frn) || self.resolved.contains_key(&frn) {
                continue;
            }

            // Build the ancestor chain from this node upward.
            chain.clear();
            seen.clear();
            let mut cursor = frn;
            let mut base: Option<String> = None;

            loop {
                if cursor == ROOT_FRN {
                    base = index.paths.get(&ROOT_FRN).cloned();
                    break;
                }
                if let Some(p) = index.paths.get(&cursor) {
                    base = Some(p.clone());
                    break;
                }
                if let Some(p) = self.resolved.get(&cursor) {
                    base = Some(p.clone());
                    break;
                }
                if !seen.insert(cursor) {
                    stats.cycles += 1;
                    break; // parent chain loops back on itself
                }
                match self.edges.get(&cursor) {
                    Some(e) => {
                        chain.push(cursor);
                        cursor = e.parent_frn;
                    }
                    None => {
                        stats.orphans += 1; // parent record was never indexed
                        break;
                    }
                }
            }

            let Some(base) = base else {
                continue;
            };

            // Walk back down the chain, building each child's path.
            let mut current = base;
            while let Some(node) = chain.pop() {
                let e = &self.edges[&node];
                current = join(&current, &e.name);
                self.resolved.insert(node, current.clone());
            }
        }

        for (frn, path) in self.resolved.drain() {
            let Some(e) = self.edges.get(&frn) else {
                continue;
            };
            index.paths.insert(frn, path);
            index.parents.insert(frn, e.parent_frn);
            index.names.insert(frn, e.name.clone());
            index.is_dir.insert(frn, e.kind == NodeKind::Directory);
            index.sizes.insert(frn, e.size);
            index.attributes.insert(frn, e.attributes);
            stats.resolved += 1;
        }

        // Aliases resolve against their own parent's canonical path, which is
        // now in the index. An alias whose parent never resolved is dropped
        // rather than left dangling — it is not an extra orphan, the record
        // already has its canonical path (or is itself an orphan).
        for (frn, parent_frn, name) in std::mem::take(&mut self.aliases) {
            if let Some(parent_path) = index.paths.get(&parent_frn) {
                index.add_alias(frn, join(parent_path, &name));
            }
        }

        stats
    }
}

impl Default for PathResolver {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Default, Clone, Copy)]
pub struct ResolveStats {
    pub resolved: usize,
    pub orphans: usize,
    pub cycles: usize,
}

/// Join a parent directory path and a child name without doubling separators.
pub fn join(parent: &str, name: &str) -> String {
    let base = parent.trim_end_matches('\\');
    format!("{base}\\{name}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn join_normalises_separators() {
        assert_eq!(join("C:\\", "Windows"), "C:\\Windows");
        assert_eq!(join("C:\\Windows", "notepad.exe"), "C:\\Windows\\notepad.exe");
        assert_eq!(join("C:\\Windows\\", "System32"), "C:\\Windows\\System32");
    }

    #[test]
    fn resolver_walks_nested_chain() {
        let mut r = PathResolver::new();
        r.add(100, 5, "Users", NodeKind::Directory, 0, 0x10);
        r.add(101, 100, "dana", NodeKind::Directory, 0, 0x10);
        r.add(102, 101, "notes.txt", NodeKind::File, 42, 0x20);

        let mut idx = Index::new("C:\\");
        let stats = r.resolve_into(&mut idx);

        assert_eq!(stats.resolved, 3);
        assert_eq!(idx.path_of(102), Some("C:\\Users\\dana\\notes.txt"));
        assert_eq!(idx.path_of(101), Some("C:\\Users\\dana"));
        assert_eq!(idx.total_bytes(), 42);
    }

    #[test]
    fn resolver_reports_orphans_without_panicking() {
        let mut r = PathResolver::new();
        // Parent 999 was never added.
        r.add(500, 999, "orphan.txt", NodeKind::File, 0, 0x20);

        let mut idx = Index::new("C:\\");
        let stats = r.resolve_into(&mut idx);

        assert_eq!(stats.resolved, 0);
        assert_eq!(stats.orphans, 1);
        // Only the synthetic root remains; the orphan was never given a path.
        assert_eq!(idx.len(), 1);
        assert!(idx.path_of(500).is_none());
    }

    #[test]
    fn resolver_survives_a_parent_cycle() {
        let mut r = PathResolver::new();
        r.add(10, 11, "a", NodeKind::Directory, 0, 0x10);
        r.add(11, 10, "b", NodeKind::Directory, 0, 0x10);

        let mut idx = Index::new("C:\\");
        let stats = r.resolve_into(&mut idx);

        // Both members of the cycle are visited and each detects the loop once,
        // so the count is at least one regardless of iteration order.
        assert!(stats.cycles >= 1, "expected a detected cycle, got {stats:?}");
        assert_eq!(stats.resolved, 0);
        assert_eq!(idx.len(), 1);
    }

    #[test]
    fn deep_tree_does_not_overflow_the_stack() {
        let mut r = PathResolver::new();
        // Far deeper than any real filesystem: a recursive resolver would blow
        // the stack here. Materialising every intermediate path is quadratic in
        // depth, so this stays modest to keep the test fast.
        let depth = 20_000u64;
        for i in 0..depth {
            let parent = if i == 0 { 5 } else { 1_000 + i - 1 };
            r.add(1_000 + i, parent, "d", NodeKind::Directory, 0, 0x10);
        }

        let mut idx = Index::new("C:\\");
        let stats = r.resolve_into(&mut idx);

        assert_eq!(stats.resolved, depth as usize);
        // Starts at "C:\" (3 chars); the first join adds one separator (4), and
        // every later level adds "\d" (2), giving 2*depth + 2.
        assert_eq!(idx.path_of(1_000 + depth - 1).map(str::len), Some(depth as usize * 2 + 2));
    }

    #[test]
    fn index_uses_the_volumes_own_root_not_a_hardcoded_c() {
        let mut r = PathResolver::new();
        r.add(100, 5, "data", NodeKind::Directory, 0, 0x10);
        r.add(101, 100, "notes.txt", NodeKind::File, 7, 0x20);

        let mut idx = Index::new("D:\\");
        r.resolve_into(&mut idx);

        assert_eq!(idx.path_of(100), Some("D:\\data"));
        assert_eq!(idx.path_of(101), Some("D:\\data\\notes.txt"));
    }

    #[test]
    fn usn_create_then_delete_round_trips() {
        let mut idx = Index::new("C:\\");
        let rec = ntfs::UsnRecord {
            file_reference: 0x42,
            parent_reference: ROOT_FRN,
            usn: 100,
            reason: ntfs::USN_REASON_FILE_CREATE,
            file_attributes: 0x20,
            name: "readme.txt".to_string(),
        };
        assert!(idx.apply_usn(&rec, 0));
        assert_eq!(idx.path_of(0x42), Some("C:\\readme.txt"));

        let del = ntfs::UsnRecord { reason: ntfs::USN_REASON_FILE_DELETE, ..rec.clone() };
        assert!(idx.apply_usn(&del, 0));
        assert!(!idx.contains(0x42));
    }

    #[test]
    fn usn_rename_reparents() {
        let mut idx = Index::new("C:\\");
        idx.insert(0x42, ROOT_FRN, "old.txt", NodeKind::File, 0, 0x20);

        let rec = ntfs::UsnRecord {
            file_reference: 0x42,
            parent_reference: ROOT_FRN,
            usn: 200,
            reason: ntfs::USN_REASON_RENAME_NEW_NAME,
            file_attributes: 0x20,
            name: "new.txt".to_string(),
        };
        assert!(idx.apply_usn(&rec, 0));
        assert_eq!(idx.path_of(0x42), Some("C:\\new.txt"));
    }

    #[test]
    fn usn_duplicate_reasons_are_ignored() {
        let mut idx = Index::new("C:\\");
        idx.insert(0x42, ROOT_FRN, "keep.txt", NodeKind::File, 0, 0x20);

        let rec = ntfs::UsnRecord {
            file_reference: 0x42,
            parent_reference: ROOT_FRN,
            usn: 300,
            reason: ntfs::USN_REASON_RENAME_DUPLICATE_SOURCE,
            file_attributes: 0x20,
            name: "other.txt".to_string(),
        };
        assert!(!idx.apply_usn(&rec, 0));
        assert_eq!(idx.path_of(0x42), Some("C:\\keep.txt"));
    }

    #[test]
    fn file_attributes_directory_bit_sets_kind() {
        let mut idx = Index::new("C:\\");
        let rec = ntfs::UsnRecord {
            file_reference: 0x77,
            parent_reference: ROOT_FRN,
            usn: 1,
            reason: ntfs::USN_REASON_FILE_CREATE,
            file_attributes: 0x10, // FILE_ATTRIBUTE_DIRECTORY
            name: "dir".to_string(),
        };
        assert!(idx.apply_usn(&rec, 0));
        assert_eq!(idx.directory_count(), 2); // root plus the new directory
    }

    #[test]
    fn index_binary_roundtrip() {
        let mut idx = Index::new("D:\\");
        idx.insert(100, ROOT_FRN, "docs", NodeKind::Directory, 0, 0x10);
        idx.insert(101, 100, "report.pdf", NodeKind::File, 1048576, 0x20);
        idx.insert(102, 100, "data.csv", NodeKind::File, 2048, 0x20);

        let mut buf = Vec::new();
        idx.write_to(&mut buf).expect("should serialize index");

        let mut cursor = std::io::Cursor::new(buf);
        let loaded = Index::read_from(&mut cursor).expect("should deserialize index");

        assert_eq!(loaded.root(), "D:\\");
        assert_eq!(loaded.len(), idx.len());
        assert_eq!(loaded.directory_count(), idx.directory_count());
        assert_eq!(loaded.total_bytes(), idx.total_bytes());
        assert_eq!(loaded.path_of(101), Some("D:\\docs\\report.pdf"));
        assert_eq!(loaded.path_of(102), Some("D:\\docs\\data.csv"));
    }

    /// A v1 stream (one root, no root table) must still load: an index written
    /// before ERI2 is a real artifact and rescanning is expensive. The root
    /// becomes volume 0, which is where its keys already point.
    #[test]
    fn a_v1_stream_still_loads_with_its_single_root() {
        // Hand-build the legacy layout so this does not depend on `write_to`,
        // which always emits ERI2.
        let mut buf = Vec::new();
        buf.extend_from_slice(b"ERI1");
        buf.extend_from_slice(&1u32.to_le_bytes());
        let root = b"C:\\";
        buf.extend_from_slice(&(root.len() as u16).to_le_bytes());
        buf.extend_from_slice(root);
        buf.extend_from_slice(&1u64.to_le_bytes()); // one record

        let rec = |frn: u64, parent: u64, name: &str, path: &str| {
            let mut b = Vec::new();
            b.extend_from_slice(&frn.to_le_bytes());
            b.extend_from_slice(&parent.to_le_bytes());
            b.extend_from_slice(&0u64.to_le_bytes()); // size
            b.extend_from_slice(&0x20u32.to_le_bytes()); // attrs: archive
            b.push(0); // is_dir
            b.extend_from_slice(&(name.len() as u16).to_le_bytes());
            b.extend_from_slice(name.as_bytes());
            b.extend_from_slice(&(path.len() as u16).to_le_bytes());
            b.extend_from_slice(path.as_bytes());
            b
        };
        buf.extend_from_slice(&rec(100, ROOT_FRN, "notes.txt", "C:\\notes.txt"));

        let mut cursor = std::io::Cursor::new(buf);
        let loaded = Index::read_from(&mut cursor).expect("a v1 stream must load");

        assert_eq!(loaded.root(), "C:\\");
        assert_eq!(loaded.path_of(100), Some("C:\\notes.txt"));
    }

    /// Version 1 is only legal under the `ERI1` magic; `ERI2` at version 1 is a
    /// corrupt file and must not silently load as anything.
    #[test]
    fn a_v2_magic_with_a_v1_version_is_rejected() {
        let mut buf = Vec::new();
        buf.extend_from_slice(b"ERI2");
        buf.extend_from_slice(&1u32.to_le_bytes());
        let mut cursor = std::io::Cursor::new(buf);
        assert!(Index::read_from(&mut cursor).is_err());
    }

    #[test]
    fn multi_volume_index_round_trips_every_root() {
        // The same FRN on two volumes names unrelated files; the ordinal in the
        // key keeps them apart, and the root table must carry both prefixes so
        // neither volume resolves against the other's root.
        let mut idx = Index::default();
        idx.add_root(0, "C:\\");
        idx.add_root(1, "D:\\");
        idx.insert(volume_key(0, 100), volume_root_key(0), "a.txt", NodeKind::File, 1, 0x20);
        idx.insert(volume_key(1, 100), volume_root_key(1), "b.txt", NodeKind::File, 2, 0x20);

        let mut buf = Vec::new();
        idx.write_to(&mut buf).expect("serialize");
        let mut cursor = std::io::Cursor::new(buf);
        let loaded = Index::read_from(&mut cursor).expect("deserialize");

        assert_eq!(loaded.root(), "C:\\");
        assert_eq!(loaded.len(), idx.len());
        assert_eq!(loaded.path_of(volume_key(0, 100)), Some("C:\\a.txt"), "volume 0 keeps its own prefix");
        assert_eq!(loaded.path_of(volume_key(1, 100)), Some("D:\\b.txt"), "volume 1 must not resolve against volume 0's root");
    }

    #[test]
    fn entry_count_excludes_every_volume_root() {
        // Two roots, two real entries. Subtracting 1 (the old code) counts the
        // second root as an entry.
        let mut idx = Index::default();
        idx.add_root(0, "C:\\");
        idx.add_root(1, "D:\\");
        idx.insert(volume_key(0, 100), volume_root_key(0), "a.txt", NodeKind::File, 1, 0x20);
        idx.insert(volume_key(1, 100), volume_root_key(1), "b.txt", NodeKind::File, 2, 0x20);
        assert_eq!(idx.entry_count(), 2);
    }

    #[test]
    fn query_parse_and_matches() {
        let q = Query::parse("ext:rs dir:src size:>1000 main !test type:file");
        assert_eq!(q.extensions, vec!["rs"]);
        assert_eq!(q.dirs, vec!["src"]);
        assert_eq!(q.min_size, Some(1001));
        assert_eq!(q.terms, vec!["main"]);
        assert_eq!(q.not_terms, vec!["test"]);
        assert_eq!(q.kind, Some(NodeKind::File));

        // Matches
        assert!(q.matches("C:\\project\\src\\main.rs", "main.rs", false, 2000, 0));

        // Wrong extension
        assert!(!q.matches("C:\\project\\src\\main.cpp", "main.cpp", false, 2000, 0));

        // Wrong directory
        assert!(!q.matches("C:\\project\\bin\\main.rs", "main.rs", false, 2000, 0));

        // Size too small
        assert!(!q.matches("C:\\project\\src\\main.rs", "main.rs", false, 500, 0));

        // Contains excluded term
        assert!(!q.matches("C:\\project\\src\\main_test.rs", "main_test.rs", false, 2000, 0));

        // Is directory but query asks for file
        assert!(!q.matches("C:\\project\\src\\main", "main", true, 2000, 0));
    }

    #[test]
    fn type_link_requires_the_reparse_point_bit() {
        let link = ntfs::FILE_ATTRIBUTE_REPARSE_POINT;
        let plain = 0x20u32; // FILE_ATTRIBUTE_ARCHIVE
        let dir_link = 0x10 | link; // junction: a directory and a link

        let q = Query::parse("type:link");
        assert!(q.link);
        assert!(q.kind.is_none());
        assert!(!q.is_empty());
        assert!(q.matches("C:\\junc", "junc", true, 0, dir_link));
        assert!(q.matches("C:\\sym.txt", "sym.txt", false, 0, link));
        assert!(!q.matches("C:\\real.txt", "real.txt", false, 0, plain));
        // A plain directory is not a link either.
        assert!(!q.matches("C:\\dir", "dir", true, 0, 0x10));

        // `type:link` is orthogonal to kind: a junction is a directory *and* a
        // link, so both filters must pass together.
        let dir_only = Query::parse("type:dir type:link");
        assert!(dir_only.matches("C:\\junc", "junc", true, 0, dir_link));
        assert!(!dir_only.matches("C:\\sym.txt", "sym.txt", false, 0, link));

        let file_link = Query::parse("type:file type:link");
        assert!(file_link.matches("C:\\sym.txt", "sym.txt", false, 0, link));
        assert!(!file_link.matches("C:\\junc", "junc", true, 0, dir_link));

        // Without `type:link` the bit is ignored.
        let any = Query::parse("type:file");
        assert!(any.matches("C:\\real.txt", "real.txt", false, 0, plain));
        assert!(any.matches("C:\\sym.txt", "sym.txt", false, 0, link));
    }

    /// A size bound is a statement about files. A directory's entry size is
    /// meaningless, so it satisfies no bound at all — otherwise `size:>1gb`
    /// answers with every directory on the volume.
    #[test]
    fn a_size_filter_excludes_directories() {
        let q = Query::parse("size:>1gb");
        assert!(!q.matches("C:\\Windows", "Windows", true, 0, 0x10));
        // Even a directory whose entry happens to carry a large size.
        assert!(!q.matches("C:\\big", "big", true, 10 << 30, 0x10));
        assert!(q.matches("C:\\movie.mkv", "movie.mkv", false, 2 << 30, 0x20));
        assert!(!q.matches("C:\\tiny.txt", "tiny.txt", false, 10, 0x20));

        let small = Query::parse("size:<1kb");
        assert!(!small.matches("C:\\Windows", "Windows", true, 0, 0x10));
        assert!(small.matches("C:\\tiny.txt", "tiny.txt", false, 10, 0x20));
    }

    /// `type:image` / `type:video` are extension sets and imply "a file", which
    /// is what the reference design queried with.
    #[test]
    fn media_types_narrow_to_matching_extensions() {
        let video = Query::parse("type:video");
        assert!(video.matches("C:\\clip.mp4", "clip.mp4", false, 100, 0x20));
        assert!(video.matches("C:\\clip.mkv", "clip.mkv", false, 100, 0x20));
        assert!(!video.matches("C:\\pic.png", "pic.png", false, 100, 0x20));
        // A directory can never be a video, whatever it is called.
        assert!(!video.matches("C:\\video", "video", true, 0, 0x10));

        let image = Query::parse("type:image");
        assert!(image.matches("C:\\wallpaper.jpg", "wallpaper.jpg", false, 100, 0x20));
        assert!(image.matches("C:\\wallpaper.webp", "wallpaper.webp", false, 100, 0x20));
        assert!(!image.matches("C:\\clip.mp4", "clip.mp4", false, 100, 0x20));

        // Media kinds combine with other filters rather than replacing them.
        let both = Query::parse("size:>1gb type:video");
        assert!(both.matches("C:\\movie.mkv", "movie.mkv", false, 2 << 30, 0x20));
        assert!(!both.matches("C:\\clip.mp4", "clip.mp4", false, 100, 0x20));
    }

    /// An unknown `type:` used to be dropped, silently widening the query to
    /// every path on the volume. It now matches nothing and says why.
    #[test]
    fn an_unknown_type_matches_nothing_and_is_reported() {
        let q = Query::parse("type:bogus");
        assert_eq!(q.type_errors.len(), 1);
        assert!(q.type_errors[0].contains("bogus"));
        assert!(!q.is_empty());
        // Fail closed: nothing at all gets through.
        assert!(!q.matches("C:\\a.txt", "a.txt", false, 1, 0x20));
        assert!(!q.matches("C:\\dir", "dir", true, 0, 0x10));
    }

    #[test]
    fn parse_size_str_units() {
        assert_eq!(parse_size_str("1024"), Some(1024));
        assert_eq!(parse_size_str("4k"), Some(4096));
        assert_eq!(parse_size_str("10mb"), Some(10 * 1024 * 1024));
        assert_eq!(parse_size_str("1gb"), Some(1024 * 1024 * 1024));
        assert_eq!(parse_size_str("invalid"), None);
    }

    #[test]
    fn aliases_resolve_against_their_own_parent() {
        // A file can be reached by its Win32 name in one directory and by a
        // hard link or its 8.3 short name in another. The canonical path is the
        // Win32 one; the rest must still be searchable.
        let mut r = PathResolver::new();
        r.add(200, ROOT_FRN, "report.docx", NodeKind::File, 10, 0x20);
        r.add_alias(200, ROOT_FRN, "REPORT~1.DOC");
        r.add(201, ROOT_FRN, "Documents", NodeKind::Directory, 0, 0x10);
        r.add_alias(200, 201, "annual.docx");

        let mut idx = Index::new("C:\\");
        r.resolve_into(&mut idx);

        let mut paths: Vec<String> = idx.iter_all_paths().map(|(_, p)| p.to_string()).collect();
        paths.sort();
        assert_eq!(
            paths,
            vec![
                "C:\\".to_string(),
                "C:\\Documents".to_string(),
                "C:\\Documents\\annual.docx".to_string(),
                "C:\\REPORT~1.DOC".to_string(),
                "C:\\report.docx".to_string(),
            ]
        );
        assert_eq!(idx.alias_count(), 2);
    }

    #[test]
    fn alias_with_unresolved_parent_is_dropped_not_orphaned() {
        let mut r = PathResolver::new();
        r.add(300, ROOT_FRN, "keep.txt", NodeKind::File, 0, 0x20);
        // Parent 999999 was never indexed, so this alias has no path to hang off.
        r.add_alias(300, 999_999, "orphan-alias.txt");

        let mut idx = Index::new("C:\\");
        let stats = r.resolve_into(&mut idx);

        assert_eq!(idx.alias_count(), 0);
        assert_eq!(idx.path_of(300), Some("C:\\keep.txt"));
        // The canonical edge resolved, so this is not an orphan either.
        assert_eq!(stats.orphans, 0);
    }

    #[test]
    fn iter_all_paths_yields_canonical_and_aliases() {
        let mut idx = Index::new("C:\\");
        idx.insert(400, ROOT_FRN, "real.txt", NodeKind::File, 5, 0x20);
        idx.add_alias(400, "C:\\link.txt".to_string());

        let mut paths: Vec<&str> = idx.iter_all_paths().map(|(_, p)| p).collect();
        paths.sort_unstable();
        assert_eq!(paths, vec!["C:\\", "C:\\link.txt", "C:\\real.txt"]);
    }

    #[test]
    fn wildcard_match_handles_star_and_question() {
        assert!(wildcard_match("*.rs", "src/main.rs"));
        assert!(wildcard_match("main.rs", "main.rs"));
        assert!(!wildcard_match("*.rs", "src/main.txt"));
        // `?` is exactly one character.
        assert!(wildcard_match("a?c", "abc"));
        assert!(!wildcard_match("a?c", "ac"));
        assert!(!wildcard_match("a?c", "aXYc"));
        // A trailing `*` consumes nothing at all.
        assert!(wildcard_match("main.rs*", "main.rs"));
        // Case-insensitive: terms are lowercased at parse, paths are not.
        assert!(wildcard_match("*.rs", "SRC/Main.RS"));
    }

    #[test]
    fn wildcard_match_backtracks_to_the_last_star() {
        // Without backtracking to the last star this needs a second try: the
        // first `*` could greedily take "aaa" and leave "ab" to fail.
        assert!(wildcard_match("a*ab", "aaaab"));
        assert!(wildcard_match("*a*b*", "xaxbx"));
        assert!(!wildcard_match("*a*b*", "xbxa"));
    }

    #[test]
    fn bare_terms_are_substrings_but_wildcards_are_globs() {
        // A bare term matches anywhere in the path; a wildcard term must cover
        // the whole path, which is what `*.rs` and `report*` mean in Everything.
        assert!(term_matches("C:\\src\\report.txt", "report"));
        assert!(!term_matches("C:\\src\\report.txt", "report*"));
        assert!(term_matches("C:\\src\\report.txt", "*report*"));
        assert!(term_matches("C:\\src\\report.txt", "*.txt"));
    }

    #[test]
    fn or_query_matches_when_any_alternative_does() {
        let q = OrQuery::parse("ext:rs | ext:txt");
        assert_eq!(q.alternatives.len(), 2);
        assert!(q.matches("C:\\a.rs", "a.rs", false, 1, 0));
        assert!(q.matches("C:\\b.txt", "b.txt", false, 1, 0));
        assert!(!q.matches("C:\\c.md", "c.md", false, 1, 0));
    }

    #[test]
    fn or_query_binds_negation_to_its_own_side() {
        // `!test` negates only within its alternative: a path matching the
        // other side still matches overall.
        let q = OrQuery::parse("ext:rs !test | ext:log");
        assert!(!q.matches("C:\\test.rs", "test.rs", false, 1, 0));
        assert!(q.matches("C:\\main.rs", "main.rs", false, 1, 0));
        assert!(q.matches("C:\\test.log", "test.log", false, 1, 0));
    }

    #[test]
    fn or_query_with_no_bar_is_one_alternative() {
        let q = OrQuery::parse("ext:rs main");
        assert_eq!(q.alternatives.len(), 1);
        assert!(q.matches("C:\\src\\main.rs", "main.rs", false, 1, 0));
        assert!(!q.matches("C:\\src\\other.rs", "other.rs", false, 1, 0));
    }

    #[test]
    fn empty_or_query_matches_everything() {
        let q = OrQuery::parse("");
        assert!(q.matches("C:\\anything.txt", "anything.txt", false, 0, 0));
        // A trailing `|` contributes an empty alternative, which is dropped
        // rather than turning the query into "match nothing".
        let q = OrQuery::parse("ext:rs |");
        assert_eq!(q.alternatives.len(), 1);
    }

    #[test]
    fn or_query_terms_are_exposed_across_alternatives() {
        let q = OrQuery::parse("alpha | beta");
        let mut terms: Vec<&str> = q.terms().collect();
        terms.sort_unstable();
        assert_eq!(terms, vec!["alpha", "beta"]);
    }

    #[test]
    fn negation_stays_a_filter_when_the_ui_fuzzy_scores_the_positives() {
        // The interactive UI runs `matches_filters` and lets fuzzy own the
        // positive terms. A `!` must still bite there — it is a constraint the
        // user typed, not a ranking hint.
        let q = Query::parse("!maintenance");
        assert!(!q.matches_filters("C:\\src\\maintenance.rs", "maintenance.rs", false, 1, 0));
        assert!(q.matches_filters("C:\\src\\main.rs", "main.rs", false, 1, 0));

        // Positive terms, by contrast, are absent from `matches_filters`: the
        // fuzzy pass decides those.
        let q = Query::parse("report");
        assert!(q.matches_filters("C:\\anything.txt", "anything.txt", false, 1, 0));
        assert!(!q.matches("C:\\anything.txt", "anything.txt", false, 1, 0));
    }

    #[test]
    fn re_prefix_matches_the_path_with_a_regex() {
        let q = Query::parse(r"re:\\src\\main\.rs$");
        assert_eq!(q.regexes.len(), 1);
        assert!(q.matches("C:\\src\\main.rs", "main.rs", false, 1, 0));
        assert!(!q.matches("C:\\src\\main.rs.bak", "main.rs.bak", false, 1, 0));
        assert!(!q.matches("C:\\bin\\main.rs", "main.rs", false, 1, 0));

        // Negated regex: the path must not match.
        let n = Query::parse(r"!re:\.tmp$");
        assert_eq!(n.not_regexes.len(), 1);
        assert!(n.matches("C:\\a.txt", "a.txt", false, 1, 0));
        assert!(!n.matches("C:\\a.tmp", "a.tmp", false, 1, 0));

        // `regex:` is the long form.
        let l = Query::parse(r"regex:^C:");
        assert_eq!(l.regexes.len(), 1);
        assert!(l.matches("C:\\x", "x", false, 1, 0));
        assert!(!l.matches("D:\\x", "x", false, 1, 0));
    }

    #[test]
    fn an_invalid_regex_fails_closed_and_is_reported() {
        // A broken positive pattern must not silently admit everything.
        let q = Query::parse("re:[");
        assert_eq!(q.regexes.len(), 1);
        assert_eq!(q.regex_errors.len(), 1);
        assert!(!q.matches("C:\\anything.txt", "anything.txt", false, 1, 0));

        // An empty pattern is a no-op, not an error.
        let e = Query::parse("re:");
        assert!(e.regexes.is_empty());
        assert!(e.regex_errors.is_empty());
    }

    #[test]
    fn regex_alternation_is_not_query_level_or() {
        // `|` inside parens is always the regex engine's own alternation.
        let q = OrQuery::parse(r"re:(main|test)\.rs$");
        assert_eq!(q.alternatives.len(), 1);
        assert!(q.matches("C:\\main.rs", "main.rs", false, 1, 0));
        assert!(q.matches("C:\\test.rs", "test.rs", false, 1, 0));
        assert!(!q.matches("C:\\other.rs", "other.rs", false, 1, 0));

        // A bare `|` between two plain alternatives is still regex `|`…
        let q = OrQuery::parse(r"re:colou?r");
        assert_eq!(q.alternatives.len(), 1);
        let q = OrQuery::parse(r"re:a|b");
        assert_eq!(q.alternatives.len(), 1);
        assert!(q.matches("xa", "xa", false, 1, 0));
        assert!(q.matches("b", "b", false, 1, 0));

        // …but a `|` that re-enters query syntax splits the query instead.
        let q = OrQuery::parse(r"re:a\.rs$|re:b\.rs$");
        assert_eq!(q.alternatives.len(), 2);
        assert!(q.matches("C:\\a.rs", "a.rs", false, 1, 0));
        assert!(q.matches("C:\\b.rs", "b.rs", false, 1, 0));
        assert!(!q.matches("C:\\c.rs", "c.rs", false, 1, 0));

        // Regression: the original pipe behaviour is intact.
        let q = OrQuery::parse("ext:rs | ext:txt");
        assert_eq!(q.alternatives.len(), 2);
        let q = OrQuery::parse("ext:rs|ext:txt");
        assert_eq!(q.alternatives.len(), 2);

        // A `!re:` token also protects its `|`.
        let q = OrQuery::parse(r"!re:(tmp|bak)$ ext:rs");
        assert_eq!(q.alternatives.len(), 1);
        assert!(q.matches("C:\\main.rs", "main.rs", false, 1, 0));
        let q2 = Query::parse(r"!re:(tmp|bak)$");
        assert!(q2.matches("C:\\main.rs", "main.rs", false, 1, 0));
        assert!(!q2.matches("C:\\x\\tmp", "tmp", false, 1, 0));
        assert!(!q2.matches("C:\\x\\bak", "bak", false, 1, 0));
    }

    #[test]
    fn volumes_with_the_same_frn_do_not_collide() {
        // Both volumes have root at FRN 5, and the same FRN names unrelated
        // files on each. Without an ordinal in the key the second volume's
        // records overwrite the first's.
        let mut idx = Index::new("C:\\");
        idx.add_root(1, "D:\\");

        idx.insert(volume_key(0, 100), volume_root_key(0), "same.rs", NodeKind::File, 1, 0x20);
        idx.insert(volume_key(1, 100), volume_root_key(1), "same.rs", NodeKind::File, 2, 0x20);

        assert_eq!(idx.path_of(volume_key(0, 100)), Some("C:\\same.rs"));
        assert_eq!(idx.path_of(volume_key(1, 100)), Some("D:\\same.rs"));
        // Each resolves under its own volume's root, not the other's.
        assert_eq!(idx.sizes.get(&volume_key(0, 100)).copied(), Some(1));
        assert_eq!(idx.sizes.get(&volume_key(1, 100)).copied(), Some(2));
    }

    #[test]
    fn volume_key_masks_the_sequence_and_keeps_the_ordinal() {
        // The high 16 bits of a raw FRN are the sequence number, which is
        // exactly where the ordinal goes; the raw FRN must be masked first.
        let raw = 0xABCD_0000_0000_0123;
        assert_eq!(volume_frn(volume_key(2, raw)), 0x123);
        assert_eq!(volume_key(2, raw) >> 48, 2);
        // Volume 0 is bit-identical to a raw masked FRN, so single-volume code
        // and its tests see the same keys it always did.
        assert_eq!(volume_key(0, 0x42), 0x42);
    }

    #[test]
    fn multi_volume_resolution_uses_each_volumes_own_root() {
        let mut idx = Index::new("C:\\");
        idx.add_root(1, "D:\\");

        let mut r = PathResolver::new();
        r.add(volume_key(0, 100), volume_key(0, 5), "a.txt", NodeKind::File, 0, 0x20);
        r.add(volume_key(1, 100), volume_key(1, 5), "b.txt", NodeKind::File, 0, 0x20);
        // A second-level entry on volume 1, whose parent is a volume-1 directory.
        r.add(volume_key(1, 200), volume_key(1, 5), "sub", NodeKind::Directory, 0, 0x10);
        r.add(volume_key(1, 201), volume_key(1, 200), "c.txt", NodeKind::File, 0, 0x20);

        let stats = r.resolve_into(&mut idx);
        assert_eq!(stats.resolved, 4, "expected every edge resolved, got {stats:?}");
        assert_eq!(stats.orphans, 0);
        assert_eq!(idx.path_of(volume_key(0, 100)), Some("C:\\a.txt"));
        assert_eq!(idx.path_of(volume_key(1, 100)), Some("D:\\b.txt"));
        assert_eq!(idx.path_of(volume_key(1, 201)), Some("D:\\sub\\c.txt"));
    }
}
