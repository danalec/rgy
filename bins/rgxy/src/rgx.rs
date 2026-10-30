// SPDX-License-Identifier: Apache-2.0

//! The `rgx` fallback: the content layer without the daemon.
//!
//! The daemon carries the rgx adapters, so the content layer normally
//! answers over a pipe. But the launcher is a **client**, and a client
//! whose server is not running has nothing to say — so when the machine
//! has the `rgx` binary, the launcher asks it directly.
//!
//! It is a fallback, not the first choice: the daemon's index is warm and
//! its answers are a re-rank, while `rgx` is a process that has to be
//! started and has to walk what it is given. The scope keeps that walk
//! short.
//!
//! # The wire
//!
//! `rgx --json` answers in rg's own JSON shape — `begin`, `match`, `end`,
//! `summary` — and this reads it with the protocol crate's reader, because
//! a second JSON parser for one wire is a second thing to keep right.
//!
//! # The rules of a fallback
//!
//! A fallback answers **the question that was asked**, and not a bigger
//! one. So:
//!
//! * the scope is the files the name layer chose, and a folder in it is
//!   dropped — handing a folder to `rgx` is a walk, and the walk is the
//!   whole-tree search the fallback exists to avoid;
//! * the answer has a **deadline**, because a launcher that waits for a
//!   process that will never answer is a launcher that hangs;
//! * the exit code is part of the answer: `rg` exits 1 when nothing
//!   matched, and that is an answer, not a failure;
//! * the **case** is the engine's rule and not the fallback's: the
//!   daemon folds a pattern that has no uppercase letter, and a fallback
//!   that answered a different question would be a fallback that lies.

use std::io::BufRead;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use rgy_launcher::layer::{ContentMode, content_mode, engine_pattern};
use rgy_launcher::text::{Lang, Text};
use rgy_proto::GrepFile;

/// The content answer, from `rgx` itself.
///
/// `paths` is the scope: the files the name layer chose. An empty scope
/// is an empty answer — there is nothing to read, and walking the tree
/// instead would be a different question.
///
/// The window is the **caller's**: the launcher lives on the thread that
/// draws the window, and a fallback with a private deadline of its own
/// is a window that stopped answering the keyboard for as long as that
/// deadline is. The command line, which waits in the background, asks
/// for the client's window instead.
pub fn grep(
    binary: &str,
    query: &str,
    paths: &[String],
    window: Duration,
    lang: Lang,
) -> Result<Vec<GrepFile>, String> {
    // The window is the budget of the **whole** answer: the scope, the
    // process and the reading of its output share it. A fallback that
    // spends the window on the scope is a fallback that answers late.
    let deadline = Instant::now() + window;
    let scope = only_files(paths, deadline, lang)?;
    if Instant::now() >= deadline {
        // The window is gone before the process even starts: the
        // fallback gives up rather than answer with what it did not have
        // the time to look at. A scope that is empty because the window
        // ran out is not a scope with nothing in it.
        //
        // The scope was ready — it was the window that ran out, and the
        // sentence says so: a fallback that says the scope was not ready
        // when it was points at the wrong thing.
        return Err(rgy_launcher::text::fill(
            Text::for_lang(lang).scope_late,
            &[&said(window)],
        ));
    }
    if scope.is_empty() {
        return Ok(Vec::new());
    }
    let (mode, pattern) = split_pattern(query);
    let fixed = match mode {
        ContentMode::Literal => true,
        ContentMode::Regex => false,
        // The byte grep reads bytes: it has no notion of a symbol, and
        // grepping for the word `sym:` would answer a question nobody
        // asked. The fallback says it cannot answer this one.
        ContentMode::Symbol => {
            return Err(Text::for_lang(lang).rgx_no_symbols.to_string());
        }
    };

    let mut command = Command::new(binary);
    if fixed {
        // A bare word is a literal, the same way the daemon reads it.
        command.arg("--fixed-strings");
    }
    // The case is the engine's rule and not the fallback's: the daemon
    // folds a pattern that has no uppercase letter, and the fallback
    // that answers the same question folds it too.
    if case_insensitive(mode, pattern) {
        command.arg("-i");
    }
    command.arg("--json");
    command.arg("--");
    command.arg(pattern);
    for path in &scope {
        command.arg(path);
    }
    command.stdout(Stdio::piped());
    // The error is not read: a pipe nobody drains is a deadlock waiting to
    // happen, and the exit code says what went wrong well enough.
    command.stderr(Stdio::null());

    let mut child = command.spawn().map_err(|e| {
        Text::for_lang(lang)
            .rgx_not_runnable
            .replace("{e}", &e.to_string())
    })?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| Text::for_lang(lang).rgx_no_output.to_string())?;

    // The reading happens on its own thread so that the wait has a
    // deadline: a launcher that is blocked on a process is a launcher that
    // does not come back.
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        // The bytes are decoded with a replacement and not refused. A
        // line that is not UTF-8 used to end the answer there, and every
        // match after it was lost without a word; the stream is still an
        // answer, and the bytes it carries are the answer.
        let files = read_matches(
            std::io::BufReader::new(stdout)
                .split(b'\n')
                .map_while(Result::ok)
                .map(|line| String::from_utf8_lossy(&line).into_owned()),
        );
        let _ = tx.send(files);
    });

    // The process is read with what is left of the window: the scope
    // spent some of it, and the answer is the whole of it.
    let left = deadline.saturating_duration_since(Instant::now());
    let files = match within(&rx, left, lang) {
        Ok(files) => files,
        Err(reason) => {
            // The binary is still reading: it is not going to answer, and
            // the window is waiting for an answer that will not come.
            let _ = child.kill();
            let _ = child.wait();
            return Err(reason);
        }
    };

    // The exit code is the answer's other half. `rg` — and `rgx` with it
    // — exits 1 when nothing matched, and that is an answer and not a
    // failure; anything else is the binary saying it could not do it.
    let status = child.wait().map_err(|e| {
        rgy_launcher::text::fill(Text::for_lang(lang).rgx_ended_badly, &[&e.to_string()])
    })?;
    if !status.success() && status.code() != Some(1) {
        return Err(Text::for_lang(lang)
            .rgx_failed
            .replace("{status}", &status.to_string()));
    }
    Ok(files)
}

