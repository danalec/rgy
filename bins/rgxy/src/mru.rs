// SPDX-License-Identifier: Apache-2.0

//! The launcher's memory of what the user used.
//!
//! The **policy** — how far a used result rises — is in the model
//! (`rgy_launcher::usage`); this is the file the memory lives in, and
//! the file is the binary's business because the model does not touch
//! the machine.
//!
//! # Where it lives
//!
//! In the daemon's cache directory, under the `rgy` directory the daemon
//! already keeps its socket in: the launcher is a client of the daemon,
//! and its memory belongs next to the daemon's index — in the one place
//! the product owns on every platform.
//!
//! # What it looks like
//!
//! A text file, one line per path:
//!
//! ```text
//! # rgy usage v2: count<TAB>last<TAB>path
//! # the path is escaped: \n, \r, \t and \\ are the characters a line cannot carry
//! 3<TAB>1760000000<TAB>C:/Users/dana/notas.txt
//! ```
//!
//! The path is **last**, because the first two tabs are the only
//! separators there are. A path that carries a tab, a newline or a
//! backslash is **escaped**: a backslash stands for the character after
//! it, and the character is spelled out — `\n`, `\r`, `\t` — while a
//! backslash is a backslash. Without it a path with a newline in it is
//! two lines, and the launcher opens half a path.
//!
//! The **version** is in the header, and it is what tells the two
//! formats apart: a store that starts with the header above is read
//! with the escape, and a store that does not is the one written before
//! the escape existed and is read the old way, with the path as it is.
//! An old file whose path looks like an escape is read the old way too.
//!
//! A line that does not parse is a line that is skipped, and a file
//! whose last line was cut short does not get that line at all. A store
//! that cannot be **read** is another thing altogether: the launcher
//! says so and carries on, because a memory that is silently not there
//! is a memory the user believes in and the machine does not have — and
//! the write that follows moves the broken file aside and starts the
//! memory again, so one bad file does not cost every use from then on.
//!
//! The path is the **daemon's** spelling of the path, and it is the
//! spelling the history is looked up with: the launcher and the daemon
//! agree on the bytes, and a history keyed by anything else would never
//! match anything.
//!
//! # What a write does
//!
//! A write is not a replacement: the file is read again, and what is
//! written down is what the file says and what the caller holds. Two
//! launchers at the same time both have their say — the entry one of
//! them recorded is not lost because the other one wrote later.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use rgy_launcher::text::{Lang, Text};
use rgy_launcher::usage::History;

/// The number that tells the temporary files apart.
///
/// The counter is the writer's own: the process id says which process,
/// and the counter says which write of that process. Two writers on the
/// same machine never share a file, and the file one of them leaves
/// behind is never the file the other one is writing.
static TEMP_ID: AtomicU64 = AtomicU64::new(0);

/// How long the move that replaces the store is waited for, in
/// milliseconds.
const RENAME_WAIT_MS: u64 = 2_000;

/// How long the read of the store is waited for, in milliseconds.
const READ_WAIT_MS: u64 = 2_000;

/// How long the lock is waited for before the write is given up on, in
/// milliseconds.
const LOCK_WAIT_MS: u64 = 2_000;

/// How long the writer waits between the tries, in milliseconds.
///
/// The read and the move wait the same wait the lock waits: the three are
/// the same kind of refusal -- somebody else has the store for a moment --
/// and a refusal that lasts a moment is not a refusal.
const WAIT_MS: u64 = 5;

/// The longest the wait between the tries is, in milliseconds.
///
/// The wait **backs off**, and the backoff is the point: the writers that
/// wait for one store wake up together, and every wake-up takes the CPU
/// the writer that holds the lock needs to finish. A wait that polls
/// every few milliseconds makes the wait longer, and the update is lost
/// at the end of it.
const WAIT_MAX_MS: u64 = 100;

/// The platform's own code for "the file is held by another process".
#[cfg(windows)]
const ERROR_SHARING_VIOLATION: i32 = 32;

/// The first line of a store in the format this version writes.
///
/// The version is what tells the two formats apart: a store that starts
/// with this line is read with the escape, and a store that does not is
/// the one written before the escape existed.
const HEADER: &str = "# rgy usage v2: count\tlast\tpath";

/// The second line of the header: what the escape is.
///
/// The store is a file a person may have to read, and a backslash in a
/// path says nothing unless the file says what it means.
const HEADER_NOTE: &str =
    "# the path is escaped: \\n, \\r, \\t and \\\\ are what a line cannot carry";

/// The file the history lives in.
pub fn store_path() -> Option<PathBuf> {
    let cache = rgy_proto::endpoint::cache_dir()?;
    Some(cache.join("rgy").join("usage.tsv"))
}

/// The seconds since the Unix epoch, now.
///
/// The clock is the machine's, and the model does not have one: the
/// history asks "recently" and the caller says when now is.
pub fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|since| since.as_secs())
        .unwrap_or(0)
}

