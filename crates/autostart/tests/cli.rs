// SPDX-License-Identifier: Apache-2.0

//! The tool's own behaviour, from the outside.
//!
//! These tests run the binary the way a user runs it. Nothing here
//! registers anything: the only install the tests exercise is the one
//! that must *fail*, because the daemon is not there — a tool that
//! registers a path that does not exist is worse than no tool.

use std::path::PathBuf;
use std::process::Command;

/// The binary under test, as cargo built it.
fn tool() -> Command {
    Command::new(env!("CARGO_BIN_EXE_rgy-autostart"))
}

/// The autostart is implemented for Windows, Linux and macOS; on every
/// other system the tool refuses, and what it says then is a different
/// thing to test.
#[cfg(any(target_os = "windows", target_os = "linux", target_os = "macos"))]
#[test]
fn a_daemon_that_is_not_there_is_refused() {
    let missing = std::env::temp_dir().join("rgy-autostart-does-not-exist");
    let output = tool()
        .args(["install", "--binary"])
        .arg(&missing)
        .output()
        .expect("the tool runs");

    assert_eq!(
        output.status.code(),
        Some(1),
        "installing a daemon that is not there has to fail"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("there is no file at"),
        "the reason has to be in words: {stderr}"
    );
}

#[cfg(any(target_os = "windows", target_os = "linux", target_os = "macos"))]
#[test]
fn install_without_a_path_says_what_is_missing() {
    let output = tool().arg("install").output().expect("the tool runs");
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("--binary"),
        "the reason has to name the option: {stderr}"
    );
}

#[cfg(any(target_os = "windows", target_os = "linux", target_os = "macos"))]
#[test]
fn status_reports_without_registering_anything() {
    let output = tool().arg("status").output().expect("the tool runs");
    assert_eq!(output.status.code(), Some(0));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("platform:"), "{stdout}");
    assert!(stdout.contains("mechanism:"), "{stdout}");
}

/// On a platform with no autostart mechanism, the tool says so and
/// fails.
///
/// It does not pretend to have registered anything: a tool that answers
/// "installed" on a system where it registered nothing is worse than no
/// tool at all. This is the FreeBSD case, and it is a case of its own —
/// the tool refuses, in words, and the refusal is the answer.
#[cfg(not(any(target_os = "windows", target_os = "linux", target_os = "macos")))]
#[test]
fn a_platform_without_autostart_says_so() {
    let output = tool().arg("status").output().expect("the tool runs");
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("not implemented on this operating system"),
        "the reason has to name the platform: {stderr}"
    );
}

#[test]
fn the_help_promises_no_prompt_and_no_elevation() {
    let output = tool().arg("--help").output().expect("the tool runs");
    assert_eq!(output.status.code(), Some(0));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("no administrator") && stdout.contains("no prompt, no dialog"),
        "the promise has to be in the help: {stdout}"
    );
}

#[test]
fn an_unknown_command_is_refused() {
    let output = tool().arg("frobnicate").output().expect("the tool runs");
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("unknown command"), "{stderr}");
}

#[test]
fn no_build_directory_is_hard_coded() {
    // The daemon's path is the user's to give: a tool that guesses one
    // registers a binary the user does not have, and finds out at the
    // next login.
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let sources = std::fs::read_dir(root.join("src")).expect("the sources are there");
    for entry in sources {
        let path = entry.expect("a source file").path();
        let text = std::fs::read_to_string(&path).expect("the source is text");
        for needle in ["target/debug", "target\\debug", "/target/release"] {
            assert!(
                !text.contains(needle),
                "{} names a build directory ({needle})",
                path.display()
            );
        }
    }
}
