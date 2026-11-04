// SPDX-License-Identifier: Apache-2.0

//! Shared e2e helpers: locate the built bins, build runtime fixtures.
//!
//! The workspace root is virtual (no root package), so `CARGO_BIN_EXE_*`
//! is not set for `searchctl`/`searchd` from this crate. Walk up from the
//! test executable (`target/<profile>/deps/<harness>`) to the profile dir
//! where cargo drops workspace bin targets.

// Both harnesses include this module; each uses a subset.
#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Path to a workspace bin target built alongside this test harness.
///
/// The `tests` crate is a separate workspace member that locates the bins by
/// path rather than depending on them, so `cargo test -p rgy-e2e-tests` alone
/// does **not** rebuild them — it will happily exercise a stale exe and report
/// green. Refuse that: if the binary is older than its own sources, say so
/// instead of testing yesterday's code.
pub fn bin(name: &str) -> PathBuf {
    let key = format!("CARGO_BIN_EXE_{name}");
    if let Some(p) = std::env::var_os(&key) {
        return PathBuf::from(p);
    }
    let mut dir = std::env::current_exe().expect("current_exe");
    dir.pop(); // …/deps/<harness>
    if dir.file_name().and_then(|s| s.to_str()) == Some("deps") {
        dir.pop(); // …/<profile>
    }
    let exe = dir.join(format!("{name}{}", std::env::consts::EXE_SUFFIX));
    assert_up_to_date(name, &exe);
    exe
}

/// Panics when `exe` predates the sources it is built from.
///
/// The bin's own sources AND the crates it links: the harness resolves the
/// bin by path, so cargo never rebuilds it, and a change in a crate leaves a
/// binary that is newer than the bin's own sources and older than the code
/// it was built from. Comparing only the bin's own sources is how the
/// harness ends up exercising yesterday's code and reporting green.
///
/// The crates it links are read out of cargo's own dep-info — the file
/// next to the binary that names every file the compiler read for it. The
/// whole tree would be simpler, and it would be wrong: cargo relinks only
/// what changed, so a change in a crate the bin does not link leaves the
/// binary older than the tree for good, and "build first" would not fix it.
/// The dep-info is the exact answer to "what this binary is made of".
fn assert_up_to_date(name: &str, exe: &Path) {
    let built = match exe.metadata().and_then(|m| m.modified()) {
        Ok(t) => t,
        Err(_) => return, // no binary yet: let the spawn fail loudly instead
    };
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let src = root.join("bins").join(name);
    // The bin's own sources AND the crates it is built from: the harness
    // resolves the bin by path, so cargo never rebuilds it, and a change in
    // `crates/name-core` leaves a binary that is newer than the bin's own
    // sources and older than the code it was built from. Comparing only the
    // bin's own sources is how the harness ends up exercising yesterday's
    // code and reporting green.
    let mut newest = newest_source(&src.join("src"));
    match dep_info_sources(exe) {
        Some(sources) => {
            for path in sources {
                let meta = match path.metadata() {
                    Ok(meta) => meta,
                    Err(_) => continue,
                };
                let modified = match meta.modified() {
                    Ok(modified) => modified,
                    Err(_) => continue,
                };
                if newest.as_ref().map(|n| modified > n.0).unwrap_or(true) {
                    newest = Some((modified, path));
                }
            }
        }
        // No dep-info: the binary was not built here, so the tree is all
        // the guard knows, and the tree is the conservative answer.
        None => {
            if let Some(crates) = newest_source(&root.join("crates")) {
                if newest.as_ref().map(|n| crates.0 > n.0).unwrap_or(true) {
                    newest = Some(crates);
                }
            }
        }
    }
    if let Some((newest, path)) = newest {
        // The path is in the message on purpose. The guard compares the bin
        // against the sources it was built from, so a message that names
        // only a time leaves the reader to guess which file moved.
        let rel = path.strip_prefix(&root).unwrap_or(&path);
        assert!(
            built >= newest,
            "{name} at {} is older than the sources it is built from (the newest is {} at {:?}). \
             The e2e harness resolves bins by path, so `cargo test -p rgy-e2e-tests` \
             does not rebuild them - build first (`cargo build --workspace`), or \
             this run would test stale code. (`cargo test --workspace` alone is \
             not a build-first: on a clean tree the e2e tests race the rest of \
             the workspace and spawn nothing.)",
            exe.display(),
            rel.display(),
            newest
        );
    }
}

