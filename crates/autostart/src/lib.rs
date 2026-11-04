// SPDX-License-Identifier: Apache-2.0

//! Arming the daemon at login, without asking.
//!
//! One command, three platforms, no elevation and no dialog:
//!
//!   * Linux — a systemd **user** unit under `~/.config/systemd/user/`.
//!   * macOS — a launchd agent under `~/Library/LaunchAgents/`.
//!   * Windows — a Task Scheduler task with a logon trigger. The task
//!     scheduler has no file form, so the command line is printed instead;
//!     when the task tool is not there, the weaker `HKCU\...\Run` value is
//!     written and the tool says so in as many words.
//!
//! Two rules hold on every platform:
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
//!
//! # `no_std`
//!
//! The crate is `no_std`: the decision — which platform, which mechanism,
//! which arguments — is portable, and it is in [`plan`]. The parts that
//! touch the machine (the files written, the commands run, the paths
//! resolved) are behind the `std` feature, because a machine is a machine.

#![no_std]

extern crate alloc;
#[cfg(feature = "std")]
extern crate std;

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;

/// Which platform, which mechanism — decided before anything is touched.
pub mod plan;

/// The text of the artifacts: the systemd unit, the launchd plist, the
/// commands the Windows tools take.
#[cfg(feature = "std")]
pub mod render;

/// The systemd user unit (Linux).
#[cfg(feature = "std")]
pub mod linux;

/// The launchd agent (macOS).
#[cfg(feature = "std")]
pub mod macos;

/// The Task Scheduler task, or the registry value (Windows).
#[cfg(feature = "std")]
pub mod windows;

pub use plan::{
    Mechanism, Platform, choose_mechanism, default_name, detect_platform, launcher_entry_supported,
};

/// The daemon's file name on this platform.
///
/// The release ships the daemon next to this tool, so the name looked for
/// is the name the release ships it under: `searchd.exe` on Windows,
/// `searchd` everywhere else.
pub const DAEMON_NAME: &str = if cfg!(windows) {
    "searchd.exe"
} else {
    "searchd"
};

/// The name the launcher's entry gets on this platform.
///
/// The launchd label is a reverse-DNS name, because that is what launchd
/// expects; the other two are plain names.
///
/// The entry keeps the `rgy-rgxy` name even though the crate is `rgxy` and
/// the program the user types is `rgxy`: the entry is an identity on the
/// user's machine -- the file in Startup, the launchd label -- and the
/// uninstall finds it by that name. Renaming it would leave an installed
/// entry behind, starting the launcher at every login, with nothing left
/// to take it out.
pub fn launcher_default_name(platform: Platform) -> &'static str {
    match platform {
        Platform::MacOs => "com.rgy.rgxy",
        _ => "rgy-rgxy",
    }
}

/// The launcher's file name on this platform.
///
/// The launcher is a program of its own: the window is there in a few
/// milliseconds because the **daemon** has the index warm, not because
/// the launcher built one. So it needs its own login entry, under its
/// own name, and the release ships it next to the daemon.
pub const LAUNCHER_NAME: &str = if cfg!(windows) { "rgxy.exe" } else { "rgxy" };

/// Why the launcher has no login entry on the platforms where it has none.
///
/// The launcher is a floating window on Windows and the terminal interface
/// everywhere else, and a login entry has no terminal. Measured on the
/// CachyOS box: a unit whose `ExecStart` is `rgxy --endpoint …` runs, finds
/// no terminal, prints nothing and exits 0 — registered, enabled, and doing
/// nothing. `install --launcher` says this sentence there instead of
/// writing that unit, and the sentence is written once so the tool, the
/// installers and the tests cannot say three things.
pub const LAUNCHER_HAS_NO_LOGIN_ENTRY: &str = "the floating window is Windows-only today, and the terminal interface needs a terminal — which a login entry has not; run `rgxy` in a terminal instead";

/// The entries an uninstall takes out.
///
/// An install that registered the launcher registered **two** things, and
/// an uninstall that removes one of them leaves the other starting at
/// every login — which is how a user finds out that a tool they removed
/// is still running.
///
/// A name the user gave is the one entry they meant, and the launcher's
/// name on its own is the launcher's entry alone.
pub fn uninstall_targets(
    named: Option<&str>,
    launcher_only: bool,
    platform: Platform,
) -> Vec<String> {
    if let Some(name) = named {
        return vec![name.to_string()];
    }
    if launcher_only {
        return vec![launcher_default_name(platform).to_string()];
    }
    vec![
        default_name(platform).to_string(),
        launcher_default_name(platform).to_string(),
    ]
}

