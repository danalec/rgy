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

use rgx_core::tv::query::SearchConfig;
use rgx_core::tv_runtime::searcher::search_store;

/// One file's TV matches: path and `(line number, line text)` pairs.
pub struct TvFile {
    pub path: String,
    pub lines: Vec<(u64, String)>,
}

/// Search a TV pack store. `line_numbers` is on: the daemon reports lines
/// like the direct path does.
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
    let mut files: Vec<TvFile> = Vec::new();
    for m in matches {
        let text = String::from_utf8_lossy(&m.line).trim_end().to_string();
        let path = join_root(&root, &m.path);
        match files.iter_mut().find(|f| f.path == path) {
            Some(f) => f.lines.push((m.line_number.unwrap_or(0), text)),
            None => files.push(TvFile {
                path,
                lines: vec![(m.line_number.unwrap_or(0), text)],
            }),
        }
    }
    Ok(files)
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
}
