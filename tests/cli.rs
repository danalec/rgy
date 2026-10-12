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
    assert!(out.contains("alpha.md") && out.contains("alpha.txt"), "got {out}");

    let (code, out, _) = run(&searchctl(), &["--root", root, "alpha", "ext:md"]);
    assert_eq!(code, 0, "alpha ext:md must match; stdout={out}");
    assert!(out.contains("alpha.md"), "got {out}");
    assert!(!out.contains("alpha.txt"), "ext:md must drop the .txt hit: {out}");
}

#[test]
fn no_match_exits_one() {
    let dir = fixture_dir("nomatch");
    text_tree(&dir);
    let root = dir.to_str().unwrap();

    let (code, out, err) = run(&searchctl(), &["--root", root, "qqqzzznotfoundanywhere"]);
    assert_eq!(code, 1, "no-match must exit 1; stdout={out} stderr={err}");
    assert!(out.is_empty(), "no-match must print nothing on stdout: {out}");
}

#[test]
fn empty_tree_exits_one() {
    let dir = fixture_dir("empty");
    // deliberately no files
    let root = dir.to_str().unwrap();

    let (code, out, _) = run(&searchctl(), &["--root", root, "hello"]);
    assert_eq!(code, 1, "empty tree must exit 1; stdout={out}");
}

// --- fail-closed query parsing --------------------------------------------

#[test]
fn unknown_type_fails_closed_with_warning() {
    let dir = fixture_dir("badtype");
    text_tree(&dir);
    let root = dir.to_str().unwrap();

    // `hello` alone would match; `type:banana` must poison the alternative
    let (code, out, err) = run(&searchctl(), &["--root", root, "hello", "type:banana"]);
    assert_eq!(code, 1, "unknown type must fail closed (exit 1); stdout={out}");
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
    assert!(line.contains("\"score\":"), "documented field score: {line}");
    assert!(line.contains("hello.txt"), "path must name the hit: {line}");
}

#[test]
fn limit_caps_hits() {
    let dir = fixture_dir("limit");
    for i in 1..=5 {
        std::fs::write(dir.join(format!("limitprobe{i}.txt")), "x\n").unwrap();
    }
    let root = dir.to_str().unwrap();

    let (code, out, _) = run(&searchctl(), &["--root", root, "--limit", "2", "^limitprobe"]);
    assert_eq!(code, 0, "must match; stdout={out}");
    let hits = out.lines().filter(|l| !l.trim().is_empty()).count();
    assert_eq!(hits, 2, "--limit 2 must cap the output; got {out}");

    let (code, out, _) = run(&searchctl(), &["--root", root, "--limit", "50", "^limitprobe"]);
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
