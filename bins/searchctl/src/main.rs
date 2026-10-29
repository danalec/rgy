// SPDX-License-Identifier: Apache-2.0

//! searchctl — the merged name-search CLI.
//!
//! fsearch one-shot ergonomics + zen-rs's filter vocabulary (see
//! `rgy_name_core::name_query`), over the v2 name index. It enumerates a
//! directory tree (`--root`, portable walk / no privileges); the Windows
//! MFT fast path (`--volume`) and content `grep:` land with the engine and
//! the content layer.
//!
//! With `--index`, a built index is reused across invocations: the walk and
//! the build happen once, and later queries pay an mmap instead. The portable
//! walk has no change feed, so freshness is explicit — `--reindex` forces a
//! rebuild and `--max-age` expires one; without a `--max-age` of its own an
//! index expires after 30 minutes, because a saved index is a snapshot and a
//! snapshot of any age served without saying so is how a query ends up
//! missing a file created yesterday. The root is pinned in a sidecar so an
//! index built for another tree can never be loaded (see
//! `rgy_name_core::persist`).
//!
//! rg-compatible exit codes: 0 = matches, 1 = no matches, 2 = error.
//! `--json` prints one object per hit. Parse warnings (unknown `type:`,
//! bad `re:`) go to stderr as `warning:` lines and the affected alternative
//! matches nothing — fail closed, never silent.

use std::time::{Duration, Instant};

use rgy_discovery::interrupt;
use rgy_discovery_dispatch::source_for;
use rgy_name_core::content::{self, Grep};
use rgy_name_core::live::Live;
use rgy_name_core::name_index::Index;
use rgy_name_core::name_query::{Alternatives, Hit, Searcher, merge_hits};
use rgy_name_core::persist;

#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

/// How often an index build reports what it has covered. Fast enough to look
/// alive, slow enough that the lines stay readable.
const REPORT_EVERY: Duration = Duration::from_millis(500);

/// Print usage and leave. `code` is the exit status: 0 when the user asked
/// for help, 2 for the rg convention's "bad invocation". `--help` used to
/// exit 2 like a malformed command, which a script reads as an error.
fn usage(code: i32) -> ! {
    rgy_engine::errln!("{}", help());
    std::process::exit(code);
}

/// The help text.
///
/// The text is a function and not a literal inside `usage` so that a test
/// can read it: `usage` leaves the process, and a test cannot ask a
/// process that is gone what it said.
fn help() -> String {
    "searchctl [options] <query>\n\n\
         options:\n  \
           --root <dir>       tree to index (default: .)\n  \
           --limit <n>        max hits (default: 50)\n  \
           --threads <n>      workers for the scan and the query\n                \
           (default: 4 for the scan, the machine's cores for the query;\n    \
           RGY_NUM_THREADS sets both)\n  \
           --json             one JSON object per hit\n  \
           --count            print the match count, not the ranked hits\n  \
           --explain           report the query plan on stderr\n  \
           --index <file>     reuse/persist an index (default: cache dir)\n  \
           --reindex          rebuild even if a usable index exists\n  \
           --max-age <dur>    rebuild an index older than this (default 30m; 0 = never reuse)\n  \
           --pick             hand the results to a picker (fzf) and print the choice\n  \
           --fzf-binary <p>   the picker to run (default: the first of fzf or sk on PATH)\n  \
           --fzf-arg <arg>    one argument for the picker, repeatable\n  \
           --daemon           ask the running searchd, not the local index\n\
         \n\
         The default picker is fzf or sk: they are the two that read a list\n\
         separated by NUL. A picker named with --fzf-binary that cannot read\n\
         one (peco, fz) gets the list one path per line.\n\
         \n\
         query: fsearch tokens (bare=fuzzy, 'x exact, ^x prefix, x$ suffix,\n\
         !x negate) + zen-rs filters (ext: dir: type: size: re:),\n\
         | alternates, * and ? glob the whole path."
        .to_string()
}

/// Parse `30m` / `2h` / `7d` / `90s` / `1w` into a duration. Bare numbers are
/// seconds, so `--max-age 0` means "never reuse": the index is rebuilt every
/// time, which is the point of asking for no tolerance at all.
fn parse_age(s: &str) -> Option<std::time::Duration> {
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
        .map(|n| std::time::Duration::from_secs(n.saturating_mul(mult)))
}