/// What happened to the launcher when the install went looking for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Launcher {
    /// Registered, and armed.
    Armed,
    /// Written, but the platform could not arm it.
    NotArmed,
    /// Not on the machine.
    Missing,
}

/// The exit status of an install.
///
/// The daemon's own state decides it, and the launcher only makes it
/// worse when the launcher was **written and could not be armed**: a
/// launcher that is not on the machine is not a failure of the daemon,
/// because the daemon carries the index and works without it.
///
/// The status has to keep saying what happened to the daemon. A status
/// that says "nothing was done" when the daemon was done is a status that
/// lies, and a script that reads it would take the daemon back out.
pub fn install_status(daemon_armed: bool, launcher: Launcher) -> u8 {
    match launcher {
        Launcher::Armed => {
            if daemon_armed {
                0
            } else {
                2
            }
        }
        Launcher::NotArmed => 2,
        Launcher::Missing => {
            if daemon_armed {
                0
            } else {
                2
            }
        }
    }
}

/// The arguments the daemon is registered with, when the user names none.
///
/// The root is **always** explicit, and that is the whole point of it.
/// The daemon starts with `--root .`, and the directory a login entry
/// starts in is not the user's home: it is `C:\Windows\System32` on
/// Windows and `/` on macOS. A daemon that inherits the platform's login
/// directory indexes the wrong tree — the System32, or the root of the
/// disk — and says nothing about it.
///
/// The transport is named too, and for the same reason: the daemon's own
/// default is stdio, and stdio ends. A login entry has no standard input
/// to read from, so the daemon reads one EOF and exits — registered,
/// started, and gone. See [`crate::plan::Transport`].
pub fn default_args(root: &str, listen: &str) -> Vec<String> {
    vec![
        "--root".to_string(),
        root.to_string(),
        "--listen".to_string(),
        listen.to_string(),
    ]
}

/// The arguments actually registered: the user's, with the root and the
/// transport in front of them unless the user named one of their own.
///
/// A user who says `--args "--root /data"` gets exactly that root; a user
/// who says `--args "--listen uds:/tmp/x.sock"` gets exactly that
/// transport. The root and the transport are the tool's business when the
/// user stays quiet about them, and the user's when they do not.
pub fn effective_args(user: &[String], root: &str, listen: &str) -> Vec<String> {
    let names_a_root = user
        .iter()
        .any(|arg| arg == "--root" || arg.starts_with("--root="));
    let names_a_listen = user
        .iter()
        .any(|arg| arg == "--listen" || arg.starts_with("--listen="));

    let mut args = Vec::new();
    if !names_a_root && !root.is_empty() {
        args.push("--root".to_string());
        args.push(root.to_string());
    }
    if !names_a_listen && !listen.is_empty() {
        args.push("--listen".to_string());
        args.push(listen.to_string());
    }
    args.extend_from_slice(user);
    args
}

/// The words of a command line, with the double quotes honoured.
///
/// The registered command is read back from the platform's own artefact —
/// a unit file, a plist, a scheduled task — and the user may have edited
/// it. A path with a space in it is one word, and a tokenizer that splits
/// on whitespace alone would turn it into two.
pub fn command_words(command: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut current = String::new();
    let mut quoted = false;
    let mut started = false;
    let mut escaped = false;
    for ch in command.chars() {
        if escaped {
            // The two escapes that exist in practice are the backslash and
            // the quote. Every other backslash is a backslash: a Windows
            // path is full of them, and eating them would name a different
            // tree than the one the daemon walks.
            if ch == '"' || ch == '\\' {
                current.push(ch);
            } else {
                current.push('\\');
                current.push(ch);
            }
            escaped = false;
            started = true;
            continue;
        }
        match ch {
            '\\' if quoted => {
                escaped = true;
            }
            '"' => {
                quoted = !quoted;
                started = true;
            }
            c if c.is_whitespace() && !quoted => {
                if started {
                    words.push(core::mem::take(&mut current));
                    started = false;
                }
            }
            c => {
                current.push(c);
                started = true;
            }
        }
    }
    if escaped {
        // A trailing backslash is a backslash.
        current.push('\\');
    }
    if started {
        words.push(current);
    }
    words
}