/// The answer, within the window the caller asked for.
///
/// The window covers the whole answer and not the reading of it: the
/// process is started, it reads the scope and it writes the matches, and
/// a deadline that counted each line of output would be a deadline that
/// ran out while the answer was still on its way.
fn within(
    rx: &mpsc::Receiver<Vec<GrepFile>>,
    window: Duration,
    lang: Lang,
) -> Result<Vec<GrepFile>, String> {
    match rx.recv_timeout(window) {
        Ok(files) => Ok(files),
        Err(mpsc::RecvTimeoutError::Timeout) => Err(rgy_launcher::text::fill(
            Text::for_lang(lang).rgx_no_answer,
            &[&said(window)],
        )),
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            Err(Text::for_lang(lang).rgx_ended_silent.to_string())
        }
    }
}

/// The window, in a sentence.
fn said(window: Duration) -> String {
    if window.as_secs() == 0 {
        format!("{} ms", window.as_millis())
    } else {
        format!("{} s", window.as_secs())
    }
}

/// The scope, with the folders taken out.
///
/// The name layer answers with files **and** folders, and a folder handed
/// to `rgx` is a walk: the binary reads everything under it, which is the
/// whole-tree search the fallback exists to avoid. Only the files are
/// kept — a folder in the scope is a result the user can open, and the
/// content layer reads files, not the trees under them.
///
/// A path that is not there any more is dropped for the same reason: the
/// file it named is gone, and reading it would fail.
///
/// The stats are the fallback's own deadline: the stat of a path on a
/// disconnected network drive blocks for the SMB timeout, and a fallback
/// that waits for it is a fallback that answers late. So the stats run on
/// their own thread — the caller waits for them within the window that is
/// left, and a worker that is still blocked when the window is gone is
/// left behind.
fn only_files(paths: &[String], deadline: Instant, lang: Lang) -> Result<Vec<String>, String> {
    if paths.is_empty() {
        return Ok(Vec::new());
    }
    let started = Instant::now();
    let (tx, rx) = mpsc::channel();
    let scope: Vec<String> = paths.to_vec();
    std::thread::spawn(move || {
        let _ = tx.send(stat_files(&scope, deadline));
    });
    let left = deadline.saturating_duration_since(Instant::now());
    match rx.recv_timeout(left) {
        Ok(scope) => Ok(scope),
        // The stats are still running and the window is gone: the
        // fallback gives up instead of waiting for a stat that is not
        // coming back.
        Err(mpsc::RecvTimeoutError::Timeout) => Err(rgy_launcher::text::fill(
            Text::for_lang(lang).scope_not_ready_in,
            &[&said(deadline.saturating_duration_since(started))],
        )),
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            Err(Text::for_lang(lang).scope_not_ready.to_string())
        }
    }
}

