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
    dir.join(format!("{name}{}", std::env::consts::EXE_SUFFIX))
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

/// Run a bin capturing (exit code, stdout, stderr).
pub fn run(exe: &Path, args: &[&str]) -> (i32, String, String) {
    let out = Command::new(exe)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .unwrap_or_else(|e| panic!("spawn {}: {e}", exe.display()));
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// Run a bin with a stdin payload, then close stdin (EOF) and collect output.
/// Closing stdin matters: `searchd` serves until EOF and must not hang.
pub fn run_stdin(exe: &Path, args: &[&str], input: &str) -> (i32, String, String) {
    use std::io::Write;
    let mut child = Command::new(exe)
        .args(args)
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
