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

/// A path as the platform spells it: the long name, not the alias.
///
/// Some machines spell `%TEMP%` with the 8.3 short name of the
/// profile folder (`C:\Users\RUNNER~1\...`), while the operating
/// system answers with the long one (`C:\Users\runneradmin\...`).
/// The daemon resolves its root before it says anything, and the pack
/// store indexes the corpus under its canonical root, so their answers
/// carry the long name — and a test that compares one with the
/// spelling the environment happened to use is comparing two spellings
/// of one folder.
fn as_the_platform_spells_it(path: &std::path::Path) -> String {
    let canonical = std::fs::canonicalize(path).expect("the path resolves to itself");
    let text = canonical.to_string_lossy().into_owned();
    // The long-path prefix is how the platform spells a path, not what
    // the caller wrote: it comes off, and a UNC path keeps the `\\`
    // that says it is one.
    match text.strip_prefix(r"\\?\UNC\") {
        Some(rest) => format!(r"\\{rest}"),
        None => text.strip_prefix(r"\\?\").unwrap_or(&text).to_string(),
    }
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
/// could have done. A daemon that walked the
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

/// The store route fails closed, like the route that reads the files.
///
/// The daemon with a pack store and the daemon without one answer the
/// same query the same way: a query the engine refuses matches nothing,
/// and the reason is said. The store route used to drop the warning and
/// search the store anyway -- the same daemon, two different answers.
#[test]
fn the_pack_store_fails_closed_on_a_refused_query() {
    let dir = fixture_dir("dtvbadtype");
    text_tree(&dir);
    let root = dir.to_str().unwrap();

    let store = fixture_dir("dtvstore");
    let config = rgx_core::tv_runtime::builder::BuildConfig::default();
    rgx_core::tv_runtime::builder::build_store(&dir, &store, &config)
        .expect("the pack store is built");

    // `grep:needle` alone would match; `type:banana` poisons it.
    let req = r#"{"op":"grep","q":"type:banana grep:needle","limit":10}"#;
    let (code, out, err) = run_stdin(
        &searchd(),
        &["--root", root, "--tv-dir", store.to_str().unwrap()],
        &format!("{req}\n"),
    );
    assert_eq!(code, 0, "stderr={err}");
    assert!(
        out.contains("\"op\":\"warning\""),
        "the fail-closed warning must ride the response stream; got {out}"
    );
    assert!(
        out.contains("\"op\":\"grep-files\"") && out.contains("\"files\":[]"),
        "a refused query matches nothing, store or no store; got {out}"
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

/// The compat front spells a path the way the platform spells it.
///
/// The tree's own form is `/` everywhere, and the compat front is rg's
/// front: rg prints the paths of the platform it runs on. The whole-tree
/// answer came out with the tree's form and the scoped answer with the
/// caller's, so one front said one file two ways.
///
/// The caller asks with the platform's spelling — the long name of every
/// folder, the one the daemon resolved its root to — so the two answers
/// are the same string: the front says one file one way, and the way it
/// says it is the way the platform spells a path.
#[test]
fn the_compat_front_says_a_path_one_way() {
    let root = fixture_dir("serve-spelling");
    text_tree(&root);
    let root_arg = root.to_str().unwrap();
    // The path goes into a JSON string, so its separators are escaped on
    // the way in.
    let file = as_the_platform_spells_it(&root.join("hello.txt")).replace('\\', "\\\\");

    // The whole tree: the path comes from the tree.
    let (code, wide, err) = run_stdin(
        &searchd(),
        &["--root", root_arg, "--serve"],
        "[\"-F\",\"needle\"]\n",
    );
    assert_eq!(code, 0, "stderr={err}");

    // The same file, named: the path comes from the caller.
    let (code, narrow, err) = run_stdin(
        &searchd(),
        &["--root", root_arg, "--serve"],
        &format!("[\"-F\",\"needle\",\"{file}\"]\n"),
    );
    assert_eq!(code, 0, "stderr={err}");

    // The whole tree answers with more than one file -- the fixture holds
    // two that carry the needle -- and the order it answers in is the
    // files' own: the candidates are the newest first, and the two files
    // are written a moment apart, so which one comes first is the clock's
    // business and not the tree's. The test compares ONE file's spelling
    // in the two answers, so it names the file it compares instead of
    // taking whichever came first: comparing the first of one answer with
    // the first of the other compares two files when the order moves, and
    // a test that fails when the order moves is a test about the order.
    //
    // The file is the one the caller named: the scoped answer holds it
    // alone, so its spelling is the caller's, and the whole-tree answer
    // has to say that same file the same way.
    let named = paths_in(&narrow);
    assert_eq!(
        named.len(),
        1,
        "the scoped answer holds the file the caller named; got {narrow:?}"
    );
    assert!(
        paths_in(&wide).contains(&named[0]),
        "one file, one spelling: the caller says {:?} and the tree says {:?}",
        named[0],
        paths_in(&wide)
    );
}

/// The paths in the compat front's answer.
///
/// The wire escapes the separator, so the bytes here are the escaped
/// ones -- and that is enough: a `/` is not escaped and a `\\` is, so
/// two spellings of one file are two different answers here.
fn paths_in(out: &str) -> Vec<String> {
    out.lines()
        .filter(|l| l.contains("\"type\":\"begin\""))
        .map(|line| {
            let start = line.find("\"text\":\"").expect("the line carries a path") + 8;
            let rest = &line[start..];
            let end = rest.find('"').expect("the path ends");
            rest[..end].to_string()
        })
        .collect()
}

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

/// The protocol stream keeps its raw UTF-8 bytes.
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

/// The two layers spell the same file the same way.
///
/// The name layer's answers were spelled with the platform's separator and
/// the content layer's came out with the tree's own `/`, so one daemon
/// said one file two ways. A window that shows both layers then showed
/// the same file twice, and a script that compared a name with a content
/// hit compared two strings for one file.
#[test]
fn the_two_layers_spell_the_same_file_the_same_way() {
    let dir = fixture_dir("dspell");
    // One file, so the first path of each frame is the same file.
    std::fs::write(dir.join("only.txt"), "a needle in the body\n").unwrap();
    let root = dir.to_str().unwrap();

    let req = "{\"op\":\"search\",\"q\":\"only\",\"limit\":10}\n\
               {\"op\":\"grep\",\"q\":\"grep:needle\",\"limit\":10}\n";
    let (code, out, err) = run_stdin(&searchd(), &["--root", root], req);
    assert_eq!(code, 0, "stderr={err}");

    let hits = out
        .lines()
        .find(|l| l.contains("\"op\":\"hits\""))
        .unwrap_or_else(|| panic!("the hits frame is there: {out}"));
    let files = out
        .lines()
        .find(|l| l.contains("\"op\":\"grep-files\""))
        .unwrap_or_else(|| panic!("the grep frame is there: {out}"));

    assert_eq!(
        path_of(hits),
        path_of(files),
        "one file, one spelling: {out}"
    );
}

/// The path in a frame, as the answer carries it.
///
/// The wire escapes the separator, so the bytes on it are not the bytes
/// the answer carries: `\\` on the wire is one backslash. The test
/// compares the answers, not the wire.
fn path_of(frame: &str) -> String {
    let start = frame.find("\"path\":\"").expect("the frame carries a path") + "\"path\":\"".len();
    let mut out = String::new();
    let mut rest = frame[start..].chars();
    while let Some(c) = rest.next() {
        match c {
            '"' => break,
            '\\' => match rest.next() {
                Some('\\') => out.push('\\'),
                Some('n') => out.push('\n'),
                Some(other) => out.push(other),
                None => break,
            },
            other => out.push(other),
        }
    }
    out
}

// --- what the answer says about itself ---------------------------------------

/// The daemon's own session space.
///
/// The ids the daemon names itself live above the ids the clients name and
/// below the wire's exactness limit, so the two spaces cannot meet and the
/// number the daemon sends is the number the daemon reads back.
const DAEMON_SESSION_SPACE: std::ops::Range<u64> = (1 << 52)..(1 << 53);

/// The frame of a kind in an answer.
///
/// The answer is JSON lines and a frame is one line, so the frame is the
/// line that names itself. The fixtures hold no quotes in their paths, so a
/// frame cannot be confused with the text of another one.
fn frame_of(out: &str, op: &str) -> String {
    out.lines()
        .find(|l| l.contains(op))
        .unwrap_or_else(|| panic!("the answer carries a {op} frame; got {out}"))
        .to_string()
}

/// The name answer of an answer.
fn hits_frame(out: &str) -> String {
    frame_of(out, "\"op\":\"hits\"")
}

/// The content answer of an answer.
fn grep_frame(out: &str) -> String {
    frame_of(out, "\"op\":\"grep-files\"")
}

/// The session an answer names.
///
/// The field is the last one in the frame, so the last one in the text is
/// the field: a path in the answer cannot be mistaken for it.
fn session_of(frame: &str) -> Option<u64> {
    let at = frame.rfind("\"session\":")?;
    let rest = &frame[at + "\"session\":".len()..];
    let end = rest
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(rest.len());
    rest[..end].parse().ok()
}

/// The scope of a scoped request: the paths, as the JSON on the wire.
///
/// The paths go into a JSON string, so their separators are escaped on the
/// way in.
///
/// The scope is spelled the way the platform spells it — the long name of
/// every folder, not the 8.3 alias the environment happens to spell
/// `%TEMP%` with. The pack store indexes the corpus under its canonical
/// root and the daemon resolves its own root before it answers, so the
/// scope has to be asked for in the same spelling they hold it in: a scope
/// spelled with an alias is a scope the store does not have.
fn scope_of(dir: &std::path::Path, names: &[&str]) -> String {
    names
        .iter()
        .map(|name| {
            let path = as_the_platform_spells_it(&dir.join(name)).replace('\\', "\\\\");
            format!("\"{path}\"")
        })
        .collect::<Vec<_>>()
        .join(",")
}

/// A daemon that serves over stdio and stays up between questions.
///
/// The session round trip needs two questions, and the second one has to
/// carry the id the first answer named -- and the id is only known once the
/// first answer has come back. A daemon that is fed its whole script at
/// once cannot do that.
struct Stdio {
    child: std::process::Child,
    stdin: std::process::ChildStdin,
    reader: std::io::BufReader<std::process::ChildStdout>,
    /// The cache the daemon writes its index into.
    ///
    /// It lives as long as the daemon does: a daemon whose cache was taken
    /// away underneath it would answer from nothing, and the test would be
    /// measuring the removal instead of the answer.
    _cache: harness::TempDir,
}

impl Stdio {
    /// A daemon over `root`, serving on its standard input.
    fn start(root: &str) -> Self {
        let cache = harness::shared_cache_dir("serve-stdio");
        let mut child = std::process::Command::new(searchd())
            .args(["--root", root])
            .env("RGY_INDEX_DIR", &cache)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("the daemon starts");
        let stdin = child.stdin.take().expect("the daemon's stdin");
        let reader = std::io::BufReader::new(child.stdout.take().expect("the daemon's stdout"));
        Stdio {
            child,
            stdin,
            reader,
            _cache: cache,
        }
    }

    /// Ask one question and read the answer it gets.
    fn ask(&mut self, request: &str) -> String {
        use std::io::{BufRead, Write};
        writeln!(self.stdin, "{request}").expect("the question is written");
        self.stdin.flush().expect("the question is flushed");
        let mut answer = String::new();
        loop {
            let mut line = String::new();
            let read = self
                .reader
                .read_line(&mut line)
                .expect("the answer is read");
            assert!(read > 0, "the daemon answered {request}");
            answer.push_str(&line);
            if line.contains("\"op\":\"done\"") || line.contains("\"op\":\"error\"") {
                break;
            }
        }
        answer
    }
}

impl Drop for Stdio {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// The name answer says whether it is the whole answer.
///
/// A list of hits looks like the whole answer, and a list that reached the
/// limit is the same list as one that did not: the answer has to say which
/// it is, or the reader cannot tell a whole answer from a page of a longer
/// one.
///
/// The routes are the whole tree and the scope, and they are the same
/// question with different starting points -- so they must not disagree
/// about the same answer. The flag was in the protocol and not in the
/// answers: every route dropped it, and the command line never computed it,
/// so a truncated list was printed as if it were the whole list.
///
/// The rule is the protocol's own: fewer hits than the limit is the whole
/// answer, and an answer that reached the limit may have been cut short.
#[test]
fn every_route_says_whether_the_name_answer_is_whole() {
    let dir = fixture_dir("dwhole");
    // The scope is a folder of the tree, and its name is not a name the
    // working directory has: the scope is a path in the index, and a path
    // that resolved against the working directory would be a different
    // question.
    std::fs::create_dir_all(dir.join("rgy-scope")).unwrap();
    for i in 0..3 {
        std::fs::write(dir.join(format!("rgy-scope/hello{i}.txt")), "hello\n").unwrap();
    }
    let root = dir.to_str().unwrap();

    // The routes that answer a name query, and the question each of them
    // answers.
    let routes: [(&str, &str); 2] = [
        ("the whole tree", "hello"),
        ("the scope", "in:rgy-scope hello"),
    ];

    for (what, q) in routes {
        // Below the limit: the whole answer.
        let (code, out, err) = run_stdin(
            &searchd(),
            &["--root", root],
            &format!("{{\"op\":\"search\",\"q\":\"{q}\",\"limit\":10}}\n"),
        );
        assert_eq!(code, 0, "{what}: stderr={err}");
        let frame = hits_frame(&out);
        assert_eq!(
            frame.matches("\"path\":").count(),
            3,
            "{what} found the three files; got {frame}"
        );
        assert!(
            frame.contains("\"complete\":true"),
            "{what} says a whole answer is not whole; got {frame}"
        );

        // At the limit: the answer may have been cut short.
        let (code, out, err) = run_stdin(
            &searchd(),
            &["--root", root],
            &format!("{{\"op\":\"search\",\"q\":\"{q}\",\"limit\":2}}\n"),
        );
        assert_eq!(code, 0, "{what}: stderr={err}");
        let frame = hits_frame(&out);
        assert_eq!(
            frame.matches("\"path\":").count(),
            2,
            "{what} answered with the two hits that were asked for; got {frame}"
        );
        assert!(
            frame.contains("\"complete\":false"),
            "{what} says a cut answer is whole; got {frame}"
        );
    }
}

/// A session the daemon issued is taken back.
///
/// The session is the incremental-search id: the daemon keeps the answer
/// under it, and the next question that carries the same id is answered from
/// that answer instead of from the index. The id is a capability -- the
/// daemon's own ids are unguessable -- and the daemon takes back the ones
/// it issued and only those.
///
/// The daemon used to refuse every id in its own space, including the ones
/// it had just issued: the answer it put on the wire was a name it would not
/// take back, and the round trip the protocol promises never happened.
#[test]
fn a_session_the_daemon_issued_is_taken_back() {
    let dir = fixture_dir("dtakeback");
    for i in 0..3 {
        std::fs::write(dir.join(format!("hello{i}.txt")), "hello\n").unwrap();
    }
    let root = dir.to_str().unwrap();

    let mut daemon = Stdio::start(root);
    // The first question names no session, so the daemon names one and the
    // answer says which.
    let first = daemon.ask(r#"{"op":"search","q":"hello","limit":10}"#);
    let session = session_of(&hits_frame(&first)).expect("the answer names a session");
    assert!(
        DAEMON_SESSION_SPACE.contains(&session),
        "the daemon names a session of its own; got {session} in {first}"
    );

    // The second question carries it, and the answer carries it too: the
    // daemon took the session back and answered from the answer it keeps.
    let second = daemon.ask(&format!(
        r#"{{"op":"search","q":"hello","limit":10,"session":{session}}}"#
    ));
    assert_eq!(
        session_of(&hits_frame(&second)),
        Some(session),
        "the session the daemon issued is taken back; got {second}"
    );

    // And the re-ranked answer says whether it is whole, like every other
    // route: the flag does not depend on where the answer came from.
    let cut = daemon.ask(&format!(
        r#"{{"op":"search","q":"hello","limit":2,"session":{session}}}"#
    ));
    let frame = hits_frame(&cut);
    assert_eq!(
        frame.matches("\"path\":").count(),
        2,
        "the re-ranked answer holds the two hits that were asked for; got {frame}"
    );
    assert!(
        frame.contains("\"complete\":false"),
        "the re-ranked answer says a cut answer is whole; got {frame}"
    );
}

/// A session that is not a session is no session at all.
///
/// The session id is a number the wire carries exactly: not negative, and at
/// or below `2^53`. A negative id and an id above the line name no session,
/// and the daemon must answer them as fresh questions -- it must not take
/// back an id it never issued, and it must not re-rank anything for them.
///
/// The reader used to cast the number to `u64`, and a cast saturates: `-1`
/// arrived as the id `0`, which is in the *client's* space -- the space the
/// daemon trusts -- and the daemon kept the answer under it. The next
/// question with that id was answered from the answer before, and the caller
/// never knew it had not asked again.
///
/// The ids that name no session are three, and they are three different
/// rules: `-1` is not a number the wire carries, `2^54` is a number above
/// the line the wire carries exactly, and `2^52` is a number in the
/// daemon's own space -- an id of the space the daemon names its own, which
/// the daemon did not issue, and which is therefore a claim on somebody
/// else's session and not a session.
///
/// The id above the line is `2^54` and not `2^53 + 1`: the wire carries a
/// number as an `f64`, and `2^53 + 1` is a number the reader reads back as
/// `2^53` -- which is a session, and the last one the wire can carry.
#[test]
fn a_session_that_is_not_one_is_no_session() {
    let dir = fixture_dir("dnosession");
    for i in 0..3 {
        std::fs::write(dir.join(format!("hello{i}.txt")), "hello\n").unwrap();
    }
    let root = dir.to_str().unwrap();

    // The ids that name no session, as the wire carries them.
    for id in ["-1", "18014398547409984", "4503599627370496"] {
        let mut daemon = Stdio::start(root);
        let first = daemon.ask(&format!(
            r#"{{"op":"search","q":"hello","limit":10,"session":{id}}}"#
        ));
        let second = daemon.ask(&format!(
            r#"{{"op":"search","q":"hello","limit":10,"session":{id}}}"#
        ));
        let named = session_of(&hits_frame(&first)).expect("the answer names a session");
        let again = session_of(&hits_frame(&second)).expect("the answer names a session");
        assert_ne!(
            named.to_string(),
            id,
            "the daemon does not answer with the id it was asked about; got {named} in {first}"
        );
        assert!(
            DAEMON_SESSION_SPACE.contains(&named),
            "the daemon answers a session that is not one with a session of its own; \
             got {named} in {first}"
        );
        assert_ne!(
            named, again,
            "each question was a fresh one: nothing was taken back; got {first} and {second}"
        );
    }
}

/// The scoped content route honours the limit, and says whether it cut.
///
/// The scoped request answers with the `grep-files` frame, and the limit is
/// the caller's: the scope says which files the caller asked about, and the
/// limit says how many of them the caller gets. The answer says which it
/// is -- an answer with fewer files than the limit is the whole answer, and
/// an answer that reached the limit is a page.
///
/// The route used to answer `complete: true` whatever the limit was, so a
/// caller that asked for one file was told it had them all, and the list it
/// printed was a page wearing a whole list's clothes.
#[test]
fn the_scoped_content_route_honours_the_limit() {
    let dir = fixture_dir("dscoped");
    for i in 1..=3 {
        std::fs::write(dir.join(format!("scopeprobe{i}.txt")), "needle here\n").unwrap();
    }
    let root = dir.to_str().unwrap();
    let scope = scope_of(
        &dir,
        &["scopeprobe1.txt", "scopeprobe2.txt", "scopeprobe3.txt"],
    );

    // One file asked for out of three: the answer reached the limit, and it
    // says so. The request is spelled both ways the protocol reads it.
    for op in ["content", "grep-scoped"] {
        let (code, out, err) = run_stdin(
            &searchd(),
            &["--root", root],
            &format!("{{\"op\":\"{op}\",\"q\":\"grep:needle\",\"paths\":[{scope}],\"limit\":1}}\n"),
        );
        assert_eq!(code, 0, "{op}: stderr={err}");
        let frame = grep_frame(&out);
        assert_eq!(
            frame.matches("\"path\":").count(),
            1,
            "{op} answered with the one file that was asked for; got {frame}"
        );
        assert!(
            frame.contains("\"complete\":false"),
            "{op} says a cut answer is whole; got {frame}"
        );
    }

    // Three asked for out of three: fewer files than the limit, so the
    // answer is the whole answer.
    let (code, out, err) = run_stdin(
        &searchd(),
        &["--root", root],
        &format!("{{\"op\":\"content\",\"q\":\"grep:needle\",\"paths\":[{scope}],\"limit\":10}}\n"),
    );
    assert_eq!(code, 0, "stderr={err}");
    let frame = grep_frame(&out);
    assert_eq!(
        frame.matches("\"path\":").count(),
        3,
        "the answer holds every file in the scope; got {frame}"
    );
    assert!(
        frame.contains("\"complete\":true"),
        "an answer shorter than the limit is the whole answer; got {frame}"
    );
}

/// The pack store's scoped content route honours the limit too.
///
/// The store searched the corpus whole, the scope says which of those files
/// the caller asked about, and the limit says how many of them the caller
/// gets. The order is the contract: the limit bounds the answer, so it
/// comes after the scope and not before it.
///
/// The branch used to ignore the limit entirely and answer
/// `complete: true`, so a caller that asked for one file of the scope was
/// told it had them all.
///
/// The scope is asked for in the spelling the store holds the files in:
/// the store indexes the corpus under its canonical root, and the scope
/// is compared with the index byte for byte, so a scope spelled with the
/// 8.3 alias of a folder is a scope the store does not have.
#[test]
fn the_pack_store_scoped_content_route_honours_the_limit() {
    let dir = fixture_dir("dtvscope");
    for i in 1..=3 {
        std::fs::write(dir.join(format!("scopeprobe{i}.txt")), "needle here\n").unwrap();
    }
    let root = dir.to_str().unwrap();

    let store = fixture_dir("dtvscopestore");
    let config = rgx_core::tv_runtime::builder::BuildConfig::default();
    rgx_core::tv_runtime::builder::build_store(&dir, &store, &config)
        .expect("the pack store is built");
    let scope = scope_of(
        &dir,
        &["scopeprobe1.txt", "scopeprobe2.txt", "scopeprobe3.txt"],
    );

    let (code, out, err) = run_stdin(
        &searchd(),
        &["--root", root, "--tv-dir", store.to_str().unwrap()],
        &format!("{{\"op\":\"content\",\"q\":\"grep:needle\",\"paths\":[{scope}],\"limit\":1}}\n"),
    );
    assert_eq!(code, 0, "stderr={err}");
    let frame = grep_frame(&out);
    assert_eq!(
        frame.matches("\"path\":").count(),
        1,
        "the store answered with the one file that was asked for; got {frame}"
    );
    assert!(
        frame.contains("\"complete\":false"),
        "the store says a cut answer is whole; got {frame}"
    );
}
