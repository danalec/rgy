// SPDX-License-Identifier: Apache-2.0

//! The systemd user unit.
//!
//! A **user** unit, not a system one: it needs no root, it follows the
//! user (a system unit would need the login to be a graphical one before
//! it starts anything), and it stops when the user's session ends. The
//! unit is written out as a file the user can read and edit — see
//! [`crate::render::systemd_unit`].

use alloc::format;
use alloc::string::{String, ToString};
use std::path::{Path, PathBuf};

use crate::render::systemd_unit;

/// The directory that holds the user's systemd units.
///
/// `XDG_CONFIG_HOME` comes first, because that is where systemd looks for
/// them, and because a test — and the verification run — needs to point
/// the whole thing at a temporary directory.
pub fn config_home() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| Some(crate::home_dir().join(".config")))
        .unwrap_or_else(|| PathBuf::from(".config"))
}

/// The unit file's path under `config_home`.
pub fn unit_path_in(config_home: &Path, unit_name: &str) -> PathBuf {
    config_home.join("systemd").join("user").join(unit_name)
}

/// The unit file's path for the running user.
pub fn unit_path(unit_name: &str) -> PathBuf {
    unit_path_in(&config_home(), unit_name)
}

/// Write the unit. Returns whether the file changed.
///
/// Writing the same text again changes nothing, so installing twice leaves
/// one unit and not two, and the second install does not churn the file's
/// timestamp for no reason.
pub fn write_unit(
    config_home: &Path,
    unit_name: &str,
    binary: &Path,
    args: &[String],
) -> std::io::Result<bool> {
    let path = unit_path_in(config_home, unit_name);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let text = systemd_unit(binary, args, unit_name);
    if let Ok(current) = std::fs::read_to_string(&path)
        && current == text
    {
        return Ok(false);
    }
    std::fs::write(&path, text)?;
    Ok(true)
}

/// Remove the unit file and the symlink that enables it.
///
/// The symlink lives in `default.target.wants/` and is what `enable`
/// created; removing the file alone would leave the entry dangling, and
/// the next `systemctl --user daemon-reload` would complain about it.
pub fn remove_unit(config_home: &Path, unit_name: &str) -> std::io::Result<bool> {
    let path = unit_path_in(config_home, unit_name);
    let wants = config_home
        .join("systemd")
        .join("user")
        .join("default.target.wants")
        .join(unit_name);

    let mut removed = false;
    // `symlink_metadata` rather than `exists`: a symlink whose target is
    // gone still has to be removed, and `exists` says it does not.
    if wants.symlink_metadata().is_ok() {
        std::fs::remove_file(&wants)?;
        removed = true;
    }
    if path.symlink_metadata().is_ok() {
        std::fs::remove_file(&path)?;
        removed = true;
    }
    Ok(removed)
}

/// The command line the unit registers, read back from the file.
///
/// The status command reads what is *there* rather than what the tool
/// would write: the user may have edited the unit, and the edited unit is
/// the one that runs.
pub fn registered_command(config_home: &Path, unit_name: &str) -> Option<String> {
    let text = std::fs::read_to_string(unit_path_in(config_home, unit_name)).ok()?;
    let line = text
        .lines()
        .find(|line| line.trim_start().starts_with("ExecStart="))?
        .trim_start()
        .trim_start_matches("ExecStart=")
        .trim();
    // The line as the unit spells it, quotes and all. The quoting is what
    // keeps a path with a space in it one word, so it has to survive to
    // whoever reads it: stripping the quotes here turned
    // `"/home/Ana Maria"` into two words, and the doctor then reported the
    // root as `/home/Ana`.
    Some(line.to_string())
}

