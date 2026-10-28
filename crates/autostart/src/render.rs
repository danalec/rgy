// SPDX-License-Identifier: Apache-2.0

//! The text the user gets to read and edit.
//!
//! The unit and the plist are written out as files, in full: a user who
//! wants a different `Restart=`, a different limit, or an extra argument
//! edits the file rather than looking for a flag this tool happens to
//! support. Windows has no file form for a scheduled task, so the command
//! line *is* the artifact — it is printed for the user to read, and it is
//! the same line the tool hands to `schtasks`.

use alloc::format;
use alloc::string::{String, ToString};
use std::path::Path;

use crate::display_path;

/// The directory the daemon indexes, read out of the arguments.
///
/// The arguments carry the root (`--root <dir>`), and the artefact says
/// the same directory twice: once as the tree to walk, once as the place
/// to walk it from. One source of truth — the arguments — so the two can
/// never disagree.
pub fn working_directory(args: &[String]) -> Option<String> {
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        if arg == "--root" {
            return iter.next().cloned();
        }
        if let Some(root) = arg.strip_prefix("--root=") {
            return Some(root.to_string());
        }
    }
    None
}

/// The systemd user unit that starts the daemon at login.
///
/// The directives that matter are all here, in the open:
///
///   * `WantedBy=default.target` — the user's own target, so the unit
///     starts with the session and not with the machine's boot.
///   * `Restart=on-failure` — the daemon comes back when it dies, which is
///     the other half of "the daemon is there when the user needs it".
///   * `WorkingDirectory=` — the directory the daemon indexes. It is
///     written out because the default is not it: a user unit starts in
///     the home directory, and the daemon has to be told the tree it walks
///     rather than the one it happens to inherit.
///
/// The unit is a *user* unit: it needs no root, it follows the user, and
/// it stops with the session.
pub fn systemd_unit(binary: &Path, args: &[String], unit_name: &str) -> String {
    let mut exec = format!("ExecStart={}", quote_systemd(&display_path(binary)));
    for arg in args {
        exec.push(' ');
        exec.push_str(&quote_systemd(arg));
    }

    // The working directory is the tree the daemon indexes, read out of
    // the arguments. A unit with no root in its arguments says nothing
    // about where it runs, and the platform's default is not a promise.
    //
    // The path is written bare: systemd's `WorkingDirectory=` is a path,
    // not a word list, and it takes the quotes as part of the path. A
    // path with a space in it is fine -- the whole rest of the line is
    // the path.
    let workdir = match working_directory(args) {
        Some(dir) => format!("WorkingDirectory={dir}\n"),
        None => String::new(),
    };

    format!(
        r#"# SPDX-License-Identifier: Apache-2.0
#
# rgy search daemon, started at login by the user's systemd manager.
#
# Written by rgy-autostart as plain text: edit it, then run
#   systemctl --user daemon-reload
#   systemctl --user enable --now {unit}
# to pick the change up. To remove it:
#   systemctl --user disable --now {unit}
# and delete this file.
[Unit]
Description=rgy search daemon
After=default.target

[Service]
Type=simple
{exec}
{workdir}Restart=on-failure
RestartSec=5s

[Install]
WantedBy=default.target
"#,
        unit = unit_name,
        exec = exec,
        workdir = workdir,
    )
}

/// The launchd agent that starts the daemon at login.
///
/// `RunAtLoad` starts it when the agent is loaded — at login — and
/// `KeepAlive` brings it back when it exits, which is launchd's word for
/// the same thing `Restart=` means to systemd.
pub fn launchd_plist(binary: &Path, args: &[String], label: &str) -> String {
    let mut program = format!(
        "        <string>{}</string>\n",
        xml_escape(&display_path(binary))
    );
    for arg in args {
        program.push_str(&format!("        <string>{}</string>\n", xml_escape(arg)));
    }

    // The working directory is the tree the daemon indexes, read out of
    // the arguments. launchd starts a job in `/`, and a daemon that
    // inherits `/` walks the whole disk from the top -- every permission
    // error on the way.
    let workdir = match working_directory(args) {
        Some(dir) => format!(
            "    <key>WorkingDirectory</key>\n    <string>{}</string>\n",
            xml_escape(&dir)
        ),
        None => String::new(),
    };

    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<!-- SPDX-License-Identifier: Apache-2.0 -->
<!-- rgy search daemon, started at login by launchd. -->
<!-- Written by rgy-autostart as plain text: edit it, then run -->
<!--   launchctl bootstrap gui/$(id -u) ~/Library/LaunchAgents/{label}.plist -->
<!-- to pick the change up. To remove it: -->
<!--   launchctl bootout gui/$(id -u)/{label} -->
<!-- and delete this file. -->
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>{label}</string>
    <key>ProgramArguments</key>
    <array>
{program}    </array>
{workdir}    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <true/>
    <key>ProcessType</key>
    <string>Background</string>
</dict>
</plist>
"#,
        label = xml_escape(label),
        program = program,
        workdir = workdir,
    )
}

