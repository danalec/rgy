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

/// Panics when `exe` predates the sources of the crate that builds it.
fn assert_up_to_date(name: &str, exe: &Path) {
    let built = match exe.metadata().and_then(|m| m.modified()) {
        Ok(t) => t,
        Err(_) => return, // no binary yet: let the spawn fail loudly instead
    };
    let src = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("bins")
        .join(name);
    let newest = std::fs::read_dir(src.join("src"))
        .ok()
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| e.metadata().ok())
        .filter_map(|m| m.modified().ok())
        .max();
    if let Some(newest) = newest {
        assert!(
            built >= newest,
            "{name} at {} is older than its sources (newest source modified {:?}). \
             The e2e harness resolves bins by path, so `cargo test -p rgy-e2e-tests` \
             does not rebuild them — build first (`cargo build -p {name}` or \
             `cargo test --workspace`), or this run would test stale code.",
            exe.display(),
            newest
        );
    }
}

/// Unique temp fixture root; removed by the OS temp cleaner eventually
/// (same approach as rgx's tests/cli.rs — no tempfile crate).
pub fn fixture_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "rgy-e2e-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
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
/// `RGY_INDEX_DIR` is the same override `name_core::persist` reads.
fn index_cache_dir() -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "rgy-e2e-cache-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Run a bin capturing (exit code, stdout, stderr).
pub fn run(exe: &Path, args: &[&str]) -> (i32, String, String) {
    let out = Command::new(exe)
        .args(args)
        .env("RGY_INDEX_DIR", index_cache_dir())
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
pub struct Daemon(pub std::process::Child);

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Spawn `searchd` listening on a named pipe, left running.
///
/// Unlike [`run_stdin`], the process stays up so a test can hold several
/// clients open at the same time.
#[cfg(windows)]
pub fn spawn_pipe(exe: &Path, args: &[&str]) -> Daemon {
    Daemon(
        Command::new(exe)
            .args(args)
            .env("RGY_INDEX_DIR", index_cache_dir())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap_or_else(|e| panic!("spawn {}: {e}", exe.display())),
    )
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
pub fn shared_cache_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "rgy-e2e-shared-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

/// Run a bin with a stdin payload, then close stdin (EOF) and collect output.
/// Closing stdin matters: `searchd` serves until EOF and must not hang.
pub fn run_stdin(exe: &Path, args: &[&str], input: &str) -> (i32, String, String) {
    use std::io::Write;
    let mut child = Command::new(exe)
        .args(args)
        .env("RGY_INDEX_DIR", index_cache_dir())
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
