// SPDX-License-Identifier: Apache-2.0

//! The settings files: where the launcher's preferences live.
//!
//! `settings.tsv` is line-delimited text with a version header, the shape
//! every store in this tree has: the panel writes it, and a person
//! editing it by hand changes the launcher on the next start. Over it
//! lies `settings.json` ([`crate::settings_json`]), written by hand: a
//! key it sets wins, and the panel neither changes it nor copies it into
//! the stored file. The order is built-in defaults, then `settings.tsv`,
//! then `settings.json`.
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
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use rgy_launcher::Overrides;
use rgy_launcher::settings::{Settings, copy_key};

use crate::settings_json;

/// The number that tells the temporary files apart.
///
/// A settings key is saved on a new, unjoined thread of its own, so two
/// saves overlap while an arrow key repeats. One temporary name for the
/// whole process made both write the same file: one renamed it while the
/// other was still writing it, and the loser reported a save that never
/// landed. The process id says which process and the counter says which
/// write of that process.
static TEMP_ID: AtomicU64 = AtomicU64::new(0);

/// The saves of this process, one at a time.
///
/// The panel holds the whole of the settings and writes them on every
/// key. The unique temporary name keeps two writes from sharing a file;
/// the lock keeps the read, the write and the rename one step, so an
/// overlapping save cannot quarantine a file the other one just wrote.
static SAVE_LOCK: Mutex<()> = Mutex::new(());

/// The platform's own code for "the file is held by another process".
#[cfg(windows)]
const ERROR_SHARING_VIOLATION: i32 = 32;

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
    // One save at a time in this process. A poisoned lock is recovered:
    // a writer that panicked left the file in the state the next writer
    // reads, and refusing every save after one panic is worse.
    let _guard = SAVE_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    if path.exists()
        && let Err(cause) = load(path)
    {
        // A file another process holds open for a moment is NOT a broken
        // file. On Windows the refusal is ERROR_SHARING_VIOLATION, which
        // the standard library does not name, and quarantine would rename
        // the user's preferences for a lock that lasts a moment. Only a
        // genuine parse, UTF-8 or version error is moved aside; a busy
        // file is reported to the caller and left exactly where it is.
        if busy(&cause) {
            return Err(cause);
        }
        quarantine(path, cause)?;
    }
    write_atomic(path, &settings.render())
}

/// Replace a file with these contents, whole or not at all: a temporary
/// file beside it, then a rename over it.
pub(crate) fn write_atomic(path: &Path, contents: &str) -> std::io::Result<()> {
    let id = TEMP_ID.fetch_add(1, Ordering::Relaxed);
    let temp = temp_path(path, id);
    if let Err(err) = std::fs::write(&temp, contents) {
        let _ = std::fs::remove_file(&temp);
        return Err(err);
    }
    if let Err(err) = replace(&temp, path) {
        let _ = std::fs::remove_file(&temp);
        return Err(err);
    }
    Ok(())
}

/// The hand-edited file beside the settings file.
pub fn json_path(path: &Path) -> PathBuf {
    path.with_file_name("settings.json")
}

/// The settings in effect, and what the launcher has to say about them.
pub struct Loaded {
    /// The built-in defaults, under `settings.tsv`, under `settings.json`.
    pub settings: Settings,
    /// The keys `settings.json` holds.
    pub held: Overrides,
    /// Why `settings.tsv` was not read, when it was not.
    #[cfg_attr(
        not(any(windows, target_os = "linux", target_os = "freebsd")),
        allow(dead_code)
    )]
    pub error: Option<std::io::Error>,
    /// What could not be taken from `settings.json`.
    pub problems: Vec<String>,
}

/// Read both files. Neither one being unreadable stops the other.
pub fn load_layered(path: &Path) -> Loaded {
    let (mut settings, error) = match load(path) {
        Ok(settings) => (settings, None),
        Err(err) => (Settings::default(), Some(err)),
    };
    let file = settings_json::read(&json_path(path));
    file.held.apply(&mut settings);
    Loaded {
        settings,
        held: file.held,
        error,
        problems: file.problems,
    }
}

