//! TV pack store as the daemon's corpus store (`IMPLEMENTATION_PLAN.md`
//! §4.3/§10): the pack searcher answers `grep:` when the query names a
//! store built with `--tv-pre-extract` / `rgx-tv` — corpus bytes packed
//! once, mmap'd, incremental. The direct engine (name-core `content`)
//! stays available beside it.

use std::path::Path;

use rgx_core::tv::query::SearchConfig;
use rgx_core::tv_runtime::searcher::search_store;

/// One file's TV matches: path and `(line number, line text)` pairs.
pub struct TvFile {
    pub path: String,
    pub lines: Vec<(u64, String)>,
}

/// Search a TV pack store. `line_numbers` is on: the daemon reports lines
/// like the direct path does.
pub fn grep_tv(store_dir: &Path, pattern: &str) -> std::io::Result<Vec<TvFile>> {
    let config = SearchConfig {
        line_numbers: true,
        ..SearchConfig::default()
    };
    let (matches, _stats) = search_store(store_dir, pattern, &config)?;
    let mut files: Vec<TvFile> = Vec::new();
    for m in matches {
        let text = String::from_utf8_lossy(&m.line).trim_end().to_string();
        match files.iter_mut().find(|f| f.path == m.path) {
            Some(f) => f.lines.push((m.line_number.unwrap_or(0), text)),
            None => files.push(TvFile {
                path: m.path.clone(),
                lines: vec![(m.line_number.unwrap_or(0), text)],
            }),
        }
    }
    Ok(files)
}