/// Arm the unit: reload the manager, then enable and start it.
///
/// `enable --now` is the whole job in one call; the reload before it is
/// what makes the manager see the file that was just written.
pub fn enable(unit_name: &str) -> Result<String, String> {
    // A reload that fails is not fatal on its own: the manager may still
    // pick the file up, and `enable` is the call that decides.
    let reload = systemctl(&["daemon-reload"]);
    match systemctl(&["enable", "--now", unit_name]) {
        Ok(out) => Ok(out),
        Err(e) => Err(match reload {
            Err(reload_err) => format!("{e} (and the reload failed too: {reload_err})"),
            Ok(_) => e,
        }),
    }
}

/// Disarm the unit: stop it and drop the symlink.
pub fn disable(unit_name: &str) -> Result<String, String> {
    systemctl(&["disable", "--now", unit_name])
}

/// What `systemctl --user is-enabled` says, if it says anything.
///
/// A non-zero exit is an answer here, not a failure: `is-enabled` prints
/// `disabled` and exits 1 when the unit is there but not enabled.
pub fn is_enabled(unit_name: &str) -> Option<String> {
    let output = std::process::Command::new("systemctl")
        .arg("--user")
        .arg("is-enabled")
        .arg(unit_name)
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if text.is_empty() { None } else { Some(text) }
}

/// What `systemctl --user is-active` says, if it says anything.
pub fn is_active(unit_name: &str) -> Option<String> {
    let output = std::process::Command::new("systemctl")
        .arg("--user")
        .arg("is-active")
        .arg(unit_name)
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if text.is_empty() { None } else { Some(text) }
}

/// The Linux side, right now.
#[derive(Debug, Clone, Default)]
pub struct State {
    /// The unit file is there.
    pub file: bool,
    /// What `systemctl --user is-enabled` says.
    pub enabled: Option<String>,
    /// What `systemctl --user is-active` says.
    pub active: Option<String>,
}

/// The state of the autostart entry for this user.
pub fn status(config_home: &Path, unit_name: &str) -> State {
    State {
        file: unit_path_in(config_home, unit_name).is_file(),
        enabled: is_enabled(unit_name),
        active: is_active(unit_name),
    }
}

