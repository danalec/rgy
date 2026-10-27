// SPDX-License-Identifier: Apache-2.0

//! The Windows login entry.
//!
//! Two mechanisms, in order of preference:
//!
//!   * a **Task Scheduler task** with a logon trigger, created with
//!     `schtasks`. It is visible in Task Scheduler, it runs at `LIMITED`
//!     rights under the user's own account (no elevation, and no password
//!     stored), and it is the mechanism the parity row asks for.
//!   * the **`HKCU\...\Run` value**, used when the task tool is not there.
//!     It is the older mechanism and the weaker one: nothing for the user
//!     to look at but a string in regedit, and no restart policy.
//!
//! The task scheduler has no file form, so the artifact on this platform is
//! the command line — and the tool prints it, so the user can read what
//! was registered and run it again by hand.

use alloc::format;
use alloc::string::{String, ToString};
use std::path::Path;

use crate::render::windows_task_command;

/// Is the task scheduler's command line there?
///
/// Only the *spawn* is tested: `schtasks` is on every Windows, but it can
/// be missing from a trimmed-down image, and a tool that assumes it is
/// there fails with a message nobody can act on.
pub fn startup_task_tool_available() -> bool {
    std::process::Command::new("schtasks")
        .arg("/?")
        .output()
        .is_ok()
}

/// Create (or replace) the startup task.
///
/// `schtasks /Create` with an existing name and `/F` replaces the task, so
/// installing twice leaves one task and not two.
pub fn install_task(task_name: &str, binary: &Path, args: &[String]) -> Result<(), String> {
    let command = windows_task_command(binary, args);
    let output = std::process::Command::new("schtasks")
        .arg("/Create")
        .arg("/TN")
        .arg(task_name)
        .arg("/TR")
        .arg(&command)
        .arg("/SC")
        .arg("ONLOGON")
        // The interactive token: the task runs when the user is logged on,
        // which is what a login item is, and it needs no stored password.
        .arg("/IT")
        // Limited rights: the daemon is the user's own process. Nothing
        // here asks for an administrator, and nothing here is one.
        .arg("/RL")
        .arg("LIMITED")
        .arg("/F")
        .output()
        .map_err(|e| format!("schtasks could not be started: {e}"))?;

    if output.status.success() {
        Ok(())
    } else {
        Err(format!(
            "schtasks refused to create the task: {}",
            message(&output.stderr, &output.stdout)
        ))
    }
}

/// Delete the startup task. Returns whether it was there.
pub fn remove_task(task_name: &str) -> Result<bool, String> {
    let output = std::process::Command::new("schtasks")
        .arg("/Delete")
        .arg("/TN")
        .arg(task_name)
        .arg("/F")
        .output()
        .map_err(|e| format!("schtasks could not be started: {e}"))?;

    if output.status.success() {
        Ok(true)
    } else {
        let text = message(&output.stderr, &output.stdout);
        if says_missing(&text) {
            Ok(false)
        } else {
            Err(format!("schtasks could not delete the task: {text}"))
        }
    }
}

/// Write the `HKCU\...\Run` value: the fallback mechanism.
///
/// The user's own hive, never the machine's: a machine-wide entry would
/// need an administrator, and the daemon does not need one.
pub fn install_run_value(value_name: &str, binary: &Path, args: &[String]) -> Result<(), String> {
    let data = windows_task_command(binary, args);
    let output = std::process::Command::new("reg")
        .arg("add")
        .arg(crate::RUN_KEY)
        .arg("/v")
        .arg(value_name)
        .arg("/t")
        .arg("REG_SZ")
        .arg("/d")
        .arg(&data)
        .arg("/f")
        .output()
        .map_err(|e| format!("reg could not be started: {e}"))?;

    if output.status.success() {
        Ok(())
    } else {
        Err(format!(
            "reg refused to write the value: {}",
            message(&output.stderr, &output.stdout)
        ))
    }
}

