// SPDX-License-Identifier: Apache-2.0

//! End-to-end tests for `searchctl`: walk-style name queries, filters,
//! `--json`, `--limit`, `grep:` content search, and rg-compatible exit
//! codes (0 match / 1 no match / 2 error). Fixtures are runtime temp trees.

#[path = "harness.rs"]
mod harness;

use harness::{bin, fixture_dir, run, text_tree};

fn searchctl() -> std::path::PathBuf {
    bin("searchctl")
}

// --- walk-style name queries ----------------------------------------------

#[test]
fn walk_query_matches_expected_paths() {
    let dir = fixture_dir("walk");
    text_tree(&dir);
    let root = dir.to_str().unwrap();

    let (code, out, _) = run(&searchctl(), &["--root", root, "hello"]);
    assert_eq!(code, 0, "hello must match; stdout={out}");
    assert!(out.contains("hello.txt"), "got {out}");

    // bare token also hits the nested path
    let (code, out, _) = run(&searchctl(), &["--root", root, "util"]);
    assert_eq!(code, 0, "util must match; stdout={out}");
    assert!(out.contains("util.rs"), "got {out}");
}

#[test]
fn ext_filter_narrows_hits() {
    let dir = fixture_dir("ext");
    text_tree(&dir);
    let root = dir.to_str().unwrap();

    // `alpha` matches alpha.md and alpha.txt; ext:md keeps only one
    let (code, out, _) = run(&searchctl(), &["--root", root, "alpha"]);
    assert_eq!(code, 0, "alpha must match both stems; stdout={out}");
    assert!(
        out.contains("alpha.md") && out.contains("alpha.txt"),
        "got {out}"
    );

    let (code, out, _) = run(&searchctl(), &["--root", root, "alpha", "ext:md"]);
    assert_eq!(code, 0, "alpha ext:md must match; stdout={out}");
    assert!(out.contains("alpha.md"), "got {out}");
    assert!(
        !out.contains("alpha.txt"),
        "ext:md must drop the .txt hit: {out}"
    );
}

#[test]
fn no_match_exits_one() {
    let dir = fixture_dir("nomatch");
    text_tree(&dir);
    let root = dir.to_str().unwrap();

    let (code, out, err) = run(&searchctl(), &["--root", root, "qqqzzznotfoundanywhere"]);
    assert_eq!(code, 1, "no-match must exit 1; stdout={out} stderr={err}");
    assert!(
        out.is_empty(),
        "no-match must print nothing on stdout: {out}"
    );
}

#[test]
fn empty_tree_exits_one() {
    let dir = fixture_dir("empty");
    // deliberately no files
    let root = dir.to_str().unwrap();

    let (code, out, _) = run(&searchctl(), &["--root", root, "hello"]);
    assert_eq!(code, 1, "empty tree must exit 1; stdout={out}");
}

/// A root that lists nothing is a tree with no entries, not a crash. The
/// walk yields no blocks for a path that does not exist, and `Index::build`
/// used to walk off the end of the empty stream: `searchctl --root
/// <nonexistent>` died with `index out of bounds: the len is 0 but the index
/// is 4294967295` (`NONE` is `u32::MAX`) and exit 101.
#[test]
fn a_root_that_lists_nothing_answers_rather_than_crashing() {
    let missing = fixture_dir("absent-root").join("no-such-dir");
    let root = missing.to_str().unwrap();
    assert!(!missing.exists(), "the fixture root must not exist");

    // "no match" is exit 1, the same as any query against an empty tree —
    // not 101, and not a panic on stderr.
    let (code, _, err) = run(&searchctl(), &["--root", root, "hello"]);
    assert_eq!(
        code, 1,
        "an unlistable root is an empty tree, not a failure; stderr={err}"
    );
    assert!(
        !err.contains("panicked"),
        "the CLI must not panic; stderr={err}"
    );

    // And an explicit rebuild says what it covered. The count is 1, not 0:
    // the index always carries a synthetic root entry, and a report of zero
    // would read as "the scan found nothing", which is a different claim from
    // "the root itself is the only entry".
    let (code, _, err) = run(
        &searchctl(),
        &["--root", root, "--reindex", "--explain", "hello"],
    );
    assert_eq!(code, 1, "stderr={err}");
    assert!(
        err.contains("indexed 1 entries"),
        "expected the root-only index; stderr={err}"
    );
    assert!(
        !err.contains("panicked"),
        "the CLI must not panic; stderr={err}"
    );
}

