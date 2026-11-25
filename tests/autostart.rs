// SPDX-License-Identifier: Apache-2.0

//! What the doctor says about the daemon.
//!
//! "Registered" and "running" are two different questions, and the entry
//! answers only the first. The daemon answers the second, and the doctor
//! said "unknown" on a machine where a daemon was plainly up -- a word
//! that tells a user nothing about the thing they are trying to fix.
//!
//! The report is the doctor's answer on a platform that HAS a login item.
//! FreeBSD has none, and there the doctor refuses instead: the refusal is
//! the other half of what the doctor says, and it is pinned here too.

#[path = "harness.rs"]
mod harness;

use std::process::Command;

use harness::bin;

#[cfg(any(target_os = "windows", target_os = "linux", target_os = "macos"))]
use harness::{fixture_dir, registered, text_tree};
#[cfg(any(target_os = "windows", target_os = "linux", target_os = "macos"))]
use std::time::{Duration, Instant};

/// The doctor asks the daemon, and the daemon answers.
///
/// The entry says nothing about a daemon that was started by hand, and
/// the doctor had no way to say anything at all about it.
///
/// A report is a report about a login item, so this is the arm of the
/// file that has one -- Windows, Linux and macOS.
#[cfg(any(target_os = "windows", target_os = "linux", target_os = "macos"))]
#[test]
fn the_doctor_says_when_the_daemon_is_up() {
    let dir = fixture_dir("doctor-up");
    text_tree(&dir);
    let daemon = registered(&dir);

    let out = ask_doctor(&daemon.env, "yes");
    assert!(
        out.contains("running:    yes"),
        "the daemon is up and the doctor says so: {out}"
    );
}

/// The doctor says "no" when there is no daemon -- and not "unknown".
///
/// "No" is a thing the user can act on: the daemon is not running.
#[cfg(any(target_os = "windows", target_os = "linux", target_os = "macos"))]
#[test]
fn the_doctor_says_when_the_daemon_is_not_up() {
    let out = ask_doctor(&harness::absent(), "no");
    assert!(
        out.contains("running:    no"),
        "there is no daemon and the doctor says so: {out}"
    );
}

/// Where there is no login item, the doctor refuses, and says why.
///
/// This is the FreeBSD case, and it is a case of its own: there is no
/// login item to register, so there is nothing for the doctor to report
/// on. A report that said "registered: no" and "running: no" about a
/// mechanism the machine does not have would be a user reading two facts
/// that are not facts at all. The refusal is the answer, and it goes
/// where a refusal goes: the error stream.
#[cfg(not(any(target_os = "windows", target_os = "linux", target_os = "macos")))]
#[test]
fn the_doctor_refuses_where_there_is_no_login_item() {
    let mut command = Command::new(bin("rgy-autostart"));
    command.arg("doctor").stdin(std::process::Stdio::null());
    let out = command
        .output()
        .unwrap_or_else(|e| panic!("run the doctor: {e}"));

    assert_eq!(
        out.status.code(),
        Some(1),
        "a doctor with nothing to report has to fail, not answer"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("not implemented on this operating system"),
        "the refusal has to say why: {stderr}"
    );
    assert!(
        String::from_utf8_lossy(&out.stdout).is_empty(),
        "a doctor that cannot answer must not print a report: {}",
        String::from_utf8_lossy(&out.stdout)
    );
}

/// The doctor, asked, with the environment the test says.
///
/// The daemon opens its endpoint when it starts, and a daemon that is a
/// second into its startup is not serving yet: the test asks again, the
/// way a person does.
#[cfg(any(target_os = "windows", target_os = "linux", target_os = "macos"))]
fn ask_doctor(env: &[(&str, String)], want: &str) -> String {
    let path = bin("rgy-autostart");
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let mut command = Command::new(&path);
        command.arg("doctor").stdin(std::process::Stdio::null());
        for (key, value) in env {
            command.env(key, value);
        }
        let out = command
            .output()
            .unwrap_or_else(|e| panic!("run the doctor: {e}"));
        let text = String::from_utf8_lossy(&out.stdout).into_owned();
        if text.contains(&format!("running:    {want}")) || Instant::now() >= deadline {
            return text;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}
