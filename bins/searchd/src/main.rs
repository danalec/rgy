//! searchd — the merged search daemon (`IMPLEMENTATION_PLAN.md` §5).
//!
//! Indexes `--root` once at startup and answers JSON-lines requests. Three
//! fronts, two framings:
//!
//! - transports: **stdio** (default), **`--listen uds:<path>`** (Unix),
//!   **`--listen pipe:<name>`** (Windows named pipe `\\.\pipe\<name>`);
//! - framings: the **proto** lines shared with `searchctl` (default) and
//!   **`--serve`**, the rgx compatibility front (argv-array per line in,
//!   `{"rgx":"begin"/"end"}` framing and rg `--json` match messages out).
//!
//! The index stays warm in memory: every query opens a 250 ms keep-warm
//! window (the fsearch spinner) so typing stays at latency. Content
//! `grep:` runs the trigram candidate scan + rgx-engine confirm; with
//! `--tv-dir` it is answered by the TV pack store instead.
//!
//! `--index` reuses an index built by an earlier run (or `searchctl`) instead
//! of walking the tree at every daemon start. As there, freshness is explicit:
//! `--reindex` forces a rebuild, `--max-age` expires one, and a sidecar pins
//! the root so another tree's index is refused rather than loaded.

use std::io::{BufRead, Write};
use std::time::{Duration, Instant};

use discovery::{PortableSource, interrupt, walk::Cancelled};
use engine::{keep_warm, warm_stop};
use name_core::content::{self, Grep};
use name_core::live::Live;
use name_core::name_index::Index;
use name_core::name_query::{Alternatives, Hit, Query, Searcher};
use name_core::persist;
use proto::{GrepFile, Hit as WireHit, Request, Response};

#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

/// How often the startup walk reports what it has covered.
const REPORT_EVERY: Duration = Duration::from_millis(500);

struct Ctx {
    root: Vec<u8>,
    home: String,
    tv_dir: Option<std::path::PathBuf>,
    live: Live,
}

fn usage() -> ! {
    eprintln!(
        "searchd [--root <dir>] [--index <file>] [--reindex] [--max-age <dur>]\n         [--tv-dir <store>] [--listen stdio|uds:<path>|pipe:<name>] [--serve]\n\n\
         proto framing (default): one JSON request per line\n  \
           {{\"op\":\"search\",\"q\":\"...\",\"limit\":50}}\n  \
           {{\"op\":\"grep\",\"q\":\"...\"}}\n  \
           {{\"op\":\"status\"}}\n  \
         --serve: rgx compat — a JSON argv-array per line, answers framed\n  \
           by {{\"rgx\":\"begin\"}}/{{\"rgx\":\"end\"}} with rg --json match lines."
    );
    std::process::exit(2);
}

