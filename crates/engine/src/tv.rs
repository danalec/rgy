// SPDX-License-Identifier: Apache-2.0

//! TV pack store as the daemon's corpus store: the pack searcher answers
//! `grep:` when the query names a
//! store built with `--tv-pre-extract` / `rgx-tv` — corpus bytes packed
//! once, mmap'd, incremental. The direct engine (name-core `content`)
//! stays available beside it.

use alloc::format;
use std::path::Path;
use std::string::{String, ToString};
use std::vec;
use std::vec::Vec;

use rgx_core::tv::query::{SearchConfig, SearchMatch};
use rgx_core::tv_runtime::searcher::search_store;

/// One file's TV matches: path and `(line number, line text)` pairs.
pub struct TvFile {
    pub path: String,
    pub lines: Vec<(u64, String)>,
}

/// Search a TV pack store. `line_numbers` is on: the daemon reports lines
/// like the direct path does.
///
/// A match the store could not place on a line is not reported: the answer
/// is refused, and the refusal says what it did not report. The reader of
/// the wire refuses a line number that is not a line, and it refuses the
/// whole frame for it — so an invented `0` would take every other file's
/// matches with it.
///
/// The paths that come back are **absolute**: the store's records name the
/// files relative to the corpus root — a store is a pack of a tree, and
/// the tree's root is in the manifest, not in every record — but the
/// answer has to be a path the caller can open, and the scope it is
/// filtered against is absolute too. The root goes back on here, spelled
/// with `/` between the parts, the way the tree spells it everywhere else.
pub fn grep_tv(store_dir: &Path, pattern: &str) -> std::io::Result<Vec<TvFile>> {
    let config = SearchConfig {
        line_numbers: true,
        ..SearchConfig::default()
    };
    let (matches, _stats) = search_store(store_dir, pattern, &config)?;
    let root = match rgx_core::tv_runtime::format::load_manifest(store_dir) {
        Ok(manifest) => manifest.root_path,
        // A store without a readable manifest has no root to name, so the
        // records keep the paths they have.
        Err(_) => String::new(),
    };
    files_from_matches(&root, &matches)
}

/// The store's matches, as the files the answer carries.
///
/// A match the store could not place on a line has no line to name, and
/// the answer is refused rather than dressed up as one. The reader of the
/// wire refuses a line number that is not a line — a `0` is not a line,
/// and the store's numbers are 1-based — and it refuses the WHOLE frame
/// for it, so a made-up `0` does not name a missing line: it takes every
/// other file's matches with it.
///
/// The refusal is this route's own answer, and it says what it did not
/// report: how many matches the store could not place, and the file the
/// first of them was in. The protocol's `skipped` and its reasons are the
/// mechanism the routes that read files use — this route read the whole
/// store and could not say WHERE a match was, so there is no file it did
/// not read to count.
fn files_from_matches(root: &str, matches: &[SearchMatch]) -> std::io::Result<Vec<TvFile>> {
    let mut files: Vec<TvFile> = Vec::new();
    let mut unplaced = 0usize;
    let mut first = String::new();
    for m in matches {
        let Some((line, text)) = wire_match(m) else {
            unplaced += 1;
            if first.is_empty() {
                first = m.path.clone();
            }
            continue;
        };
        let path = join_root(root, &m.path);
        match files.iter_mut().find(|f| f.path == path) {
            Some(f) => f.lines.push((line, text)),
            None => files.push(TvFile {
                path,
                lines: vec![(line, text)],
            }),
        }
    }
    if unplaced > 0 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "{unplaced} match(es) in the pack store have no line number \
                 (the first is in {first}); the answer is refused rather than \
                 name a line the file does not have"
            ),
        ));
    }
    Ok(files)
}

/// One store match as the wire carries it: the line the match is on, and
/// the text of that line.
///
/// The store's line numbers are 1-based — the first line of a file is
/// line 1, the way `rg` prints it — and the reader of the wire refuses a
/// line number that is not a line. A match the store could not number is
/// `None` here, and not a line invented to fill the gap.
fn wire_match(m: &SearchMatch) -> Option<(u64, String)> {
    // The store is asked for the numbers, so they are there — but the
    // type says `Option`, and a `0` is not a line either: the reader
    // refuses it exactly as it refuses a missing one.
    let line = m.line_number?;
    if line == 0 {
        return None;
    }
    let text = String::from_utf8_lossy(&m.line).trim_end().to_string();
    Some((line, text))
}