// --- fail-closed query parsing --------------------------------------------

#[test]
fn unknown_type_fails_closed_with_warning() {
    let dir = fixture_dir("badtype");
    text_tree(&dir);
    let root = dir.to_str().unwrap();

    // `hello` alone would match; `type:banana` must poison the alternative
    let (code, out, err) = run(&searchctl(), &["--root", root, "hello", "type:banana"]);
    assert_eq!(
        code, 1,
        "unknown type must fail closed (exit 1); stdout={out}"
    );
    assert!(out.is_empty(), "fail-closed must yield no hits: {out}");
    assert!(
        err.contains("warning:") && err.to_lowercase().contains("type"),
        "stderr must carry a warning about the unknown type; got {err}"
    );
}

// --- --json and --limit ----------------------------------------------------

#[test]
fn json_emits_one_object_per_hit_with_path_and_score() {
    let dir = fixture_dir("json");
    text_tree(&dir);
    let root = dir.to_str().unwrap();

    let (code, out, _) = run(&searchctl(), &["--root", root, "--json", "hello"]);
    assert_eq!(code, 0, "must match; stdout={out}");

    let lines: Vec<&str> = out.lines().filter(|l| !l.trim().is_empty()).collect();
    assert_eq!(lines.len(), 1, "one object per hit; got {out}");
    let line = lines[0];
    assert!(
        line.starts_with('{') && line.ends_with('}'),
        "each hit is one JSON object: {line}"
    );
    assert!(line.contains("\"path\":"), "documented field path: {line}");
    assert!(
        line.contains("\"score\":"),
        "documented field score: {line}"
    );
    assert!(line.contains("hello.txt"), "path must name the hit: {line}");
}

#[test]
fn limit_caps_hits() {
    let dir = fixture_dir("limit");
    for i in 1..=5 {
        std::fs::write(dir.join(format!("limitprobe{i}.txt")), "x\n").unwrap();
    }
    let root = dir.to_str().unwrap();

    let (code, out, _) = run(
        &searchctl(),
        &["--root", root, "--limit", "2", "^limitprobe"],
    );
    assert_eq!(code, 0, "must match; stdout={out}");
    let hits = out.lines().filter(|l| !l.trim().is_empty()).count();
    assert_eq!(hits, 2, "--limit 2 must cap the output; got {out}");

    let (code, out, _) = run(
        &searchctl(),
        &["--root", root, "--limit", "50", "^limitprobe"],
    );
    assert_eq!(code, 0);
    let hits = out.lines().filter(|l| !l.trim().is_empty()).count();
    assert_eq!(hits, 5, "no cap below the corpus; got {out}");
}

// --- content search --------------------------------------------------------

#[test]
fn grep_finds_known_string_in_fixture() {
    let dir = fixture_dir("grep");
    text_tree(&dir);
    let root = dir.to_str().unwrap();

    let (code, out, _) = run(&searchctl(), &["--root", root, "grep:needle"]);
    assert_eq!(code, 0, "grep:needle must match; stdout={out}");
    assert!(out.contains("hello.txt"), "got {out}");
    assert!(out.contains("needle"), "rg-style path:line:text; got {out}");
    // the nested source file also holds the string
    assert!(out.contains("main.rs"), "got {out}");
}

#[test]
fn grep_no_match_exits_one() {
    let dir = fixture_dir("grepmiss");
    text_tree(&dir);
    let root = dir.to_str().unwrap();

    let (code, out, _) = run(&searchctl(), &["--root", root, "grep:qqqzzznotfound"]);
    assert_eq!(code, 1, "grep miss must exit 1; stdout={out}");
}

// --- genuinely bad invocations → exit 2 ------------------------------------

