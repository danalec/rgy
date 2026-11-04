// SPDX-License-Identifier: Apache-2.0

//! The launchd agent.
//!
//! A **LaunchAgent** under `~/Library/LaunchAgents/`: it is loaded into the
//! user's own session at login, it needs no root, and `KeepAlive` brings
//! the daemon back when it dies. The plist is written out as text the user
//! can read and edit — see [`crate::render::launchd_plist`].
//!
//! The macOS side was claimed to be native before it was: it was not.
//! the launchd path came from `fsearch`, and nothing in this tree carried
//! it. This module is that path, written here.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use std::path::{Path, PathBuf};

use crate::render::launchd_plist;

/// The plist's path under `home`.
pub fn plist_path_in(home: &Path, label: &str) -> PathBuf {
    home.join("Library")
        .join("LaunchAgents")
        .join(format!("{label}.plist"))
}

/// The plist's path for the running user.
pub fn plist_path(label: &str) -> PathBuf {
    plist_path_in(&crate::home_dir(), label)
}

/// Write the plist. Returns whether the file changed.
///
/// Writing the same text again changes nothing, so installing twice leaves
/// one agent and not two.
pub fn write_plist(
    home: &Path,
    label: &str,
    binary: &Path,
    args: &[String],
) -> std::io::Result<bool> {
    let path = plist_path_in(home, label);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let text = launchd_plist(binary, args, label);
    if let Ok(current) = std::fs::read_to_string(&path)
        && current == text
    {
        return Ok(false);
    }
    std::fs::write(&path, text)?;
    Ok(true)
}

/// Remove the plist. Returns whether it was there.
pub fn remove_plist(home: &Path, label: &str) -> std::io::Result<bool> {
    let path = plist_path_in(home, label);
    if path.symlink_metadata().is_ok() {
        std::fs::remove_file(&path)?;
        Ok(true)
    } else {
        Ok(false)
    }
}

/// The command line the agent runs, read back from the plist.
///
/// Read back from the file rather than from what the tool would write: the
/// user may have edited the plist, and the edited plist is the one that
/// runs.
pub fn registered_command(home: &Path, label: &str) -> Option<String> {
    let text = std::fs::read_to_string(plist_path_in(home, label)).ok()?;
    let mut in_arguments = false;
    let mut words = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line == "<key>ProgramArguments</key>" {
            in_arguments = true;
            continue;
        }
        if in_arguments {
            if line == "</array>" {
                break;
            }
            if let Some(word) = line
                .strip_prefix("<string>")
                .and_then(|rest| rest.strip_suffix("</string>"))
            {
                words.push(word.to_string());
            }
        }
    }
    if words.is_empty() {
        None
    } else {
        // The words, quoted the way a command line spells them. The plist
        // holds them exactly, and the doctor reads them back through the
        // same tokenizer the other platforms use -- so a path with a space
        // in it has to arrive quoted, or it arrives as two words and the
        // root is truncated at the space.
        let quoted: Vec<String> = words
            .iter()
            .map(|w| format!("\"{}\"", w.replace('\\', "\\\\").replace('"', "\\\"")))
            .collect();
        Some(quoted.join(" "))
    }
}

/// The user's numeric id, which `launchctl` wants (`gui/<uid>`).
pub fn uid() -> Result<u32, String> {
    let output = std::process::Command::new("id")
        .arg("-u")
        .output()
        .map_err(|e| format!("`id -u` could not be started: {e}"))?;
    if !output.status.success() {
        return Err("`id -u` did not answer".to_string());
    }
    String::from_utf8_lossy(&output.stdout)
        .trim()
        .parse::<u32>()
        .map_err(|e| format!("`id -u` answered something that is not a number: {e}"))
}

/// Load the agent into the user's session.
///
/// `bootstrap` is the modern call; `load -w` is the older one that still
/// works on every macOS this tool will meet. The first is tried, and the
/// second is the fallback.
pub fn load(plist: &Path) -> Result<String, String> {
    let uid = uid()?;
    let plist = plist.to_string_lossy().to_string();
    match launchctl(&["bootstrap", &format!("gui/{uid}"), &plist]) {
        Ok(out) => Ok(out),
        Err(bootstrap_err) => match launchctl(&["load", "-w", &plist]) {
            Ok(out) => Ok(out),
            Err(load_err) => Err(format!(
                "launchctl refused to load the agent ({bootstrap_err}); the older `load -w` failed too: {load_err}"
            )),
        },
    }
}

/// Unload the agent. Returns whether it was loaded.
pub fn unload(plist: &Path, label: &str) -> Result<bool, String> {
    let uid = uid()?;
    let target = format!("gui/{uid}/{label}");
    let plist_text = plist.to_string_lossy().to_string();
    match launchctl(&["bootout", &target]) {
        Ok(_) => Ok(true),
        Err(bootout_err) => match launchctl(&["unload", "-w", &plist_text]) {
            Ok(_) => Ok(true),
            Err(unload_err) => {
                let combined = format!("{bootout_err}; {unload_err}");
                if says_missing(&combined) {
                    Ok(false)
                } else {
                    Err(format!("launchctl could not unload the agent: {combined}"))
                }
            }
        },
    }
}

/// Whether the agent is loaded, as `launchctl print` says it.
pub fn is_loaded(label: &str) -> Option<bool> {
    let uid = uid().ok()?;
    let output = std::process::Command::new("launchctl")
        .arg("print")
        .arg(format!("gui/{uid}/{label}"))
        .output()
        .ok()?;
    Some(output.status.success())
}