/// Parse `30m` / `2h` / `7d` / `90s` / `1w` into a duration. Bare numbers are
/// seconds, so `--max-age 0` means "expire anything a second old".
fn parse_age(s: &str) -> Option<Duration> {
    let s = s.trim();
    let (digits, mult) = match s.as_bytes().last()? {
        b's' | b'S' => (&s[..s.len() - 1], 1u64),
        b'm' | b'M' => (&s[..s.len() - 1], 60),
        b'h' | b'H' => (&s[..s.len() - 1], 3600),
        b'd' | b'D' => (&s[..s.len() - 1], 86_400),
        b'w' | b'W' => (&s[..s.len() - 1], 604_800),
        _ => (s, 1),
    };
    digits
        .trim()
        .parse::<u64>()
        .ok()
        .map(|n| Duration::from_secs(n.saturating_mul(mult)))
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut root = String::from(".");
    let mut tv_dir = None;
    let mut listen = String::from("stdio");
    let mut serve = false;
    let mut index: Option<String> = None;
    let mut reindex = false;
    let mut max_age: Option<Duration> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--root" => {
                i += 1;
                root = args.get(i).cloned().unwrap_or_else(|| usage());
            }
            "--index" => {
                i += 1;
                index = Some(args.get(i).cloned().unwrap_or_else(|| usage()));
            }
            "--reindex" => reindex = true,
            "--max-age" => {
                i += 1;
                max_age = Some(
                    args.get(i)
                        .and_then(|s| parse_age(s))
                        .unwrap_or_else(|| usage()),
                );
            }
            "--tv-dir" => {
                i += 1;
                tv_dir = Some(std::path::PathBuf::from(
                    args.get(i).cloned().unwrap_or_else(|| usage()),
                ));
            }
            "--listen" => {
                i += 1;
                listen = args.get(i).cloned().unwrap_or_else(|| usage());
            }
            "--serve" => serve = true,
            "-h" | "--help" => usage(),
            other => {
                eprintln!("searchd: unknown argument {other}");
                std::process::exit(2);
            }
        }
        i += 1;
    }

    let idx = open_index(&root, index.as_deref(), reindex, max_age);
    let ctx = std::sync::Arc::new(Ctx {
        root: root.as_bytes().to_vec(),
        home: std::env::var("USERPROFILE")
            .or_else(|_| std::env::var("HOME"))
            .unwrap_or_default(),
        tv_dir,
        live: Live::new(idx),
    });

    match listen.split_once(':') {
        None if listen == "stdio" => {
            let stdin = std::io::stdin();
            let stdout = std::io::stdout();
            run_stream(stdin.lock(), stdout.lock(), &ctx, serve);
        }
        Some(("uds", path)) => serve_uds(path, &ctx, serve),
        Some(("pipe", name)) => serve_pipe(name, &ctx, serve),
        _ => usage(),
    }
}

/// Load a usable index for this root, or walk and build one.
///
/// The startup path is where skipping the walk matters most: the daemon runs
/// exactly once per boot, so a saved index turns a full tree walk into an
/// mmap. A refusal (wrong root, expired, corrupt) always rebuilds and says so
/// rather than starting on a suspect index.
fn open_index(root: &str, index: Option<&str>, reindex: bool, max_age: Option<Duration>) -> Index {
    let root_path = std::path::PathBuf::from(root);
    let index_path = match index {
        Some(p) => std::path::PathBuf::from(p),
        None => persist::default_index_path(&root_path),
    };

    if !reindex {
        match persist::load_for(&root_path, &index_path, max_age) {
            Ok(idx) => {
                eprintln!(
                    "searchd: reusing index {} ({} entries)",
                    index_path.display(),
                    idx.n
                );
                idx.prefault();
                return idx;
            }
            Err(why) => eprintln!("searchd: rebuilding — {why}"),
        }
    }

    let source = PortableSource::new(root.as_bytes().to_vec(), PortableSource::DEFAULT_THREADS);
    // The first thing a daemon does is walk a tree it was pointed at, and
    // that is the longest silent stretch in the program. Report it, and let
    // Ctrl-C stop it cleanly instead of killing the daemon mid-build.
    let cancel = interrupt::cancel_on_interrupt();
    let started = Instant::now();
    let last_report = std::sync::atomic::AtomicU64::new(0);
    let listings = match source.enumerate_cancellable(&cancel, &|p| {
        let now = started.elapsed().as_millis() as u64;
        if now
            > last_report.load(std::sync::atomic::Ordering::Relaxed)
                + REPORT_EVERY.as_millis() as u64
        {
            last_report.store(now, std::sync::atomic::Ordering::Relaxed);
            eprintln!("searchd: scanning… {}", interrupt::line(p));
        }
    }) {
        Ok(l) => l,
        // A daemon with no index has nothing to answer with, and a partial
        // one would answer wrongly. Exit rather than start.
        Err(Cancelled) => {
            eprintln!("searchd: scan cancelled — no index written, exiting");
            std::process::exit(2);
        }
    };
    let idx = Index::build(listings, 0, now_secs(), root.as_bytes());
    match persist::save(&idx, &root_path, &index_path) {
        Ok(saved) => eprintln!(
            "searchd: indexed {} entries → {}",
            idx.n,
            saved.index.display()
        ),
        Err(e) => eprintln!(
            "searchd: could not save index to {}: {e}",
            index_path.display()
        ),
    }
    idx
}