/// Read the history.
///
/// A file that is **not there** is no history, and not an error: a
/// launcher that has never run has no memory, and that is the normal
/// state of a new machine.
///
/// A file that is there and **busy** is read a moment later: somebody else
/// has it open, and on Windows the platform refuses the read of a file that
/// is open without the sharing the reader asks for. That is not a file that
/// cannot be read — it is a file that is held for a moment, and a refusal
/// that lasts a moment is not a refusal. The read waits, the way the lock
/// waits. The wait is bounded: a store that stays held is a store the
/// caller has to be told about.
///
/// A file that is there and cannot be read is an error, and it is kept:
/// the history is gone for this run, the ranking falls back to the
/// daemon's order and the nudge does not come back — and the launcher
/// has to say that much. The **write** is what recovers: it moves the
/// broken file aside and starts the memory again.
pub fn load(path: &Path) -> Result<History, std::io::Error> {
    let deadline = Instant::now() + Duration::from_millis(READ_WAIT_MS);
    let mut wait = WAIT_MS;
    loop {
        match std::fs::read_to_string(path) {
            Ok(text) => return Ok(parse(&text)),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(History::new()),
            Err(err) => {
                if !busy(&err) || !wait_a_little(&deadline, &mut wait) {
                    return Err(err);
                }
            }
        }
    }
}

/// Wait a little, or say that the wait is over.
///
/// The wait is a **deadline** and not a count of tries: a count of tries
/// of a fixed sleep bounds the sleeping and not the waiting, and the two
/// come apart on a machine that is busy. The writer that holds the lock
/// is off the CPU for a while, the waiters poll, and a waiter that counts
/// tries gives up while the lock is about to be free — and the update it
/// was carrying is lost.
///
/// The sleep **backs off**, so the waiters do not take the CPU the holder
/// needs to finish: the wait ends when the lock is free, and it is the
/// wait that gets longer when the machine is busy.
fn wait_a_little(deadline: &Instant, wait: &mut u64) -> bool {
    if Instant::now() >= *deadline {
        return false;
    }
    std::thread::sleep(Duration::from_millis(*wait));
    *wait = (*wait * 2).min(WAIT_MAX_MS);
    true
}

/// The sentence for a history that could not be read.
///
/// The file is the launcher's own memory, and a memory that could not
/// be read is not the same thing as a daemon that is not there: the
/// ranking falls back to the daemon's order and the nudge does not
/// come back, and the two facts have to be told apart.
pub fn read_error(err: &std::io::Error, lang: Lang) -> String {
    Text::for_lang(lang)
        .history_not_read
        .replace("{err}", &err.to_string())
}

/// The history the text of a store says.
///
/// Every line that does not make sense is skipped: the store is written
/// by a program that can be killed halfway through, and a launcher that
/// refuses to start because of half a line is a launcher nobody uses.
///
/// The **version** is what tells the two formats apart: a store that
/// carries the header of this version is read with the escape, and a
/// store that does not is the one written before the escape existed, and
/// its paths are read as they are.
pub fn parse(text: &str) -> History {
    let mut history = History::new();
    let lines: Vec<&str> = text.lines().collect();
    // The version is in the header: a store that does not carry it is
    // the one written before the escape existed, and its paths are read
    // as they are.
    let escaped = text.starts_with(HEADER);
    // A file that does not end in a newline was cut short by whatever cut
    // it, and its last line is half a line: half a line is not a path.
    let whole = text.ends_with('\n');
    for (index, line) in lines.iter().enumerate() {
        if !whole && index + 1 == lines.len() {
            break;
        }
        let line = line.trim_end_matches('\r');
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((count, rest)) = line.split_once('\t') else {
            continue;
        };
        let Some((last, path)) = rest.split_once('\t') else {
            continue;
        };
        let Ok(uses) = count.trim().parse::<u32>() else {
            continue;
        };
        let Ok(last) = last.trim().parse::<u64>() else {
            continue;
        };
        if uses == 0 || path.is_empty() {
            continue;
        }
        let path = if escaped {
            unescape_path(path)
        } else {
            path.to_string()
        };
        history.insert(&path, uses, last);
    }
    history
}

