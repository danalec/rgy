// SPDX-License-Identifier: Apache-2.0

//! The settings file: the one place the launcher's preferences live.
//!
//! The file is line-delimited text with a version header, the shape
//! every store in this tree has, and it is **both faces of the
//! settings**: the panel writes it, and a person editing it by hand
//! changes the launcher on the next start. There is no second place the
//! settings live, because two places are two that disagree.
//!
//! The rules are the usage history's, with one difference: there is no
//! merge. The history is many writers recording many uses — two
//! launchers have to lock, read, merge and write, or one throws the
//! other's uses away. The settings are **one writer with one answer**:
//! the panel holds the whole of them, and the write is the answer it
//! holds. A hand edit between two panel writes is taken by the next
//! read — and taken away by the next write, which is what "the panel
//! writes the file" means. The write itself is atomic (a temporary file
//! and a rename), so a reader never sees half a file, whatever two
//! writers race.

use std::path::{Path, PathBuf};

use rgy_launcher::settings::Settings;

/// The file the settings live in.
pub fn store_path() -> Option<PathBuf> {
    let cache = rgy_proto::endpoint::cache_dir()?;
    Some(cache.join("rgy").join("settings.tsv"))
}

/// Read the settings.
///
/// A file that is **not there** is the defaults, and not an error: a
/// launcher that has never been configured has settings, and they are
/// the ones the faces were drawn in before settings existed.
///
/// A file that is there and cannot be read — or that carries a header
/// this version does not know — is an error, so the caller can say so:
/// a launcher silently running on defaults under a file the user
/// carefully edited is a launcher that ignores its user.
pub fn load(path: &Path) -> Result<Settings, std::io::Error> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Settings::default());
        }
        Err(err) => return Err(err),
    };
    Settings::parse(&text).ok_or_else(|| {
        std::io::Error::other("the settings file carries a version this launcher does not know")
    })
}

/// Write the settings.
///
/// A file that could not be read — the broken, or from a version nobody
/// knows — is moved aside rather than refused: a settings file that
/// stays broken keeps every panel change from ever landing, and the
/// person who has to look at what happened can still read it under its
/// new name.
pub fn save(path: &Path, settings: &Settings) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    if path.exists()
        && let Err(cause) = load(path)
    {
        quarantine(path, cause)?;
    }
    let temp = temp_path(path);
    if let Err(err) = std::fs::write(&temp, settings.render()) {
        let _ = std::fs::remove_file(&temp);
        return Err(err);
    }
    if let Err(err) = replace(&temp, path) {
        let _ = std::fs::remove_file(&temp);
        return Err(err);
    }
    Ok(())
}

/// Move a settings file that could not be read aside.
///
/// Only a file is moved: a directory at the file's path is something
/// else that took the name, and moving it away would move a thing the
/// launcher does not own.
fn quarantine(path: &Path, cause: std::io::Error) -> std::io::Result<()> {
    if path.is_dir() {
        return Err(cause);
    }
    std::fs::rename(path, quarantine_path(path))
}

/// The move that replaces the settings with the temporary file.
///
/// The platform's own call, tried a few times: another process can have
/// the file open at that instant, and a refusal that lasts a moment is
/// not a refusal.
fn replace(temp: &Path, store: &Path) -> std::io::Result<()> {
    let mut attempt = 0;
    loop {
        match std::fs::rename(temp, store) {
            Ok(()) => return Ok(()),
            Err(err) => {
                attempt += 1;
                if attempt >= 5 {
                    return Err(err);
                }
                std::thread::sleep(std::time::Duration::from_millis(2));
            }
        }
    }
}

/// The file the settings are written to before they replace the file.
///
/// Beside the file and not in the temporary folder of the machine: the
/// move that replaces a file has to be a move **inside one volume**.
fn temp_path(path: &Path) -> PathBuf {
    sibling(path, &format!(".{}.tmp", std::process::id()))
}