fn run_stream(input: impl BufRead, mut out: impl Write, ctx: &std::sync::Arc<Ctx>, serve: bool) {
    if serve {
        serve_stream(input, out, ctx);
        return;
    }
    for line in input.lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        let responses = match Request::from_json(&line) {
            None => vec![Response::Error {
                message: "malformed request".into(),
            }],
            Some(req) => handle(req, ctx),
        };
        for r in responses {
            let _ = writeln!(out, "{}", r.to_json());
        }
        let _ = out.flush();
    }
}

fn handle(req: Request, ctx: &Ctx) -> Vec<Response> {
    keep_warm(Duration::from_millis(250));
    // `keep_warm` only sets two atomics and unparks the spinner, so starting
    // the clock here measures the request and not the warm window. This used
    // to be hardcoded 0, which made the wire latency field a lie.
    let t0 = Instant::now();
    let mut responses = Vec::new();
    match req {
        Request::Status => responses.push(Response::Status {
            docs: 0,
            event_id: 0,
        }),
        Request::Search { q, limit } => match Alternatives::parse(&q, &ctx.home) {
            Err(e) => responses.push(Response::Error { message: e }),
            Ok(alts) => {
                for w in &alts.warnings {
                    responses.push(Response::Warning { message: w.clone() });
                }
                let searcher = Searcher { live: &ctx.live };
                let mut hits: Vec<Hit> = Vec::new();
                for alt in &alts.alts {
                    let mut a: Query = alt.clone();
                    a.limit = limit;
                    for h in searcher.search(&a) {
                        match hits.iter_mut().find(|e| e.idx == h.idx) {
                            Some(e) if e.score < h.score => *e = h,
                            Some(_) => {}
                            None => hits.push(h),
                        }
                    }
                }
                hits.sort_by(|a, b| b.score.cmp(&a.score).then(a.idx.cmp(&b.idx)));
                hits.truncate(limit);
                let mut buf = Vec::new();
                let wire: Vec<WireHit> = hits
                    .iter()
                    .map(|h| {
                        ctx.live.base.path(h.idx as usize, &mut buf);
                        let rel = String::from_utf8_lossy(&buf).into_owned();
                        buf.clear();
                        let mut full = String::from_utf8_lossy(&ctx.root).into_owned();
                        let rel = rel.trim_start_matches(['/', '\\']);
                        if !rel.is_empty() {
                            full.push(std::path::MAIN_SEPARATOR);
                            full.push_str(rel);
                        }
                        WireHit {
                            path: full,
                            score: h.score,
                        }
                    })
                    .collect();
                let count = wire.len();
                let took = t0.elapsed().as_micros() as u64;
                responses.push(Response::Hits {
                    hits: wire,
                    took_us: took,
                });
                responses.push(Response::Done { count });
            }
        },
        Request::Grep { q, limit } => responses.extend(grep(&q, limit, ctx)),
    }
    warm_stop();
    responses
}

fn grep(q: &str, limit: usize, ctx: &Ctx) -> Vec<Response> {
    let t0 = Instant::now();
    let mut files: Vec<GrepFile> = Vec::new();
    // TV pack store answers when the daemon was given one: the corpus
    // bytes are packed once and mmap'd (plan 4.3); the direct engine
    // below stays for trees without a store.
    if let Some(dir) = &ctx.tv_dir {
        match Alternatives::parse(q, &ctx.home) {
            Ok(alts) => {
                for alt in &alts.alts {
                    let pat = alt.grep.clone().unwrap_or_else(|| q.to_string());
                    match engine::tv::grep_tv(dir, &pat) {
                        Ok(fs) => files.extend(fs.into_iter().map(|f| GrepFile {
                            path: f.path,
                            lines: f.lines.into_iter().map(|(n, t)| (n as usize, t)).collect(),
                        })),
                        Err(e) => {
                            return vec![Response::Error {
                                message: format!("tv store: {e}"),
                            }];
                        }
                    }
                }
            }
            Err(e) => return vec![Response::Error { message: e }],
        }
        let count = files.len();
        let took = t0.elapsed().as_micros() as u64;
        return vec![
            Response::GrepFiles {
                files,
                took_us: took,
            },
            Response::Done { count },
        ];
    }
    match Alternatives::parse(q, &ctx.home) {
        Err(e) => vec![Response::Error { message: e }],
        Ok(alts) => {
            let mut out = Vec::new();
            for w in &alts.warnings {
                out.push(Response::Warning { message: w.clone() });
            }
            let searcher = Searcher { live: &ctx.live };
            let mut jobs: Vec<(String, name_core::name_query::GrepMode, Query)> = Vec::new();
            for alt in &alts.alts {
                match &alt.grep {
                    Some(pat) => jobs.push((pat.clone(), alt.grep_mode, alt.clone_for_scan())),
                    None => jobs.push((
                        q.to_string(),
                        name_core::name_query::GrepMode::Literal,
                        alt.clone_for_scan(),
                    )),
                }
            }
            for (pat, mode, filt) in jobs {
                let Ok(g) = Grep::new(&pat, mode) else {
                    continue;
                };
                let paths = content::scan_paths(&ctx.root, searcher.live, filt);
                let res = content::verify(&g, &paths, limit);
                for f in res.files {
                    files.push(GrepFile {
                        path: String::from_utf8_lossy(&f.path).into_owned(),
                        lines: f.lines,
                    });
                }
            }
            let count = files.len();
            let took = t0.elapsed().as_micros() as u64;
            out.push(Response::GrepFiles {
                files,
                took_us: took,
            });
            out.push(Response::Done { count });
            out
        }
    }
}