/// The files the binary was built from, as cargo recorded them.
///
/// Cargo writes a dep-info file next to every artifact: the output and
/// every file the compiler read for it, one per line, Makefile-style. It
/// names the bin's own sources and the crates it links — and nothing else,
/// which is the point.
///
/// A path with a space in it is written with the space escaped, so the
/// fragments come apart and have to be put back: the file that exists is
/// the one with the space in it.
fn dep_info_sources(exe: &Path) -> Option<Vec<PathBuf>> {
    let stem = exe.file_stem()?.to_str()?;
    let text = std::fs::read_to_string(exe.with_file_name(format!("{stem}.d"))).ok()?;
    // The line is "<output>: <inputs>". The separator is the first colon
    // followed by a space: a drive letter's colon is followed by a
    // backslash.
    let (_, inputs) = text.split_once(": ")?;
    let mut out = Vec::new();
    let mut buffer = String::new();
    for fragment in inputs.split_whitespace() {
        if buffer.is_empty() {
            buffer.push_str(fragment);
            continue;
        }
        let joined = format!("{buffer} {fragment}");
        if Path::new(&joined).exists() {
            buffer = joined;
            continue;
        }
        if Path::new(&buffer).exists() {
            out.push(PathBuf::from(&buffer));
        }
        buffer.clear();
        buffer.push_str(fragment);
    }
    if !buffer.is_empty() && Path::new(&buffer).exists() {
        out.push(PathBuf::from(&buffer));
    }
    Some(out)
}

/// The newest source under `dir`: its modification time, and the path of
/// the file it belongs to.
///
/// The path comes back with the time because the guard's message names
/// it: a time alone says that something moved and not what.
fn newest_source(dir: &Path) -> Option<(std::time::SystemTime, PathBuf)> {
    let entries = std::fs::read_dir(dir).ok()?;
    let mut newest: Option<(std::time::SystemTime, PathBuf)> = None;
    for entry in entries.flatten() {
        let Ok(meta) = entry.metadata() else { continue };
        if meta.is_dir() {
            if let Some(inner) = newest_source(&entry.path()) {
                if newest.as_ref().map(|n| inner.0 > n.0).unwrap_or(true) {
                    newest = Some(inner);
                }
            }
            continue;
        }
        if let Ok(modified) = meta.modified() {
            if newest.as_ref().map(|n| modified > n.0).unwrap_or(true) {
                newest = Some((modified, entry.path()));
            }
        }
    }
    newest
}

/// A directory in the temp folder that takes itself away.
///
/// The fixture of a test is a fresh tree, and a fresh tree per test means
/// a directory per test: twenty thousand of them, and a hundred and fifty
/// megabytes of index caches, were measured in the temp folder of the
/// machine that ran the suite all day. The guard removes the directory
/// when the test is done with it, and a directory that something still
/// holds stays where it is — the platform's own rules about an open file
/// say what happens then, and a directory that cannot be removed is not
/// worth a failed test.
pub struct TempDir {
    path: PathBuf,
}

impl TempDir {
    /// A fresh directory, named after what it is for.
    fn new(tag: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "rgy-e2e-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&path).unwrap();
        Self { path }
    }

    /// The directory itself.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl std::ops::Deref for TempDir {
    type Target = Path;

    fn deref(&self) -> &Path {
        &self.path
    }
}

impl AsRef<Path> for TempDir {
    fn as_ref(&self) -> &Path {
        &self.path
    }
}