/// Write the history.
///
/// The write goes through a temporary file and a rename: a launcher that
/// is killed in the middle of a write leaves the old store, and not half
/// a new one.
///
/// The rename **replaces** the store, on every platform. On Windows
/// `std::fs::rename` is `MoveFileExW` with `MOVEFILE_REPLACE_EXISTING`:
/// the two files are swapped and the destination is never absent, so the
/// store on the disk is the old one or the new one and never nothing.
/// The code used to remove the store first — "Windows will not rename a
/// file over a file that is there" — and that is what made the promise
/// false: a launcher killed between the removal and the rename left **no**
/// store at all, and with it the history of every file the user ever
/// opened.
///
/// The temporary file has a **name of its own** — the process, and the
/// number of the write — because the name used to be the same for
/// everybody: two writers wrote into the same file, the first one to
/// move it took the other's content with it, and the other's move failed
/// because the file was not there any more. The update the caller had
/// made was dropped, and the caller was never told.
///
/// A write that does not become the store takes its temporary file away
/// with it: the cache folder holds the store, the lock file beside it and
/// nothing else, and a temporary file left behind there says nothing about
/// anything.
///
/// What is written is what the file says **now** and what the caller
/// holds: the file is read again under the lock, so the entry another
/// launcher recorded while this one was running is still there. The
/// counts are not added — the caller's history was read from the file,
/// so its counts already say what the file said — and the more recent
/// word about a path wins.
pub fn save(path: &Path, history: &History) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    // One writer at a time. The read, the merge and the write are one
    // step, or two launchers read the same old file and the second one
    // writes a store that says nothing about what the first one did.
    let _lock = Lock::take(&lock_path(path))?;
    // The file says what it says now: another launcher may have written
    // while this one was running.
    let on_disk = match load(path) {
        Ok(history) => history,
        Err(err) => {
            // A store that is **busy** is not a store that is broken, and a
            // busy store is not moved aside: the move would take the
            // history away from a file that is perfectly good, and the
            // file that is there is the user's memory. The update is lost
            // -- the caller is told -- but the history is not.
            if busy(&err) {
                return Err(err);
            }
            // A file that cannot be read is not a file with no history:
            // the merge would throw away what is in it, and the write is
            // refused rather than the history lost. But a store that
            // stays broken takes every use with it, for every run after
            // this one, so the broken file is moved aside and the memory
            // starts again from what the caller holds.
            quarantine(path, err)?;
            History::new()
        }
    };
    let merged = merge(&on_disk, history);
    let id = TEMP_ID.fetch_add(1, Ordering::Relaxed);
    write_store(path, &merged, id)
}

/// Move a store that could not be read aside.
///
/// The file stays on the disk under a name of its own: the launcher
/// cannot use what is in it, but the person who has to look at what
/// happened can still read it.
fn quarantine(path: &Path, cause: std::io::Error) -> std::io::Result<()> {
    // Only a file is moved: a directory at the store's path is
    // something else that took the name, and moving it away would move
    // a thing the launcher does not own.
    if path.is_dir() {
        return Err(cause);
    }
    std::fs::rename(path, quarantine_path(path))
}

/// The history that is written down: what the file says, and what the
/// caller holds.
///
/// The entries the caller does not know about are kept: an entry another
/// launcher recorded is not lost because this one did not know about it.
///
/// The counts are not added. The caller's history was read from the
/// file, so its counts already say what the file said, and a sum would
/// count the same use twice.
///
/// Each field is taken at its largest: the count is the bigger of the two
/// counts, and the time is the later of the two times. Taking one entry
/// whole threw a use away — a file written by another launcher with a
/// later time and a smaller count replaced the count the caller knew, and
/// the history went backwards.
fn merge(on_disk: &History, caller: &History) -> History {
    let mut merged = on_disk.clone();
    for entry in caller.entries() {
        match merged.get(&entry.path) {
            Some(known) => {
                let uses = known.uses.max(entry.uses);
                let last = known.last.max(entry.last);
                merged.insert(&entry.path, uses, last);
            }
            None => {
                merged.insert(&entry.path, entry.uses, entry.last);
            }
        }
    }
    merged
}

/// The write, with the number of the temporary file spelled out.
///
/// The number is an argument so that a test can name the file it takes:
/// a temporary file with a name of its own cannot be taken by accident,
/// and the test that wants to take one has to say which one it takes.
fn write_store(path: &Path, history: &History, id: u64) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let temp = temp_path(path, id);
    if let Err(err) = std::fs::write(&temp, render(history)) {
        // The file never became the store: it is the piece of a write
        // that did not go through, and the store's folder is not where
        // the pieces of a write are kept.
        let _ = std::fs::remove_file(&temp);
        return Err(err);
    }
    if let Err(err) = replace(&temp, path) {
        // The same here: what did not become the store is taken away.
        let _ = std::fs::remove_file(&temp);
        return Err(err);
    }
    Ok(())
}

/// The move that replaces the store with the temporary file.
///
/// The platform's own call: `std::fs::rename` is `MoveFileExW` with
/// `MOVEFILE_REPLACE_EXISTING` on Windows, and it is the call that
/// handles the long-path form.
///
/// The move is tried a few times before it is given up on: another
/// process can have the store open at that exact instant, and the
/// platform refuses the replace while it is. A refusal that lasts a
/// moment is not a refusal, and a history update dropped over it is a
/// history the user never had. The wait is the lock's wait, and the
/// refusal that is waited for is the one that is about **time**: a move
/// that fails because the store's path is a directory fails the same way
/// every time, and waiting for it only makes the caller wait for nothing.
fn replace(temp: &Path, store: &Path) -> std::io::Result<()> {
    let deadline = Instant::now() + Duration::from_millis(RENAME_WAIT_MS);
    let mut wait = WAIT_MS;
    loop {
        match std::fs::rename(temp, store) {
            Ok(()) => return Ok(()),
            Err(err) => {
                if !busy(&err) || !wait_a_little(&deadline, &mut wait) {
                    return Err(err);
                }
            }
        }
    }
}

/// The file the store is written to before it replaces the store.
///
/// Beside the store and not in the temporary folder of the machine: the
/// move that replaces a file has to be a move **inside one volume**, and
/// the temporary folder is another one on most machines.
///
/// The name carries the writer's own: the process, and the number of the
/// write. The name used to be the same for everybody, and two writers
/// then wrote into the same file.
fn temp_path(path: &Path, id: u64) -> PathBuf {
    sibling(path, &format!(".{}.{id}.tmp", std::process::id()))
}

