//! End-to-end tests for `searchd`: JSON-lines protocol over stdio and the
//! rgx `--serve` argv-array front. Each case feeds stdin and closes it, so
//! a hang would show up as a test timeout rather than a silent pass.

#[path = "harness.rs"]
mod harness;

use harness::{bin, fixture_dir, run, run_stdin, shared_cache_dir, text_tree};

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
    assert!(
        out.contains("needle"),
        "line text must carry the match; got {out}"
    );
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
    assert_eq!(
        code, 2,
        "unknown listen transport is usage error; stderr={err}"
    );

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

#[test]
fn took_us_is_measured_not_hardcoded() {
    let dir = fixture_dir("dtook");
    text_tree(&dir);
    // A 9-file tree answers in under a microsecond, and `took_us` is whole
    // microseconds — so 0 would be a *correct* reading there and the test
    // could not tell measurement from a hardcoded 0. Give it enough files
    // that the query genuinely takes milliseconds.
    for i in 0..4000 {
        std::fs::write(dir.join(format!("probe{i:05}.txt")), "x\n").unwrap();
    }
    let root = dir.to_str().unwrap();

    // `took_us` used to be a literal 0, so the wire latency was fiction.
    // Over a workload that cannot finish in under a microsecond, every
    // response must now carry a real measurement.
    for req in [
        r#"{"op":"search","q":"probe","limit":10}"#,
        r#"{"op":"grep","q":"grep:needle","limit":10}"#,
    ] {
        let (_, out, err) = run_stdin(&searchd(), &["--root", root], &format!("{req}\n"));
        let frame = out
            .lines()
            .find(|l| l.contains("\"took_us\""))
            .unwrap_or_else(|| panic!("no took_us frame for {req}; got {out} stderr={err}"));
        let digits: String = frame
            .split("\"took_us\":")
            .nth(1)
            .map(|s| s.chars().take_while(char::is_ascii_digit).collect())
            .unwrap_or_default();
        let took: u64 = digits
            .parse()
            .unwrap_or_else(|_| panic!("took_us is not a number; frame={frame}"));
        assert!(
            took > 0,
            "took_us must carry a real measurement, not 0; frame={frame}"
        );
    }
}

// --- named pipe concurrency ------------------------------------------------

/// The Windows front used to answer one client at a time: it created the next
/// pipe instance only after the previous client disconnected, so a second
/// client blocked until the first hung up.
#[cfg(windows)]
#[test]
fn the_pipe_front_serves_two_clients_at_once() {
    let dir = fixture_dir("dpipe");
    text_tree(&dir);
    let root = dir.to_str().unwrap();
    let pipe = format!("rgy-e2e-pipe-{}", std::process::id());

    let child = harness::spawn_pipe(
        &searchd(),
        &["--root", root, "--listen", &format!("pipe:{pipe}")],
    );
    // The daemon indexes the tree before binding, so give it a moment to
    // create its first instance rather than racing it.
    let ready = std::time::Instant::now() + std::time::Duration::from_secs(20);
    let mut first = None;
    while std::time::Instant::now() < ready {
        match harness::PipeClient::connect(&pipe) {
            Ok(c) => {
                first = Some(c);
                break;
            }
            Err(_) => std::thread::sleep(std::time::Duration::from_millis(50)),
        }
    }
    let mut first = first.expect("daemon never offered a pipe instance");

    // The real assertion: this connect must succeed while the first client is
    // still open. With one instance at a time it blocks until `first` drops.
    let mut second = harness::PipeClient::connect(&pipe).expect(
        "a second client must connect while the first is still open — the pipe front is serial",
    );

    let req = r#"{"op":"search","q":"hello","limit":10}"#;
    for (who, c) in [("first", &mut first), ("second", &mut second)] {
        let lines = c
            .request(req)
            .unwrap_or_else(|e| panic!("{who} client failed: {e}"));
        assert!(
            lines.iter().any(|l| l.contains("hello.txt")),
            "{who} client got no hit; got {lines:?}"
        );
        assert!(
            lines.iter().any(|l| l.contains("\"op\":\"done\"")),
            "{who} client never got a terminator; got {lines:?}"
        );
    }

    let _ = &child;
}

// --- index reuse across daemon starts --------------------------------------

#[test]
fn a_second_daemon_start_reuses_the_saved_index() {
    let dir = fixture_dir("dreuse");
    text_tree(&dir);
    let root = dir.to_str().unwrap();
    let cache = shared_cache_dir("dreuse");
    let req = r#"{"op":"search","q":"hello","limit":10}"#;

    let (code, out, err) = run_stdin(
        &searchd(),
        &[
            "--root",
            root,
            "--index",
            cache.join("d.idx").to_str().unwrap(),
        ],
        &format!("{req}\n"),
    );
    assert_eq!(code, 0, "first start must serve; stderr={err}");
    assert!(
        err.contains("indexed"),
        "first start must report building: {err}"
    );
    assert!(
        out.contains("hello.txt"),
        "first start must answer; got {out}"
    );

    let (code, out, err) = run_stdin(
        &searchd(),
        &[
            "--root",
            root,
            "--index",
            cache.join("d.idx").to_str().unwrap(),
        ],
        &format!("{req}\n"),
    );
    assert_eq!(code, 0, "second start must serve; stderr={err}");
    assert!(
        err.contains("reusing"),
        "second start must reuse the index it saved; stderr={err}"
    );
    assert!(
        out.contains("hello.txt"),
        "a reused index must still answer correctly; got {out}"
    );
}
