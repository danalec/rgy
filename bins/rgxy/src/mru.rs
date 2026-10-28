// SPDX-License-Identifier: Apache-2.0

//! The launcher's memory of what the user used.
//!
//! The **policy** — how far a used result rises — is in the model
//! (`rgy_launcher::usage`); this is the file the memory lives in, and
//! the file is the binary's business because the model does not touch
//! the machine.
//!
//! # Where it lives
//!
//! In the daemon's cache directory, under the `rgy` directory the daemon
//! already keeps its socket in: the launcher is a client of the daemon,
//! and its memory belongs next to the daemon's index — in the one place
//! the product owns on every platform.
//!
//! # What it looks like
//!
//! A text file, one line per path:
//!
//! ```text
//! # rgy usage: count<TAB>last<TAB>path
//! 3<TAB>1760000000<TAB>C:/Users/dana/notas.txt
//! ```
//!
//! The path is **last**, because a path may carry a tab and the first two
//! tabs are the only separators there are. A line that does not parse is
//! a line that is skipped, and a file whose last line was cut short does
//! not get that line at all: a store that cannot be read is no history,
//! never an error that stops the launcher.
//!
//! The path is the **daemon's** spelling of the path, and it is the
//! spelling the history is looked up with: the launcher and the daemon
//! agree on the bytes, and a history keyed by anything else would never
//! match anything.

use std::path::{Path, PathBuf};

use rgy_launcher::usage::History;

/// The file the history lives in.
pub fn store_path() -> Option<PathBuf> {
    let cache = rgy_proto::endpoint::cache_dir()?;
    Some(cache.join("rgy").join("usage.tsv"))
}

/// The seconds since the Unix epoch, now.
///
/// The clock is the machine's, and the model does not have one: the
/// history asks "recently" and the caller says when now is.
pub fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|since| since.as_secs())
        .unwrap_or(0)
}

/// Read the history. A file that cannot be read is no history.
pub fn load(path: &Path) -> History {
    match std::fs::read_to_string(path) {
        Ok(text) => parse(&text),
        Err(_) => History::new(),
    }
}

/// The history the text of a store says.
///
/// Every line that does not make sense is skipped: the store is written
/// by a program that can be killed halfway through, and a launcher that
/// refuses to start because of half a line is a launcher nobody uses.
pub fn parse(text: &str) -> History {
    let mut history = History::new();
    let lines: Vec<&str> = text.lines().collect();
    // A file that does not end in a newline was cut short by whatever cut
    // it, and its last line is half a line: half a line is not a path.
    let whole = text.ends_with('\n');
    for (index, line) in lines.iter().enumerate() {
        if !whole && index + 1 == lines.len() {
            break;
        }
        let line = line.trim_end_matches('\r');
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((count, rest)) = line.split_once('\t') else {
            continue;
        };
        let Some((last, path)) = rest.split_once('\t') else {
            continue;
        };
        let Ok(uses) = count.trim().parse::<u32>() else {
            continue;
        };
        let Ok(last) = last.trim().parse::<u64>() else {
            continue;
        };
        if uses == 0 || path.is_empty() {
            continue;
        }
        history.insert(path, uses, last);
    }
    history
}

/// Write the history.
pub fn save(path: &Path, history: &History) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let temp = path.with_extension("tsv.tmp");
    std::fs::write(&temp, render(history))?;
    // The write goes through a temporary file and a rename: a launcher
    // that is killed in the middle of a write leaves the old store, and
    // not half a new one.
    #[cfg(windows)]
    {
        // Windows will not rename a file over a file that is there.
        let _ = std::fs::remove_file(path);
    }
    std::fs::rename(&temp, path)
}

/// The text of the store.
pub fn render(history: &History) -> String {
    let mut text = String::from("# rgy usage: count\tlast\tpath\n");
    for entry in history.entries() {
        text.push_str(&entry.uses.to_string());
        text.push('\t');
        text.push_str(&entry.last.to_string());
        text.push('\t');
        text.push_str(&entry.path);
        text.push('\n');
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A directory of the test's own.
    fn scratch(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("rgy-rgxy-{name}-{}", std::process::id()))
    }

    #[test]
    fn the_store_survives_a_restart() {
        let dir = scratch("mru");
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("usage.tsv");

        let mut history = History::new();
        history.record("C:/Users/dana/notas.txt", 1_000);
        history.record("C:/Users/dana/notas.txt", 2_000);
        save(&path, &history).expect("the store is written");

        // The restart: a history that knows nothing, reading the file.
        let read = load(&path);
        let entry = read
            .get("C:/Users/dana/notas.txt")
            .expect("the history is there");
        assert_eq!(entry.uses, 2);
        assert_eq!(entry.last, 2_000);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_store_that_is_not_there_is_no_history() {
        let dir = scratch("mru-missing");
        let _ = std::fs::remove_dir_all(&dir);
        assert!(load(&dir.join("usage.tsv")).is_empty());
    }

    #[test]
    fn a_corrupt_store_is_no_history_and_not_a_crash() {
        let text = "# rgy usage\nnot a number\t12\t/a.txt\n5\tnot a time\t/b.txt\n5\t12\n";
        let history = parse(text);
        assert!(history.is_empty(), "{:?}", history.entries());
    }

    #[test]
    fn a_truncated_store_keeps_what_it_can() {
        // The last line was cut short: it is half a path, and half a path
        // is not a path.
        let text = "# rgy usage\n3\t1000\t/a.txt\n4\t2000\t/b.txt\n5\t3000\t/c.tx";
        let history = parse(text);
        assert_eq!(history.len(), 2);
        assert!(history.get("/a.txt").is_some());
        assert!(history.get("/c.tx").is_none());
    }

    #[test]
    fn a_path_may_carry_a_tab() {
        // The path is last: the first two tabs are the separators, and the
        // rest belongs to the path.
        let history = parse("1\t1000\t/a/we\tird.txt\n");
        assert!(history.get("/a/we\tird.txt").is_some());
    }

    #[test]
    fn the_store_is_a_text_a_person_can_read() {
        let mut history = History::new();
        history.record("/a/notas.txt", 1_000);
        let text = render(&history);
        assert!(text.contains("/a/notas.txt"), "{text}");
        assert!(text.contains("1\t1000\t"), "{text}");
    }
}