/// The files of the scope, and the folders left out.
///
/// The deadline is checked before every stat: a stat is a call to the
/// filesystem that cannot be interrupted, and the next one does not start
/// when the window is gone. The rest of the scope is dropped.
fn stat_files(paths: &[String], deadline: Instant) -> Vec<String> {
    let mut scope = Vec::new();
    for path in paths {
        if Instant::now() >= deadline {
            break;
        }
        if std::fs::metadata(path).is_ok_and(|meta| meta.is_file()) {
            scope.push(path.clone());
        }
    }
    scope
}

/// The pattern, and what it asks for.
///
/// The vocabulary is the **model's**: one table, one answer. The daemon
/// parses the content part with the engine's own parser, and this reads
/// the same table — a prefix that only one half knows is a prefix the user
/// gets two different answers for.
///
/// The prefix is read the way a person types it — `Grep:` is `grep:` — and
/// it is a whole word: `grepx:total` is a word the engine does not know.
///
/// A bare word is a literal, the same way the daemon reads it.
///
/// The pattern is the one the **engine** reads: a filter takes one word
/// of the query, and a quoted run is one word. So the byte grep and the
/// daemon look for the same thing.
pub fn split_pattern(query: &str) -> (ContentMode, &str) {
    match content_mode(query) {
        Some((mode, _)) => (mode, engine_pattern(query)),
        None => (ContentMode::Literal, query.trim()),
    }
}

/// The engine's smart case.
///
/// A pattern with no uppercase letter matches case-insensitively, and a
/// pattern that has one matches case-sensitively: the engine's rule, read
/// on the pattern as the user wrote it, before anything is escaped. A
/// symbol is never folded.
///
/// The fallback answers the same question the daemon does, so it folds the
/// same way — `grep:total` finds `Total` on both sides, and `grep:Total`
/// finds only `Total` on both sides.
fn case_insensitive(mode: ContentMode, pattern: &str) -> bool {
    mode != ContentMode::Symbol && !pattern.chars().any(|c| c.is_uppercase())
}

/// Read the matches out of rgx's JSON stream.
///
/// The records come one per line, and the ones that matter are `match`:
/// they carry the path, the line and the line's number. The `begin` and
/// `end` records bracket a file, and the `summary` is about the whole
/// run — neither says anything about a line that matched.
fn read_matches(lines: impl Iterator<Item = String>) -> Vec<GrepFile> {
    let mut files: Vec<GrepFile> = Vec::new();
    for line in lines {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Some(value) = rgy_proto::read_json(line) else {
            continue;
        };
        let Some(kind) = value.get("type").and_then(|t| t.as_str()) else {
            continue;
        };
        if kind != "match" {
            continue;
        }
        let Some(data) = value.get("data") else {
            continue;
        };
        let Some(path) = data
            .get("path")
            .and_then(|p| p.get("text"))
            .and_then(|t| t.as_str())
        else {
            continue;
        };
        let number = data
            .get("line_number")
            .and_then(|n| n.as_num())
            .unwrap_or(0.0) as usize;
        let text = data
            .get("lines")
            .and_then(|l| l.get("text"))
            .and_then(|t| t.as_str())
            .unwrap_or("");
        // The line comes with its terminator, the way rg reports it.
        let text = text.strip_suffix('\n').unwrap_or(text);
        let text = text.strip_suffix('\r').unwrap_or(text);

        match files.iter_mut().find(|f| f.path == path) {
            Some(file) => file.lines.push((number, text.to_string())),
            None => files.push(GrepFile {
                path: path.to_string(),
                lines: vec![(number, text.to_string())],
            }),
        }
    }
    files
}