/// The root a registered command indexes, if it names one.
///
/// Both spellings are read: `--root /data` and `--root=/data`. The
/// answer is the tree the daemon will walk, and the doctor prints it so
/// the user can see what their daemon is looking at.
pub fn root_of(command: &str) -> Option<String> {
    let words = command_words(command);
    let mut iter = words.iter();
    while let Some(word) = iter.next() {
        if word == "--root" {
            return iter.next().cloned();
        }
        if let Some(root) = word.strip_prefix("--root=") {
            return Some(root.to_string());
        }
    }
    None
}

/// The transport a registered command listens on, if it names one.
///
/// Both spellings are read: `--listen uds:/path` and `--listen=uds:/path`.
/// The answer is where the daemon takes its clients, and the doctor prints
/// it: a client that does not know where the daemon is cannot reach it.
pub fn listen_of(command: &str) -> Option<String> {
    let words = command_words(command);
    let mut iter = words.iter();
    while let Some(word) = iter.next() {
        if word == "--listen" {
            return iter.next().cloned();
        }
        if let Some(value) = word.strip_prefix("--listen=") {
            return Some(value.to_string());
        }
    }
    None
}

/// The directory the platform keeps its cache in, given the environment.
///
/// The same shape the daemon's own index uses (`name-core`'s
/// `persist::cache_dir_from`): `XDG_CACHE_HOME` when it is set, else
/// `~/.cache` on Linux and `~/Library/Caches` on macOS; Windows keeps
/// `%LOCALAPPDATA%`, and `%USERPROFILE%\AppData\Local` when that is unset.
///
/// The socket goes in the same place, so the daemon's index and the
/// socket the clients reach it through are in one directory.
///
/// Split from the path logic so the test does not have to mutate the
/// process environment: a test that sets `HOME` races every other test in
/// the process.
///
/// The rule itself lives in `rgy-proto`, next to the client that has to
/// find this daemon: a registration that writes one path and a client that
/// looks for another is a client that never answers.
#[cfg(feature = "std")]
pub fn cache_dir_from(
    xdg: Option<&std::ffi::OsStr>,
    home: Option<&std::ffi::OsStr>,
    local: Option<&std::ffi::OsStr>,
) -> Option<std::path::PathBuf> {
    rgy_proto::endpoint::cache_dir_from(xdg, home, local)
}

/// The platform's cache directory.
///
/// `None` when the platform has no home to speak of: a machine with
/// neither `XDG_CACHE_HOME` nor `HOME` has nowhere to keep a socket, and
/// the install says so rather than inventing a path in a shared
/// directory.
#[cfg(feature = "std")]
pub fn cache_dir() -> Option<std::path::PathBuf> {
    rgy_proto::endpoint::cache_dir()
}

/// The socket the registered daemon listens on, under a cache directory.
///
/// It lives in the `rgy` directory the daemon already keeps its index in:
/// one directory for this root's files, in the place the platform owns.
#[cfg(feature = "std")]
pub fn socket_path_in(cache_dir: &std::path::Path) -> std::path::PathBuf {
    rgy_proto::endpoint::socket_path_in(cache_dir)
}

/// The name of the named pipe the registered daemon listens on, for a user.
///
/// The pipe namespace belongs to the machine and not to the user, so the
/// user's own name is part of the name: two people logged into the same
/// Windows have two daemons, and each one answers on its own pipe.
///
/// The name is spelled with the characters a pipe name can carry, and
/// anything else becomes an underscore — a name the platform will refuse
/// is a service that does not start.
pub fn pipe_name_for(user: &str) -> String {
    rgy_proto::endpoint::pipe_name_for(user)
}

/// The named pipe the registered daemon listens on.
#[cfg(feature = "std")]
pub fn pipe_name() -> String {
    rgy_proto::endpoint::pipe_name()
}

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
#[cfg(feature = "std")]
#[derive(Debug, Clone)]
pub struct Report {
    /// The mechanism that is in place.
    pub mechanism: Mechanism,
    /// The file that was written, when the mechanism has one.
    pub artifact: Option<std::path::PathBuf>,
    /// The program command line that was registered.
    pub command: String,
    /// Whether the platform's own tool was told to arm it.
    pub armed: bool,
    /// What the user should know, in sentences.
    pub notes: Vec<String>,
}

