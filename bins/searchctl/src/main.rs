// SPDX-License-Identifier: Apache-2.0

//! searchctl — the merged name-search CLI.
//!
//! name-search one-shot ergonomics + zen-rs's filter vocabulary (see
//! `rgy_name_core::name_query`), over the v2 name index. It enumerates a
//! directory tree (`--root`, portable walk / no privileges); the Windows
//! MFT fast path (a `--root` that names the volume root, and a volume this
//! process may open) and content `grep:` land with the engine and the
//! content layer.
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
use rgy_discovery::walk::KIND_DIR;
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

/// Print the usage and leave. `code` is the exit status: 0 when the
/// user asked for help, 2 for the rg convention's "bad invocation".
/// `--help` used to exit 2 like a malformed command, which a script
/// reads as an error.
///
/// The stream follows the status: the help is an answer, and an answer
/// goes to stdout — `searchctl --help | less` shows the reader nothing
/// when the help goes to stderr. The complaint about a bad invocation
/// goes to stderr, because that one is not an answer.
fn usage(code: i32) -> ! {
    if code == 0 {
        rgy_engine::outln!("{}", help());
    } else {
        rgy_engine::errln!("{}", help());
    }
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
           -h, --help         this text\n\
           -V, --version      the version\n\
         \n\
         The default picker is fzf or sk: they are the two that read a list\n\
         separated by NUL. A picker named with --fzf-binary that cannot read\n\
         one (peco, fz) gets the list one path per line.\n\
         \n\
         query: name tokens (bare=fuzzy, 'x exact, ^x prefix, x$ suffix,\n\
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

/// The limit the CLI honours, on every route.
///
/// The limit is the caller's, but it is the same limit on both routes: the
/// daemon's reader clamps the request to the protocol's cap, and a local
/// route that honoured a bigger one would answer a question the daemon
/// answers differently — the same flag, the same tool, two answers. The
/// cap is the protocol's, so the two routes cannot drift apart again.
fn clamp_limit(limit: usize) -> usize {
    limit.min(rgy_proto::MAX_LIMIT)
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
                    .map(clamp_limit)
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
            "-V" | "--version" => {
                rgy_engine::outln!("searchctl {}", env!("CARGO_PKG_VERSION"));
                std::process::exit(0);
            }
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

    // A path that begins with `~` names the home directory. A POSIX shell
    // expands it before the command ever runs and Windows PowerShell does
    // not, so the tree expands it here: a search of a folder literally
    // named `~` answers about nothing and does not say so, which is the one
    // failure a reader cannot tell apart from an empty tree.
    let root = rgy_engine::paths::expand_home(&root);
    let index = index.map(|p| rgy_engine::paths::expand_home(&p));

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
    let mut idx = {
        // The user's priority rules, beside the index: `<index>.rules`. No
        // file is not an error -- the defaults are the rules. The message
        // the rules file comes back with says what is in use: the defaults
        // when the file could not be read, the parsed rules when a line of
        // it did not parse.
        let (rules, why) = rgy_name_core::rules::Rules::beside(&index_path);
        if let Some(why) = why {
            rgy_engine::errln!("searchctl: {why}");
        }
        Index::build_with_rules(listings, 0, now_secs(), opts.root.as_bytes(), &rules)
    };
    match persist::save(&mut idx, &root_path, &index_path) {
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

/// The path of a hit inside the tree, as the index spells it.
///
/// An overlay hit already carries its own path; reading it back from the
/// index instead would index `parent[u32::MAX]` -- out of bounds -- and
/// panic.
fn hit_rel(searcher: &Searcher, hit: &Hit, buf: &mut Vec<u8>) -> String {
    match &hit.over {
        Some(p) => rgy_engine::console::spell(p),
        None => {
            searcher.live.base.path(hit.idx as usize, buf);
            rgy_engine::console::spell(buf)
        }
    }
}

/// The path of a hit, as the platform spells it: the searched tree, so a
/// hit is clickable.
///
/// The bare path, and the path the picker is handed. What the picker
/// hands back is what the CLI opens, so the mark the list prints is never
/// glued to it.
fn hit_path(rel: &str, root_disp: &str) -> String {
    if rel.is_empty() {
        root_disp.to_string()
    } else {
        let rel = rel.trim_start_matches(['/', '\\']);
        format!("{root_disp}{}{rel}", std::path::MAIN_SEPARATOR)
    }
}

/// Whether the hit is a folder.
///
/// The index knows and the path does not say: a folder's path is spelled
/// like a file's. The base's kind byte says it for an indexed entry, and
/// the overlay's own entry says it for the entries added since the last
/// compaction.
fn is_folder(live: &Live, hit: &Hit) -> bool {
    match &hit.over {
        Some(path) => live
            .over
            .get(path.as_slice())
            .map_or_else(|| spelled_as_folder(path), |o| o.kind & 3 == KIND_DIR),
        None => live.base.kind()[hit.idx as usize] & 3 == KIND_DIR,
    }
}

/// Whether the path says it is a folder: it ends in the separator the
/// platform spells a folder with.
///
/// The tree reads a path that way already -- `rgy_launcher`'s `IconKind`
/// takes a trailing separator as a folder.
fn spelled_as_folder(path: &[u8]) -> bool {
    matches!(path.last(), Some(b'/') | Some(b'\\'))
}

/// The path as the list shows it.
///
/// A folder's path ends in the separator the platform spells a folder
/// with, and a file's does not. The line is a path either way -- the same
/// path, spelled the way the platform spells a folder -- so a script that
/// reads the list still reads a path it can open, and the user tells a
/// folder from a file without opening it.
fn marked(path: &str, folder: bool) -> String {
    if !folder || path.ends_with(['/', '\\']) {
        return path.to_string();
    }
    format!("{path}{}", std::path::MAIN_SEPARATOR)
}

/// The JSON's folder field, in the wire's own spelling.
///
/// The field is additive: it is there only when the hit is a folder, so a
/// reader that does not know it reads a hit without it as a file. The
/// path in the JSON stays the path -- the field is what says the hit is a
/// folder.
fn folder_field(folder: bool) -> &'static str {
    if folder { ",\"folder\":true" } else { "" }
}

/// What the CLI says about a name answer the daemon gave.
///
/// The daemon's answer carries the fact — whether it is whole — and the
/// sentence is the protocol's own, the one the daemon's content answer
/// says too: two clients that read the same daemon have to say the same
/// words about the same answer. The command line's own search has no
/// daemon to ask, so it decides the fact with the protocol's rule and says
/// the same sentence.
fn daemon_name_note(outcome: &rgy_proto::SearchOutcome) -> Option<&'static str> {
    if outcome.complete {
        None
    } else {
        Some(rgy_proto::client::ANSWER_STOPS_AT_THE_LIMIT)
    }
}

/// What the CLI says about the name answer the command line produced.
///
/// The rule is the protocol's — the one every producer of a name answer
/// follows: the daemon's whole tree and its scope, the fresh search and
/// the session's re-rank, and this search too — and the sentence is the
/// protocol's own, so the two routes say the same words about the same
/// answer. A flag that changes where the answer comes from must not change
/// what the answer says about itself.
fn local_name_note(hits: usize, limit: usize) -> Option<&'static str> {
    if rgy_proto::name_answer_is_complete(hits, limit) {
        None
    } else {
        Some(rgy_proto::client::ANSWER_STOPS_AT_THE_LIMIT)
    }
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

    // The query is parsed once, here: the layer the query belongs to decides
    // which question is asked, and a query with a content operator is a
    // content question.
    //
    // The picker used to ask the *name* layer whatever the query said: the
    // content operator was parsed away, the query was left with no name
    // token at all, and the name layer matched *everything*. So
    // `--pick 'grep:needle'` offered the whole tree, and the user opened a
    // file that does not contain the text they searched for — while the
    // same query without `--pick` answered correctly. Two forms of the
    // same tool, two answers to the same question.
    let home = std::env::var("USERPROFILE")
        .or_else(|_| std::env::var("HOME"))
        .unwrap_or_default();
    let alts = Alternatives::parse(query, &home)?;
    for w in &alts.warnings {
        rgy_engine::errln!("warning: {w}");
    }
    let content = alts.alts.iter().any(|a| a.grep.is_some());

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

    // What the answer says about itself, when it is not the whole answer.
    //
    // The search runs on the picker's own thread, while the picker is up
    // and owns the terminal: a note written then lands in the middle of
    // its picture. So the fact is carried out of the search and said
    // after the picker is done.
    let note = std::sync::Arc::new(std::sync::Mutex::new(None::<&'static str>));

    if let Some(mut c) = client.take() {
        let limit = opts.limit;
        let q = query.to_string();
        let carried = note.clone();
        rgy_engine::errln!("searchctl: asking the running daemon (its own tree, not --root)");
        let search = Box::new(
            move |out: &mut (dyn Write + Send + 'static)| -> Result<bool, String> {
                if content {
                    // The content route: the files that hold the text, and
                    // not the files whose name matches nothing.
                    let outcome = c.grep(&q, limit).map_err(|e| e.to_string())?;
                    let mut any = false;
                    for file in &outcome.files {
                        out.write_all(file.path.as_bytes())
                            .map_err(|e| format!("the results could not reach the picker: {e}"))?;
                        out.write_all(&[0])
                            .map_err(|e| format!("the results could not reach the picker: {e}"))?;
                        any = true;
                    }
                    return Ok(any);
                }
                let outcome = c.search(&q, limit).map_err(|e| e.to_string())?;
                // The list the picker is given may be a page of a longer
                // one, and the picker cannot say so: the user chooses from
                // what it shows.
                *carried.lock().unwrap() = daemon_name_note(&outcome);
                let mut any = false;
                for hit in &outcome.hits {
                    // The record is the bare path: the picker hands the
                    // line it chose back, and the CLI opens that. The
                    // daemon's folder flag is not part of the path.
                    out.write_all(hit.path.as_bytes())
                        .map_err(|e| format!("the results could not reach the picker: {e}"))?;
                    out.write_all(&[0])
                        .map_err(|e| format!("the results could not reach the picker: {e}"))?;
                    any = true;
                }
                Ok(any)
            },
        );
        let (code, records) = match rgy_pick::pick_records(search, &req) {
            Ok(picked) => picked,
            Err(e) => {
                // A picker that cannot run is a failure of the picker, and
                // the exit table gives it 1 — "the picker failed". The error
                // used to travel as an Err and the command answered 2, which
                // the table reserves for a command that could not run at
                // all; and `rgxy`, which has the same picker, answers 2 for
                // it on purpose, so the two tools were saying different
                // things about the same failure.
                rgy_engine::errln!("searchctl: {e}");
                return Ok(1);
            }
        };
        print_selection(&records);
        if let Some(why) = *note.lock().unwrap() {
            eprintln!("searchctl: {why}");
        }
        return Ok(code);
    }

    // The local index: the same walk and the same ranking, paid for here.
    let idx = obtain_index(opts)?;
    let live = Live::with_root(idx, opts.root.as_bytes());
    let root_disp = opts.root.trim_end_matches(['/', '\\']).to_string();
    let limit = opts.limit;
    let carried = note.clone();
    // The content index, when one is already built alongside this root's
    // name index (the daemon keeps it fresh). The same rule the plain route
    // follows: with one the search skips the reads the grams rule out, and
    // without one it reads the files.
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
    let root = opts.root.clone();
    let home = home.clone();
    let search = Box::new(
        move |out: &mut (dyn Write + Send + 'static)| -> Result<bool, String> {
            if content {
                // The content route: the files that hold the text, and not
                // the files whose name matches nothing.
                let mut any = false;
                let mut shown = 0usize;
                for alt in &alts.alts {
                    let Some(pat) = &alt.grep else { continue };
                    let g = match Grep::new(pat, alt.grep_mode) {
                        Ok(g) => g,
                        Err(e) => {
                            rgy_engine::errln!("warning: {e}");
                            continue;
                        }
                    };
                    let res = content::answer_with(
                        &g,
                        alt,
                        content::Scope {
                            root: root.as_bytes(),
                            live: &live,
                            home: home.as_bytes(),
                            index: content_index.as_ref(),
                            // The formats the adapters claim are read through
                            // them, so a PDF's words and an archive member's
                            // content are searched like any other file's bytes.
                            adapter: Some(&rgy_adapters::Adapters),
                        },
                        limit.saturating_sub(shown),
                    );
                    shown += res.files.len();
                    for f in &res.files {
                        // The record is the bare path: the picker hands the
                        // line it chose back, and the CLI opens that. The
                        // path is spelled the way the tree spells it.
                        out.write_all(&f.path)
                            .map_err(|e| format!("the results could not reach the picker: {e}"))?;
                        out.write_all(&[0])
                            .map_err(|e| format!("the results could not reach the picker: {e}"))?;
                        any = true;
                    }
                }
                return Ok(any);
            }
            let searcher = Searcher { live: &live };
            let hits = searcher.search_alternatives(&alts.alts, limit);
            // The list the picker is given may be a page of a longer one,
            // and the picker cannot say so: the user chooses from what it
            // shows.
            *carried.lock().unwrap() = local_name_note(hits.len(), limit);
            let mut buf = Vec::new();
            let mut any = false;
            for h in &hits {
                // The record is the bare path, for the same reason the
                // daemon's is: the picker hands the line back and the CLI
                // opens it. The mark the list prints is not glued to it.
                let rel = hit_rel(&searcher, h, &mut buf);
                let path = hit_path(&rel, &root_disp);
                out.write_all(path.as_bytes())
                    .map_err(|e| format!("the results could not reach the picker: {e}"))?;
                out.write_all(&[0])
                    .map_err(|e| format!("the results could not reach the picker: {e}"))?;
                any = true;
            }
            Ok(any)
        },
    );
    let (code, records) = match rgy_pick::pick_records(search, &req) {
        Ok(picked) => picked,
        Err(e) => {
            // A picker that cannot run is a failure of the picker, and the
            // exit table gives it 1 — "the picker failed". The error used
            // to travel as an Err and the command answered 2, which the
            // table reserves for a command that could not run at all.
            rgy_engine::errln!("searchctl: {e}");
            return Ok(1);
        }
    };
    print_selection(&records);
    if let Some(why) = *note.lock().unwrap() {
        eprintln!("searchctl: {why}");
    }
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

/// Ask the running daemon, and print what it says.
///
/// The flag's words are the contract: **the running daemon, never the local
/// index**. The daemon's tree is not `--root` — it is the tree the daemon
/// was started on — and an answer from the wrong tree answers a question
/// nobody asked. So there is no fallback here: a daemon that is not there
/// is an error, and the error says where it looked.
///
/// The shapes are the local path's shapes: a path per line for the names,
/// `path:line:text` for the content, and the same JSON. A flag that
/// changes where the answer comes from must not change what the answer
/// looks like.
fn run_daemon(opts: &Opts, query: &str) -> Result<(), String> {
    if opts.count {
        // The daemon's answer is a page: it stops at the limit, and the
        // limit is what the user asked for. A count taken from a page is
        // a preview wearing a count's clothes.
        return Err(
            "--count cannot be answered by the daemon: its answer stops at the limit".to_string(),
        );
    }
    let endpoint = rgy_proto::endpoint::Endpoint::registered()
        .ok_or_else(|| "this machine has no registered daemon to ask".to_string())?;
    let mut client = rgy_proto::Client::connect(&endpoint)
        .map_err(|e| format!("the daemon at {} did not answer: {e}", endpoint.describe()))?;
    rgy_engine::errln!("searchctl: asking the running daemon (its own tree, not --root)");

    // The query is parsed here for the same reason the local path parses
    // it: the layer the query belongs to decides which question the
    // daemon is asked. The warnings are **not** printed from here — the
    // daemon parses the same query with the same parser and says them
    // on the wire, and the answer's own warnings are the ones about the
    // answer.
    let home = std::env::var("USERPROFILE")
        .or_else(|_| std::env::var("HOME"))
        .unwrap_or_default();
    let alts = Alternatives::parse(query, &home)?;
    let content = alts.alts.iter().any(|a| a.grep.is_some());

    if content {
        let outcome = match client.grep(query, opts.limit) {
            Ok(o) => o,
            Err(e) => {
                // The daemon reported an error, and the exit table gives
                // that 1 — "the daemon reported an error". The error used
                // to travel as an Err and the command answered 2, which the
                // table reserves for a command that could not run at all,
                // and the one code the table reserves for a daemon-sourced
                // error was the one the CLI never produced for one. A pipe
                // that broke is not the daemon's report, and that stays 2.
                if rgy_proto::client::is_the_daemon_saying_no(&e) {
                    rgy_engine::errln!("searchctl: {e}");
                    std::process::exit(1);
                }
                return Err(e.to_string());
            }
        };
        for w in &outcome.warnings {
            rgy_engine::errln!("warning: {w}");
        }
        let printed = daemon_content_lines(&outcome.files, opts.json);
        print_lines(&printed.lines);
        if printed.dropped > 0 {
            eprintln!(
                "searchctl: {}",
                rgy_pick::uncarryable_paths(printed.dropped)
            );
        }
        if let Some(why) = outcome.partial() {
            // The daemon says what it did not read. A search that did not
            // read everything did not say it read everything, and an
            // answer that does not say so is an answer that looks whole.
            eprintln!("searchctl: {why}");
        }
        // The reasons are the other half of the same fact: the count says
        // how many files were not read, and the reasons say why, and which
        // file it was when the daemon knows. The daemon's own client prints
        // them, and a CLI that dropped them would leave the user guessing
        // whether the file was empty, locked, or never there -- while the
        // CLI's own route, one code path away, says it.
        for reason in outcome.skip_reasons() {
            eprintln!("searchctl: {reason}");
        }
        if opts.explain {
            rgy_engine::errln!(
                "searchctl: the running daemon answered the content layer in {} us",
                outcome.took_us
            );
        }
        if printed.found == 0 {
            std::process::exit(1);
        }
        return Ok(());
    }

    let outcome = match client.search(query, opts.limit) {
        Ok(o) => o,
        Err(e) => {
            // The daemon reported an error, and the exit table gives that
            // 1 — "the daemon reported an error". The error used to travel
            // as an Err and the command answered 2, which the table
            // reserves for a command that could not run at all. A pipe that
            // broke is not the daemon's report, and that stays 2.
            if rgy_proto::client::is_the_daemon_saying_no(&e) {
                rgy_engine::errln!("searchctl: {e}");
                std::process::exit(1);
            }
            return Err(e.to_string());
        }
    };
    for w in &outcome.warnings {
        rgy_engine::errln!("warning: {w}");
    }
    let printed = daemon_name_lines(&outcome.hits, opts.json);
    print_lines(&printed.lines);
    if printed.dropped > 0 {
        eprintln!(
            "searchctl: {}",
            rgy_pick::uncarryable_paths(printed.dropped)
        );
    }
    if let Some(why) = daemon_name_note(&outcome) {
        // The daemon says whether its answer is whole, and the name route
        // says it too: a name answer that stopped at the limit is a page
        // of a longer list, and a page that does not say so reads as the
        // whole list. The content route above already says it, and a flag
        // that changes where the answer comes from must not change what
        // the answer says about itself.
        eprintln!("searchctl: {why}");
    }
    if opts.explain {
        rgy_engine::errln!(
            "searchctl: the running daemon answered the name layer in {} us with {} hit(s); the daemon does not report its plan",
            outcome.took_us,
            outcome.hits.len()
        );
    }
    if printed.found == 0 {
        std::process::exit(1);
    }
    Ok(())
}

/// What the daemon's answer is printed as.
///
/// The lines are what the caller prints. The two counts are what the
/// caller has to say about them: the records that were found — the answer
/// is empty when there were none — and the ones that could not be one
/// line, because half of a path is a path that does not exist.
#[derive(Debug)]
struct Printed {
    lines: Vec<String>,
    found: usize,
    dropped: usize,
}

/// The lines the daemon's names are printed as.
///
/// The shapes are the local path's shapes: a path per line, or one JSON
/// object per hit. The JSON carries the score, and the characters the
/// query matched when the daemon could say which they were — the daemon
/// leaves the span empty when it could not, and a span that guessed would
/// be a lie in the place the user looks first.
///
/// The daemon says which of its hits are folders, and the answer says it
/// too: a folder's path is spelled the way the platform spells a folder,
/// and the JSON carries the wire's own field for it.
fn daemon_name_lines(hits: &[rgy_proto::Hit], json: bool) -> Printed {
    let mut printed = Printed {
        lines: Vec::new(),
        found: hits.len(),
        dropped: 0,
    };
    for hit in hits {
        let path = &hit.path;
        if json {
            let esc = rgy_proto::escape_json_string(path);
            let matched = (!hit.span.is_empty()).then(|| {
                hit.span
                    .iter()
                    .map(|n| n.to_string())
                    .collect::<Vec<_>>()
                    .join(",")
            });
            // The daemon says which of its hits are folders, and the JSON
            // says it too: the field is the wire's own, and it is there
            // only for a folder.
            let folder = folder_field(hit.folder);
            printed.lines.push(match matched {
                Some(m) => format!(
                    "{{\"path\":\"{esc}\",\"score\":{},\"match\":[{m}]{folder}}}",
                    hit.score
                ),
                None => format!("{{\"path\":\"{esc}\",\"score\":{}{folder}}}", hit.score),
            });
        } else if rgy_pick::carries_line(path.as_bytes()) {
            // The folder's path, spelled the way the platform spells a
            // folder: the line is a path, and it says what it is.
            printed.lines.push(marked(path, hit.folder));
        } else {
            // The path cannot be one line, and half of it is a path that
            // does not exist.
            printed.dropped += 1;
        }
    }
    printed
}

/// The lines the daemon's content answer is printed as.
///
/// The shape is rg's: `path:line:text`, or one JSON object per line. The
/// path that carries a line break cannot be one line, and it is left out
/// and counted — the same rule the local path follows.
fn daemon_content_lines(files: &[rgy_proto::GrepFile], json: bool) -> Printed {
    let mut printed = Printed {
        lines: Vec::new(),
        found: 0,
        dropped: 0,
    };
    for file in files {
        let path = rgy_engine::console::spell(file.path.as_bytes());
        for (ln, text) in &file.lines {
            printed.found += 1;
            if json {
                let esc = rgy_proto::escape_json_string;
                printed.lines.push(format!(
                    "{{\"path\":\"{}\",\"line\":{ln},\"text\":\"{}\"}}",
                    esc(&path),
                    esc(text)
                ));
            } else if rgy_pick::carries_line(path.as_bytes()) {
                printed.lines.push(format!("{path}:{ln}:{text}"));
            } else {
                // The record cannot be one line: the path carries a line
                // break, and half of it is a path that does not exist.
                printed.dropped += 1;
            }
        }
    }
    printed
}

/// Print the lines, one per line.
///
/// The console writes them: the bytes are the same, and a console that
/// refused the UTF-8 code page gets the text as UTF-16 instead of
/// mojibake.
fn print_lines(lines: &[String]) {
    for line in lines {
        rgy_engine::outln!("{line}");
    }
}

fn run(opts: &Opts, query: &str) -> Result<(), String> {
    // `--pick` owns the whole run: it asks, it feeds a picker, and it exits
    // with the picker's status. It is before the index, because the daemon
    // already has one and the local build is the fallback.
    if opts.pick {
        let code = run_pick(opts, query)?;
        std::process::exit(code);
    }
    // `--daemon` asks the running daemon and nothing else. The daemon's
    // tree is not `--root` — it is the tree the daemon was started on — so
    // a client that quietly answered from a local index would be
    // answering a different question than the one that was asked, and the
    // user would never know.
    if opts.daemon {
        return run_daemon(opts, query);
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

    // `grep:`/`regex:`/`sym:` content search (the tree's model: the name
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
            // A lane that panicked is a search that lost part of itself:
            // the count is not the count, and it says so.
            let mut lost: Option<String> = None;
            for alt in &alts.alts {
                let Some(pat) = &alt.grep else { continue };
                let mut g = match Grep::new(pat, alt.grep_mode) {
                    Ok(g) => g,
                    Err(e) => {
                        // A pattern the engine refuses is said out loud, and
                        // the count goes on with the alternatives that did
                        // compile: the daemon route answers the same refusal
                        // with a warning and an empty answer. The `?` used
                        // to turn it into an error and the exit code 2, so
                        // one refusal had two codes depending on the route.
                        rgy_engine::errln!("warning: {e}");
                        continue;
                    }
                };
                // No preview caps. `Grep` is built for *showing* results: it
                // stops at 5 lines a file and gives up after 250 ms, which is
                // right for a preview and wrong for a number. Counting what
                // the preview happened to reach would report 44 where there
                // are 352, and look confident about it.
                g.max_per_file = usize::MAX;
                g.budget = None;
                let res = content::answer_with(
                    &g,
                    alt,
                    content::Scope {
                        root: root.as_bytes(),
                        live: searcher.live,
                        home: home.as_bytes(),
                        index: content_index.as_ref(),
                        adapter: Some(&rgy_adapters::Adapters),
                    },
                    usize::MAX,
                );
                n += res.files.iter().map(|f| f.lines.len()).sum::<usize>();
                if let Some(why) = &res.error {
                    lost = Some(why.clone());
                }
            }
            if let Some(why) = lost {
                eprintln!("searchctl: {why}");
                std::process::exit(1);
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
        let mut files_shown = 0usize;
        // A lane that panicked is a search that lost part of itself: the
        // rows are what was found, and they are not all of it.
        let mut lost: Option<String> = None;
        for alt in &alts.alts {
            let Some(pat) = &alt.grep else { continue };
            if files_shown >= limit {
                break;
            }
            let g = match Grep::new(pat, alt.grep_mode) {
                Ok(g) => g,
                Err(e) => {
                    // A pattern the engine refuses is said out loud, and the
                    // search goes on with the alternatives that did compile:
                    // the daemon route answers the same refusal with a
                    // warning and an empty answer. The `?` used to turn it
                    // into an error and the exit code 2, so one refusal had
                    // two codes depending on the route.
                    rgy_engine::errln!("warning: {e}");
                    continue;
                }
            };
            let res = content::answer_with(
                &g,
                alt,
                content::Scope {
                    root: root.as_bytes(),
                    live: searcher.live,
                    home: home.as_bytes(),
                    index: content_index.as_ref(),
                    // The formats the adapters claim are read through them,
                    // so a PDF's words and an archive member's content are
                    // searched like any other file's bytes.
                    adapter: Some(&rgy_adapters::Adapters),
                },
                limit.saturating_sub(files_shown),
            );
            files_shown += res.files.len();
            if let Some(why) = &res.error {
                lost = Some(why.clone());
            }
            // The records that cannot be one line: the line is one path,
            // and a path that carries a line break is not one line.
            let mut dropped = 0usize;
            for f in &res.files {
                let path = rgy_engine::console::spell(&f.path);
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
        if let Some(why) = lost {
            eprintln!("searchctl: {why}");
            std::process::exit(1);
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

    let mut out = Vec::new();
    let root_disp = root.trim_end_matches(['/', '\\']);
    // The paths that cannot be one line: the line is one path, and a
    // path that carries a line break is not one line.
    let mut dropped = 0usize;
    for h in &hits {
        let rel = hit_rel(&searcher, h, &mut out);
        // `Index::path` reconstructs from the root's children; show the
        // searched tree so a hit is clickable.
        let path = hit_path(&rel, root_disp);
        // The index says which of its hits are folders, and the path does
        // not: a folder's path is spelled like a file's.
        let is_dir = is_folder(searcher.live, h);
        // The field is the wire's own, and it is there only for a folder.
        let folder = folder_field(is_dir);
        if json {
            let esc = rgy_proto::escape_json_string(&path);
            // The characters the query matched: the span the score came
            // from. With more than one alternative a hit does not say which
            // alternative it came from, so the span would be a guess and it
            // is left out.
            let matched = match alts.alts.as_slice() {
                [one] => {
                    let name = rel.rsplit(['/', '\\']).next().unwrap_or(&rel);
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
                    "{{\"path\":\"{esc}\",\"score\":{},\"match\":[{m}]{folder}}}",
                    h.score()
                ),
                None => {
                    rgy_engine::outln!("{{\"path\":\"{esc}\",\"score\":{}{folder}}}", h.score())
                }
            }
        } else if rgy_pick::carries_line(path.as_bytes()) {
            // The folder's path, spelled the way the platform spells a
            // folder: the line is a path, and it says what it is.
            rgy_engine::outln!("{}", marked(&path, is_dir));
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
    if let Some(why) = local_name_note(hits.len(), limit) {
        // The local route says what the daemon's route says: the answer is
        // a page of a longer list, or it is the whole list, and the reader
        // has to know which. The rule is the protocol's — the one every
        // producer of a name answer follows — and so is the sentence, so
        // the same query asked of the daemon and of the local index says
        // the same thing about the same answer.
        eprintln!("searchctl: {why}");
    }
    if hits.is_empty() {
        // rg convention: 0 = matches, 1 = none, 2 = error.
        std::process::exit(1);
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

    /// The daemon's names are printed the way the local names are.
    ///
    /// The flag changes where the answer comes from, not what it looks
    /// like: a path per line, and the same JSON object with the score.
    #[test]
    fn the_daemon_names_are_printed_the_way_the_local_names_are() {
        let hits = vec![rgy_proto::Hit::new("C:/a/hello.txt".into(), 12)];
        let printed = daemon_name_lines(&hits, false);
        assert_eq!(printed.lines, vec!["C:/a/hello.txt".to_string()]);
        assert_eq!(printed.found, 1);
        assert_eq!(printed.dropped, 0);

        let printed = daemon_name_lines(&hits, true);
        assert_eq!(
            printed.lines,
            vec![r#"{"path":"C:/a/hello.txt","score":12}"#.to_string()]
        );
    }

    /// A folder is printed as a folder.
    ///
    /// The daemon says which of its hits are folders and the path does
    /// not: a folder's path is spelled like a file's. The list prints the
    /// folder's path the way the platform spells a folder — with the
    /// separator at the end — so the line says what it is and stays a
    /// path the user can open.
    #[test]
    fn a_folder_is_printed_as_a_folder() {
        let hits = vec![
            rgy_proto::Hit::new("C:/a/docs".into(), 12).with_folder(true),
            rgy_proto::Hit::new("C:/a/notes.txt".into(), 9),
        ];
        let printed = daemon_name_lines(&hits, false);
        assert_eq!(
            printed.lines,
            vec![
                format!("C:/a/docs{}", std::path::MAIN_SEPARATOR),
                "C:/a/notes.txt".to_string(),
            ]
        );
    }

    /// The JSON says which hits are folders.
    ///
    /// The field is the wire's own spelling, and it is additive: it is
    /// there only when the hit is a folder, so a reader that does not
    /// know it reads a hit without it as a file. The path in the JSON
    /// stays the path — the field is what says the hit is a folder.
    #[test]
    fn the_json_says_which_hits_are_folders() {
        let hits = vec![
            rgy_proto::Hit::new("C:/a/docs".into(), 12).with_folder(true),
            rgy_proto::Hit::new("C:/a/notes.txt".into(), 9),
        ];
        let printed = daemon_name_lines(&hits, true);
        assert_eq!(
            printed.lines,
            vec![
                r#"{"path":"C:/a/docs","score":12,"folder":true}"#.to_string(),
                r#"{"path":"C:/a/notes.txt","score":9}"#.to_string(),
            ]
        );
    }

    /// The characters the query matched travel with the hit.
    ///
    /// The span is what lets an interface say *which* part of the name
    /// matched. The daemon leaves it empty when it could not say, and the
    /// JSON then carries the score alone: a span that guessed would be a
    /// lie in the place the user looks first.
    #[test]
    fn the_daemon_json_carries_the_matched_characters() {
        let mut hit = rgy_proto::Hit::new("C:/a/hello.txt".into(), 12);
        hit.span = vec![0, 1, 2];
        let printed = daemon_name_lines(&[hit], true);
        assert_eq!(
            printed.lines,
            vec![r#"{"path":"C:/a/hello.txt","score":12,"match":[0,1,2]}"#.to_string()]
        );
    }

    /// A path that cannot be one line is left out, and counted.
    ///
    /// Half of a path is a path that does not exist, so the record is
    /// dropped rather than printed in two pieces — and the count says so
    /// on stderr. The JSON carries it: the escape is what a line cannot
    /// do.
    #[test]
    fn a_path_that_cannot_be_one_line_is_left_out_and_counted() {
        let hits = vec![rgy_proto::Hit::new("C:/a/one\ntwo.txt".into(), 3)];
        let printed = daemon_name_lines(&hits, false);
        assert!(
            printed.lines.is_empty(),
            "the path is not one line: {printed:?}"
        );
        assert_eq!(printed.found, 1);
        assert_eq!(printed.dropped, 1);

        let printed = daemon_name_lines(&hits, true);
        assert_eq!(printed.dropped, 0);
        assert_eq!(printed.lines.len(), 1);
    }

    /// The daemon's content answer is rg's shape.
    ///
    /// `path:line:text` for the plain output, and the same three fields
    /// in one JSON object — the local path's shapes, so a script reads
    /// either answer the same way.
    #[test]
    fn the_daemon_content_is_printed_as_path_line_text() {
        let files = vec![rgy_proto::GrepFile {
            path: "C:/a/notes.md".into(),
            lines: vec![(7, "the zebra".into())],
        }];
        let printed = daemon_content_lines(&files, false);
        assert_eq!(printed.lines, vec!["C:/a/notes.md:7:the zebra".to_string()]);
        assert_eq!(printed.found, 1);
        assert_eq!(printed.dropped, 0);

        let printed = daemon_content_lines(&files, true);
        assert_eq!(
            printed.lines,
            vec![r#"{"path":"C:/a/notes.md","line":7,"text":"the zebra"}"#.to_string()]
        );
    }

    /// The content answer counts the records it found, not the lines it
    /// printed.
    ///
    /// The answer is empty when there were no records, and the exit
    /// status says so. A file with no line in it is a file that was
    /// found: it is not an answer that was empty.
    #[test]
    fn the_content_answer_counts_the_records_it_found() {
        let files = vec![rgy_proto::GrepFile {
            path: "C:/a/notes.md".into(),
            lines: Vec::new(),
        }];
        let printed = daemon_content_lines(&files, false);
        assert_eq!(printed.found, 0);
        assert!(printed.lines.is_empty(), "{printed:?}");
    }

    /// A tree with a folder and a file in it, and the hits over it.
    ///
    /// The index is the tree's own: an entry's kind byte is what says it
    /// is a folder, and the path says nothing.
    fn a_tree_with_a_folder_and_a_file() -> (Live, Vec<Hit>) {
        use rgy_discovery::walk::{KIND_FILE, Listing, NONE, RawEnt};
        let names = b"docsdocs.txt".to_vec();
        let ents = vec![
            RawEnt {
                name_off: 0,
                name_len: 4,
                kind: KIND_DIR,
                size: 0,
                mtime: 7,
                child: NONE,
            },
            RawEnt {
                name_off: 4,
                name_len: 8,
                kind: KIND_FILE,
                size: 3,
                mtime: 7,
                child: NONE,
            },
        ];
        let ls = vec![Listing { id: 0, names, ents }];
        let live = Live::with_root(Index::build(ls, 0, 1, b"/"), b"/");
        let searcher = Searcher { live: &live };
        let alts = Alternatives::parse("docs", "").expect("the query parses");
        let hits = searcher.search_alternatives(&alts.alts, 10);
        (live, hits)
    }

    /// The index says which of its hits are folders.
    ///
    /// The path does not: a folder's path is spelled like a file's, and
    /// the CLI prints paths. The kind byte is what says it, and the
    /// daemon's answer says it in its own field.
    #[test]
    fn the_index_says_which_hits_are_folders() {
        let (live, hits) = a_tree_with_a_folder_and_a_file();
        let folders = hits.iter().filter(|h| is_folder(&live, h)).count();
        let files = hits.iter().filter(|h| !is_folder(&live, h)).count();
        assert_eq!(folders, 1, "the folder is the folder");
        assert_eq!(files, 1, "the file is the file");
    }

    /// The picker is handed the bare path.
    ///
    /// The picker hands the line it chose back and the CLI opens it: a
    /// mark glued to the record would be a path that does not exist. The
    /// mark belongs to the line the list prints, and only there.
    #[test]
    fn the_picker_is_handed_the_bare_path() {
        let (live, hits) = a_tree_with_a_folder_and_a_file();
        let searcher = Searcher { live: &live };
        let folder = hits
            .iter()
            .find(|h| is_folder(&live, h))
            .expect("the folder is a hit");
        let mut buf = Vec::new();
        let rel = hit_rel(&searcher, folder, &mut buf);
        let record = hit_path(&rel, ".");
        // The record is the path, and the path is not the mark.
        assert!(!record.ends_with(std::path::MAIN_SEPARATOR), "{record}");
        assert!(record.ends_with("docs"), "{record}");
        // The line the list prints is the mark.
        assert!(marked(&record, true).ends_with(std::path::MAIN_SEPARATOR));
    }

    /// The daemon's name answer says whether it is whole.
    ///
    /// The daemon's answer carries the fact — whether it is whole — and the
    /// CLI says it: an answer the daemon says was cut at the limit is
    /// reported as cut, and one it says is whole is not reported at all.
    /// The words are the protocol's own, the sentence the protocol's client
    /// turns the flag into, so the CLI is not a second spelling of it.
    #[test]
    fn the_daemon_name_answer_is_reported_as_cut_when_the_daemon_says_so() {
        let cut = rgy_proto::SearchOutcome {
            complete: false,
            ..Default::default()
        };
        assert_eq!(
            daemon_name_note(&cut),
            Some(rgy_proto::client::ANSWER_STOPS_AT_THE_LIMIT),
            "an answer the daemon says was cut is reported as cut"
        );
        assert_eq!(
            cut.partial().as_deref(),
            daemon_name_note(&cut),
            "the CLI says what the protocol's client says"
        );

        let whole = rgy_proto::SearchOutcome {
            complete: true,
            ..Default::default()
        };
        assert_eq!(
            daemon_name_note(&whole),
            None,
            "an answer the daemon says is whole is not reported"
        );
    }

    /// The local answer says whether it is the whole answer.
    ///
    /// The tree has two entries that match "docs": a limit of one cuts the
    /// answer and the CLI has to say so, and a limit of two does not. The
    /// measure is the protocol's rule on the list the search returned —
    /// the same rule the daemon applies to the list it sends.
    #[test]
    fn the_local_answer_is_reported_as_cut_when_it_reached_the_limit() {
        let (live, hits) = a_tree_with_a_folder_and_a_file();
        let searcher = Searcher { live: &live };
        let alts = Alternatives::parse("docs", "").expect("the query parses");
        assert_eq!(hits.len(), 2, "the tree has two entries that match");

        let cut = searcher.search_alternatives(&alts.alts, 1);
        assert_eq!(cut.len(), 1, "the answer is a page");
        assert_eq!(
            local_name_note(cut.len(), 1),
            Some(rgy_proto::client::ANSWER_STOPS_AT_THE_LIMIT),
            "an answer that reached the limit is reported as cut"
        );

        let whole = searcher.search_alternatives(&alts.alts, 10);
        assert_eq!(whole.len(), 2, "the answer is the whole list");
        assert_eq!(
            local_name_note(whole.len(), 10),
            None,
            "an answer under the limit is not reported as cut"
        );
    }

    /// The two routes say the same words about the same answer.
    ///
    /// The daemon's answer carries the fact and the command line's own
    /// search decides it with the protocol's rule; the sentence is the
    /// protocol's in both, so a flag that changes where the answer comes
    /// from does not change what the answer says about itself.
    #[test]
    fn the_two_routes_say_the_same_words() {
        let outcome = rgy_proto::SearchOutcome {
            complete: false,
            ..Default::default()
        };
        assert_eq!(
            daemon_name_note(&outcome),
            local_name_note(10, 10),
            "the daemon's answer and the command line's own answer say the same thing"
        );
        let whole = rgy_proto::SearchOutcome {
            complete: true,
            ..Default::default()
        };
        assert_eq!(
            daemon_name_note(&whole),
            local_name_note(9, 10),
            "and an answer under the limit is not reported by either"
        );
    }

    /// The limit is the protocol's cap, on every route.
    ///
    /// The daemon's reader clamps the request to the cap, so a local route
    /// that honoured a bigger one would answer a question the daemon
    /// answers differently: the same flag, the same tool, two answers.
    #[test]
    fn the_limit_is_clamped_to_the_protocol_cap() {
        assert_eq!(clamp_limit(50), 50, "a limit under the cap is the limit");
        assert_eq!(
            clamp_limit(rgy_proto::MAX_LIMIT),
            rgy_proto::MAX_LIMIT,
            "the cap is the cap"
        );
        assert_eq!(
            clamp_limit(rgy_proto::MAX_LIMIT + 1),
            rgy_proto::MAX_LIMIT,
            "a limit over the cap is the cap"
        );
        assert_eq!(
            clamp_limit(usize::MAX),
            rgy_proto::MAX_LIMIT,
            "a limit the flag cannot hold is the cap"
        );
    }
}