/// The program command line a scheduled task runs.
///
/// The task scheduler has no file form, so this line *is* the artifact:
/// it is printed for the user to read, and it is what the tool hands to
/// `schtasks`.
///
/// The arguments are quoted when they have to be. The root the daemon
/// indexes is the user's home directory, and a home directory can be
/// `C:\Users\Ana Maria` — an unquoted path with a space in it is two
/// arguments, and the daemon would walk `C:\Users\Ana` instead.
pub fn windows_task_command(binary: &Path, args: &[String]) -> String {
    let mut line = format!("\"{}\"", display_path(binary));
    for arg in args {
        line.push(' ');
        line.push_str(&quote_windows(arg));
    }
    line
}

/// A command-line word, quoted when it has to be.
fn quote_windows(word: &str) -> String {
    if word.contains(' ') || word.contains('"') {
        format!("\"{}\"", word.replace('"', "\\\""))
    } else {
        word.to_string()
    }
}

/// The `schtasks` line, as it is typed in `cmd.exe`.
///
/// The inner quotes of the `/TR` value are escaped the way `cmd.exe`
/// needs them. The tool itself passes the value as one argument, so it
/// does not depend on that escaping — the line is here so the user can
/// see, copy, and re-run it.
pub fn schtasks_command_line(binary: &Path, args: &[String], task_name: &str) -> String {
    let tr = windows_task_command(binary, args).replace('"', "\\\"");
    format!(
        "schtasks /Create /TN \"{task}\" /TR \"{tr}\" /SC ONLOGON /IT /RL LIMITED /F",
        task = task_name,
        tr = tr,
    )
}

/// The `reg` line the fallback writes, as it is typed in `cmd.exe`.
pub fn reg_add_command_line(binary: &Path, args: &[String], value_name: &str) -> String {
    let data = windows_task_command(binary, args).replace('"', "\\\"");
    format!(
        "reg add \"{key}\" /v {value} /t REG_SZ /d \"{data}\" /f",
        key = crate::RUN_KEY,
        value = value_name,
        data = data,
    )
}