#[test]
fn missing_query_exits_two() {
    let (code, _, err) = run(&searchctl(), &[]);
    assert_eq!(code, 2, "no query is usage error; stderr={err}");
    assert!(err.contains("searchctl"), "usage goes to stderr; got {err}");
}

#[test]
fn unknown_option_exits_two() {
    let (code, _, err) = run(&searchctl(), &["--bogus", "hello"]);
    assert_eq!(code, 2, "unknown option must exit 2; stderr={err}");
    assert!(err.contains("unknown option"), "got {err}");
}

/// Asking for help is not a failed invocation. It exited 2 alongside a
/// malformed command, which the rg convention this CLI follows reads as
/// "error" and a script acts on; `rg --help` exits 0.
///
/// The usage goes to stdout: the help is the program's answer, and an
/// answer that goes to stderr shows the reader's pipe nothing.
#[test]
fn asking_for_help_is_not_an_error() {
    for flag in ["-h", "--help"] {
        let (code, out, err) = run(&searchctl(), &[flag]);
        assert_eq!(code, 0, "{flag} must exit 0, not 2; stderr={err}");
        assert!(
            out.contains("searchctl"),
            "{flag} printed no usage; stdout={out}"
        );
    }
    // ...while an invocation that is actually missing its query is still a
    // usage error, and must stay 2.
    let (code, _, err) = run(&searchctl(), &[]);
    assert_eq!(code, 2, "a missing query must exit 2; stderr={err}");
}

#[test]
fn bad_limit_value_exits_two() {
    let dir = fixture_dir("badlimit");
    text_tree(&dir);
    let root = dir.to_str().unwrap();

    let (code, _, err) = run(&searchctl(), &["--root", root, "--limit", "abc", "hello"]);
    assert_eq!(code, 2, "non-numeric --limit must exit 2; stderr={err}");
}

#[test]
fn missing_root_value_exits_two() {
    let (code, _, _) = run(&searchctl(), &["--root"]);
    assert_eq!(code, 2, "--root without a value must exit 2");
}

// --- --explain -------------------------------------------------------------

#[test]
fn explain_reports_the_plan_that_ran() {
    let dir = fixture_dir("explain");
    text_tree(&dir);
    let root = dir.to_str().unwrap();

    let (code, out, err) = run(&searchctl(), &["--root", root, "--explain", "hello"]);
    assert_eq!(code, 0, "must match; stdout={out}");
    let plan_line = err
        .lines()
        .find(|l| l.contains("plan="))
        .unwrap_or_else(|| panic!("no plan line in stderr; got {err}"));
    // The plan is one field on a line that also carries the match count and,
    // when a fast path declined the query, what it declined after. Split on
    // the tab boundary rather than reading to end of line, so a richer line
    // does not read as an unknown plan name.
    let plan = plan_line
        .split_once("plan=")
        .expect("plan=")
        .1
        .split('\t')
        .next()
        .expect("plan field")
        .trim();
    assert!(
        !plan.is_empty() && plan != "none",
        "a real query must report a real plan; got {plan_line}"
    );
    // And the plan must name something the engine actually has.
    assert!(
        [
            "ranked",
            "ranked (tentative)",
            "driven",
            "selective",
            "full"
        ]
        .contains(&plan),
        "unknown plan name {plan:?} in {plan_line}"
    );
}

#[test]
fn explain_reports_fail_closed_rather_than_a_scan_plan() {
    let dir = fixture_dir("explained");
    text_tree(&dir);
    let root = dir.to_str().unwrap();

    // `hello` alone would match; the bad type must be reported as what
    // happened, not silently attributed to a scan path.
    let (code, _, err) = run(
        &searchctl(),
        &["--root", root, "--explain", "hello", "type:banana"],
    );
    assert_eq!(code, 1, "fail-closed must exit 1; stderr={err}");
    assert!(
        err.contains("plan=fail-closed"),
        "a poisoned alternative must report fail-closed; got {err}"
    );
}

// --- --count ---------------------------------------------------------------

