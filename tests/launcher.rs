// SPDX-License-Identifier: Apache-2.0

//! The launcher, from the outside.
//!
//! The launcher is a **client**: the index lives in the daemon, and a
//! keystroke is a question sent down a pipe. These tests start a daemon
//! of their own — on a pipe of their own — and ask the launcher
//! questions, the way a user asks them.
//!
//! The launcher is asked twice, and the two answers are different
//! questions: the **name** layer asks the engine to rank the names in the
//! index, and the **content** layer asks what is *inside* the files. The
//! second one is the second layer, and its scope is the first one's
//! answer.

mod harness;

use std::path::Path;
use std::time::{Duration, Instant};

use harness::{bin, fixture_dir, text_tree, Daemon};
// The daemon is reached over a named pipe on Windows and over a unix
// socket everywhere else, and the helper that spawns it is the platform's
// own: one endpoint shape, one helper.
#[cfg(windows)]
use harness::spawn_pipe;

/// How long the tests wait for a daemon that is still starting.
const READY: Duration = Duration::from_secs(20);

/// A daemon of the test's own, on an endpoint of the test's own.
struct Fixture {
    /// The guard that kills the daemon when the test ends.
    _daemon: Daemon,
    /// The directory the socket lives in.
    ///
    /// The socket is a file in a directory, and the directory has to be
    /// there while the daemon listens: a client that arrives at a path
    /// whose directory is gone finds nothing there, and the test would be
    /// measuring the removal instead of the answer. The field is declared
    /// after the daemon so that it is dropped after it — the daemon goes
    /// first, and the directory goes with it.
    #[cfg(unix)]
    _dir: harness::TempDir,
    /// The endpoint the launcher is pointed at.
    endpoint: String,
}

/// A token that is different for every call, so no two tests share an
/// endpoint.
///
/// The endpoint is the one thing a parallel test run cannot share: a
/// daemon that owns the pipe is the daemon every client talks to, and two
/// tests pointing at the same pipe are two tests talking to whichever
/// daemon won the race — with the other's tree. The tree is the same in
/// both fixtures, but the *session* is not, and the answer is not.
fn unique() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    format!(
        "{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    )
}

/// Run the launcher, in English.
///
/// The language is pinned so that the assertions do not depend on the
/// machine the suite runs on: a machine whose locale is Portuguese would
/// otherwise fail a suite that is about English, and a suite that only
/// passes on an English machine says nothing about the language at all.
fn run_launcher(args: &[&str]) -> (i32, String, String) {
    run_in("en", args)
}

