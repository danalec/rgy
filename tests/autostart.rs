// SPDX-License-Identifier: Apache-2.0

//! What the doctor says about the daemon.
//!
//! "Registered" and "running" are two different questions, and the entry
//! answers only the first. The daemon answers the second, and the doctor
//! said "unknown" on a machine where a daemon was plainly up -- a word
//! that tells a user nothing about the thing they are trying to fix.

#[path = "harness.rs"]
mod harness;

use std::process::Command;
use std::time::{Duration, Instant};

use harness::{bin, fixture_dir, registered, text_tree};

/// The doctor asks the daemon, and the daemon answers.
///
/// The entry says nothing about a daemon that was started by hand, and
/// the doctor had no way to say anything at all about it.
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
#[test]
fn the_doctor_says_when_the_daemon_is_not_up() {
    let out = ask_doctor(&harness::absent(), "no");
    assert!(
        out.contains("running:    no"),
        "there is no daemon and the doctor says so: {out}"
    );
}

/// The doctor, asked, with the environment the test says.
///
/// The daemon opens its endpoint when it starts, and a daemon that is a
/// second into its startup is not serving yet: the test asks again, the
/// way a person does.
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