#[test]
fn count_reports_matches_beyond_the_hit_limit() {
    let dir = fixture_dir("count");
    text_tree(&dir);
    // More files than the interactive hit limit, so a `--count` that
    // inherited the limit would print the limit instead of the truth.
    for i in 0..200 {
        std::fs::write(dir.join(format!("cntprobe{i:04}.txt")), "x\n").unwrap();
    }
    let root = dir.to_str().unwrap();

    let (code, out, err) = run(&searchctl(), &["--root", root, "--count", "cntprobe"]);
    assert_eq!(code, 0, "must find matches; stderr={err}");
    let n: usize = out.trim().parse().expect("count is a number");
    assert_eq!(
        n, 200,
        "200 probes must be counted, not the 50-hit limit; got {out}"
    );

    // And it must agree with what a full search of the same query returns.
    let (_, full, _) = run(
        &searchctl(),
        &["--root", root, "--limit", "5000", "cntprobe"],
    );
    assert_eq!(
        full.lines().filter(|l| !l.trim().is_empty()).count(),
        n,
        "count and search disagree"
    );
}

#[test]
fn count_of_alternates_does_not_double_count() {
    let dir = fixture_dir("countalts");
    text_tree(&dir);
    let root = dir.to_str().unwrap();

    // `alpha` is matched by both alternatives; it must be counted once.
    let (code, out, err) = run(&searchctl(), &["--root", root, "--count", "alpha|alpha"]);
    assert_eq!(code, 0, "must match; stderr={err}");
    let union: usize = out.trim().parse().expect("count is a number");
    let (_, single, _) = run(&searchctl(), &["--root", root, "--count", "alpha"]);
    let one: usize = single.trim().parse().expect("count is a number");
    assert_eq!(
        union, one,
        "identical alternatives must not double count; {out} vs {single}"
    );
}

/// The `--explain` header reports how many entries matched. For a `|` query
/// that has to be the union -- the number of results the command actually
/// prints -- and not the sum of what each alternative matched, which counts
/// an entry twice when two alternatives hit it.
#[test]
fn explain_total_agrees_with_the_count_it_prints() {
    let dir = fixture_dir("explainalts");
    text_tree(&dir);
    let root = dir.to_str().unwrap();

    // `alpha` is matched by both sides, so a summed total overshoots.
    let (code, _, err) = run(&searchctl(), &["--root", root, "--explain", "alpha|alpha"]);
    assert_eq!(code, 0, "must match; stderr={err}");
    let header = err
        .lines()
        .find(|l| l.contains("entries indexed"))
        .unwrap_or_else(|| panic!("no header in stderr; got {err}"));
    // The header is "{n} entries indexed, {total} matches" -- the match count
    // is the number before the word, not the first number in the line.
    let total: usize = header
        .split("matches")
        .next()
        .expect("the header names a match count")
        .split_whitespace()
        .last()
        .and_then(|w| w.trim_end_matches(',').parse().ok())
        .expect("a number of matches in the header");

    // `--count` takes a different path to the same question; if the two
    // disagree the header is reporting something other than the result set.
    let (_, counted, _) = run(&searchctl(), &["--root", root, "--count", "alpha|alpha"]);
    let want: usize = counted.trim().parse().expect("count is a number");
    assert_eq!(
        total, want,
        "--explain said {total} matches but --count says {want}; header={header}"
    );
}

/// `--count` on a content query must be the true count, not the preview.
///
/// Two ways it was not. The content path returns before the `--count` check
/// further down `main`, so the flag was parsed and silently ignored -- it
/// printed the matching lines instead of their number. And once honoured, the
/// scan would still have been `Grep`'s display defaults: 5 lines a file and a
/// 250 ms budget. That is right for showing results and wrong for counting
/// them -- it reports what the preview happened to reach, and sounds sure.
#[test]
fn content_count_is_the_true_count_not_a_preview() {
    let dir = fixture_dir("grepcount");
    text_tree(&dir);
    // One file with far more matching lines than the 5-per-file preview cap.
    std::fs::write(dir.join("many.txt"), "needle\n".repeat(40)).unwrap();

    let (code, out, err) = run(
        &searchctl(),
        &["--root", dir.to_str().unwrap(), "--count", "grep:needle"],
    );
    assert_eq!(code, 0, "must match; stderr={err}");
    let n: usize = out
        .trim()
        .parse()
        .expect("a number, not the matching lines");
    // 40 in many.txt, one in hello.txt, one in text_tree's src/main.rs.
    assert_eq!(n, 42, "--count must count every matching line; got {out}");

    // The non-count path is still the preview it is meant to be: it shows a
    // handful, and that is the point of it.
    let (code, shown, _) = run(
        &searchctl(),
        &["--root", dir.to_str().unwrap(), "grep:needle"],
    );
    assert_eq!(code, 0);
    assert!(
        shown.lines().count() < n,
        "the preview must stay a preview; it printed every match"
    );
}

