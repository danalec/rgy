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

use std::io::BufRead;
use std::process::{Command, Stdio};

use rgy_proto::GrepFile;

/// The prefix that asks for a regular expression.
const REGEX_PREFIX: &str = "regex:";

/// The prefix that asks for a literal.
const LITERAL_PREFIX: &str = "grep:";

/// The content answer, from `rgx` itself.
///
/// `paths` is the scope: the files the name layer chose. An empty scope
/// is an empty answer — there is nothing to read, and walking the tree
/// instead would be a different question.
pub fn grep(binary: &str, query: &str, paths: &[String]) -> Result<Vec<GrepFile>, String> {
    if paths.is_empty() {
        return Ok(Vec::new());
    }
    let (pattern, fixed) = split_pattern(query);

    let mut command = Command::new(binary);
    if fixed {
        // A bare word is a literal, the same way the daemon reads it.
        command.arg("--fixed-strings");
    }
    command.arg("--json");
    command.arg("--");
    command.arg(pattern);
    for path in paths {
        command.arg(path);
    }
    command.stdout(Stdio::piped());
    command.stderr(Stdio::null());

    let mut child = command
        .spawn()
        .map_err(|e| format!("o rgx não pôde ser executado: {e}"))?;
    let stdout = child.stdout.take().ok_or("o rgx não devolveu nada")?;
    let files = read_matches(std::io::BufReader::new(stdout).lines().map_while(Result::ok));
    let _ = child.wait();
    Ok(files)
}

/// The pattern, and whether it is a literal.
///
/// The daemon's own rule, which is the engine's: `grep:` is a literal,
/// `regex:` asks for the engine, and a bare word is a literal too. The
/// two have to agree, or the same question would be answered two
/// different ways depending on which half of the launcher answered it.
pub fn split_pattern(query: &str) -> (&str, bool) {
    if let Some(rest) = query.strip_prefix(REGEX_PREFIX) {
        return (rest, false);
    }
    if let Some(rest) = query.strip_prefix(LITERAL_PREFIX) {
        return (rest, true);
    }
    if let Some(rest) = query.strip_prefix("content:") {
        return (rest, true);
    }
    (query, true)
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
        assert_eq!(split_pattern("invoice"), ("invoice", true));
        assert_eq!(split_pattern("grep:invoice"), ("invoice", true));
        assert_eq!(split_pattern("content:invoice"), ("invoice", true));
        assert_eq!(split_pattern("regex:^inv"), ("^inv", false));
    }

    #[test]
    fn an_empty_scope_is_an_empty_answer() {
        let answer = grep("rgx", "invoice", &[]);
        assert!(answer.expect("the answer").is_empty());
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
}