fn main() {
    // The console decides how the UTF-8 bytes below are decoded, so it is
    // put into UTF-8 before anything is printed. A
    // redirected stdout keeps its raw bytes: a machine consumer wants
    // UTF-8, not a console's idea of it.
    rgy_engine::console::init();

    // Which view of an adapter-claimed file a search reads. rgy answers to
    // RGY_ADAPTER_VIEW and falls back to rgx's RGX_ADAPTER_VIEW, so a shell
    // configured for rgx is configured for rgy too.
    rgy_engine::adapter_view::apply();

    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut root = String::from(".");
    let mut limit = 50usize;
    let mut json = false;
    let mut query = String::new();
    let mut index: Option<String> = None;
    let mut reindex = false;
    let mut count = false;
    let mut explain = false;
    let mut max_age: Option<std::time::Duration> = None;
    let mut threads: Option<usize> = None;
    let mut pick = false;
    let mut fzf_binary: Option<String> = None;
    let mut fzf_arg: Vec<String> = Vec::new();
    let mut daemon = false;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--root" => {
                i += 1;
                root = args.get(i).cloned().unwrap_or_else(|| usage(2));
            }
            "--limit" => {
                i += 1;
                limit = args
                    .get(i)
                    .and_then(|s| s.parse().ok())
                    .unwrap_or_else(|| usage(2));
            }
            "--index" => {
                i += 1;
                index = Some(args.get(i).cloned().unwrap_or_else(|| usage(2)));
            }
            "--reindex" => reindex = true,
            "--count" => count = true,
            "--explain" => explain = true,
            "--threads" => {
                i += 1;
                threads = Some(
                    args.get(i)
                        .and_then(|s| s.parse::<usize>().ok())
                        .filter(|&n| n > 0)
                        .unwrap_or_else(|| usage(2)),
                );
            }
            "--max-age" => {
                i += 1;
                max_age = Some(
                    args.get(i)
                        .and_then(|s| parse_age(s))
                        .unwrap_or_else(|| usage(2)),
                );
            }
            "--json" => json = true,
            "--pick" => pick = true,
            "--daemon" => daemon = true,
            "--fzf-binary" => {
                i += 1;
                fzf_binary = Some(args.get(i).cloned().unwrap_or_else(|| usage(2)));
            }
            "--fzf-arg" => {
                i += 1;
                fzf_arg.push(args.get(i).cloned().unwrap_or_else(|| usage(2)));
            }
            "-h" | "--help" => usage(0),
            a if a.starts_with('-') => {
                rgy_engine::errln!("searchctl: unknown option {a}");
                std::process::exit(2);
            }
            a => {
                if !query.is_empty() {
                    query.push(' ');
                }
                query.push_str(a);
            }
        }
        i += 1;
    }
    if query.is_empty() {
        usage(2);
    }

    let opts = Opts {
        root,
        limit,
        json,
        index,
        reindex,
        count,
        explain,
        max_age,
        threads,
        pick,
        fzf_binary,
        fzf_arg,
        daemon,
    };
    if let Err(e) = run(&opts, &query) {
        rgy_engine::errln!("searchctl: {e}");
        std::process::exit(2);
    }
}

struct Opts {
    root: String,
    limit: usize,
    json: bool,
    index: Option<String>,
    reindex: bool,
    /// Report the match count instead of the ranked hits.
    count: bool,
    /// Report which execution path answered, on stderr.
    explain: bool,
    max_age: Option<std::time::Duration>,
    /// How many workers the scan and the query use. `None` keeps each
    /// one's own default.
    threads: Option<usize>,
    /// Hand the results to a picker and print what the user chose.
    pick: bool,
    /// The picker to run.
    fzf_binary: Option<String>,
    /// One argument for the picker, repeatable.
    fzf_arg: Vec<String>,
    /// Ask the running daemon, never the local index.
    daemon: bool,
}