/// What an uninstall did, in words.
#[cfg(feature = "std")]
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
#[cfg(feature = "std")]
pub fn home_dir() -> std::path::PathBuf {
    std::env::var_os(home_key())
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from("."))
}

/// The environment variable that holds the user's home directory.
#[cfg(feature = "std")]
fn home_key() -> &'static str {
    if cfg!(windows) { "USERPROFILE" } else { "HOME" }
}

/// The path as the user should read it.
///
/// `canonicalize` on Windows returns the long-path form (`\\?\C:\...`),
/// which the file APIs want and a *command line* does not: a scheduled
/// task created with that prefix holds a command that cannot be started.
/// So the prefix comes off before the path is written into anything the
/// platform will run.
#[cfg(feature = "std")]
pub fn display_path(path: &std::path::Path) -> String {
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
#[cfg(feature = "std")]
pub fn resolve_binary(path: &std::path::Path) -> Result<std::path::PathBuf, String> {
    if !path.exists() {
        return Err(format!("there is no file at {}", path.display()));
    }
    if !path.is_file() {
        return Err(format!("{} is not a file", path.display()));
    }
    std::fs::canonicalize(path).map_err(|e| format!("cannot resolve {}: {e}", path.display()))
}

/// The daemon this tool registers.
///
/// Two places are looked at, in this order: the directory this tool is in
/// — the release installs the two side by side — and the PATH. The error
/// names both, because "there is no daemon" without saying where it
/// looked is a sentence the user cannot act on.
#[cfg(feature = "std")]
pub fn find_daemon() -> Result<std::path::PathBuf, String> {
    let sibling = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.to_path_buf()));
    find_daemon_in(sibling.as_deref(), std::env::var_os("PATH").as_deref())
}

/// The launcher this tool registers.
///
/// The same two places, and in the same order: next to this tool, and
/// then the PATH.
#[cfg(feature = "std")]
pub fn find_launcher() -> Result<std::path::PathBuf, String> {
    let sibling = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.to_path_buf()));
    find_launcher_in(sibling.as_deref(), std::env::var_os("PATH").as_deref())
}

/// The launcher, looked for in two places.
///
/// The launcher is optional: the daemon is the product and the launcher
/// is a face on it. A machine without the launcher has a daemon and no
/// window, and that is a machine that works.
#[cfg(feature = "std")]
pub fn find_launcher_in(
    sibling_dir: Option<&std::path::Path>,
    path_var: Option<&std::ffi::OsStr>,
) -> Result<std::path::PathBuf, String> {
    let mut looked = Vec::new();

    if let Some(dir) = sibling_dir {
        let candidate = dir.join(LAUNCHER_NAME);
        looked.push(candidate.display().to_string());
        if candidate.is_file() && is_executable(&candidate) {
            return resolve_binary(&candidate);
        }
    }

    if let Some(path) = path_var {
        for dir in std::env::split_paths(path) {
            let candidate = dir.join(LAUNCHER_NAME);
            if candidate.is_file() && is_executable(&candidate) {
                return resolve_binary(&candidate);
            }
        }
        looked.push(format!("the PATH ({})", path.to_string_lossy()));
    }

    Err(format!(
        "there is no {LAUNCHER_NAME} next to this tool and none on the PATH.\n  \
         looked in: {}\n  \
         the launcher is optional: the daemon works without it",
        if looked.is_empty() {
            "nowhere: this tool has no directory and the PATH is empty".to_string()
        } else {
            looked.join(", ")
        }
    ))
}