/// Remove the `HKCU\...\Run` value. Returns whether it was there.
pub fn remove_run_value(value_name: &str) -> Result<bool, String> {
    let output = std::process::Command::new("reg")
        .arg("delete")
        .arg(crate::RUN_KEY)
        .arg("/v")
        .arg(value_name)
        .arg("/f")
        .output()
        .map_err(|e| format!("reg could not be started: {e}"))?;

    if output.status.success() {
        Ok(true)
    } else {
        let text = message(&output.stderr, &output.stdout);
        if says_missing(&text) {
            Ok(false)
        } else {
            Err(format!("reg could not delete the value: {text}"))
        }
    }
}

/// The command line the task runs, if the task is there.
///
/// Read back from the task scheduler rather than from what the tool
/// would write: the user may have changed the task, and the changed task
/// is the one that runs.
pub fn task_command(task_name: &str) -> Option<String> {
    let output = std::process::Command::new("schtasks")
        .arg("/Query")
        .arg("/TN")
        .arg(task_name)
        .arg("/FO")
        .arg("LIST")
        .arg("/V")
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout).to_string();
    text.lines()
        .find_map(|line| {
            let (key, value) = line.split_once(':')?;
            if key.trim().eq_ignore_ascii_case("Task To Run") {
                Some(value.trim().to_string())
            } else {
                None
            }
        })
        .filter(|value| !value.is_empty())
}

/// The `Run` value's data, if the value is there.
pub fn run_value_data(value_name: &str) -> Option<String> {
    let output = std::process::Command::new("reg")
        .arg("query")
        .arg(crate::RUN_KEY)
        .arg("/v")
        .arg(value_name)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout).to_string();
    text.lines()
        .find_map(|line| {
            let (name, rest) = line.split_once("REG_SZ")?;
            if name.trim() != value_name {
                return None;
            }
            Some(rest.trim().to_string())
        })
        .filter(|value| !value.is_empty())
}

/// The Windows side, right now.
#[derive(Debug, Clone, Default)]
pub struct State {
    /// The task is there, with the command line it runs.
    pub task: Option<String>,
    /// The `Run` value is there, with its data.
    pub run_value: Option<String>,
}

impl State {
    /// Whether anything at all is registered.
    pub fn registered(&self) -> bool {
        self.task.is_some() || self.run_value.is_some()
    }
}

/// The state of the autostart entry for this user.
pub fn status(task_name: &str, value_name: &str) -> State {
    State {
        task: task_command(task_name),
        run_value: run_value_data(value_name),
    }
}

/// The tool's own message: stderr if there is one, stdout otherwise.
fn message(stderr: &[u8], stdout: &[u8]) -> String {
    let stderr = String::from_utf8_lossy(stderr).trim().to_string();
    if stderr.is_empty() {
        let stdout = String::from_utf8_lossy(stdout).trim().to_string();
        if stdout.is_empty() {
            "(no message)".to_string()
        } else {
            stdout
        }
    } else {
        stderr
    }
}

/// Whether the message says the thing was not there.
///
/// Windows says it in two ways depending on the tool and the version
/// ("The system cannot find the file specified" and "unable to find the
/// specified registry key or value"), and both mean the same thing here:
/// there was nothing to remove.
fn says_missing(text: &str) -> bool {
    let text = text.to_lowercase();
    text.contains("cannot find") || text.contains("unable to find") || text.contains("not found")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn the_task_command_is_quoted_and_carries_the_arguments() {
        let command = windows_task_command(
            &PathBuf::from(r"C:\rgy\searchd.exe"),
            &["--json".to_string()],
        );
        assert_eq!(command, r#""C:\rgy\searchd.exe" --json"#);
    }

    #[test]
    fn a_missing_thing_is_recognised_from_its_message() {
        assert!(says_missing(
            "ERROR: The system cannot find the file specified."
        ));
        assert!(says_missing(
            "ERROR: The system was unable to find the specified registry key or value."
        ));
        assert!(!says_missing("ERROR: Access is denied."));
    }

    #[test]
    fn the_state_knows_when_nothing_is_registered() {
        let state = State::default();
        assert!(!state.registered());
    }
}