/// The agent's state, as `launchctl print` says it.
///
/// The line is `state = running`, in launchd's own word for it: the agent
/// is loaded, and the daemon behind it is up. The doctor asks, because
/// "registered" and "running" are two different questions and only one of
/// them is answered by the file being there.
pub fn agent_state(label: &str) -> Option<String> {
    let uid = uid().ok()?;
    let output = std::process::Command::new("launchctl")
        .arg("print")
        .arg(format!("gui/{uid}/{label}"))
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    state_from(&String::from_utf8_lossy(&output.stdout))
}

/// The state line out of what `launchctl print` said.
fn state_from(text: &str) -> Option<String> {
    text.lines().find_map(|line| {
        let line = line.trim();
        line.strip_prefix("state = ")
            .map(|state| state.trim().to_string())
            .filter(|state| !state.is_empty())
    })
}

/// Whether a launchd state says the agent is running.
pub fn state_is_running(state: &str) -> bool {
    state.trim().eq_ignore_ascii_case("running")
}

/// The macOS side, right now.
#[derive(Debug, Clone, Default)]
pub struct State {
    /// The plist is there.
    pub file: bool,
    /// The agent is loaded into the session.
    pub loaded: Option<bool>,
}

/// The state of the autostart entry for this user.
pub fn status(home: &Path, label: &str) -> State {
    State {
        file: plist_path_in(home, label).is_file(),
        loaded: is_loaded(label),
    }
}

/// Run `launchctl` with the given arguments.
fn launchctl(args: &[&str]) -> Result<String, String> {
    let output = std::process::Command::new("launchctl")
        .args(args)
        .output()
        .map_err(|e| format!("launchctl is not there to run: {e}"))?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
        let detail = if stderr.is_empty() { stdout } else { stderr };
        Err(format!(
            "launchctl {} failed: {}",
            args.join(" "),
            if detail.is_empty() {
                "(no message)".to_string()
            } else {
                detail
            }
        ))
    }
}

/// Whether the message says the agent was not loaded.
fn says_missing(text: &str) -> bool {
    let text = text.to_lowercase();
    text.contains("could not find service") || text.contains("no such file")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("rgy-autostart-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("a temporary directory");
        dir
    }

    #[test]
    fn the_plist_lives_in_the_users_launch_agents() {
        let path = plist_path_in(Path::new("/Users/u"), "com.rgy.searchd");
        assert_eq!(
            path,
            PathBuf::from("/Users/u/Library/LaunchAgents/com.rgy.searchd.plist")
        );
    }

    #[test]
    fn install_uninstall_install_leaves_one_plist_and_the_same_text() {
        let home = temp_dir("macos-round-trip");
        let binary = PathBuf::from("/opt/rgy/bin/searchd");
        let label = "com.rgy.searchd";

        assert!(write_plist(&home, label, &binary, &[]).expect("the plist is written"));
        let first =
            std::fs::read_to_string(plist_path_in(&home, label)).expect("the plist is there");
        assert!(first.contains("<key>RunAtLoad</key>"), "{first}");
        assert!(first.contains("<key>KeepAlive</key>"), "{first}");

        assert!(
            !write_plist(&home, label, &binary, &[]).expect("the plist is written"),
            "a second install changed the plist"
        );
        let second =
            std::fs::read_to_string(plist_path_in(&home, label)).expect("the plist is there");
        assert_eq!(
            first, second,
            "the plist's text changed under a second install"
        );

        assert!(remove_plist(&home, label).expect("the plist is removed"));
        assert!(!plist_path_in(&home, label).exists());
        assert!(
            !remove_plist(&home, label).expect("the removal is quiet"),
            "a second uninstall found something to remove"
        );

        assert!(write_plist(&home, label, &binary, &[]).expect("the plist is written"));
        let third =
            std::fs::read_to_string(plist_path_in(&home, label)).expect("the plist is there");
        assert_eq!(first, third, "the plist's text changed over the round trip");

        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn the_registered_command_is_read_back_from_the_plist() {
        let home = temp_dir("macos-read-back");
        let binary = PathBuf::from("/opt/rgy/bin/searchd");
        let label = "com.rgy.searchd";
        write_plist(&home, label, &binary, &["--json".to_string()]).expect("the plist is written");
        let command = registered_command(&home, label).expect("the command is there");
        // The command as a command line spells it: the plist holds the
        // words exactly, and the doctor reads them back through the same
        // tokenizer the other platforms use.
        assert_eq!(command, r#""/opt/rgy/bin/searchd" "--json""#);
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn the_root_the_doctor_reports_is_the_root_the_daemon_walks() {
        let home = temp_dir("macos-read-back-root");
        let binary = PathBuf::from("/opt/rgy/bin/searchd");
        let label = "com.rgy.searchd";
        write_plist(
            &home,
            label,
            &binary,
            &["--root".to_string(), "/Users/Ana Maria".to_string()],
        )
        .expect("the plist is written");
        let command = registered_command(&home, label).expect("the command is there");
        assert_eq!(
            crate::root_of(&command),
            Some("/Users/Ana Maria".to_string()),
            "the root was truncated at the space"
        );
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn the_state_is_read_out_of_what_launchctl_printed() {
        let printed = "com.rgy.searchd = {\n\tactive count = 1\n\tstate = running\n\tpath = /opt/rgy/bin/searchd\n}";
        assert_eq!(
            state_from(printed),
            Some("running".to_string()),
            "the state line is the one that says whether the daemon is up"
        );
        assert_eq!(state_from("com.rgy.searchd = {\n\tpath = /x\n}"), None);
    }

    #[test]
    fn a_running_agent_says_so() {
        assert!(state_is_running("running"));
        assert!(state_is_running("Running"));
        assert!(!state_is_running("exited"));
        assert!(!state_is_running("waiting"));
    }
}