/// The width the scan and the query use.
///
/// The flag wins; then the environment (`RGY_NUM_THREADS`), which is the
/// project's own setting and says how wide everything is; then each one's
/// own default — four for the scan (the config's default), and the machine's
/// own core count for the query, which is what the pool it replaced did.
fn widths(threads: Option<usize>) -> (usize, usize) {
    // The environment is asked once, so the two widths cannot come from
    // two different answers.
    let set = threads.or_else(rgy_par::configured);
    let query = set.unwrap_or_else(rgy_par::workers).max(1);
    let scan = set
        .unwrap_or(rgy_discovery::PortableSource::DEFAULT_THREADS)
        .max(1);
    (scan, query)
}

/// Load a usable index for this root, or build one and save it.
///
/// Always says which of the two happened, on stderr: a stale or foreign index
/// is a silent-wrong-answer risk otherwise, and a one-shot CLI has nowhere
/// else to report it.
fn obtain_index(opts: &Opts) -> Result<Index, String> {
    // Every process gets its own interrupt, so this is scoped to one build
    // rather than installed for the lifetime of the binary: a Ctrl-C that
    // lands while the query runs keeps terminating the process, as before.
    let cancel = interrupt::cancel_on_interrupt();
    let root_path = std::path::PathBuf::from(&opts.root);
    let index_path = match &opts.index {
        Some(p) => std::path::PathBuf::from(p),
        None => persist::default_index_path(&root_path),
    };

    if !opts.reindex {
        match persist::load_for(&root_path, &index_path, opts.max_age) {
            Ok(idx) => {
                // The age goes with the reuse: an index that is "reused" is a
                // snapshot, and a snapshot of unknown age is how a query
                // answers "no such file" for a file that exists.
                rgy_engine::errln!(
                    "searchctl: reusing index {} ({} entries, built {})",
                    index_path.display(),
                    idx.n,
                    persist::age_text(age_of(&idx))
                );
                return Ok(idx);
            }
            Err(why) => rgy_engine::errln!("searchctl: rebuilding — {why}"),
        }
    }

    // The platform walk where one exists,
    // the portable walk where none does or where the native one refuses —
    // no privileges needed either way, the selector falls back on its own.
    let (scan_threads, query_threads) = widths(opts.threads);
    // The query's workers: the width the user asked for, the environment's,
    // or the machine's own count — whichever it is, pinned here so every
    // parallel path in the process agrees on it.
    rgy_par::set_workers(query_threads);
    let source = source_for(opts.root.as_bytes().to_vec(), scan_threads);
    // A scan of a large tree is seconds of silence otherwise, which reads as
    // a hang. One line every `REPORT_EVERY`; the count is a real tally of
    // directories listed and entries seen, never an estimate. The callback
    // has to be `Fn` (the walk may call it from several threads at once), so
    // the clock behind it is an atomic rather than a plain `Instant`.
    let started = Instant::now();
    let last_report = std::sync::atomic::AtomicU64::new(0);
    let listings = source
        .enumerate_cancellable(&cancel, &|p| {
            let now = started.elapsed().as_millis() as u64;
            // `>` not `>=`: a second call in the same millisecond must not
            // reprint, and the ticker's own resolution is far coarser than
            // this anyway.
            if now
                > last_report.load(std::sync::atomic::Ordering::Relaxed)
                    + REPORT_EVERY.as_millis() as u64
            {
                last_report.store(now, std::sync::atomic::Ordering::Relaxed);
                rgy_engine::errln!("searchctl: scanning… {}", interrupt::line(p));
            }
        })
        .map_err(|_| "scan cancelled — no index written".to_string())?;
    let idx = {
        // The user's priority rules, beside the index: `<index>.rules`. No
        // file is not an error -- the defaults are the rules.
        let (rules, why) = rgy_name_core::rules::Rules::beside(&index_path);
        if let Some(why) = why {
            rgy_engine::errln!("searchctl: {why} -- using the default rules");
        }
        Index::build_with_rules(listings, 0, now_secs(), opts.root.as_bytes(), &rules)
    };
    match persist::save(&idx, &root_path, &index_path) {
        Ok(saved) => rgy_engine::errln!(
            "searchctl: indexed {} entries → {}",
            idx.n,
            saved.index.display()
        ),
        // A cache miss must not fail the query: the index is built and
        // usable, it just will not be there next time.
        Err(e) => rgy_engine::errln!(
            "searchctl: could not save index to {}: {e}",
            index_path.display()
        ),
    }
    Ok(idx)
}

