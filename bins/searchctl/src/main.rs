//! searchctl — the merged name-search CLI.
//!
//! fsearch one-shot ergonomics + zen-rs's filter vocabulary (see
//! `name_core::name_query`), over the v2 name index. It enumerates a
//! directory tree (`--root`, portable walk / no privileges); the Windows
//! MFT fast path (`--volume`) and content `grep:` land with the engine and
//! the content layer.
//!
//! With `--index`, a built index is reused across invocations: the walk and
//! the build happen once, and later queries pay an mmap instead. The portable
//! walk has no change feed, so freshness is explicit — `--reindex` forces a
//! rebuild and `--max-age` expires one, and the root is pinned in a sidecar
//! so an index built for another tree can never be loaded (see
//! `name_core::persist`).
//!
//! rg-compatible exit codes: 0 = matches, 1 = no matches, 2 = error.
//! `--json` prints one object per hit. Parse warnings (unknown `type:`,
//! bad `re:`) go to stderr as `warning:` lines and the affected alternative
//! matches nothing — fail closed, never silent.

use std::time::{Duration, Instant};

use discovery::{PortableSource, interrupt};
use name_core::content::{self, Grep};
use name_core::live::Live;
use name_core::name_index::Index;
use name_core::name_query::{Alternatives, Hit, Searcher, merge_hits};
use name_core::persist;

#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

/// How often an index build reports what it has covered. Fast enough to look
/// alive, slow enough that the lines stay readable.
const REPORT_EVERY: Duration = Duration::from_millis(500);

fn usage() -> ! {
    eprintln!(
        "searchctl [options] <query>\n\n\
         options:\n  \
           --root <dir>       tree to index (default: .)\n  \
           --limit <n>        max hits (default: 50)\n  \
           --json             one JSON object per hit\n  \
           --count            print the match count, not the ranked hits\n  \
           --explain           report the query plan on stderr\n  \
           --index <file>     reuse/persist an index (default: cache dir)\n  \
           --reindex          rebuild even if a usable index exists\n  \
           --max-age <dur>    rebuild an index older than 30m/2h/7d\n\n\
         query: fsearch tokens (bare=fuzzy, 'x exact, ^x prefix, x$ suffix,\n\
         !x negate) + zen-rs filters (ext: dir: type: size: re:),\n\
         | alternates, * and ? glob the whole path."
    );
    std::process::exit(2);
}

/// Parse `30m` / `2h` / `7d` / `90s` / `1w` into a duration. Bare numbers are
/// seconds, so `--max-age 0` means "expire anything a second old".
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

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--root" => {
                i += 1;
                root = args.get(i).cloned().unwrap_or_else(|| usage());
            }
            "--limit" => {
                i += 1;
                limit = args
                    .get(i)
                    .and_then(|s| s.parse().ok())
                    .unwrap_or_else(|| usage());
            }
            "--index" => {
                i += 1;
                index = Some(args.get(i).cloned().unwrap_or_else(|| usage()));
            }
            "--reindex" => reindex = true,
            "--count" => count = true,
            "--explain" => explain = true,
            "--max-age" => {
                i += 1;
                max_age = Some(
                    args.get(i)
                        .and_then(|s| parse_age(s))
                        .unwrap_or_else(|| usage()),
                );
            }
            "--json" => json = true,
            "-h" | "--help" => usage(),
            a if a.starts_with('-') => {
                eprintln!("searchctl: unknown option {a}");
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
        usage();
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
    };
    if let Err(e) = run(&opts, &query) {
        eprintln!("searchctl: {e}");
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
                eprintln!(
                    "searchctl: reusing index {} ({} entries)",
                    index_path.display(),
                    idx.n
                );
                return Ok(idx);
            }
            Err(why) => eprintln!("searchctl: rebuilding — {why}"),
        }
    }

    // Portable walk: no privileges, any platform.
    let source = PortableSource::new(
        opts.root.as_bytes().to_vec(),
        PortableSource::DEFAULT_THREADS,
    );
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
                eprintln!("searchctl: scanning… {}", interrupt::line(p));
            }
        })
        .map_err(|_| "scan cancelled — no index written".to_string())?;
    let idx = Index::build(listings, 0, now_secs(), opts.root.as_bytes());
    match persist::save(&idx, &root_path, &index_path) {
        Ok(saved) => eprintln!(
            "searchctl: indexed {} entries → {}",
            idx.n,
            saved.index.display()
        ),
        // A cache miss must not fail the query: the index is built and
        // usable, it just will not be there next time.
        Err(e) => eprintln!(
            "searchctl: could not save index to {}: {e}",
            index_path.display()
        ),
    }
    Ok(idx)
}

fn run(opts: &Opts, query: &str) -> Result<(), String> {
    let root = &opts.root;
    let limit = opts.limit;
    let json = opts.json;
    let count = opts.count;
    let idx = obtain_index(opts)?;
    let live = Live::new(idx);
    let searcher = Searcher { live: &live };

    // Merged query syntax: `|` alternatives, fail-closed filters.
    let home = std::env::var("USERPROFILE")
        .or_else(|_| std::env::var("HOME"))
        .unwrap_or_default();
    let alts = Alternatives::parse(query, &home)?;
    for w in &alts.warnings {
        eprintln!("warning: {w}");
    }

    // `grep:`/`regex:`/`sym:` content search (fsearch model: the name
    // tokens and filters narrow WHICH files are read; the rgx engine
    // confirms each one). rg-style `path:line:text` output.
    if alts.alts.iter().any(|a| a.grep.is_some()) {
        let mut shown = 0usize;
        for alt in &alts.alts {
            let Some(pat) = &alt.grep else { continue };
            let g = Grep::new(pat, alt.grep_mode)?;
            let paths = content::scan_paths(root.as_bytes(), searcher.live, alt.clone_for_scan());
            let res = content::verify(&g, &paths, limit);
            for f in &res.files {
                let path = String::from_utf8_lossy(&f.path);
                for (ln, text) in &f.lines {
                    shown += 1;
                    if json {
                        let esc = |s: &str| s.replace('\\', "\\\\").replace('"', "\\\"");
                        println!(
                            "{{\"path\":\"{}\",\"line\":{ln},\"text\":\"{}\"}}",
                            esc(&path),
                            esc(text)
                        );
                    } else {
                        println!("{path}:{ln}:{text}");
                    }
                }
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
        eprintln!(
            "searchctl: {} entries indexed, {total} matches",
            searcher.live.base.n
        );
        for line in &summary {
            eprintln!("searchctl: {line}");
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
            println!("{{\"count\":{n}}}");
        } else {
            println!("{n}");
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
            let esc = path.replace('\\', "\\\\").replace('"', "\\\"");
            println!("{{\"path\":\"{esc}\",\"score\":{}}}", h.score);
        } else {
            println!("{path}");
        }
        out.clear();
    }
    Ok(())
}

fn now_secs() -> u32 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as u32)
}