impl AsRef<std::ffi::OsStr> for TempDir {
    fn as_ref(&self) -> &std::ffi::OsStr {
        self.path.as_os_str()
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        // A directory that cannot be removed — a daemon still holding it,
        // say — stays. The test is worth more than the tidy folder.
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// Unique temp fixture root, taken away when the test is done with it.
pub fn fixture_dir(tag: &str) -> TempDir {
    TempDir::new(tag)
}

/// A small tree with a known name and a known content string.
pub fn text_tree(root: &Path) {
    std::fs::write(root.join("hello.txt"), "hello needle world\nsecond line\n").unwrap();
    std::fs::write(root.join("notes.md"), "draft with zebra marker\n").unwrap();
    // same stem, different extension — for ext: narrowing
    std::fs::write(root.join("alpha.md"), "markdown alpha body\n").unwrap();
    std::fs::write(root.join("alpha.txt"), "plain alpha body\n").unwrap();
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(
        root.join("src/main.rs"),
        "fn main() {\n    let needle = 1;\n}\n",
    )
    .unwrap();
    std::fs::write(root.join("src/util.rs"), "pub fn helper() {}\n").unwrap();
    std::fs::create_dir_all(root.join("docs")).unwrap();
    std::fs::write(root.join("docs/readme.txt"), "see the zebra in docs\n").unwrap();
}

/// Isolated index-cache root for one spawned bin.
///
/// Without this a spawned `searchctl`/`searchd` writes its index into the
/// developer's real `%LOCALAPPDATA%\rgy`, so every test run leaves a stray
/// index behind — and since fixtures are fresh temp trees, a cached index
/// from a previous run could answer a query it has no business answering.
/// `RGY_INDEX_DIR` is the same override `rgy_name_core::persist` reads.
fn index_cache_dir() -> TempDir {
    TempDir::new("cache")
}

/// Run a bin capturing (exit code, stdout, stderr).
pub fn run(exe: &Path, args: &[&str]) -> (i32, String, String) {
    let cache = index_cache_dir();
    let out = Command::new(exe)
        .args(args)
        .env("RGY_INDEX_DIR", &cache)
        .stdin(Stdio::null())
        .output()
        .unwrap_or_else(|e| panic!("spawn {}: {e}", exe.display()));
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// Same as [`run`], but against a caller-chosen index cache.
///
/// The default [`run`] hands every spawn a fresh cache, which is what keeps
/// the fixtures hermetic — but a test that wants to prove a second run
/// *reuses* what the first saved needs the same cache twice.
pub fn run_in_cache(exe: &Path, args: &[&str], cache: &Path) -> (i32, String, String) {
    std::fs::create_dir_all(cache).unwrap();
    let out = Command::new(exe)
        .args(args)
        .env("RGY_INDEX_DIR", cache)
        .stdin(Stdio::null())
        .output()
        .unwrap_or_else(|e| panic!("spawn {}: {e}", exe.display()));
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// A daemon left running, killed on drop.
///
/// Without this a test that panics leaves `searchd.exe` alive, which then
/// holds the binary and makes the *next* `cargo build` fail with "failed to
/// remove file" — a failure that looks nothing like its real cause.
pub struct Daemon {
    /// The daemon itself.
    pub child: std::process::Child,
    /// The index cache it writes into.
    ///
    /// The cache lives as long as the daemon does: a daemon whose cache
    /// was taken away underneath it would answer from nothing, and the
    /// test would be measuring the removal instead of the answer.
    _cache: TempDir,
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// A daemon on the endpoint the clients look for, and the environment that
/// finds it.
///
/// The registered endpoint is the named pipe that carries the user's name
/// on Windows, and the socket in the cache directory everywhere else. A
/// test points the client at it through the environment -- `USERNAME` on
/// Windows, the cache directory everywhere else -- so a test never touches
/// the daemon the machine really has.
pub struct Registered {
    /// The daemon, killed when the test ends.
    pub daemon: Daemon,
    /// The environment a client has to see.
    pub env: Vec<(&'static str, String)>,
    /// The cache directory, where the socket lives.
    ///
    /// It lives as long as the daemon does: a socket whose directory is
    /// gone is a socket nobody can reach, and the test would be measuring
    /// the removal instead of the answer.
    _cache: Option<TempDir>,
}

/// A daemon over `root`, on the registered endpoint.
#[cfg(windows)]
pub fn registered(root: &Path) -> Registered {
    let user = format!("rgy-e2e-{}", unique());
    let args = [
        "--root".to_string(),
        root.to_str().unwrap().to_string(),
        "--listen".to_string(),
        format!("pipe:rgy-searchd-{user}"),
    ];
    let borrowed: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
    Registered {
        daemon: spawn_pipe(&bin("searchd"), &borrowed),
        env: vec![("USERNAME", user)],
        _cache: None,
    }
}

/// A daemon over `root`, on the registered endpoint.
#[cfg(unix)]
pub fn registered(root: &Path) -> Registered {
    let cache = TempDir::new("registered");
    std::fs::create_dir_all(cache.join("rgy")).unwrap();
    let args = [
        "--root".to_string(),
        root.to_str().unwrap().to_string(),
        "--listen".to_string(),
        format!("uds:{}/rgy/searchd.sock", cache.path().display()),
    ];
    let borrowed: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
    Registered {
        daemon: spawn_uds(&bin("searchd"), &borrowed),
        env: vec![(
            "XDG_CACHE_HOME",
            cache.path().to_string_lossy().into_owned(),
        )],
        _cache: Some(cache),
    }
}

/// The environment of an endpoint nobody listens on.
pub fn absent() -> Vec<(&'static str, String)> {
    #[cfg(windows)]
    {
        vec![("USERNAME", format!("rgy-e2e-none-{}", unique()))]
    }
    #[cfg(unix)]
    {
        let cache = std::env::temp_dir().join(format!("rgy-e2e-none-{}", unique()));
        vec![("XDG_CACHE_HOME", cache.to_string_lossy().into_owned())]
    }
}

/// A token that is different for every call, so no two tests share a
/// daemon.
pub fn unique() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    format!(
        "{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    )
}

/// Spawn `searchd` listening on a named pipe, left running.
///
/// Unlike [`run_stdin`], the process stays up so a test can hold several
/// clients open at the same time.
#[cfg(windows)]
pub fn spawn_pipe(exe: &Path, args: &[&str]) -> Daemon {
    let cache = index_cache_dir();
    let child = Command::new(exe)
        .args(args)
        .env("RGY_INDEX_DIR", &cache)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap_or_else(|e| panic!("spawn {}: {e}", exe.display()));
    Daemon {
        child,
        _cache: cache,
    }
}

/// Spawn `searchd` listening on a unix socket, left running.
///
/// The unix counterpart of [`spawn_pipe`]: the socket has to be in a
/// directory that exists, and the path has to be short enough for the
/// platform's own limit.
#[cfg(unix)]
pub fn spawn_uds(exe: &Path, args: &[&str]) -> Daemon {
    let cache = index_cache_dir();
    let child = Command::new(exe)
        .args(args)
        .env("RGY_INDEX_DIR", &cache)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap_or_else(|e| panic!("spawn {}: {e}", exe.display()));
    Daemon {
        child,
        _cache: cache,
    }
}

/// A named-pipe client held open by the caller.
///
/// Opening blocks until the server has a free instance, so constructing two
/// of these before releasing either one is what proves the daemon can take
/// two clients at once.
#[cfg(windows)]
pub struct PipeClient(std::fs::File);

#[cfg(windows)]
impl PipeClient {
    /// Connect, blocking until the daemon offers an instance.
    ///
    /// A client arriving in the instant between an acceptor connecting and its
    /// replacement being created sees ERROR_PIPE_BUSY (231) rather than
    /// waiting, so retry briefly — that is what any real client has to do.
    pub fn connect(pipe: &str) -> std::io::Result<Self> {
        let path = format!(r"\\.\pipe\{pipe}");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            match std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(&path)
            {
                Ok(f) => return Ok(Self(f)),
                Err(e) if e.raw_os_error() == Some(231) || e.raw_os_error() == Some(2) => {
                    if std::time::Instant::now() >= deadline {
                        return Err(e);
                    }
                    std::thread::sleep(std::time::Duration::from_millis(25));
                }
                Err(e) => return Err(e),
            }
        }
    }

    /// Send one request and collect lines up to the `done` terminator.
    pub fn request(&mut self, request: &str) -> std::io::Result<Vec<String>> {
        use std::io::{BufRead, BufReader, Write};
        self.0.write_all(format!("{request}\n").as_bytes())?;
        self.0.flush()?;
        let mut r = BufReader::new(&self.0);
        let mut out = Vec::new();
        loop {
            let mut line = String::new();
            if r.read_line(&mut line)? == 0 {
                break;
            }
            let line = line.trim().to_string();
            if line.is_empty() {
                continue;
            }
            let done = line.contains("\"op\":\"done\"");
            out.push(line);
            if done {
                break;
            }
        }
        Ok(out)
    }
}

/// A cache directory a test can reuse across spawns.
pub fn shared_cache_dir(tag: &str) -> TempDir {
    TempDir::new(&format!("shared-{tag}"))
}

/// Run a bin with a stdin payload, then close stdin (EOF) and collect output.
/// Closing stdin matters: `searchd` serves until EOF and must not hang.
pub fn run_stdin(exe: &Path, args: &[&str], input: &str) -> (i32, String, String) {
    use std::io::Write;
    let cache = index_cache_dir();
    let mut child = Command::new(exe)
        .args(args)
        .env("RGY_INDEX_DIR", &cache)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap_or_else(|e| panic!("spawn {}: {e}", exe.display()));
    {
        let mut stdin = child.stdin.take().expect("stdin");
        stdin.write_all(input.as_bytes()).expect("write stdin");
        // drop closes the handle → EOF
    }
    let out = child.wait_with_output().expect("wait");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The dep-info names the files the binary was built from.
    ///
    /// The guard reads it instead of walking the tree, so a parser that
    /// reads it wrong makes the guard wrong: too few files and it misses a
    /// stale binary, too many and it fails on a binary that is current.
    #[test]
    fn the_dep_info_names_the_sources_the_binary_was_built_from() {
        let dir = TempDir::new("dep-info");
        let one = dir.path().join("one.rs");
        let two = dir.path().join("two.rs");
        std::fs::write(&one, "fn one() {}").expect("write one");
        std::fs::write(&two, "fn two() {}").expect("write two");

        let exe = dir.path().join("fake.exe");
        std::fs::write(&exe, b"").expect("write exe");
        std::fs::write(
            dir.path().join("fake.d"),
            format!("{}: {} {}\n", exe.display(), one.display(), two.display()),
        )
        .expect("write dep-info");

        let sources = dep_info_sources(&exe).expect("the dep-info is read");
        assert!(
            sources.contains(&one) && sources.contains(&two),
            "the two sources are named: {sources:?}"
        );
        assert_eq!(sources.len(), 2, "and nothing else is: {sources:?}");
    }

    /// A path with a space in it is written escaped, so the fragments come
    /// apart and have to be put back.
    #[test]
    fn a_source_with_a_space_in_its_path_is_put_back_together() {
        let dir = TempDir::new("dep-info-space");
        let spaced = dir.path().join("a file.rs");
        std::fs::write(&spaced, "fn spaced() {}").expect("write the source");

        let exe = dir.path().join("fake.exe");
        std::fs::write(&exe, b"").expect("write exe");
        std::fs::write(
            dir.path().join("fake.d"),
            format!("{}: {}\n", exe.display(), spaced.display()),
        )
        .expect("write dep-info");

        let sources = dep_info_sources(&exe).expect("the dep-info is read");
        assert!(
            sources.contains(&spaced),
            "the path with the space in it is one path again: {sources:?}"
        );
    }

    /// A binary with no dep-info beside it was not built here: the guard
    /// has no record of its sources and falls back to the whole tree.
    #[test]
    fn a_binary_without_a_dep_info_has_no_record() {
        let dir = TempDir::new("dep-info-missing");
        let exe = dir.path().join("fake.exe");
        std::fs::write(&exe, b"").expect("write exe");
        assert!(
            dep_info_sources(&exe).is_none(),
            "there is no dep-info, so there is no record"
        );
    }
}