// --- the rgx --serve compatibility front -----------------------------------

fn serve_stream(mut input: impl BufRead, mut out: impl Write, ctx: &Ctx) {
    use rgx_core::djson;
    let tag = format!("{:016x}", std::process::id());
    let _ = writeln!(
        out,
        "{{\"rgx\":\"ready\",\"tag\":\"{tag}\",\"pid\":{}}}",
        std::process::id()
    );
    let _ = out.flush();
    let mut seq = 0u64;
    let mut line = String::new();
    loop {
        line.clear();
        match input.read_line(&mut line) {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
        if line.trim().is_empty() {
            continue;
        }
        seq += 1;
        let argv: Vec<String> = match djson::parse(&line) {
            Ok(arena) => match arena.root() {
                djson::Value::Array(idx) => arena
                    .get_array(idx)
                    .iter()
                    .filter_map(|v| v.as_str(&arena).map(str::to_string))
                    .collect(),
                _ => {
                    serve_end(
                        &mut out,
                        seq,
                        &tag,
                        2,
                        false,
                        Some("query must be a JSON array"),
                    );
                    continue;
                }
            },
            Err(e) => {
                serve_end(
                    &mut out,
                    seq,
                    &tag,
                    2,
                    false,
                    Some(&format!("invalid JSON query: {e}")),
                );
                continue;
            }
        };
        let (pattern, fixed) = match serve_pattern(&argv) {
            Some(p) => p,
            None => {
                serve_end(&mut out, seq, &tag, 2, false, Some("no pattern given"));
                continue;
            }
        };
        let _ = writeln!(out, "{{\"rgx\":\"begin\",\"seq\":{seq},\"tag\":\"{tag}\"}}");
        let _ = out.flush();
        // Compat mode always answers content search (rgx is a content
        // grep); the pattern is literal under -F, regex otherwise.
        let mode = if fixed {
            name_core::name_query::GrepMode::Literal
        } else {
            name_core::name_query::GrepMode::Regex
        };
        let mut matched = false;
        if let Some(dir) = &ctx.tv_dir {
            match engine::tv::grep_tv(dir, &pattern) {
                Ok(fs) => {
                    for f in fs {
                        matched = true;
                        serve_file(
                            &mut out,
                            &f.path,
                            &f.lines
                                .iter()
                                .map(|(n, t)| (*n as usize, t.clone()))
                                .collect::<Vec<_>>(),
                        );
                    }
                }
                Err(e) => {
                    serve_end(
                        &mut out,
                        seq,
                        &tag,
                        2,
                        false,
                        Some(&format!("tv store: {e}")),
                    );
                    continue;
                }
            }
        } else {
            let g = match Grep::new(&pattern, mode) {
                Ok(g) => g,
                Err(e) => {
                    serve_end(&mut out, seq, &tag, 2, false, Some(&e));
                    continue;
                }
            };
            let searcher = Searcher { live: &ctx.live };
            let paths = content::scan_paths(&ctx.root, searcher.live, Query::default());
            let res = content::verify(&g, &paths, 100);
            for f in &res.files {
                matched = true;
                serve_file(&mut out, &String::from_utf8_lossy(&f.path), &f.lines);
            }
        }
        serve_end(
            &mut out,
            seq,
            &tag,
            if matched { 0 } else { 1 },
            matched,
            None,
        );
    }
}

/// rgx argv: pattern is `-e`/`--regexp`'s value or the first positional;
/// `-F`/`--fixed-strings` makes it literal.
fn serve_pattern(argv: &[String]) -> Option<(String, bool)> {
    let mut fixed = false;
    let mut pattern = None;
    let mut i = 0;
    while i < argv.len() {
        match argv[i].as_str() {
            "-F" | "--fixed-strings" => fixed = true,
            "-e" | "--regexp" => {
                i += 1;
                pattern = argv.get(i).cloned();
            }
            a if a.starts_with('-') => {}
            a if pattern.is_none() => pattern = Some(a.to_string()),
            _ => {}
        }
        i += 1;
    }
    pattern.map(|p| (p, fixed))
}

fn serve_file(out: &mut impl Write, path: &str, lines: &[(usize, String)]) {
    let esc = |s: &str| s.replace('\\', "\\\\").replace('"', "\\\"");
    let _ = writeln!(
        out,
        "{{\"type\":\"begin\",\"data\":{{\"path\":{{\"text\":\"{}\"}}}}}}",
        esc(path)
    );
    for (n, text) in lines {
        let _ = writeln!(
            out,
            "{{\"type\":\"match\",\"data\":{{\"path\":{{\"text\":\"{}\"}},\"lines\":{{\"text\":\"{}\\n\"}},\"line_number\":{n}}}}}",
            esc(path),
            esc(text)
        );
    }
    let _ = writeln!(
        out,
        "{{\"type\":\"end\",\"data\":{{\"path\":{{\"text\":\"{}\"}}}}}}",
        esc(path)
    );
    let _ = out.flush();
}

fn serve_end(
    out: &mut impl Write,
    seq: u64,
    tag: &str,
    status: i32,
    matched: bool,
    error: Option<&str>,
) {
    let err = error
        .map(|e| {
            format!(
                ",\"error\":\"{}\"",
                e.replace('\\', "\\\\").replace('"', "\\\"")
            )
        })
        .unwrap_or_default();
    let _ = writeln!(
        out,
        "{{\"rgx\":\"end\",\"seq\":{seq},\"tag\":\"{tag}\",\"status\":{status},\"matched\":{matched}{err}}}"
    );
    let _ = out.flush();
}

// --- transports -----------------------------------------------------------

#[cfg(unix)]
fn serve_uds(path: &str, ctx: &std::sync::Arc<Ctx>, serve: bool) {
    use std::os::unix::net::UnixListener;
    let _ = std::fs::remove_file(path);
    let listener = match UnixListener::bind(path) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("searchd: bind {path}: {e}");
            std::process::exit(2);
        }
    };
    for conn in listener.incoming() {
        let Ok(stream) = conn else { continue };
        let ctx = ctx.clone();
        let _ = std::thread::spawn(move || {
            // std UnixStream has no into_split (that is tokio's); clone the
            // handle the way serve_pipe does and split the directions.
            let (r, w) = (stream.try_clone().unwrap(), stream);
            run_stream(std::io::BufReader::new(r), w, &ctx, serve);
        });
    }
}