/// Run `systemctl --user` with the given arguments.
fn systemctl(args: &[&str]) -> Result<String, String> {
    let output = std::process::Command::new("systemctl")
        .arg("--user")
        .args(args)
        .output()
        .map_err(|e| format!("systemctl is not there to run: {e}"))?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
        let detail = if stderr.is_empty() { stdout } else { stderr };
        Err(format!(
            "systemctl --user {} failed: {}",
            args.join(" "),
            if detail.is_empty() {
                "(no message)".to_string()
            } else {
                detail
            }
        ))
    }
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
    fn the_unit_lives_under_the_user_config_home() {
        let path = unit_path_in(Path::new("/home/u/.config"), "rgy-searchd.service");
        assert_eq!(
            path,
            PathBuf::from("/home/u/.config/systemd/user/rgy-searchd.service")
        );
    }

    #[test]
    fn install_uninstall_install_leaves_one_unit_and_the_same_text() {
        let home = temp_dir("linux-round-trip");
        let binary = PathBuf::from("/opt/rgy/bin/searchd");
        let name = "rgy-searchd.service";

        // The first install writes the unit.
        assert!(write_unit(&home, name, &binary, &[]).expect("the unit is written"));
        let first = std::fs::read_to_string(unit_path_in(&home, name)).expect("the unit is there");
        assert!(first.contains("WantedBy=default.target"), "{first}");
        assert!(first.contains("Restart=on-failure"), "{first}");

        // The second install writes the same text: nothing changes.
        assert!(
            !write_unit(&home, name, &binary, &[]).expect("the unit is written"),
            "a second install changed the unit"
        );
        let second = std::fs::read_to_string(unit_path_in(&home, name)).expect("the unit is there");
        assert_eq!(
            first, second,
            "the unit's text changed under a second install"
        );

        // Uninstalling takes it away, and says there was something there.
        assert!(remove_unit(&home, name).expect("the unit is removed"));
        assert!(!unit_path_in(&home, name).exists());

        // Uninstalling again finds nothing, and says so.
        assert!(
            !remove_unit(&home, name).expect("the removal is quiet"),
            "a second uninstall found something to remove"
        );

        // And the third install is the first one again.
        assert!(write_unit(&home, name, &binary, &[]).expect("the unit is written"));
        let third = std::fs::read_to_string(unit_path_in(&home, name)).expect("the unit is there");
        assert_eq!(first, third, "the unit's text changed over the round trip");

        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn the_enabled_entry_goes_with_the_unit() {
        let home = temp_dir("linux-wants");
        let binary = PathBuf::from("/opt/rgy/bin/searchd");
        let name = "rgy-searchd.service";
        write_unit(&home, name, &binary, &[]).expect("the unit is written");

        // What `systemctl --user enable` leaves behind, in the wants
        // directory of the user's default target.
        let wants = home.join("systemd/user/default.target.wants");
        std::fs::create_dir_all(&wants).expect("the wants directory");
        std::fs::write(wants.join(name), "").expect("the enabled entry stands in");

        assert!(remove_unit(&home, name).expect("the removal is quiet"));
        assert!(
            !wants.join(name).symlink_metadata().is_ok(),
            "the enabled entry was left behind"
        );

        let _ = std::fs::remove_dir_all(&home);
    }

    /// A symlink whose target is gone still has to go: `exists` follows
    /// the link and would call it absent.
    #[cfg(unix)]
    #[test]
    fn a_dangling_symlink_goes_with_the_unit() {
        let home = temp_dir("linux-dangling");
        let binary = PathBuf::from("/opt/rgy/bin/searchd");
        let name = "rgy-searchd.service";
        write_unit(&home, name, &binary, &[]).expect("the unit is written");

        let wants = home.join("systemd/user/default.target.wants");
        std::fs::create_dir_all(&wants).expect("the wants directory");
        std::fs::remove_file(unit_path_in(&home, name)).expect("the unit is gone");
        std::os::unix::fs::symlink(unit_path_in(&home, name), wants.join(name))
            .expect("the dangling symlink");

        assert!(remove_unit(&home, name).expect("the removal is quiet"));
        assert!(
            !wants.join(name).symlink_metadata().is_ok(),
            "the dangling symlink was left behind"
        );

        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn the_registered_command_is_read_back_from_the_unit() {
        let home = temp_dir("linux-read-back");
        let binary = PathBuf::from("/opt/rgy/bin/searchd");
        let name = "rgy-searchd.service";
        write_unit(&home, name, &binary, &["--json".to_string()]).expect("the unit is written");
        let command = registered_command(&home, name).expect("the command is there");
        // The command as the unit spells it: the quoting is what keeps a
        // path with a space in it one word.
        assert_eq!(command, r#""/opt/rgy/bin/searchd" "--json""#);
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn the_root_the_doctor_reports_is_the_root_the_daemon_walks() {
        let home = temp_dir("linux-read-back-root");
        let binary = PathBuf::from("/opt/rgy/bin/searchd");
        let name = "rgy-searchd.service";
        write_unit(
            &home,
            name,
            &binary,
            &["--root".to_string(), "/home/Ana Maria".to_string()],
        )
        .expect("the unit is written");
        let command = registered_command(&home, name).expect("the command is there");
        assert_eq!(
            crate::root_of(&command),
            Some("/home/Ana Maria".to_string()),
            "the root was truncated at the space"
        );
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn a_quoted_word_comes_back_as_one_word() {
        // The quoting is not decoration: it is what keeps a path with a
        // space in it one word, and the doctor reads the root out of it.
        assert_eq!(
            crate::command_words(r#""/opt/rgy tools/searchd""#),
            ["/opt/rgy tools/searchd".to_string()]
        );
        assert_eq!(
            crate::command_words("/opt/rgy/searchd"),
            ["/opt/rgy/searchd".to_string()]
        );
    }
}
