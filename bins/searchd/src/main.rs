//! searchd — the merged search daemon (`IMPLEMENTATION_PLAN.md` §5).
//!
//! Indexes `--root` once at startup and answers JSON-lines requests on
//! stdio (the named-pipe / UDS fronts speak the same `proto` lines). The
//! index stays warm in memory — the keep-warm spinner and follower
//! takeover are Phase 6/`engine` work.
//!
//! Content `grep:` answers with an explicit "not yet" error until the
//! content layer's rgx confirm path lands; the daemon never silently
//! pretends a query succeeded.

use std::io::{BufRead, Write};

use discovery::{PortableSource, Source};
use name_core::content::{self, Grep};
use name_core::live::Live;
use name_core::name_index::Index;
use name_core::name_query::{Alternatives, Hit, Query, Searcher};
use proto::{GrepFile, Hit as WireHit, Request, Response};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut root = String::from(".");
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--root" => {
                i += 1;
                root = args.get(i).cloned().unwrap_or_else(|| usage());
            }
            "-h" | "--help" => usage(),
            other => {
                eprintln!("searchd: unknown argument {other}");
                std::process::exit(2);
            }
        }
        i += 1;
    }

    let source = PortableSource {
        root: root.as_bytes().to_vec(),
    };
    let listings = match source.enumerate() {
        Ok(l) => l,
        Err(e) => {
            eprintln!("searchd: {e}");
            std::process::exit(2);
        }
    };
    let idx = Index::build(listings, 0, now_secs(), root.as_bytes());
    let live = Live::new(idx);
    let searcher = Searcher { live: &live };
    let home = std::env::var("USERPROFILE")
        .or_else(|_| std::env::var("HOME"))
        .unwrap_or_default();

    let stdin = std::io::stdin();
    let mut out = std::io::stdout();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        let mut responses: Vec<Response> = Vec::new();
        match Request::from_json(&line) {
            None => responses.push(Response::Error {
                message: "malformed request".into(),
            }),
            Some(Request::Status) => responses.push(Response::Status {
                docs: 0,
                event_id: 0,
            }),
            Some(Request::Grep { q, limit }) => {
                let t0 = std::time::Instant::now();
                match Alternatives::parse(&q, &home) {
                    Err(e) => responses.push(Response::Error { message: e }),
                    Ok(alts) => {
                        for w in &alts.warnings {
                            responses.push(Response::Warning { message: w.clone() });
                        }
                        // `grep:`/`regex:`/`sym:` filters name the pattern;
                        // without one the whole q is a literal pattern.
                        let mut jobs: Vec<(String, name_core::name_query::GrepMode, Query)> =
                            Vec::new();
                        for alt in &alts.alts {
                            match &alt.grep {
                                Some(pat) => {
                                    jobs.push((pat.clone(), alt.grep_mode, alt.clone_for_scan()))
                                }
                                None => jobs.push((
                                    q.clone(),
                                    name_core::name_query::GrepMode::Literal,
                                    alt.clone_for_scan(),
                                )),
                            }
                        }
                        let mut files: Vec<GrepFile> = Vec::new();
                        for (pat, mode, filt) in jobs {
                            let Ok(g) = Grep::new(&pat, mode) else { continue };
                            let paths = content::scan_paths(root.as_bytes(), &searcher.live, filt);
                            let res = content::verify(&g, &paths, limit);
                            for f in res.files {
                                files.push(GrepFile {
                                    path: String::from_utf8_lossy(&f.path).into_owned(),
                                    lines: f.lines,
                                });
                            }
                        }
                        let took_us = t0.elapsed().as_micros() as u64;
                        let count = files.len();
                        responses.push(Response::GrepFiles { files, took_us });
                        responses.push(Response::Done { count });
                    }
                }
            }
            Some(Request::Search { q, limit }) => match Alternatives::parse(&q, &home) {
                Err(e) => responses.push(Response::Error { message: e }),
                Ok(alts) => {
                    let t0 = std::time::Instant::now();
                    for w in &alts.warnings {
                        responses.push(Response::Warning { message: w.clone() });
                    }
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
                            searcher.live.base.path(h.idx as usize, &mut buf);
                            let path = String::from_utf8_lossy(&buf).into_owned();
                            buf.clear();
                            WireHit {
                                path,
                                score: h.score,
                            }
                        })
                        .collect();
                    let took_us = t0.elapsed().as_micros() as u64;
                    let count = wire.len();
                    responses.push(Response::Hits {
                        hits: wire,
                        took_us,
                    });
                    responses.push(Response::Done { count });
                }
            },
        }
        for r in responses {
            let _ = writeln!(out, "{}", r.to_json());
        }
        let _ = out.flush();
    }
}

fn usage() -> ! {
    eprintln!(
        "searchd --root <dir>\n\n\
         Indexes <dir> and answers JSON-lines requests on stdio:\n  \
           {{\"op\":\"search\",\"q\":\"...\",\"limit\":50}}\n  \
           {{\"op\":\"grep\",\"q\":\"...\"}}   (Phase 4)\n  \
           {{\"op\":\"status\"}}"
    );
    std::process::exit(2);
}

fn now_secs() -> u32 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as u32)
}