/// The settings in effect on this machine.
pub fn current() -> Loaded {
    match store_path() {
        Some(path) => load_layered(&path),
        None => Loaded {
            settings: Settings::default(),
            held: Overrides::default(),
            error: None,
            problems: Vec::new(),
        },
    }
}

/// Make `settings.json` where there is none, from what `settings.tsv`
/// holds. The stored file is only read. One that cannot be read makes
/// nothing: a copy of the defaults would pass for the user's settings.
#[cfg_attr(
    not(any(windows, target_os = "linux", target_os = "freebsd")),
    allow(dead_code)
)]
pub fn create_json(path: &Path) -> std::io::Result<()> {
    let json = json_path(path);
    if std::fs::symlink_metadata(&json).is_ok() {
        return Ok(());
    }
    if let Ok(settings) = load(path) {
        settings_json::create_if_absent(&json, &settings)?;
    }
    Ok(())
}

/// Write what a panel holds, without taking `settings.json` into the
/// stored file.
///
/// `held` is what the panel was loaded with. Those keys, and the keys the
/// file sets by now, keep their **stored** values, so removing a key from
/// `settings.json` brings back what was there. A panel change to a key
/// the file started setting after the panel was loaded is not written,
/// and is an error that says so; the other changes are written.
pub fn save_layered(path: &Path, settings: &Settings, held: &Overrides) -> std::io::Result<()> {
    let now = settings_json::read(&json_path(path)).held;
    if held.is_empty() && now.is_empty() {
        return save(path, settings);
    }
    let stored = match load(path) {
        Ok(stored) => stored,
        Err(err) if busy(&err) => return Err(err),
        // `save` moves the broken file aside.
        Err(_) => Settings::default(),
    };
    let mut out = settings.clone();
    for key in held.keys() {
        copy_key(&stored, &mut out, key);
    }
    let mut with_file = settings.clone();
    now.apply(&mut with_file);
    let mut refused = Vec::new();
    for key in now.keys().filter(|key| !held.holds(key)) {
        if !same_key(settings, &stored, key) && !same_key(settings, &with_file, key) {
            refused.push(key);
        }
        copy_key(&stored, &mut out, key);
    }
    save(path, &out)?;
    if refused.is_empty() {
        Ok(())
    } else {
        Err(std::io::Error::other(format!(
            "settings.json now sets {}: that change was not saved; restart rgxy",
            refused.join(", ")
        )))
    }
}

/// Whether two settings agree on one key.
fn same_key(a: &Settings, b: &Settings, key: &str) -> bool {
    let only = |from: &Settings| {
        let mut settings = Settings::default();
        copy_key(from, &mut settings, key);
        settings
    };
    only(a) == only(b)
}

/// Whether the error says the file is held for a moment by another process.
///
/// Windows says it with `ERROR_SHARING_VIOLATION`, which the standard
/// library does not map to an `ErrorKind`: it is the raw code that says
/// it. A momentary lock is not corruption, and the usage history's store
/// already carries the same rule.
#[cfg(windows)]
fn busy(err: &std::io::Error) -> bool {
    err.raw_os_error() == Some(ERROR_SHARING_VIOLATION)
}