/// Run the launcher, in the language the environment asks for.
///
/// The harness's own runner does not take an environment, and the
/// language is exactly what the environment says: a test that cannot set
/// it cannot prove that the launcher reads it.
fn run_in(lang: &str, args: &[&str]) -> (i32, String, String) {
    let cache = harness::shared_cache_dir("rgxy-lang");
    let out = std::process::Command::new(bin("rgxy"))
        .args(args)
        .env("RGY_LANG", lang)
        .env("RGY_INDEX_DIR", &cache)
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap_or_else(|e| panic!("spawn rgxy: {e}"));
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// Start a daemon over `root`, and wait for it to be there.
///
/// The launcher connects on demand and says so when it cannot: a
/// launcher that is started before the daemon is a launcher that is
/// asked to retry, and the test does exactly what a person does — asks
/// again.
#[cfg(windows)]
fn start(root: &Path) -> Fixture {
    start_with(root, &[])
}

/// A daemon over `root`, with the extra flags the test needs.
#[cfg(windows)]
fn start_with(root: &Path, extra: &[&str]) -> Fixture {
    let pipe = format!("rgy-e2e-rgxy-{}", unique());
    let mut args: Vec<String> = vec![
        "--root".to_string(),
        root.to_str().unwrap().to_string(),
        "--listen".to_string(),
        format!("pipe:{pipe}"),
    ];
    args.extend(extra.iter().map(|s| s.to_string()));
    let borrowed: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
    let daemon = spawn_pipe(&bin("searchd"), &borrowed);
    Fixture {
        _daemon: daemon,
        endpoint: format!("pipe:{pipe}"),
    }
}

/// Start a daemon over `root`, and wait for it to be there.
#[cfg(unix)]
fn start(root: &Path) -> Fixture {
    start_with(root, &[])
}

/// A daemon over `root`, with the extra flags the test needs.
#[cfg(unix)]
fn start_with(root: &Path, extra: &[&str]) -> Fixture {
    let dir = fixture_dir("rgxy-sock");
    let socket = dir.join(format!("searchd-{}.sock", unique()));
    let mut args: Vec<String> = vec![
        "--root".to_string(),
        root.to_str().unwrap().to_string(),
        "--listen".to_string(),
        format!("uds:{}", socket.display()),
    ];
    args.extend(extra.iter().map(|s| s.to_string()));
    let borrowed: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
    let daemon = harness::spawn_uds(&bin("searchd"), &borrowed);
    Fixture {
        _daemon: daemon,
        _dir: dir,
        endpoint: format!("uds:{}", socket.display()),
    }
}

/// Ask the launcher a question, and retry while the daemon is starting.
///
/// The retry is the point: the launcher is resident and the daemon may
/// come up a moment later, so a question asked too early is answered
/// "not yet" and asked again — not lost.
fn ask(fixture: &Fixture, query: &str) -> (i32, String, String) {
    let deadline = Instant::now() + READY;
    loop {
        let (code, stdout, stderr) =
            run_launcher(&["--endpoint", &fixture.endpoint, "--query", query]);
        if code != 1 || Instant::now() >= deadline {
            return (code, stdout, stderr);
        }
        // Exit 1 is "no results" *or* "no daemon": the difference is in
        // the stderr, and the launcher says which.
        if !stderr.contains("no connection to the daemon") {
            return (code, stdout, stderr);
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// The launcher answers the **name** layer.
#[test]
fn the_name_layer_answers_with_the_files() {
    let root = fixture_dir("rgxy-names");
    text_tree(&root);
    let fixture = start(&root);

    let (code, stdout, stderr) = ask(&fixture, "hello");
    assert_eq!(code, 0, "a match exits 0: {stderr}");
    assert!(stdout.contains("hello.txt"), "{stdout}");
    assert!(
        !stdout.contains("alpha.md"),
        "the rank is the answer: {stdout}"
    );
}

/// The name layer does not read the content.
///
/// This is the boundary between the two layers, and it is worth a test:
/// a launcher that answered "hello.txt" for the word `needle` would be
/// reading files on every keystroke, and the first layer is a re-rank.
#[test]
fn the_name_layer_does_not_read_the_content() {
    let root = fixture_dir("rgxy-names-only");
    text_tree(&root);
    let fixture = start(&root);

    // `needle` is in the body of hello.txt and in the body of main.rs,
    // and in **no** name.
    let (code, stdout, stderr) = ask(&fixture, "needle");
    assert_eq!(code, 1, "the name layer is the names: {stderr}");
    assert!(stdout.trim().is_empty(), "{stdout}");
}

/// The launcher answers the **content** layer.
#[test]
fn the_content_layer_answers_with_the_files_that_say_it() {
    let root = fixture_dir("rgxy-content");
    text_tree(&root);
    let fixture = start(&root);

    // `zebra` is in two files, and in **neither** of their names.
    let (code, stdout, stderr) = ask(&fixture, "grep:zebra");
    assert_eq!(code, 0, "a match exits 0: {stderr}");
    assert!(stdout.contains("notes.md"), "{stdout}");
    assert!(stdout.contains("readme.txt"), "{stdout}");
    assert!(
        !stdout.contains("hello.txt"),
        "the content layer answers the content: {stdout}"
    );
}

/// The content layer's scope is the name layer's answer.
///
/// The names before the prefix choose the files; the pattern after it is
/// what is read inside them. This is the whole performance story of the
/// second layer: a content search over the tree reads the tree, and a
/// content search over the files that were chosen reads those files.
#[test]
fn the_content_layer_reads_the_files_the_names_chose() {
    let root = fixture_dir("rgxy-scope");
    text_tree(&root);
    let fixture = start(&root);

    // `alpha` names two files; only one of them says `zebra`… neither,
    // in fact, and that is the point: the scope empties the answer.
    let (code, stdout, stderr) = ask(&fixture, "alpha grep:zebra");
    assert_eq!(code, 1, "nothing in the scope says it: {stderr}");
    assert!(stdout.trim().is_empty(), "{stdout}");

    // `docs` names one file, and it says `zebra`.
    let (code, stdout, stderr) = ask(&fixture, "readme grep:zebra");
    assert_eq!(code, 0, "{stderr}");
    assert!(stdout.contains("readme.txt"), "{stdout}");
    assert!(
        !stdout.contains("notes.md"),
        "the scope is the scope: {stdout}"
    );
}

/// The launcher's own filter narrows the content layer's scope.
///
/// The scope is a name query, and a name query reaches the engine in the
/// engine's words: `doc:` is the launcher's word and `type:doc` is the
/// engine's. Without the rewrite the engine is asked for a filter it has
/// never heard of — and it fails closed on it, which is the right answer
/// for a word it does not know — so the scope comes back empty and a
/// content search that had an answer answers nothing.
#[test]
fn the_short_filter_narrows_the_scope_of_the_content_layer() {
    let root = fixture_dir("rgxy-scope-filter");
    text_tree(&root);
    // A file that says the word and is **not** a document: the filter is
    // the filter, and the code file is out of the scope.
    std::fs::write(root.join("src/notes.rs"), "let zebra = 1;\n").unwrap();
    let fixture = start(&root);

    // The documents that say `zebra`: notes.md and docs/readme.txt.
    let (code, stdout, stderr) = ask(&fixture, "doc: grep:zebra");
    assert_eq!(code, 0, "the filter narrows the scope: {stderr}");
    assert!(stdout.contains("notes.md"), "{stdout}");
    assert!(stdout.contains("readme.txt"), "{stdout}");
    assert!(
        !stdout.contains("notes.rs"),
        "the scope is the scope: {stdout}"
    );
}

/// The launcher's own filters reach the engine in the engine's words.
///
/// `doc:` is the launcher's word; `type:doc` is the engine's. The user
/// types the first and the engine reads the second, and nothing in
/// between has to know about it.
#[test]
fn the_short_filters_reach_the_engine() {
    let root = fixture_dir("rgxy-filters");
    text_tree(&root);
    let fixture = start(&root);

    // `notes.md` is a document; `src/main.rs` is not.
    let (code, stdout, stderr) = ask(&fixture, "doc:notes");
    assert_eq!(code, 0, "{stderr}");
    assert!(stdout.contains("notes.md"), "{stdout}");
    assert!(
        !stdout.contains("main.rs"),
        "the filter is the filter: {stdout}"
    );
}

/// A question with no answer is not a failure of the launcher.
///
/// The exit status is the search-shaped one: 0 for a match, 1 for none.
/// A launcher that answered 1 for "no results" and 2 for "no daemon"
/// would be a launcher whose exit code says two different things.
#[test]
fn no_results_is_a_non_zero_exit_and_not_a_crash() {
    let root = fixture_dir("rgxy-empty");
    text_tree(&root);
    let fixture = start(&root);

    let (code, stdout, stderr) = ask(&fixture, "there-is-no-such-name");
    assert_eq!(code, 1, "{stderr}");
    assert!(stdout.trim().is_empty(), "{stdout}");
    assert!(
        !stderr.contains("panicked"),
        "a miss is not a crash: {stderr}"
    );
}

/// A launcher without a daemon says so.
///
/// The exit status is 1 for "no results" and 1 for "the daemon is not
/// there", so the reason has to be on stderr. A launcher that answers
/// "nothing here" when the truth is "the daemon is not running" is a
/// launcher that lies — and the lie is invisible to a script.
#[test]
fn without_a_daemon_the_launcher_says_so() {
    // An endpoint nobody is listening on. The unix socket is used on every
    // platform: a path that cannot exist is a path that cannot be served,
    // and the launcher has to say so rather than answer "no results".
    let (code, stdout, stderr) = run_launcher(&[
        "--endpoint",
        "uds:/rgy-e2e-no-daemon-here/x.sock",
        "--query",
        "hello",
    ]);
    assert_eq!(code, 1, "no daemon is not a match: {stderr}");
    assert!(stdout.trim().is_empty(), "{stdout}");
    assert!(
        stderr.contains("no connection to the daemon"),
        "the reason belongs on stderr: {stderr}"
    );
}

/// The pack store answers the content layer.
///
/// The daemon started with `--tv-dir` answers `grep:` from the packed
/// corpus instead of reading the files. This is the third backend of the
/// second layer, and the one that turns a search over a big corpus into
/// a mmap instead of a walk.
///
/// The scope is the part worth proving: the store names its files
/// **relative to the corpus root**, and the scope the name layer chose is
/// **absolute**. The two are only comparable because the root goes back
/// on the record's path — and a launcher that answered "no results" for a
/// corpus that holds the word would be a launcher that lies.
#[test]
fn the_pack_store_answers_the_content_layer() {
    let root = fixture_dir("rgxy-tv-root");
    text_tree(&root);
    let store = fixture_dir("rgxy-tv-store");
    let config = rgx_core::tv_runtime::builder::BuildConfig::default();
    rgx_core::tv_runtime::builder::build_store(&root, &store, &config)
        .expect("the pack store is built");

    let fixture = start_with(&root, &["--tv-dir", store.to_str().unwrap()]);

    // The whole corpus: the store is the answer.
    let (code, stdout, stderr) = ask(&fixture, "grep:zebra");
    assert_eq!(code, 0, "the store answers the content layer: {stderr}");
    assert!(stdout.contains("notes.md"), "{stdout}");
    assert!(stdout.contains("readme.txt"), "{stdout}");

    // The scope: the name layer chose one file, and the store answers
    // about that file alone.
    let (code, stdout, stderr) = ask(&fixture, "readme grep:zebra");
    assert_eq!(code, 0, "{stderr}");
    assert!(stdout.contains("readme.txt"), "{stdout}");
    assert!(
        !stdout.contains("notes.md"),
        "the scope is the scope, over the store too: {stdout}"
    );
}

/// The launcher says what it can do.
///
/// The filters and the commands are the launcher's own vocabulary, and a
/// user who does not know them is a user who types `folder:` and gets
/// nothing.
#[test]
fn the_help_names_the_filters_and_the_commands() {
    let output = run_launcher(&["--help"]);
    let (code, stdout, _) = output;
    assert_eq!(code, 0);
    for needle in ["file:", "folder:", "doc:", "pic:", "video:", "audio:"] {
        assert!(
            stdout.contains(needle),
            "the help needs `{needle}`: {stdout}"
        );
    }
    for needle in [">open", ">folder", ">copy", ">preview", ">menu"] {
        assert!(
            stdout.contains(needle),
            "the help needs `{needle}`: {stdout}"
        );
    }
    assert!(
        stdout.contains("grep:"),
        "the content layer has to be in the help: {stdout}"
    );
}

/// The launcher refuses an option it does not know.
#[test]
fn an_unknown_option_is_refused() {
    let (code, _, stderr) = run_launcher(&["--nope"]);
    assert_eq!(code, 1);
    assert!(stderr.contains("unknown option"), "{stderr}");
}

/// The launcher speaks the language the environment asks for.
///
/// The default is English, and a translation nobody exercises is a
/// translation that rots: the Portuguese is proved here, on the words a
/// user reads — the help and the sentence the launcher says when the
/// daemon is not there.
#[test]
fn the_launcher_speaks_portuguese_when_the_environment_says_so() {
    let (code, stdout, stderr) = run_in("pt-BR", &["--help"]);
    assert_eq!(code, 0, "{stderr}");
    assert!(
        stdout.contains("o lançador residente"),
        "the help is in the language the environment asked for: {stdout}"
    );
    assert!(
        !stdout.contains("the resident launcher"),
        "the help is in one language, and it is the one that was asked for: {stdout}"
    );

    let (code, stdout, stderr) = run_in(
        "pt-BR",
        &[
            "--endpoint",
            "uds:/rgy-e2e-no-daemon-here/x.sock",
            "--query",
            "hello",
        ],
    );
    assert_eq!(code, 1, "no daemon is not a match: {stderr}");
    assert!(stdout.trim().is_empty(), "{stdout}");
    assert!(
        stderr.contains("sem ligação ao daemon"),
        "the reason is said in the language the environment asked for: {stderr}"
    );
}

/// The launcher falls back to English when the environment says nothing.
///
/// The machine's locale is not the launcher's language: the launcher
/// speaks the language it was given, and a machine that gives none gets
/// the language the code is written in.
#[test]
fn a_machine_that_says_nothing_gets_english() {
    let cache = harness::shared_cache_dir("rgxy-lang");
    let out = std::process::Command::new(bin("rgxy"))
        .arg("--help")
        .env_remove("RGY_LANG")
        .env_remove("LC_ALL")
        .env_remove("LANG")
        .env("RGY_INDEX_DIR", &cache)
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap_or_else(|e| panic!("spawn rgxy: {e}"));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("the resident launcher"),
        "a machine that says nothing gets English: {stdout}"
    );
}