#[cfg(test)]
mod tests {

    /// A tree in the temporary directory that takes itself away.
    ///
    /// A test that leaves a folder behind is a test that fills the
    /// temporary directory, and that directory is the one place every
    /// test on the machine shares. The tree is removed when it is
    /// dropped, so a test that panics takes its folder with it.
    struct TempTree(std::path::PathBuf);

    impl TempTree {
        /// A tree of its own, named after what it is for.
        fn new(tag: &str) -> TempTree {
            let dir = std::env::temp_dir().join(format!(
                "{tag}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            std::fs::create_dir_all(&dir).unwrap();
            TempTree(dir)
        }
    }

    impl std::ops::Deref for TempTree {
        type Target = std::path::Path;

        fn deref(&self) -> &std::path::Path {
            &self.0
        }
    }

    impl Drop for TempTree {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    use super::*;

    /// One record, the way rgx writes it.
    const SAMPLE: &[&str] = &[
        r#"{"type":"begin","data":{"path":{"text":"/a/notes.txt"}}}"#,
        r#"{"type":"match","adapter":"raw","data":{"path":{"text":"/a/notes.txt"},"lines":{"text":"o total do invoice\n"},"line_number":42,"absolute_offset":100,"submatches":[{"match":{"text":"invoice"},"start":12,"end":19}]}}"#,
        r#"{"type":"match","adapter":"raw","data":{"path":{"text":"/a/notes.txt"},"lines":{"text":"outra invoice\n"},"line_number":43,"absolute_offset":120,"submatches":[]}}"#,
        r#"{"type":"end","data":{"path":{"text":"/a/notes.txt"},"binary_offset":0,"stats":{"matched_lines":2}}}"#,
        r#"{"type":"summary","data":{"elapsed_total":{"human":"1 ms","nanos":1,"secs":0}}}"#,
    ];

    #[test]
    fn the_matches_are_read_out_of_the_stream() {
        let files = read_matches(SAMPLE.iter().map(|s| s.to_string()));
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path, "/a/notes.txt");
        assert_eq!(files[0].lines.len(), 2);
        assert_eq!(files[0].lines[0], (42, "o total do invoice".to_string()));
        assert_eq!(files[0].lines[1], (43, "outra invoice".to_string()));
    }

    #[test]
    fn a_line_that_is_not_json_is_skipped() {
        let mixed = [
            "not json at all",
            r#"{"type":"match","data":{"path":{"text":"/a/x.txt"},"lines":{"text":"y\n"},"line_number":1}}"#,
            "",
        ];
        let files = read_matches(mixed.iter().map(|s| s.to_string()));
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path, "/a/x.txt");
    }

    #[test]
    fn the_pattern_is_a_literal_unless_it_asks_for_the_engine() {
        assert_eq!(split_pattern("invoice"), (ContentMode::Literal, "invoice"));
        assert_eq!(
            split_pattern("grep:invoice"),
            (ContentMode::Literal, "invoice")
        );
        assert_eq!(
            split_pattern("content:invoice"),
            (ContentMode::Literal, "invoice")
        );
        assert_eq!(split_pattern("regex:^inv"), (ContentMode::Regex, "^inv"));
    }

    #[test]
    fn the_prefix_is_read_the_way_a_person_types_it() {
        // `Grep:` is `grep:`: the case of the prefix is the launcher's
        // business, and the case of the pattern is the user's.
        assert_eq!(
            split_pattern("Grep:Invoice"),
            (ContentMode::Literal, "Invoice")
        );
        assert_eq!(split_pattern("REGEX:^inv"), (ContentMode::Regex, "^inv"));
    }

    #[test]
    fn a_symbol_is_a_mode_of_its_own() {
        // The byte grep cannot answer it, and it has to be able to say so.
        assert_eq!(split_pattern("sym:total"), (ContentMode::Symbol, "total"));
    }