#[test]
fn count_with_no_match_exits_one() {
    let dir = fixture_dir("countmiss");
    text_tree(&dir);
    let root = dir.to_str().unwrap();

    let (code, out, err) = run(&searchctl(), &["--root", root, "--count", "zzzznomatch"]);
    assert_eq!(code, 1, "no match must exit 1; stdout={out} stderr={err}");
    assert!(out.trim().is_empty(), "no match prints nothing; got {out}");
}

// --- index reuse -----------------------------------------------------------

#[test]
fn a_second_run_reuses_the_saved_index() {
    let dir = fixture_dir("reuse");
    text_tree(&dir);
    let root = dir.to_str().unwrap();
    let idx_path = dir.join("saved.idx");
    let idx = idx_path.to_str().unwrap();

    let (code, out, err) = run(&searchctl(), &["--root", root, "--index", idx, "hello"]);
    assert_eq!(code, 0, "first run must match; stdout={out} stderr={err}");
    assert!(
        err.contains("indexed"),
        "first run must report building: {err}"
    );
    assert!(idx_path.exists(), "the index must be written to disk");

    let (code, out, err) = run(&searchctl(), &["--root", root, "--index", idx, "hello"]);
    assert_eq!(code, 0, "second run must match; stdout={out} stderr={err}");
    assert!(
        err.contains("reusing"),
        "second run must report reuse: {err}"
    );
    assert!(out.contains("hello.txt"), "reuse must still answer: {out}");
}

#[test]
fn reindex_forces_a_rebuild() {
    let dir = fixture_dir("reindex");
    text_tree(&dir);
    let root = dir.to_str().unwrap();
    let idx = dir.join("saved.idx");
    let idx = idx.to_str().unwrap();

    run(&searchctl(), &["--root", root, "--index", idx, "hello"]);
    let (code, _, err) = run(
        &searchctl(),
        &["--root", root, "--index", idx, "--reindex", "hello"],
    );
    assert_eq!(
        code, 0,
        "must still match after a forced rebuild; stderr={err}"
    );
    assert!(err.contains("indexed"), "--reindex must rebuild: {err}");
    assert!(!err.contains("reusing"), "--reindex must not reuse: {err}");
}

#[test]
fn an_index_built_for_another_tree_is_refused() {
    let a = fixture_dir("ownera");
    let b = fixture_dir("ownerb");
    text_tree(&a);
    text_tree(&b);
    let idx = a.join("shared.idx");
    let idx_s = idx.to_str().unwrap();

    let (code, _, err) = run(
        &searchctl(),
        &["--root", a.to_str().unwrap(), "--index", idx_s, "hello"],
    );
    assert_eq!(code, 0, "first tree must match; stderr={err}");

    // Same index file, different tree: must rebuild rather than answer with
    // paths from the other root.
    let (code, out, err) = run(
        &searchctl(),
        &["--root", b.to_str().unwrap(), "--index", idx_s, "hello"],
    );
    assert_eq!(
        code, 0,
        "second tree must still match its own files; stderr={err}"
    );
    assert!(
        err.contains("rebuilding") && err.contains("built for"),
        "must say why it refused the other tree's index; stderr={err}"
    );
    assert!(
        out.contains(b.to_str().unwrap()) || out.contains("hello.txt"),
        "hits must come from the requested tree; got {out}"
    );
    assert!(
        !out.contains(a.to_str().unwrap()),
        "no path from the other tree may leak into results; got {out}"
    );
}

// --- console encoding --------------------------------------------------------

