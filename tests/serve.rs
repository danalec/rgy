// SPDX-License-Identifier: Apache-2.0

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

/// The session answers with the number of hits the request asked for.
///
/// The request carries a limit and the query carries its own; the session
/// path used to ignore the request's and answer with the query's default,
/// so a request for two hits came back with every hit there was.
#[test]
fn the_session_answers_with_the_limit_the_request_asked_for() {
    let dir = fixture_dir("dsess");
    text_tree(&dir);
    let root = dir.to_str().unwrap();

    let reqs = concat!(
        r#"{"op":"search","q":"a","limit":2}"#,
        "\n",
        r#"{"op":"search","q":"a","limit":2,"session":7}"#,
        "\n",
        r#"{"op":"search","q":"a","limit":2,"session":7}"#,
        "\n",
    );
    let (code, out, err) = run_stdin(&searchd(), &["--root", root], reqs);
    assert_eq!(code, 0, "stderr={err}");
    let hits: Vec<&str> = out
        .lines()
        .filter(|l| l.contains("\"op\":\"hits\""))
        .collect();
    assert_eq!(hits.len(), 3, "three searches, three answers; got {out}");
    for h in &hits {
        assert_eq!(
            h.matches("\"path\":").count(),
            2,
            "the answer holds two hits; got {h}"
        );
    }
}

/// A client that writes a UTF-8 BOM before its first request gets it
/// answered.
///
/// The BOM is not JSON, so the first request used to be answered with
/// "malformed request" and the client's first keystroke went unanswered.
#[test]
fn a_client_that_writes_a_bom_still_gets_its_first_answer() {
    let dir = fixture_dir("dbom");
    text_tree(&dir);
    let root = dir.to_str().unwrap();

    let req = format!("\u{feff}{}\n", r#"{"op":"search","q":"hello","limit":10}"#);
    let (code, out, err) = run_stdin(&searchd(), &["--root", root], &req);
    assert_eq!(code, 0, "stderr={err}");
    assert!(
        out.contains("\"op\":\"hits\"") && out.contains("hello.txt"),
        "the first request is answered; got {out}"
    );
    assert!(
        !out.contains("malformed"),
        "the BOM is not a malformed request; got {out}"
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

/// The status answer must say *how* the index caught up after the last
/// restart, and the answer is what this process did — not what the platform
/// could have done (`docs/design.md` §3, rule 5). A daemon that walked the
/// tree says "relisted"; a daemon that resumed the change journal at the
/// saved watermark says "replayed".
///
/// The field is the whole point of the answer — a reader that cannot tell
/// "replayed" from "relisted" cannot tell a caught-up index from a relisted
/// one.
#[test]
fn the_status_answer_says_how_the_index_caught_up() {
    let dir = fixture_dir("dstatus");
    text_tree(&dir);
    let root = dir.to_str().unwrap();
    let cache = shared_cache_dir("dstatus");
    let index = cache.join("d.idx");
    let index_arg = index.to_str().unwrap();

    // First start: there is no index to resume, so the daemon walks the
    // tree. That is a relist, whatever the platform could have replayed.
    let (code, out, err) = run_stdin(
        &searchd(),
        &["--root", root, "--index", index_arg],
        "{\"op\":\"status\"}\n",
    );
    assert_eq!(code, 0, "stderr={err}");
    assert!(
        out.contains("\"catch_up\":\"relisted\""),
        "a daemon that walked the tree must say so; got {out}"
    );
    // And it must still be the status answer the protocol promises.
    assert!(
        out.contains("\"op\":\"status\"") && out.contains("\"docs\":"),
        "the status frame must keep its shape; got {out}"
    );

    // Second start: the saved index carries the change-feed position it is
    // current as of. Where the platform's journal still covers it, the
    // daemon resumes there and replays the backlog; where there is no
    // journal to resume from, the tree is walked again. Either way the
    // answer has to match what the daemon actually did.
    let (code, out, err) = run_stdin(
        &searchd(),
        &["--root", root, "--index", index_arg],
        "{\"op\":\"status\"}\n",
    );
    assert_eq!(code, 0, "stderr={err}");
    let replayed = err.contains("resuming the change journal");
    let want = if replayed { "replayed" } else { "relisted" };
    assert!(
        out.contains(&format!("\"catch_up\":\"{want}\"")),
        "the status answer must match what the daemon did ({want}); got {out}, stderr={err}"
    );
}

/// An index older than the default limit is not served: the daemon rebuilds
/// it, and says how old the index it refused was — a snapshot of unknown age
/// is how a search answers "no such file" for a file that exists.
#[test]
fn a_daemon_refuses_an_index_older_than_the_default_limit() {
    let dir = fixture_dir("dold");
    text_tree(&dir);
    let root = dir.to_str().unwrap();
    let cache = shared_cache_dir("dold");
    let index = cache.join("d.idx");
    let index_arg = index.to_str().unwrap();
    let req = r#"{"op":"search","q":"hello","limit":10}"#;

    // First start: build the index and save it.
    let (code, out, err) = run_stdin(
        &searchd(),
        &["--root", root, "--index", index_arg],
        &format!("{req}\n"),
    );
    assert_eq!(code, 0, "stderr={err}");
    assert!(
        out.contains("hello.txt"),
        "the first start must answer; got {out}"
    );

    // Age the sidecar past the default limit (30 minutes).
    let mut meta = index.as_os_str().to_os_string();
    meta.push(".meta");
    let meta = std::path::PathBuf::from(meta);
    let stamp = std::fs::read_to_string(&meta).expect("sidecar");
    let old = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
        - 31 * 60;
    let aged = stamp
        .lines()
        .map(|l| {
            if l.starts_with("built ") {
                format!("built {old}")
            } else {
                l.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    std::fs::write(&meta, aged).expect("age the stamp");

    // Second start: the index is past the limit, so it is rebuilt and the
    // reason says how old it was.
    let (code, out, err) = run_stdin(
        &searchd(),
        &["--root", root, "--index", index_arg],
        &format!("{req}\n"),
    );
    assert_eq!(code, 0, "stderr={err}");
    assert!(
        err.contains("rebuilding"),
        "an index past the limit must be rebuilt, not served; stderr={err}"
    );
    assert!(
        err.contains("31m old"),
        "the refusal must say how old the index was; stderr={err}"
    );
    assert!(
        out.contains("hello.txt"),
        "the rebuilt index must still answer; got {out}"
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

// --- console encoding --------------------------------------------------------

/// The protocol stream keeps its raw UTF-8 bytes (parity row 4.7).
///
/// The daemon's stdout is a machine channel: the client reads the bytes
/// and decodes them itself. The console's code page is for rendering on a
/// terminal, and it must not touch what a client receives -- a `?` where
/// the accent was would be a wrong answer, not a cosmetic one.
#[test]
fn the_protocol_stream_keeps_the_utf8_bytes() {
    let dir = fixture_dir("dutf8");
    let name = "ação-nota.txt";
    std::fs::write(dir.join(name), "needle\n").unwrap();
    let root = dir.to_str().unwrap();

    let req = r#"{"op":"search","q":"nota","limit":10}"#;
    let (code, out, err) = run_stdin(&searchd(), &["--root", root], &format!("{req}\n"));
    assert_eq!(code, 0, "stderr={err}");
    assert!(
        out.contains(name),
        "the hit path must reach the client as UTF-8: {out}"
    );
    assert!(
        !out.contains('?'),
        "the accent must not become a question mark: {out}"
    );
}