/// A systemd word, quoted.
///
/// systemd's `Exec` lines take C-style quoting, so a path with a space —
/// or a backslash, on the odd system — has to be quoted and escaped.
/// Without it the unit file names the wrong binary, and the failure shows
/// up at login, where nobody is looking.
fn quote_systemd(word: &str) -> String {
    let mut out = String::with_capacity(word.len() + 2);
    out.push('"');
    for ch in word.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// XML text, escaped.
///
/// The plist is XML, and a path with an ampersand in it — a name like
/// `A & B` — has to be escaped or the plist does not parse, and launchd
/// says nothing useful about it.
fn xml_escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::string::ToString;
    use alloc::vec::Vec;
    use std::path::PathBuf;

    fn args(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn the_unit_names_the_daemon_and_survives_a_crash() {
        let unit = systemd_unit(
            &PathBuf::from("/opt/rgy/bin/searchd"),
            &[],
            "rgy-searchd.service",
        );
        assert!(
            unit.contains("ExecStart=\"/opt/rgy/bin/searchd\""),
            "{unit}"
        );
        assert!(unit.contains("Restart=on-failure"), "{unit}");
        assert!(unit.contains("WantedBy=default.target"), "{unit}");
        assert!(unit.contains("[Install]"), "{unit}");
        assert!(unit.contains("[Service]"), "{unit}");
    }

    #[test]
    fn the_unit_is_a_user_unit_and_asks_for_no_elevation() {
        let unit = systemd_unit(
            &PathBuf::from("/opt/rgy/bin/searchd"),
            &[],
            "rgy-searchd.service",
        );
        // A system unit would tie itself to the machine's boot; this one
        // ties itself to the user's session.
        assert!(!unit.contains("multi-user.target"), "{unit}");
        assert!(!unit.contains("sudo"), "{unit}");
        assert!(!unit.contains("pkexec"), "{unit}");
    }

    #[test]
    fn the_unit_carries_the_daemon_arguments() {
        let unit = systemd_unit(
            &PathBuf::from("/opt/rgy/bin/searchd"),
            &args(&["--index", "/var/lib/rgy/index"]),
            "rgy-searchd.service",
        );
        assert!(
            unit.contains("ExecStart=\"/opt/rgy/bin/searchd\" \"--index\" \"/var/lib/rgy/index\""),
            "{unit}"
        );
    }

    #[test]
    fn a_path_with_a_space_is_quoted_in_the_unit() {
        let unit = systemd_unit(
            &PathBuf::from("/opt/rgy tools/searchd"),
            &[],
            "rgy-searchd.service",
        );
        assert!(
            unit.contains("ExecStart=\"/opt/rgy tools/searchd\""),
            "{unit}"
        );
    }

    #[test]
    fn the_plist_runs_at_load_and_stays_up() {
        let plist = launchd_plist(
            &PathBuf::from("/opt/rgy/bin/searchd"),
            &[],
            "com.rgy.searchd",
        );
        assert!(plist.contains("<key>RunAtLoad</key>"), "{plist}");
        assert!(plist.contains("<key>KeepAlive</key>"), "{plist}");
        assert!(plist.contains("<true/>"), "{plist}");
        assert!(
            plist.contains("<string>com.rgy.searchd</string>"),
            "{plist}"
        );
        assert!(
            plist.contains("<string>/opt/rgy/bin/searchd</string>"),
            "{plist}"
        );
    }

    #[test]
    fn the_plist_escapes_what_xml_would_refuse() {
        let plist = launchd_plist(&PathBuf::from("/opt/A & B/searchd"), &[], "com.rgy.searchd");
        assert!(plist.contains("/opt/A &amp; B/searchd"), "{plist}");
    }

    #[test]
    fn the_task_command_quotes_the_binary() {
        let command = windows_task_command(
            &PathBuf::from(r"C:\Program Files\rgy\searchd.exe"),
            &args(&["--json"]),
        );
        assert_eq!(command, r#""C:\Program Files\rgy\searchd.exe" --json"#);
    }

    #[test]
    fn the_task_command_line_is_shown_to_the_user() {
        let line = schtasks_command_line(&PathBuf::from(r"C:\rgy\searchd.exe"), &[], "rgy-searchd");
        assert!(line.contains("/Create"), "{line}");
        assert!(line.contains("/TN \"rgy-searchd\""), "{line}");
        assert!(line.contains("/SC ONLOGON"), "{line}");
        // Limited rights: the task runs as the user, with no elevation.
        assert!(line.contains("/RL LIMITED"), "{line}");
        assert!(!line.contains("HIGHEST"), "{line}");
    }

    #[test]
    fn the_registry_fallback_stays_in_the_user_hive() {
        let line = reg_add_command_line(&PathBuf::from(r"C:\rgy\searchd.exe"), &[], "rgy-searchd");
        assert!(line.contains("HKCU"), "{line}");
        assert!(!line.contains("HKLM"), "{line}");
    }

    #[test]
    fn the_working_directory_is_the_tree_the_daemon_indexes() {
        assert_eq!(
            working_directory(&args(&["--root", "/home/u"])),
            Some("/home/u".to_string())
        );
        assert_eq!(
            working_directory(&args(&["--root=/data"])),
            Some("/data".to_string())
        );
        assert_eq!(working_directory(&args(&["--json"])), None);
    }

    #[test]
    fn the_unit_says_where_the_daemon_runs() {
        let unit = systemd_unit(
            &PathBuf::from("/opt/rgy/bin/searchd"),
            &args(&["--root", "/home/u"]),
            "rgy-searchd.service",
        );
        // The path is bare: systemd's WorkingDirectory= takes the quotes
        // as part of the path, and a quoted path is "not absolute".
        assert!(unit.contains("WorkingDirectory=/home/u"), "{unit}");
        assert!(!unit.contains("WorkingDirectory=\""), "{unit}");
    }

    #[test]
    fn a_unit_without_a_root_says_nothing_about_where_it_runs() {
        let unit = systemd_unit(
            &PathBuf::from("/opt/rgy/bin/searchd"),
            &[],
            "rgy-searchd.service",
        );
        assert!(!unit.contains("WorkingDirectory"), "{unit}");
    }

    #[test]
    fn the_plist_says_where_the_daemon_runs() {
        let plist = launchd_plist(
            &PathBuf::from("/opt/rgy/bin/searchd"),
            &args(&["--root", "/Users/u"]),
            "com.rgy.searchd",
        );
        assert!(plist.contains("<key>WorkingDirectory</key>"), "{plist}");
        assert!(plist.contains("<string>/Users/u</string>"), "{plist}");
    }

    #[test]
    fn a_root_with_a_space_is_quoted_in_the_task_command() {
        let command = windows_task_command(
            &PathBuf::from(r"C:\rgy\searchd.exe"),
            &args(&["--root", r"C:\Users\Ana Maria"]),
        );
        assert_eq!(
            command,
            r#""C:\rgy\searchd.exe" --root "C:\Users\Ana Maria""#
        );
    }
}