/// The file the lock is taken on.
///
/// Beside the store, and never replaced: the store itself is replaced by
/// the write, and a lock on a file that is replaced is a lock on nothing.
fn lock_path(path: &Path) -> PathBuf {
    sibling(path, ".lock")
}

/// The file a store that could not be read is moved to.
///
/// The number keeps every broken store: the one with no number is the
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

/// A file of the store's own, beside it.
fn sibling(path: &Path, suffix: &str) -> PathBuf {
    let mut name = match path.file_name() {
        Some(name) => name.to_os_string(),
        None => OsString::from("usage.tsv"),
    };
    name.push(suffix);
    path.with_file_name(name)
}

/// The lock that says the store belongs to one writer.
///
/// The lock is the operating system's, and the operating system takes it
/// back when the process dies. A lock that is a **file** — a file whose
/// existence is the lock — is a lock that a killed process leaves
/// behind, and nobody can take it again.
struct Lock {
    /// The file the lock is taken on: the file being open **is** the
    /// lock, and closing it gives the lock back.
    _file: std::fs::File,
}

impl Lock {
    /// Take the lock, or wait for the writer that has it.
    ///
    /// The wait is bounded: a store that cannot be written is a store
    /// the caller has to be told about, and a launcher that waits for
    /// ever is a launcher the user cannot close. The bound is a **deadline**
    /// and not a count of tries, and the sleep backs off: the writer that
    /// holds the lock needs the CPU to finish, and a waiter that polls
    /// every few milliseconds takes it away from it.
    fn take(path: &Path) -> std::io::Result<Lock> {
        let deadline = Instant::now() + Duration::from_millis(LOCK_WAIT_MS);
        let mut wait = WAIT_MS;
        loop {
            match Lock::once(path) {
                Ok(file) => return Ok(Lock { _file: file }),
                Err(err) => {
                    if !busy(&err) || !wait_a_little(&deadline, &mut wait) {
                        return Err(err);
                    }
                }
            }
        }
    }

    /// Take the lock once, or say why not.
    #[cfg(windows)]
    fn once(path: &Path) -> std::io::Result<std::fs::File> {
        use std::os::windows::fs::OpenOptionsExt;

        // The platform's own way of saying "this file is mine": the
        // handle is opened with no sharing at all, and the platform
        // takes it back when the handle closes — the process dying
        // included.
        std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .share_mode(0)
            .open(path)
    }

    /// Take the lock once, or say why not.
    #[cfg(unix)]
    fn once(path: &Path) -> std::io::Result<std::fs::File> {
        let file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)?;
        unix_lock::take(&file)?;
        Ok(file)
    }
}

/// Whether the error says the lock is held by another writer.
#[cfg(windows)]
fn busy(err: &std::io::Error) -> bool {
    // The platform says "the file is held by another process" with
    // ERROR_SHARING_VIOLATION, and the standard library does not know
    // that one: it is the raw code that says it.
    err.raw_os_error() == Some(ERROR_SHARING_VIOLATION)
}

/// Whether the error says the lock is held by another writer.
#[cfg(unix)]
fn busy(err: &std::io::Error) -> bool {
    // The platform says "the lock is taken" with EWOULDBLOCK, and the
    // standard library knows that one.
    err.kind() == std::io::ErrorKind::WouldBlock
}

/// The lock the file system gives a writer on Unix.
///
/// `flock` is the platform's own: the lock belongs to the open file,
/// and the platform takes it back when the file is closed — the process
/// dying included.
#[cfg(unix)]
mod unix_lock {
    use std::io;
    use std::os::fd::AsRawFd;

    /// Take the lock, or say that it is taken.
    const LOCK_EX: i32 = 2;
    /// Say that the lock is taken without waiting for it.
    const LOCK_NB: i32 = 4;

    unsafe extern "C" {
        fn flock(fd: i32, operation: i32) -> i32;
    }

    /// Take the lock on the file, or fail.
    pub fn take(file: &std::fs::File) -> io::Result<()> {
        // SAFETY: the file is open, and `flock` only reads its number.
        let taken = unsafe { flock(file.as_raw_fd(), LOCK_EX | LOCK_NB) };
        if taken == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }
}

/// The text of the store.
pub fn render(history: &History) -> String {
    let mut text = String::from(HEADER);
    text.push('\n');
    text.push_str(HEADER_NOTE);
    text.push('\n');
    for entry in history.entries() {
        text.push_str(&entry.uses.to_string());
        text.push('\t');
        text.push_str(&entry.last.to_string());
        text.push('\t');
        text.push_str(&escape_path(&entry.path));
        text.push('\n');
    }
    text
}

/// The path, with the characters a line cannot carry escaped.
///
/// The escape is the one everybody knows: a backslash stands for the
/// character after it. The characters the format needs are spelled out —
/// a newline, a carriage return and a tab — and a backslash is a
/// backslash.
pub fn escape_path(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    for ch in path.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            _ => out.push(ch),
        }
    }
    out
}

