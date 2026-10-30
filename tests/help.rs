// SPDX-License-Identifier: Apache-2.0

//! What every shipped binary says when it is asked what it is.
//!
//! The product ships five binaries, and a user asking a program what it
//! is gets one answer. The help is that answer: it goes to stdout, and
//! the exit status says the question was answered. A program that
//! answers on stderr shows the reader's pipe nothing, and a program that
//! answers with the status of an error is a program a script cannot ask.
//!
//! Measured on 2026-10-30, before the fix: `searchctl --help` and
//! `searchd --help` wrote to stderr, `zen-rs --help` exited 2, and
//! `searchctl --version` and `searchd --version` answered "unknown
//! option".

#[path = "harness.rs"]
mod harness;

use std::process::Command;

use harness::bin;

/// The binaries the product ships.
const BINS: &[&str] = &["searchctl", "searchd", "rgxy", "rgy-autostart", "zen-rs"];

/// The help is an answer, and an answer goes to stdout with a status that
/// says the question was answered.
#[test]
fn every_binary_answers_the_help_on_stdout_with_a_zero_status() {
    for name in BINS {
        let path = bin(name);
        let out = Command::new(&path)
            .arg("--help")
            .output()
            .unwrap_or_else(|e| panic!("run {} --help: {e}", path.display()));
        let stdout = String::from_utf8_lossy(&out.stdout);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert_eq!(
            out.status.code(),
            Some(0),
            "{name} --help exits 0; stderr={stderr}"
        );
        assert!(
            stdout.starts_with(name),
            "{name} --help names itself: {stdout}"
        );
        assert!(
            stderr.trim().is_empty(),
            "{name} --help says nothing on stderr: {stderr}"
        );
    }
}

/// The version is an answer too, and every binary knows its own.
#[test]
fn every_binary_answers_the_version_on_stdout_with_a_zero_status() {
    for name in BINS {
        let path = bin(name);
        let out = Command::new(&path)
            .arg("--version")
            .output()
            .unwrap_or_else(|e| panic!("run {} --version: {e}", path.display()));
        let stdout = String::from_utf8_lossy(&out.stdout);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert_eq!(
            out.status.code(),
            Some(0),
            "{name} --version exits 0; stderr={stderr}"
        );
        assert!(
            stderr.trim().is_empty(),
            "{name} --version says nothing on stderr: {stderr}"
        );

        let first = stdout.lines().next().unwrap_or_default();
        let mut parts = first.split_whitespace();
        assert_eq!(
            parts.next(),
            Some(*name),
            "{name} --version names itself: {first}"
        );
        let version = parts.next().unwrap_or_default();
        assert!(
            !version.is_empty() && version.chars().all(|c| c.is_ascii_digit() || c == '.'),
            "{name} --version is a version: {first}"
        );
    }
}