/// The corpus root and a record's own path, as one path.
///
/// The separators are `/` on every platform: the tree's internal spelling
/// is the one with `/`, and a path that has to be compared with another
/// path — the scope's — has to be spelled the same way as the other one.
fn join_root(root: &str, rel: &str) -> String {
    let rel = rel.replace('\\', "/");
    if root.is_empty() {
        return rel;
    }
    format!("{}/{}", root_as_spelled(root).trim_end_matches('/'), rel)
}

/// The corpus root as the tree spells it.
///
/// Windows canonicalization answers with the `\\?\` form, and that prefix
/// is the platform's, not the caller's: the daemon takes it off its own
/// root, and a path that has to be compared with another one has to be
/// spelled the same way as the other one.
fn root_as_spelled(root: &str) -> String {
    let text = root.replace('\\', "/");
    if let Some(rest) = text.strip_prefix("//?/UNC/") {
        return format!("//{rest}");
    }
    text.strip_prefix("//?/").unwrap_or(&text).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_record_gains_the_corpus_root() {
        assert_eq!(
            join_root("C:/tree", "notes.md"),
            "C:/tree/notes.md",
            "the answer is a path the caller can open"
        );
    }

    #[test]
    fn the_separators_are_the_tree_ones() {
        // The store keeps what the disk gave it; the answer does not.
        assert_eq!(
            join_root("C:\\tree", "docs\\readme.txt"),
            "C:/tree/docs/readme.txt"
        );
        assert_eq!(join_root("C:\\tree\\", "readme.txt"), "C:/tree/readme.txt");
    }

    #[test]
    fn the_windows_prefix_is_the_platforms_and_not_the_callers() {
        // The daemon's own root has the prefix taken off, so a path that
        // is compared with the scope's has to have it taken off too.
        assert_eq!(join_root("\\\\?\\C:\\tree", "notes.md"), "C:/tree/notes.md");
        assert_eq!(root_as_spelled("\\\\?\\UNC\\srv\\share"), "//srv/share");
    }

    #[test]
    fn a_store_without_a_root_keeps_its_own_paths() {
        assert_eq!(join_root("", "notes.md"), "notes.md");
    }

    /// A match the store could not number is not a match on line 0.
    ///
    /// The reader of the wire refuses a line number that is not a line,
    /// and it refuses the whole frame for it — so a made-up `0` does not
    /// name a missing line, it takes every other file's matches with it.
    #[test]
    fn a_match_without_a_line_is_not_a_match_on_line_zero() {
        let m = SearchMatch {
            path: "notes.md".to_string(),
            line_number: None,
            line: b"a needle in the haystack".to_vec(),
        };
        assert!(
            wire_match(&m).is_none(),
            "a match with no line number has no line to name"
        );
    }

    /// A line number of 0 is not a line either.
    #[test]
    fn the_line_zero_is_not_a_line() {
        let m = SearchMatch {
            path: "notes.md".to_string(),
            line_number: Some(0),
            line: b"a needle in the haystack".to_vec(),
        };
        assert!(
            wire_match(&m).is_none(),
            "the reader refuses a line number that is not a line"
        );
    }

    /// The answer says what it did not report.
    ///
    /// The route refuses the answer rather than put a `0` on the wire, and
    /// the refusal names how many matches it could not place and the file
    /// the first of them was in.
    #[test]
    fn the_answer_says_what_it_did_not_report() {
        let matches = vec![
            SearchMatch {
                path: "notes.md".to_string(),
                line_number: Some(7),
                line: b"a needle in the haystack".to_vec(),
            },
            SearchMatch {
                path: "other.md".to_string(),
                line_number: None,
                line: b"a needle in the haystack".to_vec(),
            },
        ];
        let err = match files_from_matches("", &matches) {
            Ok(files) => panic!("the answer was given: {} file(s)", files.len()),
            Err(e) => e,
        };
        let message = err.to_string();
        assert!(
            message.contains("1 match(es)"),
            "the count of what was not reported is said: {message}"
        );
        assert!(
            message.contains("other.md"),
            "the file the match was in is named: {message}"
        );
    }

    /// A match the store numbered is reported on the line it named.
    #[test]
    fn a_numbered_match_is_reported_on_its_line() {
        let matches = vec![SearchMatch {
            path: "notes.md".to_string(),
            line_number: Some(7),
            line: b"a needle in the haystack\n".to_vec(),
        }];
        let files = files_from_matches("C:/tree", &matches).expect("a numbered match is a match");
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path, "C:/tree/notes.md");
        assert_eq!(
            files[0].lines,
            vec![(7u64, "a needle in the haystack".to_string())],
            "the line is the one the store named"
        );
    }
}
