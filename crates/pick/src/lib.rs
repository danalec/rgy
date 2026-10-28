// SPDX-License-Identifier: Apache-2.0

//! The picker: rgy's results handed to an external `fzf`.
//!
//! The picker is a separate program, found by scanning `PATH` for the
//! candidate names below — and the scan is memoised per process, because an
//! unscanned `PATH` costs one stat per candidate per call.
//!
//! Nothing here runs unless the caller asked for a picker: no probe, no
//! spawn, no terminal check. The exit status is the whole contract of a
//! search-shaped tool, so it is spelled out rather than inherited:
//!
//! | code | meaning |
//! |------|---------|
//! | 0    | the search matched and the picker returned a selection |
//! | 1    | nothing matched, or the user dismissed the picker |
//! | 2    | no picker on `PATH`, or it could not be run |
//!
//! # The separator
//!
//! A path is bytes, and on unix a byte may be a tab or a newline. A picker
//! that reads one record per line cannot be handed a path that contains one.
//! So the record separator is a decision, and it is [`Separator::Nul`] by
//! default: the NUL byte cannot appear in a path on any platform, and the
//! pickers that read it (`fzf`, `sk`) are the ones this crate looks for
//! first.
//!
//! # The ranking
//!
//! rgy scores a name with its own fzf-v1 scorer, and the picker would
//! score it again with a newer one. The caller decides: send the score
//! along and let the picker filter without reordering
//! (`--with-nth=2.. --no-sort`), and the order the user sees is the order
//! rgy ranked. A search engine's ranking is the product; a picker that
//! replaces it is a picker that throws it away.

#![no_std]

extern crate alloc;
#[cfg(any(test, feature = "std"))]
#[macro_use]
extern crate std;

// The machine — the process, the pipes, the `PATH` — is behind the `std`
// feature, and so are the imports it needs. A build without it carries
// the vocabulary and the decisions, and not one line that touches the
// machine: which picker, and what its exit status means.
#[cfg(any(test, feature = "std"))]
use alloc::boxed::Box;
use alloc::string::String;
#[cfg(any(test, feature = "std"))]
use alloc::string::ToString;
use alloc::vec::Vec;

/// The error the picker reports: a sentence a person reads.
pub type Error = String;

/// The result of an operation that can fail with a sentence.
pub type Result<T> = core::result::Result<T, Error>;

/// Picker binaries, first hit on `PATH` wins.
pub const CANDIDATES: &[&str] = &["fzf", "sk", "peco", "fz"];

/// The pickers that read and write NUL-separated records.
///
/// The first two are the ones that know `--read0` and `--print0`; the last
/// two do not, and a NUL-separated list handed to one of them is a list the
/// picker cannot read. When the separator is [`Separator::Nul`] and the
/// caller did not name a picker, only these are looked for.
pub const NUL_CAPABLE: &[&str] = &["fzf", "sk"];

/// How the picker reads its list and writes its selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Separator {
    /// One record per line.
    Line,
    /// One record per NUL byte.
    Nul,
}

impl Separator {
    /// The flags a picker needs to read and write this separator.
    ///
    /// Empty for a line separator: that is what every picker does already.
    pub fn args(&self) -> &'static [&'static str] {
        match self {
            Separator::Line => &[],
            Separator::Nul => &["--read0", "--print0"],
        }
    }

    /// Split what the picker wrote into records.
    pub fn split<'a>(&self, bytes: &'a [u8]) -> Vec<&'a [u8]> {
        let mut out = Vec::new();
        match self {
            Separator::Line => {
                for part in bytes.split(|b| *b == b'\n') {
                    if !part.is_empty() {
                        out.push(part);
                    }
                }
            }
            Separator::Nul => {
                for part in bytes.split(|b| *b == 0) {
                    if !part.is_empty() {
                        out.push(part);
                    }
                }
            }
        }
        out
    }
}

/// Whether the picker keeps the order it was given.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Order {
    /// The list stays in the order the search produced it.
    ///
    /// rgy's ranking is the product: a picker that re-scores the list with
    /// a newer algorithm replaces it with an answer nobody asked for. The
    /// flag that says so is `--no-sort`, and only the pickers that know it
    /// get it — a picker that does not understand the flag would refuse to
    /// start, which is loud but useless.
    Keep,
    /// The picker sorts the list its own way.
    Sort,
}

/// The pickers that understand `--no-sort`.
///
/// `fzf` and `sk` both do; `peco` does not, and handing it the flag is a
/// picker that refuses to run.
pub const ORDER_AWARE: &[&str] = &["fzf", "sk"];

/// How the picker is invoked.
pub struct Request<'a> {
    /// argv appended after the binary, unparsed — one flag per element, so
    /// nothing the user wrote is re-split by a shell.
    pub args: &'a [String],
    /// An explicit picker path; `PATH` is scanned when this is `None`.
    pub binary: Option<&'a str>,
    /// How the records are separated.
    pub separator: Separator,
    /// Whether the picker keeps the order it was given.
    pub order: Order,
}

