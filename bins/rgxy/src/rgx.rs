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
use std::time::Duration;

use rgy_launcher::layer::{ContentMode, content_mode, engine_pattern};
use rgy_proto::GrepFile;

/// How long the fallback may take before it is given up on.
///
/// The daemon answers in milliseconds; the fallback answers in seconds, at
/// worst. Ten seconds is generous for a process reading sixty-four files,
/// and past that the process is not slow — it is stuck.
const TIMEOUT: Duration = Duration::from_secs(10);

/// The content answer, from `rgx` itself.
///
/// `paths` is the scope: the files the name layer chose. An empty scope
/// is an empty answer — there is nothing to read, and walking the tree
/// instead would be a different question.
pub fn grep(binary: &str, query: &str, paths: &[String]) -> Result<Vec<GrepFile>, String> {
    let scope = only_files(paths);
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
            return Err("o rgx não responde a símbolos".to_string());
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

    let mut child = command
        .spawn()
        .map_err(|e| format!("o rgx não pôde ser executado: {e}"))?;
    let stdout = child.stdout.take().ok_or("o rgx não devolveu nada")?;

    // The reading happens on its own thread so that the wait has a
    // deadline: a launcher that is blocked on a process is a launcher that
    // does not come back.
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let files = read_matches(
            std::io::BufReader::new(stdout)
                .lines()
                .map_while(Result::ok),
        );
        let _ = tx.send(files);
    });

    let files = match rx.recv_timeout(TIMEOUT) {
        Ok(files) => files,
        Err(mpsc::RecvTimeoutError::Timeout) => {
            // The binary is still reading: it is not going to answer, and
            // the window is waiting for an answer that will not come.
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!("o rgx não respondeu em {} s", TIMEOUT.as_secs()));
        }
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            let _ = child.wait();
            return Err("o rgx terminou sem responder".to_string());
        }
    };

    // The exit code is the answer's other half. `rg` — and `rgx` with it
    // — exits 1 when nothing matched, and that is an answer and not a
    // failure; anything else is the binary saying it could not do it.
    let status = child
        .wait()
        .map_err(|e| format!("o rgx terminou mal: {e}"))?;
    if !status.success() && status.code() != Some(1) {
        return Err(format!("o rgx falhou ({status})"));
    }
    Ok(files)
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
fn only_files(paths: &[String]) -> Vec<String> {
    paths
        .iter()
        .filter(|path| std::fs::metadata(path).is_ok_and(|meta| meta.is_file()))
        .cloned()
        .collect()
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

        let answer = grep("rgx", "sym:total", &scope);
        assert!(answer.is_err(), "a symbol is not a byte grep: {answer:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_empty_scope_is_an_empty_answer() {
        let answer = grep("rgx", "invoice", &[]);
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
        let kept = only_files(&scope);
        assert_eq!(kept.len(), 1, "only the file is kept: {kept:?}");
        assert!(kept[0].ends_with("notes.txt"));
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
        let dir = std::env::temp_dir().join(format!("rgy-rgxy-rgx-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("notes.txt");
        std::fs::write(&file, "draft with zebra marker\n").unwrap();
        let scope = vec![file.to_string_lossy().into_owned()];

        let files = grep(&binary, "grep:zebra", &scope).expect("the fallback answers");
        assert_eq!(files.len(), 1, "the fallback found the file");
        assert_eq!(files[0].lines.len(), 1);
        assert!(
            files[0].lines[0].1.contains("zebra"),
            "the line that matched is in the answer: {:?}",
            files[0].lines
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

        let files = grep(&binary, "grep:quimera", &scope).expect("no match is not a failure");
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

        let folded = grep(&binary, "grep:zebra", &scope).expect("the fallback answers");
        assert_eq!(folded.len(), 1, "the pattern has no uppercase, so it folds");

        let exact = grep(&binary, "grep:ZEBRA", &scope).expect("the fallback answers");
        assert!(
            exact.is_empty(),
            "the pattern has an uppercase, so the match is exact: {exact:?}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