#[cfg(not(unix))]
fn serve_uds(_path: &str, _ctx: &std::sync::Arc<Ctx>, _serve: bool) {
    eprintln!("searchd: uds listen is unix-only; use pipe:<name> or stdio");
    std::process::exit(2);
}

#[cfg(windows)]
fn serve_pipe(name: &str, ctx: &std::sync::Arc<Ctx>, serve: bool) {
    // Named pipe server, proto framing.
    //
    // A pool of instances is created up front and each one replenishes itself
    // the moment it takes a client, so there is always a listening slot. The
    // previous shape created the next instance only after `run_stream`
    // returned, which made the daemon answer exactly one client at a time —
    // the bottleneck on Windows, the platform this front exists for.
    //
    // The pool matters for more than throughput: a single acceptor that only
    // creates its replacement after connecting leaves a window where no
    // instance is listening at all, and a client arriving in that window gets
    // ERROR_PIPE_BUSY instead of waiting.
    const ACCEPTORS: usize = 16;
    let path = format!(r"\\.\pipe\{name}");
    for _ in 0..ACCEPTORS {
        spawn_pipe_acceptor(&path, ctx.clone(), serve);
    }
    // Acceptors are detached and keep themselves alive; the main thread just
    // stays up so the process is not reaped.
    loop {
        std::thread::park();
    }
}

