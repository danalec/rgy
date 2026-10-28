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

/// The daemon is found without being told where it is.
///
/// The tool looks for it next to itself and on the PATH, and the help
/// says so: a user who has to type the path of a binary that is sitting
/// next to the tool is a user who types it wrong.
///
/// The install itself is not exercised here. It would register a startup
/// entry on the machine running the tests — and the daemon it would find
/// is the one the build put next to the test binary.
#[test]
fn the_daemon_next_to_the_tool_is_the_default() {
    let output = tool().arg("--help").output().expect("the tool runs");
    assert_eq!(output.status.code(), Some(0));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("next to this tool"),
        "the default has to be in the help: {stdout}"
    );
}

/// The tree the daemon indexes is named, always.
///
/// The daemon's own default is `.`, and a login entry does not start in
/// the user's home: it starts in `C:\Windows\System32` on Windows and in
/// `/` on macOS. The root is therefore part of the help, and the doctor
/// prints the root that was registered.
#[test]
fn the_tree_the_daemon_indexes_is_named() {
    let output = tool().arg("--help").output().expect("the tool runs");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("--root"), "{stdout}");
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

/// The doctor answers without touching the machine.
///
/// It is the command a user runs when something is not working, so it
/// must not change anything to find out what is wrong.
#[cfg(any(target_os = "windows", target_os = "linux", target_os = "macos"))]
#[test]
fn the_doctor_reports_without_changing_anything() {
    let output = tool().arg("doctor").output().expect("the tool runs");
    assert_eq!(output.status.code(), Some(0));
    let stdout = String::from_utf8_lossy(&output.stdout);
    for line in [
        "platform:",
        "daemon:",
        "registered:",
        "running:",
        "root:",
        "listens:",
        "path:",
    ] {
        assert!(stdout.contains(line), "the report needs `{line}`: {stdout}");
    }
}

/// The daemon is registered listening on a transport that has no end.
///
/// The daemon's own default reads from standard input, and a login entry
/// has no standard input: the daemon would start, walk the tree, and exit
/// -- registered, and gone. So the transport is part of what the tool
/// promises, in the help the user reads before running it.
#[test]
fn the_daemon_listens_where_it_can_be_reached() {
    let output = tool().arg("--help").output().expect("the tool runs");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("--listen"),
        "the transport has to be in the help: {stdout}"
    );
    assert!(
        stdout.contains("socket") && stdout.contains("pipe"),
        "both transports have to be named: {stdout}"
    );
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

/// The launcher is registered too, and the help says so.
///
/// The launcher is a program of its own: the window is there in a few
/// milliseconds because the **daemon** has the index warm, not because
/// the launcher built one. So it needs its own login entry, and a user
/// who has to find out about it from a missing hotkey is a user who
/// never gets the hotkey.
#[test]
fn the_launcher_is_registered_too() {
    let output = tool().arg("--help").output().expect("the tool runs");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("--launcher"),
        "the launcher has to be in the help: {stdout}"
    );
}

/// The launcher's entry is named apart from the daemon's.
///
/// The two are two programs with two names: the daemon is `rgy-searchd`
/// and the launcher is `rgy-rgxy`, and one entry cannot hold both.
#[test]
fn the_launcher_has_an_entry_of_its_own() {
    let output = tool().arg("--help").output().expect("the tool runs");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("rgxy"),
        "the launcher's name has to be in the help: {stdout}"
    );
}

/// The launcher's entry points at the daemon.
///
/// The launcher is a client: the index lives in the daemon, and a
/// keystroke is a question sent down a pipe. The endpoint is named in the
/// entry, for the same reason the daemon's root is: a login entry does
/// not inherit the environment of the session that installed it.
#[test]
fn the_launcher_is_pointed_at_the_daemon() {
    // The rule is in the code, and the code is what the tests read.
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let text = std::fs::read_to_string(root.join("src/main.rs")).expect("the source is text");
    assert!(
        text.contains("--endpoint"),
        "the launcher has to be told where the daemon is: the entry names the endpoint"
    );
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