/// The daemon, looked for in two places.
///
/// The two places are a parameter and not the environment, so the search
/// can be tested without a machine: a test that looks for the daemon on
/// the machine running it finds the one the build put next to the test
/// binary, and registers it.
#[cfg(feature = "std")]
pub fn find_daemon_in(
    sibling_dir: Option<&std::path::Path>,
    path_var: Option<&std::ffi::OsStr>,
) -> Result<std::path::PathBuf, String> {
    let mut looked = Vec::new();

    if let Some(dir) = sibling_dir {
        let candidate = dir.join(DAEMON_NAME);
        looked.push(candidate.display().to_string());
        if candidate.is_file() && is_executable(&candidate) {
            return resolve_binary(&candidate);
        }
    }

    if let Some(path) = path_var {
        for dir in std::env::split_paths(path) {
            let candidate = dir.join(DAEMON_NAME);
            if candidate.is_file() && is_executable(&candidate) {
                return resolve_binary(&candidate);
            }
        }
        looked.push(format!("the PATH ({})", path.to_string_lossy()));
    }

    Err(format!(
        "there is no {DAEMON_NAME} next to this tool and none on the PATH.\n  \
         looked in: {}\n  \
         give the daemon's path with: --binary <path>",
        if looked.is_empty() {
            "nowhere: this tool has no directory and the PATH is empty".to_string()
        } else {
            looked.join(", ")
        }
    ))
}