    #[test]
    fn the_fallback_refuses_a_symbol() {
        // Grepping for the word `sym:` would answer a question nobody
        // asked, so the fallback says it cannot answer this one.
        let dir = std::env::temp_dir().join(format!("rgy-rgxy-sym-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("notes.txt");
        std::fs::write(&file, "zebra\n").unwrap();
        let scope = vec![file.to_string_lossy().into_owned()];

        let answer = grep(
            "rgx",
            "sym:total",
            &scope,
            Duration::from_secs(5),
            Lang::English,
        );
        assert!(answer.is_err(), "a symbol is not a byte grep: {answer:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_empty_scope_is_an_empty_answer() {
        let answer = grep("rgx", "invoice", &[], Duration::from_secs(5), Lang::English);
        assert!(answer.expect("the answer").is_empty());
    }

    #[test]
    fn a_folder_in_the_scope_is_not_read() {
        // A folder handed to `rgx` is a walk, and the walk is the
        // whole-tree search the fallback exists to avoid.
        let dir = std::env::temp_dir().join(format!("rgy-rgxy-scope-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("notes.txt");
        std::fs::write(&file, "zebra\n").unwrap();
        let scope = vec![
            dir.to_string_lossy().into_owned(),
            file.to_string_lossy().into_owned(),
        ];
        let kept = only_files(
            &scope,
            Instant::now() + Duration::from_secs(5),
            Lang::English,
        )
        .expect("the scope resolves");
        assert_eq!(kept.len(), 1, "only the file is kept: {kept:?}");
        assert!(kept[0].ends_with("notes.txt"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The stats of the scope do not outlive the deadline.
    ///
    /// The stat of a path on a disconnected network drive blocks for the
    /// SMB timeout, and the budget of the answer is gone before the grep
    /// even starts. So the deadline is checked before every stat, and a
    /// scope whose deadline is gone is not stat'ed at all.
    #[test]
    fn the_scope_stats_stop_at_the_deadline() {
        let dir = std::env::temp_dir().join(format!("rgy-rgxy-deadline-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("notes.txt");
        std::fs::write(&file, "zebra\n").unwrap();
        let scope = vec![file.to_string_lossy().into_owned()];

        let gone = Instant::now() - Duration::from_millis(1);
        let kept = stat_files(&scope, gone);
        assert!(
            kept.is_empty(),
            "the stats do not start after the deadline: {kept:?}"
        );

        // And the caller does not wait for a stat that is not coming back.
        let err = only_files(&scope, gone, Lang::English).expect_err("the window is gone");
        assert!(err.contains("scope"), "{err}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The fallback gives up rather than answer late.
    ///
    /// A window of nothing is a window that cannot be met: the fallback
    /// says so instead of answering with what it did not have the time to
    /// look at.
    #[test]
    fn the_fallback_gives_up_when_the_window_is_gone() {
        let dir = std::env::temp_dir().join(format!("rgy-rgxy-nowindow-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("notes.txt");
        std::fs::write(&file, "zebra\n").unwrap();
        let scope = vec![file.to_string_lossy().into_owned()];

        let err = grep("rgx", "grep:zebra", &scope, Duration::ZERO, Lang::English)
            .expect_err("there was no time to answer");
        assert!(err.contains("scope"), "{err}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The fallback, end to end, on a machine that has the binary.
    ///
    /// The daemon carries the adapters, so the content layer normally
    /// answers over a pipe. This proves the other half: the machine has
    /// the `rgx` binary, the launcher finds it, runs it over the scope,
    /// and reads its JSON into the same answer the daemon's would have
    /// been.
    ///
    /// A machine without `rgx` has nothing to prove here: the launcher
    /// says so instead of pretending, and saying so is the other half of
    /// this contract.
    #[test]
    fn the_fallback_answers_when_the_machine_has_the_binary() {
        let Some(binary) = crate::tools::rgx() else {
            eprintln!("the rgx binary is not installed here; the fallback is not exercised");
            return;
        };
        let dir = TempTree::new("rgy-rgxy-rgx");
        let file = dir.join("notes.txt");
        std::fs::write(&file, "draft with zebra marker\n").unwrap();
        let scope = vec![file.to_string_lossy().into_owned()];

        let files = grep(
            &binary,
            "grep:zebra",
            &scope,
            Duration::from_secs(5),
            Lang::English,
        )
        .expect("the fallback answers");
        assert_eq!(files.len(), 1, "the fallback found the file");
        assert_eq!(files[0].lines.len(), 1);
        assert!(
            files[0].lines[0].1.contains("zebra"),
            "the line that matched is in the answer: {:?}",
            files[0].lines
        );
    }

    /// The fallback gives up in the window it was given.
    ///
    /// The window belongs to the caller: the launcher lives on the thread
    /// that draws the window, and a fallback with a private deadline of
    /// ten seconds is a window that stopped answering the keyboard for
    /// ten seconds.
    #[test]
    fn the_fallback_gives_up_in_the_window_it_is_given() {
        // The sender is kept: a channel with nobody on the other end is
        // a channel that is closed, and the case under test is the wait.
        let (_tx, rx) = mpsc::channel();
        let started = std::time::Instant::now();
        let err = within(&rx, Duration::from_millis(200), Lang::English)
            .expect_err("no answer is coming");
        assert!(err.contains("did not answer"), "{err}");
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "the fallback waited for its window, not for a private one"
        );
    }

    #[test]
    fn nothing_to_match_is_not_a_failure() {
        // `rg` exits 1 when nothing matched, and that is an answer.
        let Some(binary) = crate::tools::rgx() else {
            return;
        };
        let dir = std::env::temp_dir().join(format!("rgy-rgxy-nomatch-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("notes.txt");
        std::fs::write(&file, "draft with zebra marker\n").unwrap();
        let scope = vec![file.to_string_lossy().into_owned()];

        let files = grep(
            &binary,
            "grep:quimera",
            &scope,
            Duration::from_secs(5),
            Lang::English,
        )
        .expect("no match is not a failure");
        assert!(
            files.is_empty(),
            "nothing matched, and the launcher says so"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_case_is_the_engines_rule() {
        // The engine's smart case: a pattern with no uppercase letter
        // matches case-insensitively, and a pattern that has one matches
        // case-sensitively. The fallback answers the same question the
        // daemon does, so it folds the same way.
        assert!(case_insensitive(ContentMode::Literal, "total"));
        assert!(!case_insensitive(ContentMode::Literal, "Total"));
        assert!(case_insensitive(ContentMode::Regex, "^inv"));
        assert!(!case_insensitive(ContentMode::Regex, "^Inv"));
        // The case is read on the pattern as the user wrote it, before
        // anything is escaped: the escape is not a letter.
        assert!(case_insensitive(ContentMode::Literal, r"total\d"));
        assert!(!case_insensitive(ContentMode::Literal, r"Total\d"));
        // And a symbol is never folded.
        assert!(!case_insensitive(ContentMode::Symbol, "total"));
    }

    #[test]
    fn the_fallback_folds_the_case_the_way_the_daemon_does() {
        // The end of the rule: `grep:zebra` finds `Zebra` on both sides,
        // and `grep:ZEBRA` finds nothing on either — the pattern has an
        // uppercase letter, so the match is exact.
        let Some(binary) = crate::tools::rgx() else {
            eprintln!("the rgx binary is not installed here; the fallback is not exercised");
            return;
        };
        let dir = std::env::temp_dir().join(format!("rgy-rgxy-case-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("notes.txt");
        std::fs::write(&file, "draft with Zebra marker\n").unwrap();
        let scope = vec![file.to_string_lossy().into_owned()];

        let folded = grep(
            &binary,
            "grep:zebra",
            &scope,
            Duration::from_secs(5),
            Lang::English,
        )
        .expect("the fallback answers");
        assert_eq!(folded.len(), 1, "the pattern has no uppercase, so it folds");

        let exact = grep(
            &binary,
            "grep:ZEBRA",
            &scope,
            Duration::from_secs(5),
            Lang::English,
        )
        .expect("the fallback answers");
        assert!(
            exact.is_empty(),
            "the pattern has an uppercase, so the match is exact: {exact:?}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
