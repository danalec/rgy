// SPDX-License-Identifier: Apache-2.0

//! Autostart: the daemon is there before the user needs it.
//!
//! A search that only finds the files created *after* the daemon started is
//! half a search. The machine was off, the file was created while it was
//! off, and the first thing the user does after login is look for exactly
//! that file. So the daemon has to come up with the session.
//!
//! Each platform has its own way of saying "run this at login", and each
//! one is text the user can read:
//!
//!   * Linux — a systemd **user** unit under `~/.config/systemd/user/`.
//!   * macOS — a launchd agent under `~/Library/LaunchAgents/`.
//!   * Windows — a Task Scheduler task with a logon trigger. The task
//!     scheduler has no file form, so the command line is printed instead;
//!     when the task tool is not there, the weaker `HKCU\...\Run` value is
//!     written and the tool says so in as many words.
//!
//! Two rules hold on every platform (parity row 4.13):
//!
//!   1. **Nothing is elevated and nothing asks.** The entry goes into the
//!      *user's* own session — a user unit, a LaunchAgent, a task at
//!      `LIMITED` rights, a `HKCU` value. No root, no UAC, no dialog, no
//!      prompt: the user runs this command, and that is the consent.
//!   2. **A failure is a sentence.** The tool prints what it did and what
//!      it could not do. It never opens a message box and never blocks
//!      waiting for an answer.
//!
//! The pieces are split so that the interesting part — which platform,
//! which mechanism, what the text says — is a pure function that can be
//! tested from any operating system, and so that the parts that touch the
//! machine are thin enough to read in one go.

pub mod linux;
pub mod macos;
pub mod plan;
pub mod render;
pub mod windows;

pub use plan::{Mechanism, Platform, choose_mechanism, default_name, detect_platform};

use std::path::{Path, PathBuf};

/// The mechanism's name, with the fallback's caveat.
///
/// The caveat is not decoration: the registry value is the weaker
/// mechanism, and a user reading the output has to be able to tell which
/// of the two they got.
pub fn mechanism_label(mechanism: Mechanism) -> String {
    if mechanism.is_fallback() {
        format!(
            "{} (the fallback: no Task Scheduler entry, no restart policy)",
            mechanism.name()
        )
    } else {
        mechanism.name().to_string()
    }
}

/// The `Run` key the fallback writes to: the user's own hive, never the
/// machine's. A machine-wide entry would need an administrator, and the
/// daemon does not need one.
pub const RUN_KEY: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run";

/// What an install did, in words.
///
/// `armed` is the honest bit: the artifact (a unit file, a plist, a task)
/// and the mechanism that arms it are two different things, and the second
/// can fail for reasons that have nothing to do with the first — no user
/// bus on a headless box, no permission to write the task. The tool then
/// says so, instead of claiming a success that did not happen.
#[derive(Debug, Clone)]
pub struct Report {
    /// The mechanism that is in place.
    pub mechanism: Mechanism,
    /// The file that was written, when the mechanism has one.
    pub artifact: Option<PathBuf>,
    /// The program command line that was registered.
    pub command: String,
    /// Whether the platform's own tool was told to arm it.
    pub armed: bool,
    /// What the user should know, in sentences.
    pub notes: Vec<String>,
}

/// What an uninstall did, in words.
#[derive(Debug, Clone)]
pub struct Removal {
    /// The mechanism that was looked for.
    pub mechanism: Mechanism,
    /// Whether something was there to remove.
    pub removed: bool,
    /// What the user should know, in sentences.
    pub notes: Vec<String>,
}

/// The user's home directory.
///
/// `HOME` on the unix side and `USERPROFILE` on the Windows side: the
/// platform's own variable, so a user who overrides it gets the override.
pub fn home_dir() -> PathBuf {
    #[cfg(windows)]
    let key = "USERPROFILE";
    #[cfg(not(windows))]
    let key = "HOME";

    std::env::var_os(key)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

/// The path as the user should read it.
///
/// `canonicalize` on Windows returns the long-path form (`\\?\C:\...`),
/// which the file APIs want and a *command line* does not: a scheduled
/// task created with that prefix holds a command that cannot be started.
/// So the prefix comes off before the path is written into anything the
/// platform will run.
pub fn display_path(path: &Path) -> String {
    let text = path.display().to_string();
    #[cfg(windows)]
    {
        if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
            return format!(r"\\{rest}");
        }
        if let Some(rest) = text.strip_prefix(r"\\?\") {
            return rest.to_string();
        }
    }
    text
}

/// The absolute path of a daemon binary, or the sentence saying why there
/// is none.
///
/// A tool that registers a path that does not exist is worse than no tool:
/// the entry is there, the daemon is not, and the user finds out at the
/// next login, in a way that looks like a broken product.
pub fn resolve_binary(path: &Path) -> Result<PathBuf, String> {
    if !path.exists() {
        return Err(format!("there is no file at {}", path.display()));
    }
    if !path.is_file() {
        return Err(format!("{} is not a file", path.display()));
    }
    std::fs::canonicalize(path).map_err(|e| format!("cannot resolve {}: {e}", path.display()))
}

/// Split a daemon argument string into arguments.
///
/// The platform tools take an argument *list* (a launchd `ProgramArguments`
/// array, a systemd `ExecStart` line), while the user thinks in one string.
/// The split is on whitespace, which is what the shell would have done, and
/// it is deliberately not a shell: nothing here is quoted, escaped, or
/// interpreted.
pub fn split_args(text: &str) -> Vec<String> {
    text.split_whitespace().map(str::to_string).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_binary_that_is_not_there_is_refused() {
        let err = resolve_binary(Path::new("/definitely/not/here/searchd")).unwrap_err();
        assert!(err.contains("there is no file at"), "{err}");
    }

    #[test]
    fn a_directory_is_not_a_binary() {
        let dir = std::env::temp_dir();
        let err = resolve_binary(&dir).unwrap_err();
        assert!(err.contains("is not a file"), "{err}");
    }

    #[test]
    fn arguments_split_on_whitespace() {
        assert_eq!(
            split_args("--index /tmp/i --json"),
            vec![
                "--index".to_string(),
                "/tmp/i".to_string(),
                "--json".to_string()
            ]
        );
        assert!(split_args("   ").is_empty());
    }
}