/// Whether the file can be run.
///
/// On unix the bit is the answer; on Windows a file that is there is a
/// file that can be run.
#[cfg(all(feature = "std", unix))]
pub fn is_executable(path: &std::path::Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .map(|meta| meta.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(all(feature = "std", not(unix)))]
pub fn is_executable(path: &std::path::Path) -> bool {
    path.is_file()
}

/// Split a daemon argument string into arguments.
///
/// The platform tools take an argument *list* (a launchd `ProgramArguments`
/// array, a systemd `ExecStart` line), while the user thinks in one string.
/// The split is on whitespace, which is what the shell would have done, and
/// it is deliberately not a shell: nothing here is quoted, escaped, or
/// interpreted.
pub fn split_args(text: &str) -> Vec<String> {
    text.split_whitespace()
        .map(alloc::string::ToString::to_string)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::string::ToString;
    use alloc::vec;

    /// An uninstall takes the launcher out with the daemon.
    ///
    /// An install that registered the launcher registered two things, and
    /// an uninstall that removes one of them leaves the other starting at
    /// every login.
    #[test]
    fn an_uninstall_takes_the_launcher_out_with_the_daemon() {
        let targets = uninstall_targets(None, false, Platform::Linux);
        assert_eq!(targets.len(), 2, "the daemon and the launcher: {targets:?}");
        assert!(targets.contains(&default_name(Platform::Linux).to_string()));
        assert!(targets.contains(&launcher_default_name(Platform::Linux).to_string()));
    }

    /// The macOS label is the launchd one, and the uninstall asks for it.
    #[test]
    fn the_launchers_name_is_the_platforms_own() {
        let targets = uninstall_targets(None, false, Platform::MacOs);
        assert!(targets.contains(&"com.rgy.rgxy".to_string()), "{targets:?}");
        let targets = uninstall_targets(None, false, Platform::Windows);
        assert!(targets.contains(&"rgy-rgxy".to_string()), "{targets:?}");
    }

    /// A name the user gave is the one entry they meant.
    #[test]
    fn a_named_uninstall_removes_that_one() {
        let targets = uninstall_targets(Some("my-entry"), false, Platform::Linux);
        assert_eq!(targets, vec!["my-entry".to_string()]);
    }

    /// The launcher alone is the launcher's entry alone.
    #[test]
    fn the_launcher_alone_is_its_own_entry() {
        let targets = uninstall_targets(None, true, Platform::Linux);
        assert_eq!(targets, vec!["rgy-rgxy".to_string()]);
    }

    /// A launcher that is not on the machine does not undo the daemon.
    ///
    /// The status says what happened to the daemon, because a status that
    /// says "nothing was done" when the daemon was done is a status that
    /// lies.
    #[test]
    fn a_missing_launcher_does_not_undo_the_daemon() {
        assert_eq!(install_status(true, Launcher::Missing), 0);
        assert_eq!(install_status(false, Launcher::Missing), 2);
    }

    /// A launcher that was written and could not be armed is said so.
    #[test]
    fn a_launcher_that_could_not_be_armed_is_said_so() {
        assert_eq!(install_status(true, Launcher::NotArmed), 2);
        assert_eq!(install_status(true, Launcher::Armed), 0);
    }

    #[cfg(feature = "std")]
    #[test]
    fn a_binary_that_is_not_there_is_refused() {
        let err = resolve_binary(std::path::Path::new("/definitely/not/here/searchd")).unwrap_err();
        assert!(err.contains("there is no file at"), "{err}");
    }

    #[cfg(feature = "std")]
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

    #[test]
    fn the_root_is_always_named() {
        assert_eq!(
            default_args("/home/u", "uds:/home/u/.cache/rgy/searchd.sock"),
            vec![
                "--root".to_string(),
                "/home/u".to_string(),
                "--listen".to_string(),
                "uds:/home/u/.cache/rgy/searchd.sock".to_string()
            ]
        );
    }

    #[test]
    fn the_users_arguments_go_after_the_root_and_the_transport() {
        let user = vec!["--json".to_string()];
        assert_eq!(
            effective_args(&user, "/home/u", "uds:/home/u/.cache/rgy/searchd.sock"),
            vec![
                "--root".to_string(),
                "/home/u".to_string(),
                "--listen".to_string(),
                "uds:/home/u/.cache/rgy/searchd.sock".to_string(),
                "--json".to_string()
            ]
        );
    }

    #[test]
    fn a_root_the_user_named_wins() {
        let user = vec!["--root".to_string(), "/data".to_string()];
        let args = effective_args(&user, "/home/u", "uds:/x");
        assert!(
            args.windows(2).any(|pair| pair == ["--root", "/data"]),
            "{args:?}"
        );
        assert!(
            !args.windows(2).any(|pair| pair == ["--root", "/home/u"]),
            "the user's root is the one that counts: {args:?}"
        );

        // The inline spelling is a root too.
        let inline = vec!["--root=/data".to_string()];
        let args = effective_args(&inline, "/home/u", "uds:/x");
        assert!(
            args.iter().any(|arg| arg == "--root=/data"),
            "the user's root is kept as it was written: {args:?}"
        );
        assert!(
            !args.iter().any(|arg| arg == "/home/u"),
            "the tool's root must not be there as well: {args:?}"
        );
    }

    #[test]
    fn a_transport_the_user_named_wins() {
        let user = vec!["--listen".to_string(), "uds:/tmp/x.sock".to_string()];
        let args = effective_args(&user, "/home/u", "uds:/home/u/.cache/rgy/searchd.sock");
        assert!(
            args.windows(2)
                .any(|pair| pair == ["--listen", "uds:/tmp/x.sock"]),
            "{args:?}"
        );
        assert!(
            !args
                .windows(2)
                .any(|pair| pair[0] == "--listen" && pair[1].contains(".cache")),
            "the user's transport is the one that counts: {args:?}"
        );
    }

    #[test]
    fn the_transport_is_read_out_of_a_registered_command() {
        assert_eq!(
            listen_of("/opt/rgy/searchd --root /home/u --listen uds:/tmp/x.sock"),
            Some("uds:/tmp/x.sock".to_string())
        );
        assert_eq!(
            listen_of("/opt/rgy/searchd --listen=pipe:rgy-searchd-ana"),
            Some("pipe:rgy-searchd-ana".to_string())
        );
        assert_eq!(listen_of("/opt/rgy/searchd"), None);
    }

    #[test]
    fn the_pipe_name_carries_the_user() {
        assert_eq!(pipe_name_for("ana"), "rgy-searchd-ana");
        assert_eq!(pipe_name_for("Ana Maria"), "rgy-searchd-Ana_Maria");
        assert_eq!(pipe_name_for(""), "rgy-searchd");
    }

    #[cfg(feature = "std")]
    #[test]
    fn the_cache_directory_is_the_platforms_own() {
        if cfg!(windows) {
            // Windows keeps its local app data, and falls back to the
            // profile when that is unset.
            assert_eq!(
                cache_dir_from(
                    None,
                    Some(std::ffi::OsStr::new(r"C:\Users\u")),
                    Some(std::ffi::OsStr::new(r"C:\Users\u\AppData\Local"))
                ),
                Some(std::path::PathBuf::from(r"C:\Users\u\AppData\Local"))
            );
            assert_eq!(
                cache_dir_from(None, Some(std::ffi::OsStr::new(r"C:\Users\u")), None),
                Some(std::path::PathBuf::from(r"C:\Users\u\AppData\Local"))
            );
            return;
        }
        assert_eq!(
            cache_dir_from(None, Some(std::ffi::OsStr::new("/home/u")), None),
            Some(if cfg!(target_os = "macos") {
                std::path::PathBuf::from("/home/u/Library/Caches")
            } else {
                std::path::PathBuf::from("/home/u/.cache")
            })
        );
        // The XDG variable wins when it is set.
        assert_eq!(
            cache_dir_from(
                Some(std::ffi::OsStr::new("/var/cache/u")),
                Some(std::ffi::OsStr::new("/home/u")),
                None
            ),
            Some(std::path::PathBuf::from("/var/cache/u"))
        );
        assert_eq!(cache_dir_from(None, None, None), None);
    }

    #[cfg(feature = "std")]
    #[test]
    fn the_socket_lives_where_the_index_lives() {
        assert_eq!(
            socket_path_in(std::path::Path::new("/home/u/.cache")),
            std::path::PathBuf::from("/home/u/.cache/rgy/searchd.sock")
        );
    }

    #[test]
    fn a_quoted_path_is_one_word() {
        let words = command_words(r#""C:\Program Files\rgy\searchd.exe" --root "C:\Users\a b""#);
        assert_eq!(
            words,
            vec![
                "C:\\Program Files\\rgy\\searchd.exe".to_string(),
                "--root".to_string(),
                "C:\\Users\\a b".to_string()
            ]
        );
    }

    #[test]
    fn the_root_is_read_out_of_a_registered_command() {
        assert_eq!(
            root_of(r#""C:\rgy\searchd.exe" --root "C:\Users\a b""#),
            Some("C:\\Users\\a b".to_string())
        );
        assert_eq!(
            root_of("/opt/rgy/searchd --root=/data"),
            Some("/data".to_string())
        );
        assert_eq!(root_of("/opt/rgy/searchd"), None);
    }

    #[cfg(feature = "std")]
    fn temp_dir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("rgy-autostart-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("a temporary directory");
        dir
    }

    /// A file the platform would run.
    ///
    /// On unix the bit is the answer, and a file without it is not a
    /// daemon however much it looks like one.
    #[cfg(feature = "std")]
    fn make_runnable(path: &std::path::Path) {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))
                .expect("the file is runnable");
        }
        #[cfg(not(unix))]
        let _ = path;
    }

    #[cfg(feature = "std")]
    #[test]
    fn the_daemon_next_to_the_tool_is_found() {
        let dir = temp_dir("find-sibling");
        let daemon = dir.join(DAEMON_NAME);
        std::fs::write(&daemon, b"not really a daemon").expect("the file is there");
        make_runnable(&daemon);

        let found = find_daemon_in(Some(&dir), None).expect("the daemon is found");
        assert_eq!(found.file_name(), Some(DAEMON_NAME.as_ref()));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(feature = "std")]
    #[test]
    fn the_daemon_on_the_path_is_found() {
        let dir = temp_dir("find-path");
        let daemon = dir.join(DAEMON_NAME);
        std::fs::write(&daemon, b"not really a daemon").expect("the file is there");
        make_runnable(&daemon);

        let path = std::env::join_paths([&dir]).expect("the PATH is one entry");
        let found = find_daemon_in(None, Some(path.as_os_str())).expect("the daemon is found");
        assert_eq!(found.file_name(), Some(DAEMON_NAME.as_ref()));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(feature = "std")]
    #[cfg(unix)]
    #[test]
    fn a_file_that_cannot_be_run_is_not_a_daemon() {
        let dir = temp_dir("find-not-runnable");
        let daemon = dir.join(DAEMON_NAME);
        std::fs::write(&daemon, b"not really a daemon").expect("the file is there");

        let err = find_daemon_in(Some(&dir), None).unwrap_err();
        assert!(err.contains("none on the PATH"), "{err}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(feature = "std")]
    #[test]
    fn a_daemon_that_is_nowhere_says_where_it_looked() {
        let dir = temp_dir("find-nothing");
        let err = find_daemon_in(Some(&dir), None).unwrap_err();
        assert!(err.contains("next to this tool"), "{err}");
        assert!(err.contains("none on the PATH"), "{err}");
        assert!(err.contains("--binary"), "{err}");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
