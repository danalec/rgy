//! searchctl — the merged name-search CLI.
//!
//! fsearch one-shot ergonomics + everything-rs's filter vocabulary (see
//! `name_core::name_query`), over the v2 name index. Today it enumerates a
//! directory tree (`--root`, portable walk / no privileges); the Windows
//! MFT fast path (`--volume`) and content `grep:` land with the engine and
//! the content layer.
//!
//! rg-compatible exit codes: 0 = matches, 1 = no matches, 2 = error.
//! `--json` prints one object per hit. Parse warnings (unknown `type:`,
//! bad `re:`) go to stderr as `warning:` lines and the affected alternative
//! matches nothing — fail closed, never silent.

use discovery::{PortableSource, Source};
use name_core::content::{self, Grep};
use name_core::live::Live;
use name_core::name_index::Index;
use name_core::name_query::{Alternatives, Hit, Query, Searcher};

#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

fn usage() -> ! {
    eprintln!(
        "searchctl [options] <query>\n\n\
         options:\n  \
           --root <dir>       tree to index (default: .)\n  \
           --limit <n>        max hits (default: 50)\n  \
           --json             one JSON object per hit\n\n\
         query: fsearch tokens (bare=fuzzy, 'x exact, ^x prefix, x$ suffix,\n\
         !x negate) + everything-rs filters (ext: dir: type: size: re:),\n\
         | alternates, * and ? glob the whole path."
    );
    std::process::exit(2);
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut root = String::from(".");
    let mut limit = 50usize;
    let mut json = false;
    let mut query = String::new();

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

    if let Err(e) = run(&root, &query, limit, json) {
        eprintln!("searchctl: {e}");
        std::process::exit(2);
    }
}

fn run(root: &str, query: &str, limit: usize, json: bool) -> Result<(), String> {
    // Index the tree. Portable walk: no privileges, any platform.
    let source = PortableSource {
        root: root.as_bytes().to_vec(),
    };
    let listings = source.enumerate()?;
    let idx = Index::build(listings, 0, now_secs(), root.as_bytes());
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
            let paths = content::scan_paths(root.as_bytes(), &searcher.live, alt.clone_for_scan());
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

    // Score every alternative; a hit is the best score over them.
    let mut hits: Vec<Hit> = Vec::new();
    for alt in &alts.alts {
        let mut q: Query = alt.clone();
        q.limit = limit;
        for h in searcher.search(&q) {
            match hits.iter_mut().find(|e| e.idx == h.idx) {
                Some(e) if e.score < h.score => *e = h,
                Some(_) => {}
                None => hits.push(h),
            }
        }
    }
    hits.sort_by(|a, b| b.score.cmp(&a.score).then(a.idx.cmp(&b.idx)));
    hits.truncate(limit);

    if hits.is_empty() {
        // rg convention: 0 = matches, 1 = none, 2 = error.
        std::process::exit(1);
    }
    let mut out = Vec::new();
    let root_disp = root.trim_end_matches(['/', '\\']);
    for h in &hits {
        searcher.live.base.path(h.idx as usize, &mut out);
        let rel = String::from_utf8_lossy(&out);
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