/// Hand the results to a picker and print what the user chose.
///
/// The daemon first, when it answers: its index is warm, so the picker
/// opens over a list that is already there and the whole exchange is a
/// keystroke. The local index is the fallback, and it says so — a silent
/// fallback would leave the user wondering why the picker was slow.
///
/// The records are the paths, separated by NUL. A path may carry a tab or
/// a newline on unix, and a separator a path can carry is a separator that
/// breaks: the NUL byte cannot appear in a path anywhere.
///
/// The order is rgy's. The picker narrows the list as the user types; it
/// does not re-score it, because a search engine's ranking is the product
/// and a picker that replaces it throws the product away.
fn run_pick(opts: &Opts, query: &str) -> Result<i32, String> {
    use std::io::Write;

    let req = rgy_pick::Request {
        args: &opts.fzf_arg,
        binary: opts.fzf_binary.as_deref(),
        separator: rgy_pick::Separator::Nul,
        order: rgy_pick::Order::Keep,
    };

    let mut client = match rgy_proto::endpoint::Endpoint::registered() {
        Some(ep) => match rgy_proto::Client::connect(&ep) {
            Ok(c) => Some(c),
            Err(e) => {
                if opts.daemon {
                    return Err(format!(
                        "the daemon at {} did not answer: {e}",
                        ep.describe()
                    ));
                }
                rgy_engine::errln!(
                    "searchctl: no daemon at {} ({e}); using the local index",
                    ep.describe()
                );
                None
            }
        },
        None => {
            if opts.daemon {
                return Err("this machine has no registered daemon to ask".to_string());
            }
            None
        }
    };

    if let Some(mut c) = client.take() {
        let limit = opts.limit;
        let q = query.to_string();
        rgy_engine::errln!("searchctl: asking the running daemon (its own tree, not --root)");
        let search = Box::new(
            move |out: &mut (dyn Write + Send + 'static)| -> Result<bool, String> {
                let outcome = c.search(&q, limit).map_err(|e| e.to_string())?;
                let mut any = false;
                for hit in &outcome.hits {
                    out.write_all(hit.path.as_bytes())
                        .map_err(|e| format!("the results could not reach the picker: {e}"))?;
                    out.write_all(&[0])
                        .map_err(|e| format!("the results could not reach the picker: {e}"))?;
                    any = true;
                }
                Ok(any)
            },
        );
        let (code, records) = rgy_pick::pick_records(search, &req)?;
        print_selection(&records);
        return Ok(code);
    }

    // The local index: the same walk and the same ranking, paid for here.
    let idx = obtain_index(opts)?;
    let live = Live::with_root(idx, opts.root.as_bytes());
    let home = std::env::var("USERPROFILE")
        .or_else(|_| std::env::var("HOME"))
        .unwrap_or_default();
    let alts = Alternatives::parse(query, &home)?;
    for w in &alts.warnings {
        rgy_engine::errln!("warning: {w}");
    }
    let root_disp = opts.root.trim_end_matches(['/', '\\']).to_string();
    let limit = opts.limit;
    let search = Box::new(
        move |out: &mut (dyn Write + Send + 'static)| -> Result<bool, String> {
            let searcher = Searcher { live: &live };
            let hits = searcher.search_alternatives(&alts.alts, limit);
            let mut buf = Vec::new();
            let mut any = false;
            for h in &hits {
                let owned;
                let rel: &str = match &h.over {
                    Some(p) => {
                        owned = String::from_utf8_lossy(p).into_owned();
                        &owned
                    }
                    None => {
                        searcher.live.base.path(h.idx as usize, &mut buf);
                        owned = String::from_utf8_lossy(&buf).into_owned();
                        &owned
                    }
                };
                let path = if rel.is_empty() {
                    root_disp.clone()
                } else {
                    let rel = rel.trim_start_matches(['/', '\\']);
                    format!("{root_disp}{}{rel}", std::path::MAIN_SEPARATOR)
                };
                out.write_all(path.as_bytes())
                    .map_err(|e| format!("the results could not reach the picker: {e}"))?;
                out.write_all(&[0])
                    .map_err(|e| format!("the results could not reach the picker: {e}"))?;
                any = true;
            }
            Ok(any)
        },
    );
    let (code, records) = rgy_pick::pick_records(search, &req)?;
    print_selection(&records);
    Ok(code)
}