/// Whether the error says the file is held for a moment by another process.
#[cfg(not(windows))]
fn busy(err: &std::io::Error) -> bool {
    err.kind() == std::io::ErrorKind::WouldBlock
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
/// The name carries the process and the number of the write, so two
/// overlapping saves never write or rename the same file.
fn temp_path(path: &Path, id: u64) -> PathBuf {
    sibling(path, &format!(".{}.{id}.tmp", std::process::id()))
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

    /// The first `settings.json` is a copy that sets nothing, the stored
    /// file is untouched, and an existing file is never written again.
    #[test]
    fn the_migration_makes_an_inert_file_once() {
        let path = store("migration");
        let mut settings = Settings::default();
        settings.overlay.width = 780;
        save(&path, &settings).expect("the stored file");
        let before = std::fs::read(&path).unwrap();

        create_json(&path).expect("the file is made");
        let made = std::fs::read_to_string(json_path(&path)).unwrap();
        assert!(made.contains("// \"overlay.width\": 780,"), "{made}");
        assert_eq!(std::fs::read(&path).unwrap(), before);
        let loaded = load_layered(&path);
        assert!(loaded.held.is_empty() && loaded.problems.is_empty());
        assert_eq!(loaded.settings, settings);

        let mine = "// mine\n{ \"unknown\": 1, }\n";
        std::fs::write(json_path(&path), mine).unwrap();
        create_json(&path).expect("nothing to do");
        assert_eq!(std::fs::read_to_string(json_path(&path)).unwrap(), mine);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    /// A stored file that cannot be read is not copied.
    #[test]
    fn a_broken_stored_file_makes_no_json() {
        let path = store("migration-broken");
        std::fs::write(&path, "# rgy settings v99\n").unwrap();
        create_json(&path).expect("nothing to do");
        assert!(!json_path(&path).exists());
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    /// The JSON file wins per key, and a save neither copies it into the
    /// stored file nor touches it.
    #[test]
    fn the_json_wins_and_a_save_preserves_both_layers() {
        let path = store("layers");
        let mut stored = Settings::default();
        stored.overlay.width = 700;
        stored.overlay.rows = 5;
        save(&path, &stored).expect("the stored file");
        let mine = "{\n // keep me\n \"overlay.width\": 900, \"later\": true,\n}\n";
        std::fs::write(json_path(&path), mine).unwrap();

        let mut loaded = load_layered(&path);
        assert_eq!(loaded.settings.overlay.width, 900);
        assert_eq!(loaded.settings.overlay.rows, 5);
        assert_eq!(loaded.problems.len(), 1, "{:?}", loaded.problems);

        loaded.settings.overlay.rows = 8;
        save_layered(&path, &loaded.settings, &loaded.held).expect("saved");
        let after = load(&path).unwrap();
        assert_eq!((after.overlay.width, after.overlay.rows), (700, 8));
        assert_eq!(std::fs::read_to_string(json_path(&path)).unwrap(), mine);

        // The key taken out while the panel holds it: still not baked in.
        std::fs::write(json_path(&path), "{}").unwrap();
        save_layered(&path, &loaded.settings, &loaded.held).expect("saved");
        assert_eq!(load(&path).unwrap().overlay.width, 700);
        assert_eq!(load_layered(&path).settings.overlay.width, 700);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    /// A key the file starts setting after the load refuses a change out
    /// loud, and lets an untouched one pass.
    #[test]
    fn a_key_set_after_the_load_is_not_silently_saved() {
        let path = store("late");
        let loaded = load_layered(&path);
        std::fs::write(json_path(&path), "{\"overlay.width\": 900}").unwrap();

        let mut settings = loaded.settings.clone();
        settings.overlay.rows = 8;
        save_layered(&path, &settings, &loaded.held).expect("width was not touched");

        settings.overlay.width = 620;
        let refused = save_layered(&path, &settings, &loaded.held).expect_err("held");
        assert!(refused.to_string().contains("overlay.width"), "{refused}");
        let after = load(&path).unwrap();
        assert_eq!((after.overlay.width, after.overlay.rows), (600, 8));
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    /// An invalid JSON file leaves the stored settings in effect.
    #[test]
    fn an_invalid_json_file_leaves_the_stored_settings() {
        let path = store("invalid-json");
        let mut stored = Settings::default();
        stored.overlay.width = 700;
        save(&path, &stored).expect("the stored file");
        std::fs::write(json_path(&path), "{ nope").unwrap();
        let loaded = load_layered(&path);
        assert_eq!(loaded.settings, stored);
        assert!(loaded.error.is_none() && loaded.held.is_empty());
        assert_eq!(loaded.problems.len(), 1);
        save_layered(&path, &stored, &loaded.held).expect("saved");
        assert_eq!(std::fs::read_to_string(json_path(&path)).unwrap(), "{ nope");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
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

    /// A momentary lock is not corruption.
    ///
    /// The busy guard is what keeps `save()` from renaming a perfectly
    /// good settings file to `.settings.tsv.broken` when another process
    /// holds it open for an instant.
    #[test]
    fn a_busy_error_is_not_a_broken_file() {
        #[cfg(windows)]
        let held = std::io::Error::from_raw_os_error(32);
        #[cfg(not(windows))]
        let held = std::io::Error::from(std::io::ErrorKind::WouldBlock);
        assert!(busy(&held), "a momentary lock is busy");
        let broken = std::io::Error::other("a version this launcher does not know");
        assert!(!busy(&broken), "a parse error is not a momentary lock");
    }

    /// A settings file held by another process is not moved aside.
    ///
    /// The store is held here with no sharing at all, so the read is
    /// refused the way the platform refuses it: with the sharing
    /// violation, and not with the "this file is broken" that the
    /// quarantine is for. The write fails, the caller is told, and the
    /// user's preferences are where they were.
    #[cfg(windows)]
    #[test]
    fn a_store_that_is_held_is_not_moved_aside() {
        use std::os::windows::fs::OpenOptionsExt;

        let path = store("held");
        let mut settings = Settings::default();
        settings.overlay.width = 812;
        save(&path, &settings).expect("the settings are written");
        let before = std::fs::read(&path).expect("the file is there");

        // The file, there and held: nobody can read it and nobody can
        // move it, and the write has to say so.
        let held = std::fs::OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(&path)
            .expect("the settings file is held");

        let mut other = Settings::default();
        other.overlay.width = 999;
        assert!(save(&path, &other).is_err(), "the file cannot be read");

        drop(held);
        assert_eq!(
            std::fs::read(&path).expect("the file is there"),
            before,
            "the settings survived the failed write"
        );
        assert!(
            !quarantine_path(&path).exists(),
            "the settings were moved aside"
        );
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    /// Overlapping saves do not share a temporary file.
    ///
    /// The panel spawns a new, unjoined thread on every settings key, so
    /// holding an arrow key overlaps many writes. One temporary name for
    /// the whole process made one save rename the file another was still
    /// writing: the loser failed with a spurious "settings not saved"
    /// note, a change was lost, and the file could be half-interleaved.
    /// Every save now has its own temporary name and the saves are one
    /// at a time, so all of them land and the file stays whole.
    #[test]
    fn overlapping_saves_both_land_and_the_file_is_whole() {
        let path = store("overlapping");
        let mut first = Settings::default();
        first.overlay.width = 711;
        let mut second = Settings::default();
        second.overlay.width = 933;
        save(&path, &first).expect("the file starts out whole");

        let mut writers = Vec::new();
        for writer in 0..6 {
            let path = path.clone();
            let settings = if writer % 2 == 0 {
                first.clone()
            } else {
                second.clone()
            };
            writers.push(std::thread::spawn(move || {
                let mut outcomes = Vec::new();
                for _ in 0..20 {
                    outcomes.push(save(&path, &settings));
                }
                outcomes
            }));
        }
        for writer in writers {
            for outcome in writer.join().expect("the writer did not panic") {
                assert!(outcome.is_ok(), "an overlapping save failed: {outcome:?}");
            }
        }
        let back = load(&path).expect("the file is still whole");
        assert!(
            back == first || back == second,
            "the file is one whole settings or the other: {back:?}"
        );
        // No piece of any write is left beside the file.
        for entry in std::fs::read_dir(path.parent().unwrap()).unwrap() {
            let name = entry.unwrap().file_name().to_string_lossy().into_owned();
            assert_eq!(name, "settings.tsv", "no temporary file was left: {name}");
        }
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }
}