/// The file a settings file that could not be read is moved to.
///
/// The number keeps every broken file: the one with no number is the
/// one that broke last, and the ones before it are still there.
fn quarantine_path(path: &Path) -> PathBuf {
    let mut name = sibling(path, ".broken");
    let mut number = 1;
    while std::fs::symlink_metadata(&name).is_ok() {
        name = sibling(path, &format!(".broken.{number}"));
        number += 1;
    }
    name
}

/// A file of the settings' own, beside them.
fn sibling(path: &Path, name: &str) -> PathBuf {
    let mut sibling = path.to_path_buf();
    sibling.set_file_name(format!(
        ".{}{name}",
        path.file_name()
            .map(|file| file.to_string_lossy().into_owned())
            .unwrap_or_default()
    ));
    sibling
}

#[cfg(test)]
mod tests {
    use super::*;
    use rgy_launcher::TapKey;
    use rgy_launcher::settings::Hotkey;

    /// A folder of this test's own, under the machine's temporary one.
    ///
    /// Two tests of the same process cannot share a folder: each builds
    /// the store under the other, and a read that catches a half-built
    /// store reads a store of nothing.
    fn store(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "rgy-settings-store-{}-{name}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("settings.tsv")
    }

    /// A file that is not there is the defaults, and not an error.
    #[test]
    fn a_file_that_is_not_there_is_the_defaults() {
        let path = store("missing");
        let settings = load(&path).expect("no file is the defaults");
        assert_eq!(settings, Settings::default());
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    /// The file round-trips: what the panel writes is what the next
    /// start reads.
    #[test]
    fn the_file_round_trips() {
        let path = store("round-trip");
        let mut settings = Settings::default();
        settings.overlay.width = 780;
        settings.overlay.palette.template = "nord".to_string();
        settings.overlay.hotkey = Hotkey::DoubleTap {
            key: TapKey::Alt,
            window_ms: 300,
        };
        save(&path, &settings).expect("the settings were written");
        let back = load(&path).expect("the file was read");
        assert_eq!(back, settings);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    /// The write is whole or nothing: the file is replaced, never
    /// rewritten in place, so a reader never meets it half-written.
    #[test]
    fn the_write_is_whole_or_nothing() {
        let path = store("whole");
        let first = Settings::default();
        save(&path, &first).expect("the first write");
        let bytes = std::fs::read(&path).unwrap();
        assert!(bytes.ends_with(b"\n"), "the file ends in a newline");
        save(&path, &first).expect("the second write");
        let again = std::fs::read(&path).unwrap();
        assert_eq!(bytes, again, "the same settings write the same bytes");
        // No piece of a write is left beside the file.
        for entry in std::fs::read_dir(path.parent().unwrap()).unwrap() {
            let name = entry.unwrap().file_name().to_string_lossy().into_owned();
            assert_eq!(name, "settings.tsv", "no temporary file was left: {name}");
        }
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    /// A file this version does not know is an error, and the next
    /// write moves it aside rather than writing over what happened.
    #[test]
    fn a_file_this_version_does_not_know_is_moved_aside() {
        let path = store("future");
        std::fs::write(&path, "# rgy settings v99\noverlay.width\t700\n").unwrap();
        let refused = load(&path).expect_err("a future file is not this file");
        assert!(
            refused.to_string().contains("version"),
            "the reason says it is the version: {refused}"
        );
        // The write quarantines it and lands.
        save(&path, &Settings::default()).expect("the write lands");
        let after = load(&path).expect("the new file reads");
        assert_eq!(after, Settings::default());
        let dir = path.parent().unwrap();
        let names: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert!(
            names.iter().any(|name| name.contains(".broken")),
            "the future file is kept aside: {names:?}"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    /// A directory at the file's path is not moved and not written.
    #[test]
    fn a_directory_at_the_file_s_path_is_not_moved() {
        let path = store("directory");
        std::fs::create_dir_all(&path).unwrap();
        assert!(load(&path).is_err(), "a directory is not settings");
        assert!(save(&path, &Settings::default()).is_err(), "not written");
        assert!(path.is_dir(), "the directory is still there");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }
}