impl<'a> Request<'a> {
    /// A request with the line separator and no picker named.
    pub fn new(args: &'a [String]) -> Request<'a> {
        Request {
            args,
            binary: None,
            separator: Separator::Line,
            order: Order::Sort,
        }
    }
}

/// The search to offer, run on its own thread so its bytes reach the picker
/// as they are produced rather than after the whole walk finishes.
#[cfg(feature = "std")]
pub type Search =
    Box<dyn FnOnce(&mut (dyn std::io::Write + Send + 'static)) -> Result<bool> + Send + 'static>;

/// Maps a finished picker to the exit status.
///
/// The picker exits 130 when the user dismisses it and 1 when its list is
/// empty; both are "the user got nothing", which is what 1 already means
/// to every other caller. Anything else is a broken picker, not a search
/// with no matches, and it says so.
pub fn status(code: i32, no_selection: bool) -> Result<i32> {
    match (code, no_selection) {
        (0, false) => Ok(0),
        (0 | 1 | 130, _) => Ok(1),
        c => Err(alloc::format!("the picker exited with {}", c.0)),
    }
}

/// Runs `search` with its output on the picker's stdin and yields what the
/// picker returned, plus the process exit status.
///
/// The records come back rather than going to stdout, because the caller
/// knows what a record means: a path for a search, a line for a grep, and
/// only the caller can strip a column it added.
#[cfg(feature = "std")]
pub fn pick_records(search: Search, req: &Request<'_>) -> Result<(i32, Vec<Vec<u8>>)> {
    use std::io::Read;
    use std::process::{Command, Stdio};
    use std::sync::mpsc;

    let binary = match req.binary {
        Some(p) => p.to_string(),
        None => locate(req.separator).ok_or_else(|| {
            let names: Vec<&str> = candidates_for(req.separator).to_vec();
            alloc::format!(
                "the picker needs one of {} on PATH (install one, or point --fzf-binary at it)",
                names.join(", ")
            )
        })?,
    };

    let mut command = Command::new(&binary);
    command
        .args(req.separator.args())
        .args(order_args(req.order, &binary))
        .args(req.args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        // The picker draws on the terminal it opens for itself; its own
        // complaints belong on our stderr, not folded into a pipe.
        .stderr(Stdio::inherit());
    let mut child = command
        .spawn()
        .map_err(|e| alloc::format!("the picker cannot run {binary}: {e}"))?;

    let mut stdin = child.stdin.take().expect("stdin was piped");
    let (tx, rx) = mpsc::channel();
    let writer = std::thread::spawn(move || {
        let out = search(&mut stdin);
        // The picker's list is only final at EOF.
        drop(stdin);
        let _ = tx.send(out);
    });

    let mut selected: Vec<u8> = Vec::new();
    if let Some(mut stdout) = child.stdout.take() {
        // A picker that dies mid-read must not take the search down with it.
        let _ = stdout.read_to_end(&mut selected);
    }
    let code = wait(&mut child);
    let searched = rx.recv().unwrap_or(Ok(false));
    let _ = writer.join();

    searched?;
    let records: Vec<Vec<u8>> = req
        .separator
        .split(&selected)
        .into_iter()
        .map(|r| r.to_vec())
        .collect();
    Ok((status(code, records.is_empty())?, records))
}

/// Runs `search` with its output on the picker's stdin, prints what the
/// picker returns, and yields the process exit status.
#[cfg(feature = "std")]
pub fn pick(search: Search, req: &Request<'_>) -> Result<i32> {
    use std::io::Write;
    let (code, records) = pick_records(search, req)?;
    if code == 0 && !records.is_empty() {
        let mut out = std::io::stdout();
        for record in &records {
            out.write_all(record)
                .map_err(|e| alloc::format!("the selection could not be printed: {e}"))?;
            if req.separator == Separator::Nul {
                out.write_all(&[0])
                    .map_err(|e| alloc::format!("the selection could not be printed: {e}"))?;
            } else {
                out.write_all(b"\n")
                    .map_err(|e| alloc::format!("the selection could not be printed: {e}"))?;
            }
        }
        out.flush()
            .map_err(|e| alloc::format!("the selection could not be printed: {e}"))?;
    }
    Ok(code)
}

/// The flags that carry the order, when the picker understands them.
#[cfg(feature = "std")]
fn order_args(order: Order, binary: &str) -> &'static [&'static str] {
    match order {
        Order::Sort => &[],
        Order::Keep => {
            let stem = std::path::Path::new(binary)
                .file_stem()
                .map(|s| s.to_string_lossy().to_ascii_lowercase())
                .unwrap_or_default();
            if ORDER_AWARE.contains(&stem.as_str()) {
                &["--no-sort"]
            } else {
                &[]
            }
        }
    }
}

/// The candidates the separator allows.
#[cfg(any(test, feature = "std"))]
fn candidates_for(separator: Separator) -> &'static [&'static str] {
    match separator {
        Separator::Line => CANDIDATES,
        Separator::Nul => NUL_CAPABLE,
    }
}

/// Reaps the picker. A child that died from a signal has no exit code of its
/// own; it is reported as 1, which reads as "nothing was selected".
#[cfg(feature = "std")]
fn wait(child: &mut std::process::Child) -> i32 {
    match child.wait() {
        Ok(s) => s.code().unwrap_or(1),
        Err(_) => 1,
    }
}

/// The picker on `PATH`, memoised per process.
#[cfg(feature = "std")]
fn locate(separator: Separator) -> Option<String> {
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock};
    static FOUND: OnceLock<Mutex<HashMap<String, Option<String>>>> = OnceLock::new();
    let mut memo = FOUND
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap();
    for name in candidates_for(separator) {
        let hit = memo
            .entry((*name).to_string())
            .or_insert_with(|| scan(name));
        if let Some(path) = hit.as_ref() {
            return Some(path.clone());
        }
    }
    None
}

/// Look for one picker on `PATH`.
#[cfg(feature = "std")]
fn scan(binary: &str) -> Option<String> {
    use std::path::Path;
    if binary.contains('/') || binary.contains('\\') {
        return Path::new(binary).is_file().then(|| binary.to_string());
    }
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).find_map(|dir| {
        let candidate = dir.join(binary);
        if candidate.is_file() {
            return Some(candidate.to_string_lossy().into_owned());
        }
        #[cfg(windows)]
        {
            for ext in ["exe", "bat", "cmd"] {
                let candidate = candidate.with_extension(ext);
                if candidate.is_file() {
                    return Some(candidate.to_string_lossy().into_owned());
                }
            }
        }
        None
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_selection_is_zero_and_a_dismissal_is_one() {
        assert_eq!(status(0, false).unwrap(), 0);
        assert_eq!(status(0, true).unwrap(), 1);
        // The picker's own "no match" and "user pressed ESC" statuses.
        assert_eq!(status(1, true).unwrap(), 1);
        assert_eq!(status(130, true).unwrap(), 1);
        // Anything else is a broken picker, not a search with no matches.
        assert!(status(2, true).is_err());
        assert!(status(127, true).is_err());
    }

    #[test]
    fn candidates_include_fallbacks() {
        assert!(CANDIDATES.contains(&"fzf"));
        assert!(CANDIDATES.contains(&"sk"));
        assert!(CANDIDATES.contains(&"peco"));
        assert!(CANDIDATES.contains(&"fz"));
    }

    #[test]
    fn a_nul_separator_asks_for_a_picker_that_reads_it() {
        // A NUL-separated list handed to a picker that cannot read one is a
        // list that never arrives.
        assert_eq!(candidates_for(Separator::Nul), NUL_CAPABLE);
        assert!(NUL_CAPABLE.contains(&"fzf"));
        assert!(!NUL_CAPABLE.contains(&"peco"));
    }

    #[test]
    fn a_nul_record_keeps_a_path_that_has_a_tab_in_it() {
        let raw = b"src/a\tb.txt\0src/c.txt\0";
        let records = Separator::Nul.split(raw);
        assert_eq!(records.len(), 2);
        assert_eq!(records[0], b"src/a\tb.txt");
        assert_eq!(records[1], b"src/c.txt");
    }

    #[test]
    fn a_line_record_splits_on_the_newline() {
        let raw = b"src/a.txt\nsrc/b.txt\n";
        let records = Separator::Line.split(raw);
        assert_eq!(records.len(), 2);
        assert_eq!(records[0], b"src/a.txt");
    }

    #[test]
    fn an_empty_selection_is_no_records() {
        assert!(Separator::Nul.split(b"").is_empty());
        assert!(Separator::Nul.split(b"\0\0").is_empty());
    }

    #[cfg(feature = "std")]
    #[test]
    fn an_absent_name_is_not_located() {
        assert!(scan("rgy-no-such-picker").is_none());
        assert!(scan("rgy-no-such-picker/sub").is_none());
    }

    #[cfg(feature = "std")]
    #[test]
    fn only_a_picker_that_knows_the_flag_is_told_to_keep_the_order() {
        // fzf and sk both know `--no-sort`; handing it to a picker that does
        // not is a picker that refuses to start.
        assert_eq!(order_args(Order::Keep, "/usr/bin/fzf"), &["--no-sort"]);
        assert_eq!(order_args(Order::Keep, "C:/bin/sk.exe"), &["--no-sort"]);
        assert!(order_args(Order::Keep, "/usr/bin/peco").is_empty());
        assert!(order_args(Order::Sort, "/usr/bin/fzf").is_empty());
    }
}
