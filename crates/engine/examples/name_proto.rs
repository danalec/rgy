// SPDX-License-Identifier: Apache-2.0

//! name_proto -- the name-tier protocol harness behind the paper's
//! "an instance at filesystem scale: name search" case study.
//!
//! Three arms over one materialized corpus, so the amortisation boundary
//! of the content tier can be measured for names:
//!
//! * `materialize <dir> <n>` -- write the deterministic `name_bench`
//!   corpus as a real tree (same generator, same names, so the two arms
//!   see identical bytes);
//! * `build <dir> <index> <threads>` -- walk the tree, build the name
//!   index, persist it. This is the `T_build` of the break-even ratio.
//! * `q-index <dir> <index> <query>` -- load the persisted index and
//!   answer one query, printing only the match count. The per-query cost
//!   of the prepared arm, setup included.
//! * `q-walk <dir> <threads> <query>` -- walk the tree and match names
//!   with the reference matcher below, printing only the match count.
//!   The per-query cost of the direct arm: enumeration plus matching, no
//!   index built (what `fd` / `dir /s` do).
//! * `floor` -- parse and exit, nothing else. The process floor the
//!   driver subtracts context from, never numbers from another run.
//!
//! Counts, not times, are this binary's own contract: the driver runs the
//! two query arms over the same query and suppresses the pair unless the
//! counts agree exactly (the paper's count gate). Timing is the shell's
//! wall clock around the whole invocation -- setup, search and teardown
//! -- so no stage timer here adjudicates anything.
//!
//! The reference matcher covers the query forms the case study uses:
//! `'x` exact, `^x` prefix, `x$` suffix, bare `x` substring, all
//! case-insensitive -- the literal vocabulary the name index agrees with
//! on this corpus, which the count gate verifies rather than assumes.
//!
//! Run: `cargo run --release -p rgy-engine --example name_proto -- <mode> ...`

use std::path::Path;
use std::time::Instant;

use rgy_discovery::walk;
use rgy_name_core::live::Live;
use rgy_name_core::name_index::Index;
use rgy_name_core::name_query::{Query, Searcher};
use rgy_name_core::persist;

/// The match count is reported unsaturated: the interactive limit would
/// pin broad queries at the cap and the count gate would then compare the
/// cap with itself.
const COUNT_LIMIT: usize = 10_000_000;

fn materialize(dir: &Path, n: usize, threads: usize) {
    let dirs = (n / 200).clamp(8, 2000);
    let per = n.div_ceil(dirs).max(1);
    std::thread::scope(|s| {
        let chunk = dirs.div_ceil(threads.max(1));
        for t in 0..threads.max(1) {
            let (d0, d1) = (t * chunk, ((t + 1) * chunk).min(dirs));
            if d0 >= d1 {
                break;
            }
            s.spawn(move || {
                for d in d0..d1 {
                    let sub = dir.join(format!("dir_{d:04}"));
                    std::fs::create_dir_all(&sub).expect("materialize: create dir");
                    for i in 0..per {
                        // The name formula of `name_bench`'s
                        // `synthetic_listings`, copied exactly: both
                        // harnesses must generate the same names.
                        let name = if i % 7 == 0 {
                            format!("Report_{:05}.PDF", d * per + i)
                        } else if i % 3 == 0 {
                            format!("src_{:05}-module.rs", d * per + i)
                        } else {
                            format!("file_{:05}.dat", d * per + i)
                        };
                        std::fs::write(sub.join(name), b"x").expect("materialize: write file");
                    }
                }
            });
        }
    });
}

/// The reference matcher for the direct arm: the literal vocabulary above,
/// case-insensitive. The count gate against `q-index` is what licenses it.
fn matches_ref(q: &str, name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    let q = q.to_ascii_lowercase();
    if let Some(x) = q.strip_prefix('\'') {
        return n == x;
    }
    if let Some(x) = q.strip_prefix('^') {
        return n.starts_with(x);
    }
    if let Some(x) = q.strip_suffix('$') {
        return n.ends_with(x);
    }
    n.contains(&q)
}

fn usage() -> ! {
    eprintln!(
        "name_proto materialize <dir> <n> <threads>\n\
         name_proto build <dir> <index> <threads>\n\
         name_proto q-index <dir> <index> <query>\n\
         name_proto q-walk <dir> <threads> <query>\n\
         name_proto floor"
    );
    std::process::exit(2);
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(mode) = args.first().map(String::as_str) else {
        usage();
    };
    match mode {
        "floor" => {}
        "materialize" => {
            let (Some(dir), Some(n), Some(threads)) = (args.get(1), args.get(2), args.get(3))
            else {
                usage();
            };
            let n: usize = n.parse().expect("materialize: n");
            let threads: usize = threads.parse().expect("materialize: threads");
            materialize(Path::new(dir), n, threads);
        }
        "build" => {
            let (Some(dir), Some(index), Some(threads)) = (args.get(1), args.get(2), args.get(3))
            else {
                usage();
            };
            let threads: usize = threads.parse().expect("build: threads");
            let root = Path::new(dir);
            let t0 = Instant::now();
            let listings = walk::scan(dir.as_bytes(), threads);
            let mut idx = Index::build(listings, 0, 1_700_000_000, b"/");
            persist::save(&mut idx, root, Path::new(index)).expect("build: persist");
            eprintln!("name_proto: built in {:.3} s", t0.elapsed().as_secs_f64());
        }
        "q-index" => {
            let (Some(dir), Some(index), Some(q)) = (args.get(1), args.get(2), args.get(3)) else {
                usage();
            };
            let root = Path::new(dir);
            let idx = persist::load_for(root, Path::new(index), None).expect("q-index: load");
            let live = Live::new(idx);
            let searcher = Searcher { live: &live };
            let mut parsed = Query::parse(q, "").expect("q-index: parse");
            parsed.limit = COUNT_LIMIT;
            println!("{}", searcher.search(&parsed).len());
        }
        "q-walk" => {
            let (Some(dir), Some(threads), Some(q)) = (args.get(1), args.get(2), args.get(3))
            else {
                usage();
            };
            let threads: usize = threads.parse().expect("q-walk: threads");
            let listings = walk::scan(dir.as_bytes(), threads);
            let mut count = 0usize;
            for ls in &listings {
                for ent in &ls.ents {
                    let name = &ls.names
                        [ent.name_off as usize..(ent.name_off as usize + ent.name_len as usize)];
                    if let Ok(name) = std::str::from_utf8(name)
                        && matches_ref(q, name)
                    {
                        count += 1;
                    }
                }
            }
            println!("{count}");
        }
        _ => usage(),
    }
}