/// Create one instance, wait for a client, and hand the connection to a
/// thread — leaving a fresh acceptor listening as soon as the client lands.
#[cfg(windows)]
fn spawn_pipe_acceptor(path: &str, ctx: std::sync::Arc<Ctx>, serve: bool) {
    let path = path.to_string();
    std::thread::spawn(move || match open_pipe_server(&path) {
        Ok(conn) => {
            // Connected: replace this acceptor before serving, so the next
            // client does not queue behind this one.
            spawn_pipe_acceptor(&path, ctx.clone(), serve);
            match conn.try_clone() {
                Ok(r) => run_stream(std::io::BufReader::new(r), conn, &ctx, serve),
                Err(e) => eprintln!("searchd: pipe {path}: {e}"),
            }
        }
        Err(e) => eprintln!("searchd: pipe {path}: {e}"),
    });
}

#[cfg(windows)]
fn open_pipe_server(path: &str) -> std::io::Result<std::fs::File> {
    use std::os::windows::io::FromRawHandle;
    // CreateNamedPipeW + ConnectNamedPipe, the one-connection server.
    unsafe extern "system" {
        fn CreateNamedPipeW(
            name: *const u16,
            open_mode: u32,
            pipe_mode: u32,
            max_instances: u32,
            out_size: u32,
            in_size: u32,
            timeout: u32,
            sec: *mut core::ffi::c_void,
        ) -> *mut core::ffi::c_void;
        fn ConnectNamedPipe(
            pipe: *mut core::ffi::c_void,
            overlapped: *mut core::ffi::c_void,
        ) -> i32;
        fn CloseHandle(h: *mut core::ffi::c_void) -> i32;
    }
    const PIPE_ACCESS_DUPLEX: u32 = 0x3;
    const PIPE_TYPE_BYTE: u32 = 0x0;
    // Acceptors are created ahead of demand, so more than one instance of
    // this pipe can exist at once. `PIPE_UNLIMITED_INSTANCES`: with the old
    // value of 1 a second `CreateNamedPipeW` fails with ERROR_PIPE_BUSY,
    // which is exactly the serialization this front no longer has.
    const PIPE_UNLIMITED_INSTANCES: u32 = 255;
    let wide: Vec<u16> = path.encode_utf16().chain(std::iter::once(0)).collect();
    let handle = unsafe {
        CreateNamedPipeW(
            wide.as_ptr(),
            PIPE_ACCESS_DUPLEX,
            PIPE_TYPE_BYTE,
            PIPE_UNLIMITED_INSTANCES,
            64 * 1024,
            64 * 1024,
            0,
            std::ptr::null_mut(),
        )
    };
    if handle.is_null() {
        return Err(std::io::Error::last_os_error());
    }
    let ok = unsafe { ConnectNamedPipe(handle, std::ptr::null_mut()) };
    if ok == 0 {
        let err = std::io::Error::last_os_error();
        if err.raw_os_error() != Some(535) {
            // ERROR_PIPE_CONNECTED: a client connected during the call.
            unsafe {
                CloseHandle(handle);
            }
            return Err(err);
        }
    }
    Ok(unsafe { std::fs::File::from_raw_handle(handle as *mut _) })
}

#[cfg(not(windows))]
fn serve_pipe(name: &str, _ctx: &std::sync::Arc<Ctx>, _serve: bool) {
    eprintln!("searchd: pipe:{name} is windows-only; use uds:<path> or stdio");
    std::process::exit(2);
}

fn now_secs() -> u32 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as u32)
}