/// The path the escaped text says.
///
/// A backslash before a character the escape does not name is the
/// backslash and the character: the store is a file a person may fix by
/// hand, and a path that carries a backslash nobody meant to escape is
/// still the path it was.
pub fn unescape_path(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut escaped = false;
    for ch in text.chars() {
        if escaped {
            escaped = false;
            match ch {
                'n' => out.push('\n'),
                'r' => out.push('\r'),
                't' => out.push('\t'),
                '\\' => out.push('\\'),
                other => {
                    out.push('\\');
                    out.push(other);
                }
            }
            continue;
        }
        if ch == '\\' {
            escaped = true;
            continue;
        }
        out.push(ch);
    }
    if escaped {
        // A backslash at the end of the line is a backslash.
        out.push('\\');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::sync::{Arc, Barrier};

    /// A directory of the test's own.
    fn scratch(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("rgy-rgxy-{name}-{}", std::process::id()))
    }

    /// The temporary files the folder holds.
    ///
    /// The name of a temporary file ends in `.tmp`, whoever wrote it:
    /// the file of a write that did not go through is a leftover, and a
    /// leftover is what this looks for.
    fn temporary_files(dir: &Path) -> Vec<String> {
        std::fs::read_dir(dir)
            .map(|entries| {
                entries
                    .flatten()
                    .map(|entry| entry.file_name().to_string_lossy().into_owned())
                    .filter(|name| name.ends_with(".tmp"))
                    .collect()
            })
            .unwrap_or_default()
    }

    #[test]
    fn the_store_survives_a_restart() {
        let dir = scratch("mru");
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("usage.tsv");

        let mut history = History::new();
        history.record("C:/Users/dana/notas.txt", 1_000);
        history.record("C:/Users/dana/notas.txt", 2_000);
        save(&path, &history).expect("the store is written");

        // The restart: a history that knows nothing, reading the file.
        let read = load(&path).expect("the store is read");
        let entry = read
            .get("C:/Users/dana/notas.txt")
            .expect("the history is there");
        assert_eq!(entry.uses, 2);
        assert_eq!(entry.last, 2_000);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_store_that_is_not_there_is_no_history() {
        let dir = scratch("mru-missing");
        let _ = std::fs::remove_dir_all(&dir);
        assert!(
            load(&dir.join("usage.tsv"))
                .expect("no history, and no error")
                .is_empty()
        );
    }

    /// A store that cannot be read is an error.
    ///
    /// The read used to be swallowed: a file that could not be read
    /// became "no history", and the launcher said nothing — the
    /// ranking fell back to the daemon's order, the nudge never came
    /// back, and nobody knew why.
    #[test]
    fn a_store_that_cannot_be_read_is_an_error() {
        let dir = scratch("mru-unreadable");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("the folder is there");
        // The store's own path, taken by a directory: the read fails.
        let path = dir.join("usage.tsv");
        std::fs::create_dir_all(&path).expect("the store's path is taken");
        assert!(load(&path).is_err(), "the store could not be read");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_corrupt_store_is_no_history_and_not_a_crash() {
        let text = "# rgy usage\nnot a number\t12\t/a.txt\n5\tnot a time\t/b.txt\n5\t12\n";
        let history = parse(text);
        assert!(history.is_empty(), "{:?}", history.entries());
    }

    #[test]
    fn a_truncated_store_keeps_what_it_can() {
        // The last line was cut short: it is half a path, and half a path
        // is not a path.
        let text = "# rgy usage\n3\t1000\t/a.txt\n4\t2000\t/b.txt\n5\t3000\t/c.tx";
        let history = parse(text);
        assert_eq!(history.len(), 2);
        assert!(history.get("/a.txt").is_some());
        assert!(history.get("/c.tx").is_none());
    }

    #[test]
    fn a_path_may_carry_a_tab() {
        // The path is last: the first two tabs are the separators, and the
        // rest belongs to the path.
        let history = parse("1\t1000\t/a/we\tird.txt\n");
        assert!(history.get("/a/we\tird.txt").is_some());
    }

    #[test]
    fn the_store_is_a_text_a_person_can_read() {
        let mut history = History::new();
        history.record("/a/notas.txt", 1_000);
        let text = render(&history);
        assert!(text.contains("/a/notas.txt"), "{text}");
        assert!(text.contains("1\t1000\t"), "{text}");
    }

    /// The second write keeps what the file said.
    ///
    /// The file is read again before the write, so what it says is in
    /// what is written down: a store that dropped what another launcher
    /// recorded is a store that forgets, and the file is the only memory
    /// the launcher has.
    ///
    /// The count of a path is not the sum of the two: the caller's
    /// history was read from the file, so its counts already say what
    /// the file said.
    #[test]
    fn a_second_write_keeps_what_the_file_says() {
        let dir = scratch("mru-keep");
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("usage.tsv");

        let mut first = History::new();
        first.record("C:/Users/dana/notas.txt", 1_000);
        save(&path, &first).expect("the store is written");

        let mut second = History::new();
        second.record("C:/Users/dana/outro.txt", 2_000);
        save(&path, &second).expect("the store is written again");

        let read = load(&path).expect("the store is read");
        assert!(
            read.get("C:/Users/dana/notas.txt").is_some(),
            "{:?}",
            read.entries()
        );
        assert!(
            read.get("C:/Users/dana/outro.txt").is_some(),
            "{:?}",
            read.entries()
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The merge does not lose a use.
    ///
    /// Another launcher wrote the file with a later time and a smaller
    /// count: taking its entry whole threw the caller's count away, and
    /// the history went backwards. Each field is taken at its largest.
    #[test]
    fn the_merge_does_not_lose_a_use() {
        let mut on_disk = History::new();
        on_disk.insert("/tmp/notes.txt", 3, 200);
        let mut caller = History::new();
        caller.insert("/tmp/notes.txt", 7, 100);
        let merged = merge(&on_disk, &caller);
        let entry = merged.get("/tmp/notes.txt").expect("the entry is there");
        assert_eq!(entry.uses, 7, "the count went backwards");
        assert_eq!(entry.last, 200, "the later time was lost");
    }

    /// Two launchers, one store, and no entry lost.
    ///
    /// Each one holds its own history and writes it down. The second
    /// one to write reads what the file says now, so the entry the
    /// first one recorded is still there: the read, the merge and the
    /// write are one step, and the lock is what makes it one step.
    ///
    /// The test is not flaky: the two writers start together, but the
    /// order they take in does not matter — whoever writes second reads
    /// what the first one wrote, and the store keeps both entries.
    #[test]
    fn two_writers_keep_every_entry() {
        let dir = scratch("mru-two-writers");
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("usage.tsv");

        let mut first = History::new();
        first.record("/a/notas.txt", 1_000);
        let mut second = History::new();
        second.record("/b/outro.txt", 2_000);

        let start = Arc::new(Barrier::new(2));
        let mut handles = Vec::new();
        for history in [first, second] {
            let start = Arc::clone(&start);
            let path = path.clone();
            handles.push(std::thread::spawn(move || {
                start.wait();
                save(&path, &history)
            }));
        }
        let mut failures = Vec::new();
        for handle in handles {
            if let Err(err) = handle.join().expect("the writer") {
                failures.push(err.to_string());
            }
        }
        assert!(failures.is_empty(), "a writer's save failed: {failures:?}");

        let read = load(&path).expect("the store is read");
        assert!(
            read.get("/a/notas.txt").is_some(),
            "the first writer's entry is gone: {:?}",
            read.entries()
        );
        assert!(
            read.get("/b/outro.txt").is_some(),
            "the second writer's entry is gone: {:?}",
            read.entries()
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A write that cannot go through leaves the old store.
    ///
    /// The property the comment of [`save`] promises: what is on the
    /// disk is the old store or the new one, and a write that fails is
    /// the old one. The temporary file of this write is taken by a
    /// directory here, so the write has nowhere to go.
    #[test]
    fn a_write_that_cannot_be_written_leaves_the_old_store() {
        let dir = scratch("mru-unwritable");
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("usage.tsv");

        let mut history = History::new();
        history.record("C:/Users/dana/notas.txt", 1_000);
        save(&path, &history).expect("the store is written");
        let before = std::fs::read(&path).expect("the store is there");

        // The temporary file of this write, taken by a directory: the
        // name is the writer's own, so the test has to say which one it
        // takes.
        std::fs::create_dir_all(temp_path(&path, 7)).expect("the temporary path is taken");

        let mut other = History::new();
        other.record("C:/Users/dana/outro.txt", 2_000);
        assert!(
            write_store(&path, &other, 7).is_err(),
            "the write cannot finish"
        );

        assert_eq!(
            std::fs::read(&path).expect("the old store is there"),
            before,
            "the old store survived the failed write"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A write that cannot replace the store leaves the old store.
    ///
    /// The store is held open without `FILE_SHARE_DELETE`: the platform
    /// refuses to move a file over it, and the store that was there has
    /// to be there afterwards. The code used to remove the store
    /// *before* the move, so a launcher killed between the two left
    /// **no** store at all — and this is what that looked like.
    ///
    /// The temporary file is gone afterwards: a write that could not
    /// move its file does not leave it in the folder.
    #[cfg(windows)]
    #[test]
    fn a_write_that_cannot_replace_leaves_the_old_store() {
        use std::os::windows::fs::OpenOptionsExt;

        // The platform's own share flags: the file is shared for reading
        // and for writing, and not for deleting — and the move of a file
        // over one that is open without `FILE_SHARE_DELETE` is refused.
        const FILE_SHARE_READ: u32 = 0x0000_0001;
        const FILE_SHARE_WRITE: u32 = 0x0000_0002;

        let dir = scratch("mru-locked");
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("usage.tsv");

        let mut history = History::new();
        history.record("C:/Users/dana/notas.txt", 1_000);
        save(&path, &history).expect("the store is written");
        let before = std::fs::read(&path).expect("the store is there");

        // The store, there and held: the write goes into a file of its
        // own, and the move over the store does not happen.
        let held = std::fs::OpenOptions::new()
            .read(true)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
            .open(&path)
            .expect("the store is held");

        let mut other = History::new();
        other.record("C:/Users/dana/outro.txt", 2_000);
        assert!(save(&path, &other).is_err(), "the store cannot be replaced");

        assert_eq!(
            std::fs::read(&path).expect("the old store is there"),
            before,
            "the old store survived the failed write"
        );
        assert!(
            temporary_files(&dir).is_empty(),
            "a temporary file was left behind"
        );
        drop(held);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A store that is held is not moved aside.
    ///
    /// The write that cannot read the store used to move it aside. The
    /// code read "a file that cannot be read" and "a file that somebody
    /// else has open" as the same thing, and a store that was held for a
    /// moment was renamed to `.broken` — the user's memory gone, because
    /// another process had the file open at that exact instant. The store
    /// that was moved aside is a store the user cannot get back.
    ///
    /// The store is held here with no sharing at all, so the read is
    /// refused the way the platform refuses it: with the sharing
    /// violation, and not with the "this file is broken" that the
    /// quarantine is for. The write fails, the caller is told, and the
    /// store is where it was.
    #[cfg(windows)]
    #[test]
    fn a_store_that_is_held_is_not_moved_aside() {
        use std::os::windows::fs::OpenOptionsExt;

        let dir = scratch("mru-held");
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("usage.tsv");

        let mut history = History::new();
        history.record("C:/Users/dana/notas.txt", 1_000);
        save(&path, &history).expect("the store is written");
        let before = std::fs::read(&path).expect("the store is there");

        // The store, there and held: nobody can read it and nobody can
        // move it, and the write has to say so.
        let held = std::fs::OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(&path)
            .expect("the store is held");

        let mut other = History::new();
        other.record("C:/Users/dana/outro.txt", 2_000);
        assert!(save(&path, &other).is_err(), "the store cannot be read");

        drop(held);
        assert_eq!(
            std::fs::read(&path).expect("the store is there"),
            before,
            "the store survived the failed write"
        );
        assert!(
            !quarantine_path(&path).exists(),
            "the store was moved aside"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A move that fails leaves no temporary file behind.
    ///
    /// The store's own path is taken by a directory here, so the move
    /// that replaces it cannot happen: the file that was written is not
    /// the store, and a file that is not the store is a leftover. The
    /// leftover used to stay in the cache folder for ever, saying
    /// nothing about anything.
    #[test]
    fn a_failed_rename_leaves_no_temporary_file() {
        let dir = scratch("mru-failed-rename");
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("usage.tsv");

        // The store's own path, taken by a directory.
        std::fs::create_dir_all(&path).expect("the store's path is taken");

        let mut history = History::new();
        history.record("C:/Users/dana/notas.txt", 1_000);
        assert!(save(&path, &history).is_err(), "the move cannot happen");

        assert!(path.is_dir(), "the store's path is still a directory");
        assert!(
            temporary_files(&dir).is_empty(),
            "a temporary file was left behind"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The writers do not steal each other's file.
    ///
    /// The temporary file used to have one name for everybody, so two
    /// writers wrote into the same file: the first one to move it took
    /// the other's content with it, and the other's move failed because
    /// the file was not there any more. Eight writers and 320 saves lost
    /// about one save in eight, and every lost save was an update the
    /// caller had made and was never told about.
    ///
    /// The barrier is what makes the collision real: the writers start
    /// at the same instant, so their writes overlap. The count is what
    /// makes the old shape fail every time: with one name for
    /// everybody, a move fails about one time in eight, and 320 saves do
    /// not get that lucky.
    ///
    /// The test was flaky, and the comment that stood here said it was
    /// not. The claim was about the writers: every writer writes to a
    /// file of its own, the writers do not read the store, and the lock
    /// holds one writer at a time. All of that is true — and none of it
    /// is the whole story. On Windows the platform refuses the read and
    /// the move of a file that another **process** has open, and the
    /// writers of this test are not the only processes on the machine.
    /// The platform does not say who has the file; it says that somebody
    /// does. The write that happened to be on the store at that instant
    /// was dropped, with the caller's update lost and the launcher never
    /// told.
    ///
    /// The fix is not in the test. A refusal that lasts a moment is not a
    /// refusal: the read and the move wait now, the wait the lock waits.
    /// The test still says what it says — no writer loses an update — and
    /// it says it on a machine that is busy.
    #[test]
    fn concurrent_writers_lose_no_update_and_leave_no_temporary_file() {
        const WRITERS: usize = 8;
        const ROUNDS: usize = 40;

        let dir = scratch("mru-concurrent");
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("usage.tsv");

        let start = Arc::new(Barrier::new(WRITERS));
        let mut handles = Vec::new();
        for writer in 0..WRITERS {
            let start = Arc::clone(&start);
            let path = path.clone();
            handles.push(std::thread::spawn(move || {
                start.wait();
                let mut dropped = Vec::new();
                for round in 0..ROUNDS {
                    let mut history = History::new();
                    history.record(&format!("/writer-{writer}/round-{round}.txt"), 1_000);
                    if let Err(err) = save(&path, &history) {
                        dropped.push(format!("writer {writer} round {round}: {err}"));
                    }
                }
                dropped
            }));
        }
        let mut dropped = Vec::new();
        for handle in handles {
            dropped.extend(handle.join().expect("the writer"));
        }
        assert!(
            dropped.is_empty(),
            "a writer's update was dropped: {dropped:?}"
        );

        // The store holds every writer's entry: the write reads the file
        // again, so what one writer said is in what the next one wrote.
        let read = load(&path).expect("the store is read");
        assert_eq!(
            read.len(),
            WRITERS * ROUNDS,
            "the store lost entries: {:?}",
            read.entries()
        );
        for writer in 0..WRITERS {
            for round in 0..ROUNDS {
                let entry = format!("/writer-{writer}/round-{round}.txt");
                assert!(read.get(&entry).is_some(), "the entry of {entry} is gone");
            }
        }

        // And the folder holds the store and nothing else.
        assert!(
            temporary_files(&dir).is_empty(),
            "a temporary file was left behind"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The path that carries a character a line cannot carry.
    ///
    /// A newline in a path is two lines in a file that has one entry per
    /// line, and the launcher would open the first half of the path.
    /// The escape is what keeps the entry whole.
    #[test]
    fn a_path_with_a_newline_survives_the_store() {
        let dir = scratch("mru-newline");
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("usage.tsv");

        let tricky = "/a/we\nird.txt";
        let mut history = History::new();
        history.record(tricky, 1_000);
        save(&path, &history).expect("the store is written");

        // The store is one entry per line: the newline in the path did
        // not become a second line.
        let text = std::fs::read_to_string(&path).expect("the store is there");
        assert_eq!(text.lines().count(), 3, "{text}");
        assert!(text.contains("we\\nird.txt"), "{text}");

        let read = load(&path).expect("the store is read");
        assert!(
            read.get(tricky).is_some(),
            "the path did not survive: {:?}",
            read.entries()
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// What is written is what is read back.
    ///
    /// Every shape a path can take: the separators of the format, the
    /// character the escape is made of, and a name that is not ASCII.
    #[test]
    fn the_escape_round_trips() {
        let paths = [
            "/a/plain.txt",
            "/a/we\tird.txt",
            "/a/we\nird.txt",
            "/a/we\\ird.txt",
            "/a/we\\nird.txt",
            "/a/ção — ñ.txt",
        ];
        for path in paths {
            let mut history = History::new();
            history.record(path, 1_000);
            let read = parse(&render(&history));
            assert!(read.get(path).is_some(), "{path} did not survive");
            assert_eq!(unescape_path(&escape_path(path)), path);
        }
    }

    /// The store that was written before the escape is still read.
    ///
    /// The old format has no escape in it, so a path that looks like one
    /// is still the path it was: the version in the header is what tells
    /// the two formats apart.
    #[test]
    fn a_store_written_before_the_escape_is_still_read() {
        // The old header, and a path whose backslash-n is two
        // characters and not a newline.
        let old = parse("# rgy usage: count\tlast\tpath\n3\t1000\t/a/we\\nird.txt\n");
        let entry = old.get("/a/we\\nird.txt").expect("the path is there");
        assert_eq!(entry.uses, 3);
        assert_eq!(entry.last, 1000);

        // A file with no header at all is the old format too.
        let old = parse("3\t1000\t/a/we\\nird.txt\n");
        assert!(old.get("/a/we\\nird.txt").is_some());
    }

    /// The same path, written by the two formats, is the same path.
    ///
    /// The new format escapes the backslash, so the file says what the
    /// old file said: the escape is not a change of the path, it is a
    /// change of the spelling.
    #[test]
    fn the_header_tells_the_two_formats_apart() {
        let old = parse("# rgy usage: count\tlast\tpath\n1\t1000\t/a/we\\nird.txt\n");
        let new = parse("# rgy usage v2: count\tlast\tpath\n1\t1000\t/a/we\\\\nird.txt\n");
        assert!(old.get("/a/we\\nird.txt").is_some(), "{:?}", old.entries());
        assert!(new.get("/a/we\\nird.txt").is_some(), "{:?}", new.entries());
    }

    /// A store that cannot be read does not take every use with it.
    ///
    /// The broken file is moved aside and the store is written again: a
    /// store that stays broken is a launcher that forgets everything
    /// from now on, and the file that broke is still there to look at.
    #[test]
    fn a_store_that_cannot_be_read_is_moved_aside_and_written_again() {
        let dir = scratch("mru-recover");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("the folder is there");
        let path = dir.join("usage.tsv");
        // A file that is not text: the read fails on the bytes.
        let broken = [0xff_u8, 0xfe, 0x00];
        std::fs::write(&path, broken).expect("the broken store is there");

        let mut history = History::new();
        history.record("/a/notas.txt", 1_000);
        save(&path, &history).expect("the store is written again");

        let read = load(&path).expect("the store is read");
        assert!(
            read.get("/a/notas.txt").is_some(),
            "the entry did not survive: {:?}",
            read.entries()
        );

        // The broken file is still there, under a name of its own.
        let aside = dir.join("usage.tsv.broken");
        assert_eq!(
            std::fs::read(&aside).expect("the broken store is there"),
            broken,
            "the broken store was not kept"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
