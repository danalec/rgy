//! End-to-end tests for `searchd`: JSON-lines protocol over stdio and the
//! rgx `--serve` argv-array front. Each case feeds stdin and closes it, so
//! a hang would show up as a test timeout rather than a silent pass.

#[path = "harness.rs"]
mod harness;

use harness::{bin, fixture_dir, run, run_stdin, text_tree};

fn searchd() -> std::path::PathBuf {
    bin("searchd")
}

// --- proto framing (JSON lines) -------------------------------------------

#[test]
fn name_search_round_trip_over_stdio() {
    let dir = fixture_dir("dname");
    text_tree(&dir);
    let root = dir.to_str().unwrap();

    let req = r#"{"op":"search","q":"hello","limit":10}"#;
    let (code, out, err) = run_stdin(&searchd(), &["--root", root], &format!("{req}\n"));
    assert_eq!(code, 0, "searchd must exit cleanly on EOF; stderr={err}");

    let lines: Vec<&str> = out.lines().filter(|l| !l.trim().is_empty()).collect();
    assert!(lines.len() >= 2, "hits then done; got {out}");
    assert!(
        lines[0].contains("\"op\":\"hits\"") && lines[0].contains("hello.txt"),
        "first line is the hits frame; got {}",
        lines[0]
    );
    assert!(
        lines.iter().any(|l| l.contains("\"op\":\"done\"")),
        "a done terminator must close the stream; got {out}"
    );
}

#[test]
fn grep_op_finds_content() {
    let dir = fixture_dir("dgrep");
    text_tree(&dir);
    let root = dir.to_str().unwrap();

    let req = r#"{"op":"grep","q":"grep:needle","limit":10}"#;
    let (code, out, err) = run_stdin(&searchd(), &["--root", root], &format!("{req}\n"));
    assert_eq!(code, 0, "stderr={err}");

    assert!(
        out.contains("\"op\":\"grep-files\"") && out.contains("hello.txt"),
        "grep-files frame must name the hit file; got {out}"
    );
    assert!(out.contains("needle"), "line text must carry the match; got {out}");
    assert!(
        out.contains("\"op\":\"done\""),
        "done terminator required; got {out}"
    );
}

#[test]
fn malformed_request_errors_and_does_not_hang() {
    let dir = fixture_dir("dbad");
    text_tree(&dir);
    let root = dir.to_str().unwrap();

    // A non-JSON line must produce an error response and keep serving; EOF
    // then ends the process. A hang here is a test failure via timeout.
    let (code, out, err) = run_stdin(
        &searchd(),
        &["--root", root],
        "this is not json\n{\"op\":\"status\"}\n",
    );
    assert_eq!(code, 0, "stream must survive a bad line; stderr={err}");
    assert!(
        out.contains("\"op\":\"error\""),
        "malformed request must error; got {out}"
    );
    // the following good line still answers — the stream lives
    assert!(
        out.contains("\"op\":\"status\""),
        "the daemon must keep serving after a bad line; got {out}"
    );
}

#[test]
fn unknown_type_search_warns_and_yields_no_hits() {
    let dir = fixture_dir("dbadtype");
    text_tree(&dir);
    let root = dir.to_str().unwrap();

    let req = r#"{"op":"search","q":"hello type:banana","limit":10}"#;
    let (code, out, err) = run_stdin(&searchd(), &["--root", root], &format!("{req}\n"));
    assert_eq!(code, 0, "stderr={err}");
    assert!(
        out.contains("\"op\":\"warning\""),
        "fail-closed warning must ride the response stream; got {out}"
    );
    assert!(
        out.contains("\"op\":\"done\"") && out.contains("\"count\":0"),
        "never-alternative must contribute zero hits; got {out}"
    );
}

#[test]
fn bad_invocation_exits_two() {
    let (code, _, err) = run(&searchd(), &["--listen", "bogus"]);
    assert_eq!(code, 2, "unknown listen transport is usage error; stderr={err}");

    let (code, _, err) = run(&searchd(), &["--nonsense"]);
    assert_eq!(code, 2, "unknown argument must exit 2; stderr={err}");
    assert!(err.contains("searchd"), "got {err}");
}

// --- rgx --serve compatibility front ---------------------------------------

#[test]
fn serve_front_argv_round_trip() {
    let dir = fixture_dir("dserve");
    text_tree(&dir);
    let root = dir.to_str().unwrap();

    // JSON argv array per line; -F makes the pattern literal.
    let (code, out, err) = run_stdin(
        &searchd(),
        &["--root", root, "--serve"],
        "[\"-F\",\"needle\"]\n",
    );
    assert_eq!(code, 0, "stderr={err}");

    assert!(
        out.contains("\"rgx\":\"ready\""),
        "the front announces readiness; got {out}"
    );
    assert!(
        out.contains("\"rgx\":\"begin\""),
        "query framing opens with begin; got {out}"
    );
    assert!(
        out.contains("\"type\":\"match\"") && out.contains("needle"),
        "match lines must carry the hit; got {out}"
    );
    let end = out
        .lines()
        .find(|l| l.contains("\"rgx\":\"end\""))
        .unwrap_or_else(|| panic!("end frame missing; got {out}"));
    assert!(
        end.contains("\"status\":0") && end.contains("\"matched\":true"),
        "a hit must end status 0 matched true; got {end}"
    );
}

#[test]
fn serve_front_malformed_query_errors_not_hangs() {
    let dir = fixture_dir("dservebad");
    text_tree(&dir);
    let root = dir.to_str().unwrap();

    let (code, out, err) = run_stdin(
        &searchd(),
        &["--root", root, "--serve"],
        "not json at all\n[]\n",
    );
    assert_eq!(code, 0, "the front must keep serving; stderr={err}");
    assert!(
        out.contains("\"rgx\":\"end\"") && out.contains("\"status\":2"),
        "invalid JSON query must end status 2; got {out}"
    );
    // the second line (empty argv) is also an error, not a hang
    assert!(
        out.matches("\"rgx\":\"end\"").count() >= 2,
        "both bad lines must be answered; got {out}"
    );
}