/// Print what the picker chose, one path per line.
///
/// The path, and not the record: the record is a path, and a picker that
/// chose one has chosen a path.
///
/// The line is one path, and a path that carries a line break cannot be
/// one line: it is left out, and the count says how many were left out.
/// Half of a path is a path that does not exist.
fn print_selection(records: &[Vec<u8>]) {
    use std::io::Write;
    let dropped = records
        .iter()
        .filter(|record| !rgy_pick::carries_line(record))
        .count();
    if dropped > 0 {
        eprintln!("searchctl: {}", rgy_pick::uncarryable_paths(dropped));
    }
    let mut out = std::io::stdout();
    for record in records {
        if !rgy_pick::carries_line(record) {
            continue;
        }
        let _ = out.write_all(record);
        let _ = out.write_all(b"\n");
    }
    let _ = out.flush();
}

fn run(opts: &Opts, query: &str) -> Result<(), String> {
    // `--pick` owns the whole run: it asks, it feeds a picker, and it exits
    // with the picker's status. It is before the index, because the daemon
    // already has one and the local build is the fallback.
    if opts.pick {
        let code = run_pick(opts, query)?;
        std::process::exit(code);
    }
    let root = &opts.root;
    let limit = opts.limit;
    let json = opts.json;
    let count = opts.count;
    let idx = obtain_index(opts)?;
    let live = Live::with_root(idx, root.as_bytes());
    let searcher = Searcher { live: &live };

    // Merged query syntax: `|` alternatives, fail-closed filters.
    let home = std::env::var("USERPROFILE")
        .or_else(|_| std::env::var("HOME"))
        .unwrap_or_default();
    let alts = Alternatives::parse(query, &home)?;
    for w in &alts.warnings {
        rgy_engine::errln!("warning: {w}");
    }

    // Content index, when one is already built alongside this root's name
    // index (the daemon keeps it fresh). Auto-select: use it to skip reads
    // the grams rule out; without one, `answer` falls back to the scan. The
    // union stays the scan's exact answer either way -- `Content::covers`
    // only vouches for files whose recorded stats still match disk, so a
    // stale or foreign index loses coverage and is read from disk instead
    // of silently answering wrong.
    let content_index = {
        let root_path = std::path::PathBuf::from(&opts.root);
        let base = opts
            .index
            .as_ref()
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| persist::default_index_path(&root_path));
        let dir = base.with_extension("content");
        dir.is_dir()
            .then(|| content::Content::open_shared(dir))
            .filter(|c| !c.segs.is_empty())
    };

    // `grep:`/`regex:`/`sym:` content search (fsearch model: the name
    // tokens and filters narrow WHICH files are read; the rgx engine
    // confirms each one). rg-style `path:line:text` output.
    if alts.alts.iter().any(|a| a.grep.is_some()) {
        // `--count` counts matching lines, which is the unit the
        // `path:line:text` output prints and the one rg's `-c` counts. It
        // used to be ignored here outright: this path returns before the
        // `--count` check further down, so the flag was documented, parsed,
        // and silently discarded on half the query surface. Worse, the scan
        // ran at `limit` (50), so a count taken here would have been a
        // preview wearing a count's clothes.
        if count {
            let mut n = 0usize;
            for alt in &alts.alts {
                let Some(pat) = &alt.grep else { continue };
                let mut g = Grep::new(pat, alt.grep_mode)?;
                // No preview caps. `Grep` is built for *showing* results: it
                // stops at 5 lines a file and gives up after 250 ms, which is
                // right for a preview and wrong for a number. Counting what
                // the preview happened to reach would report 44 where there
                // are 352, and look confident about it.
                g.max_per_file = usize::MAX;
                g.budget = None;
                let res = content::answer(
                    &g,
                    alt,
                    root.as_bytes(),
                    searcher.live,
                    home.as_bytes(),
                    content_index.as_ref(),
                    usize::MAX,
                );
                n += res.files.iter().map(|f| f.lines.len()).sum::<usize>();
            }
            if n == 0 {
                std::process::exit(1);
            }
            if json {
                rgy_engine::outln!("{{\"count\":{n}}}");
            } else {
                rgy_engine::outln!("{n}");
            }
            return Ok(());
        }
        let mut shown = 0usize;
        for alt in &alts.alts {
            let Some(pat) = &alt.grep else { continue };
            let g = Grep::new(pat, alt.grep_mode)?;
            let res = content::answer(
                &g,
                alt,
                root.as_bytes(),
                searcher.live,
                home.as_bytes(),
                content_index.as_ref(),
                limit,
            );
            // The records that cannot be one line: the line is one path,
            // and a path that carries a line break is not one line.
            let mut dropped = 0usize;
            for f in &res.files {
                let path = String::from_utf8_lossy(&f.path);
                for (ln, text) in &f.lines {
                    shown += 1;
                    if json {
                        let esc = rgy_proto::escape_json_string;
                        rgy_engine::outln!(
                            "{{\"path\":\"{}\",\"line\":{ln},\"text\":\"{}\"}}",
                            esc(&path),
                            esc(text)
                        );
                    } else if rgy_pick::carries_line(path.as_bytes()) {
                        rgy_engine::outln!("{path}:{ln}:{text}");
                    } else {
                        // The record cannot be one line: the path carries a
                        // line break, and half of it is a path that does not
                        // exist.
                        dropped += 1;
                    }
                }
            }
            if dropped > 0 {
                eprintln!("searchctl: {}", rgy_pick::uncarryable_paths(dropped));
            }
        }
        if shown == 0 {
            std::process::exit(1);
        }
        return Ok(());
    }

    // `--explain`: report which execution path answered, and on what evidence.
    // This is the observation `search_base` lacked — the path is chosen by
    // try-and-bail, so a slow query and a fast one used to look identical.
    if opts.explain {
        let mut summary = Vec::new();
        let mut all: Vec<Hit> = Vec::new();
        for alt in &alts.alts {
            let hits = searcher.search_uncapped(alt);
            let n = hits.len();
            all.extend(hits);
            let label = if alt.tokens.is_empty() {
                "(filters only)".to_string()
            } else {
                alt.tokens
                    .iter()
                    .map(|t| {
                        format!(
                            "{}{}",
                            if t.negate { "!" } else { "" },
                            String::from_utf8_lossy(&t.text)
                        )
                    })
                    .collect::<Vec<_>>()
                    .join(" ")
            };
            let rejected = Searcher::rejected_attempts();
            let plan = Searcher::last_plan()
                .map(|p| p.to_string())
                .unwrap_or_else(|| "none".into());
            summary.push(format!(
                "{label}\t{} matches\tplan={plan}{}",
                n,
                if rejected.is_empty() {
                    String::new()
                } else {
                    format!("\trejected-after: {}", rejected.join(","))
                }
            ));
        }
        // The union of the alternatives, not their sum: an entry two of them
        // match is one result below, and a total that disagrees with what the
        // command prints is worse than no total at all. `merge_hits` is the
        // same merge the results go through.
        let total = merge_hits(all, usize::MAX).len();
        rgy_engine::errln!(
            "searchctl: {} entries indexed, {total} matches",
            searcher.live.base.n
        );
        for line in &summary {
            rgy_engine::errln!("searchctl: {line}");
        }
    }

    // `--count`: same match logic, no ranking and no path reconstruction.
    // `count_alternatives` dedups by an entry's real identity, so an entry two
    // alternatives both match counts once and the entries added since the last
    // compaction are not collapsed into one.
    if count {
        let n = searcher.count_alternatives(&alts.alts);
        if n == 0 {
            std::process::exit(1);
        }
        if json {
            rgy_engine::outln!("{{\"count\":{n}}}");
        } else {
            rgy_engine::outln!("{n}");
        }
        return Ok(());
    }

    // Score every alternative; a hit is the best score over them. The merge is
    // `Searcher`'s, not a loop here: keying it on `Hit::idx` silently dropped
    // live entries (they all share the `u32::MAX` sentinel) and finding each
    // hit by scanning made it quadratic.
    let hits = searcher.search_alternatives(&alts.alts, limit);

    if hits.is_empty() {
        // rg convention: 0 = matches, 1 = none, 2 = error.
        std::process::exit(1);
    }
    let mut out = Vec::new();
    let root_disp = root.trim_end_matches(['/', '\\']);
    // The paths that cannot be one line: the line is one path, and a
    // path that carries a line break is not one line.
    let mut dropped = 0usize;
    for h in &hits {
        // An overlay hit already carries its own path. Reading it back from
        // the index instead would index `parent[u32::MAX]` -- out of bounds --
        // and panic.
        let owned;
        let rel: &str = match &h.over {
            Some(p) => {
                owned = String::from_utf8_lossy(p).into_owned();
                &owned
            }
            None => {
                searcher.live.base.path(h.idx as usize, &mut out);
                owned = String::from_utf8_lossy(&out).into_owned();
                &owned
            }
        };
        // `Index::path` reconstructs from the root's children; show the
        // searched tree so a hit is clickable.
        let path = if rel.is_empty() {
            root_disp.to_string()
        } else {
            let rel = rel.trim_start_matches(['/', '\\']);
            format!("{root_disp}{}{rel}", std::path::MAIN_SEPARATOR)
        };
        if json {
            let esc = rgy_proto::escape_json_string(&path);
            // The characters the query matched: the span the score came
            // from. With more than one alternative a hit does not say which
            // alternative it came from, so the span would be a guess and it
            // is left out.
            let matched = match alts.alts.as_slice() {
                [one] => {
                    let name = rel.rsplit(['/', '\\']).next().unwrap_or(rel);
                    let chars =
                        rgy_name_core::name_query::matched_chars(name.as_bytes(), &one.tokens);
                    (!chars.is_empty()).then(|| {
                        chars
                            .iter()
                            .map(|c| c.to_string())
                            .collect::<Vec<_>>()
                            .join(",")
                    })
                }
                _ => None,
            };
            match matched {
                Some(m) => rgy_engine::outln!(
                    "{{\"path\":\"{esc}\",\"score\":{},\"match\":[{m}]}}",
                    h.score()
                ),
                None => rgy_engine::outln!("{{\"path\":\"{esc}\",\"score\":{}}}", h.score()),
            }
        } else if rgy_pick::carries_line(path.as_bytes()) {
            rgy_engine::outln!("{path}");
        } else {
            // The path cannot be one line, and half of it is a path that
            // does not exist.
            dropped += 1;
        }
        out.clear();
    }
    if dropped > 0 {
        // A path that is missing from the list is a path the user has to
        // be told about.
        eprintln!("searchctl: {}", rgy_pick::uncarryable_paths(dropped));
    }
    Ok(())
}