/// A redirected stdout keeps the raw UTF-8 bytes.
///
/// The console's code page decides how the bytes are *rendered* on a
/// terminal, and that is where the fix belongs. A pipe is not a terminal:
/// the machine on the other end must get the UTF-8 the program wrote --
/// not a `?` where the accent was, and not a console's idea of the text.
#[test]
fn a_redirected_stdout_keeps_the_utf8_bytes() {
    let dir = fixture_dir("utf8");
    let name = "ação-nota.txt";
    std::fs::write(dir.join(name), "needle\n").unwrap();
    let root = dir.to_str().unwrap();

    let (code, out, err) = run(&searchctl(), &["--root", root, "nota"]);
    assert_eq!(code, 0, "the name must match; stdout={out} stderr={err}");
    assert!(
        out.contains(name),
        "the name must reach the pipe as UTF-8: {out}"
    );
    assert!(
        !out.contains('?'),
        "the accent must not become a question mark: {out}"
    );
}

/// The same, through `--json`: the machine consumer reads a field, so the
/// bytes inside it must be the UTF-8 the program wrote.
#[test]
fn a_redirected_stdout_keeps_the_utf8_bytes_in_json() {
    let dir = fixture_dir("utf8json");
    let name = "ação-nota.txt";
    std::fs::write(dir.join(name), "needle\n").unwrap();
    let root = dir.to_str().unwrap();

    let (code, out, err) = run(&searchctl(), &["--root", root, "--json", "nota"]);
    assert_eq!(code, 0, "stdout={out} stderr={err}");
    assert!(
        out.contains(name),
        "the JSON must carry the name as UTF-8: {out}"
    );
}

// --- the daemon, instead of the local index --------------------------------

/// A token that is different for every call, so no two tests share a
/// daemon.
fn unique() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    format!(
        "{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    )
}