/// How old the index is, in seconds: the header's `synced_at` is the
/// wall-clock second the index was built at.
fn age_of(idx: &Index) -> u64 {
    (now_secs() as u64).saturating_sub(idx.synced_at as u64)
}

fn now_secs() -> u32 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_help_names_the_picker_flags() {
        let text = help();
        for needle in ["--pick", "--fzf-binary", "--fzf-arg"] {
            assert!(text.contains(needle), "the help needs `{needle}`: {text}");
        }
    }

    /// The help offers only the pickers that can read the list.
    ///
    /// The default is the picker that reads the NUL-separated list:
    /// `peco` and `fz` do not, and offering them is offering a picker
    /// that never sees the results.
    #[test]
    fn the_help_offers_only_the_pickers_that_can_read_the_list() {
        let text = help();
        assert!(text.contains("the first of fzf or sk on PATH"), "{text}");
        assert!(!text.contains("peco, fz on PATH"), "{text}");
    }

    /// The help says what a picker that cannot read the list gets.
    ///
    /// A picker named with `--fzf-binary` that cannot read a
    /// NUL-separated list gets the list one path per line, and the help
    /// has to say so: the difference is the difference between a picker
    /// that shows the paths and one that shows nothing.
    #[test]
    fn the_help_says_what_a_named_picker_gets() {
        let text = help();
        assert!(text.contains("one path per line"), "{text}");
    }
}