/// The daemon of a test, and the environment that finds it.
///
/// The registered endpoint is where the clients look for the daemon: the
/// named pipe that carries the user's name on Windows, and the socket in
/// the cache directory everywhere else. The test points the client at it
/// through the environment — `USERNAME` on Windows, the cache directory
/// everywhere else — so a test never touches the daemon the machine
/// really has.
struct Registered {
    /// The daemon, killed when the test ends.
    _daemon: harness::Daemon,
    /// The environment the client has to see.
    env: Vec<(&'static str, String)>,
    /// The cache directory, where the socket lives.
    ///
    /// It lives as long as the daemon does: a socket whose directory is
    /// gone is a socket nobody can reach, and the test would be measuring
    /// the removal instead of the answer.
    _cache: Option<harness::TempDir>,
}

/// A daemon over `root`, on the registered endpoint.
#[cfg(windows)]
fn registered(root: &std::path::Path) -> Registered {
    let user = format!("rgy-e2e-{}", unique());
    let args = [
        "--root".to_string(),
        root.to_str().unwrap().to_string(),
        "--listen".to_string(),
        format!("pipe:rgy-searchd-{user}"),
    ];
    let borrowed: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
    Registered {
        _daemon: harness::spawn_pipe(&bin("searchd"), &borrowed),
        env: vec![("USERNAME", user)],
        _cache: None,
    }
}

/// A daemon over `root`, on the registered endpoint.
#[cfg(unix)]
fn registered(root: &std::path::Path) -> Registered {
    let cache = harness::fixture_dir("searchctl-registered");
    std::fs::create_dir_all(cache.join("rgy")).unwrap();
    let args = [
        "--root".to_string(),
        root.to_str().unwrap().to_string(),
        "--listen".to_string(),
        format!("uds:{}/rgy/searchd.sock", cache.display()),
    ];
    let borrowed: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
    Registered {
        _daemon: harness::spawn_uds(&bin("searchd"), &borrowed),
        env: vec![("XDG_CACHE_HOME", cache.to_str().unwrap().to_string())],
        _cache: Some(cache),
    }
}

/// The environment of an endpoint nobody listens on.
fn absent() -> Vec<(&'static str, String)> {
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

/// Run the CLI with the environment the test says it should see.
///
/// The harness's own runner does not take an environment, and the
/// registered endpoint is exactly what the environment says: a test that
/// cannot set it cannot prove that the CLI looks where it says.
fn run_with(args: &[&str], env: &[(&str, String)]) -> (i32, String, String) {
    let cache = harness::shared_cache_dir("searchctl-daemon");
    let mut command = std::process::Command::new(searchctl());
    command
        .args(args)
        .env("RGY_INDEX_DIR", &cache)
        .stdin(std::process::Stdio::null());
    for (key, value) in env {
        command.env(key, value);
    }
    let out = command
        .output()
        .unwrap_or_else(|e| panic!("spawn searchctl: {e}"));
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// Ask the CLI, and ask again while the daemon is starting.
///
/// The daemon opens its pipe only when the index is ready, so a daemon
/// that is a second into its startup has nothing to answer on yet. The
/// test asks again — the way a person does.
fn ask(args: &[&str], env: &[(&str, String)]) -> (i32, String, String) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    loop {
        let (code, out, err) = run_with(args, env);
        if code != 2 || std::time::Instant::now() >= deadline {
            return (code, out, err);
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}

/// The daemon flag asks the daemon, and the daemon's tree is the one that
/// answers.
///
/// The flag was documented and parsed, and then read in one place only:
/// the picker's path. The plain query path never looked at it, so the
/// CLI walked `--root`, built an index of its own, and answered from
/// that — a different tree from the daemon's, and the user never told.
///
/// The two trees are the point: both hold a file called `hello.txt`, and
/// only the daemon's one may be in the answer.
#[test]
fn the_daemon_flag_answers_from_the_daemons_tree() {
    let served = fixture_dir("searchctl-served");
    text_tree(&served);
    let other = fixture_dir("searchctl-other");
    std::fs::write(other.join("hello.txt"), "a hello of its own\n").unwrap();

    let daemon = registered(&served);
    let root = other.to_str().unwrap();

    let (code, out, err) = ask(&["--daemon", "--root", root, "hello"], &daemon.env);
    assert_eq!(
        code, 0,
        "the daemon's tree answers; stdout={out} stderr={err}"
    );
    assert!(
        out.contains(served.to_str().unwrap()),
        "the answer is the daemon's tree: {out}"
    );
    assert!(
        !out.contains(other.to_str().unwrap()),
        "the answer is not the local tree: {out}"
    );
    assert!(
        err.contains("asking the running daemon"),
        "the answer says where it came from: {err}"
    );
}

/// The daemon flag never falls back to the local index.
///
/// The flag's words are "the running daemon, not the local index": a
/// client that quietly built an index of its own would be answering a
/// question nobody asked — and it would be paying for a walk of the
/// whole tree to do it. A daemon that is not there is an error, and the
/// error says where it looked.
#[test]
fn the_daemon_flag_does_not_fall_back_to_the_local_index() {
    let dir = fixture_dir("searchctl-absent");
    text_tree(&dir);
    let root = dir.to_str().unwrap();

    let (code, out, err) = run_with(&["--daemon", "--root", root, "hello"], &absent());
    assert_eq!(code, 2, "no daemon is an error; stdout={out} stderr={err}");
    assert!(out.is_empty(), "nothing is printed: {out}");
    assert!(err.contains("daemon"), "the error names the daemon: {err}");
    assert!(
        !err.contains("indexed") && !err.contains("rebuilding"),
        "the local index is never built: {err}"
    );
}

/// The daemon answers the content layer too.
///
/// The content layer reads files, and the daemon is the one that reads
/// them. The answer is rg's shape — `path:line:text` — the same shape
/// the local path prints.
#[test]
fn the_daemon_flag_answers_the_content_layer() {
    let served = fixture_dir("searchctl-content");
    text_tree(&served);
    let daemon = registered(&served);

    let (code, out, err) = ask(&["--daemon", "grep:zebra"], &daemon.env);
    assert_eq!(
        code, 0,
        "the daemon reads the files; stdout={out} stderr={err}"
    );
    assert!(
        out.contains("notes.md") && out.contains("zebra"),
        "the answer is the content: {out}"
    );
}
