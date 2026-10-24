// SPDX-License-Identifier: Apache-2.0

//! searchd — the merged search daemon (`docs/design.md` §2.5).
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
//! the root so another tree's index is refused rather than loaded. An index
//! with no `--max-age` of its own expires after [`persist::DEFAULT_MAX_AGE`]
//! — a saved index is a snapshot, and a snapshot of any age served without
//! saying so is how a search ends up missing a file created yesterday.
//!
//! Where the OS keeps a change journal (Windows), the index carries the
//! position it is current as of, and a restart resumes the journal there:
//! the changes that happened while the daemon was off are replayed instead of
//! being missed. Where there is no journal, the tree is walked again — and
//! the status answer says which of the two happened, because "replayed" and
//! "relisted" are the difference between an index that caught up and one that
//! did not.

use std::io::{BufRead, Write};
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use rgy_discovery::{PortableSource, interrupt, walk::Cancelled};
use rgy_engine::{keep_warm, warm_stop};
use rgy_name_core::content::{self, Grep};
use rgy_name_core::live::Live;
use rgy_name_core::name_index::Index;
use rgy_name_core::name_query::{Alternatives, Query, Searcher};
use rgy_name_core::persist;
use rgy_proto::{CatchUp, GrepFile, Hit as WireHit, Request, Response};

#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

/// How often the startup walk reports what it has covered.
const REPORT_EVERY: Duration = Duration::from_millis(500);

/// The two values the catch-up answer carries, as the atomic holds them.
const CATCH_UP_REPLAYED: u8 = 0;
const CATCH_UP_RELISTED: u8 = 1;

struct Ctx {
    root: Vec<u8>,
    home: String,
    tv_dir: Option<std::path::PathBuf>,
    /// The live name index. `RwLock` because a live-update feed (the `Watch`
    /// thread) applies changes to it while query threads read it: readers
    /// take `read()`, the updater `write()`. Searchers hold a read guard for
    /// the length of one query, so an apply waits for in-flight answers and
    /// never lands mid-query.
    ///
    /// The `Watch` thread applies the feed's changes to this index. Its
    /// shape, so the next hand does not re-derive it:
    /// - A platform `rgy_discovery::Watch` (InotifyWatch / UsnWatch / fsevents)
    ///   is constructed on a background thread and polled with `next_batch`;
    ///   `Live::run_watch_feed` is the loop body (already tested), so this is
    ///   just the factory + `spawn`.
    /// - Each `UsnRecord` names the changed path in `name`, and
    ///   `parent_reference` is the file reference of the folder the change
    ///   happened in. So the folder to re-list is the parent of `name`,
    ///   applied via `Live::apply_dir(parent, false)`; `ChangeBatch::Rescan`
    ///   becomes `apply_dir(root, true)`.
    /// - The Windows feed rebuilds the path from the MFT: the journal names
    ///   a file and its parent's file reference, and nothing else, so the
    ///   record's name is a path the disk can open.
    /// - The separators are normalized before `apply_dir`: the record's
    ///   name arrives in the platform's separator (`\` on Windows) while
    ///   this index stores `/`-separated paths (`openable`/`join`/
    ///   `dir_prefix` all push `/`). Without that, the overlay's keys land
    ///   in a different spelling than `wanted`'s `dir_prefix` and the
    ///   change is silently missed -- the same class of path-space bug
    ///   `wanted`/`wants` already crossed once.
    /// - The Windows factory is the heavy one: `UsnWatch::new(Indexer)` and
    ///   `Indexer::new(&Volume)` borrow up a chain (Volume ->
    ///   Indexer<'a> -> UsnWatch<'a>), so a `'static` thread either holds all
    ///   three or `Box::leak`s the Volume/Indexer for the daemon's life. The
    ///   Volume open + USN journal setup is OS-specific and hard to test here;
    ///   `discovery-linux`'s `InotifyWatch::new(&root)` is the simple case.
    live: std::sync::RwLock<Live>,
    /// How this process caught up, as the status answer reports it.
    ///
    /// The platform's capability (`rgy_discovery_dispatch::catch_up_for`) says
    /// what the OS *could* do; this says what this process *did*, which is
    /// the only claim the answer can honestly make — and nothing replays
    /// unless a watermark was saved and the journal still covers it. The
    /// watch thread corrects it to "relisted" when the feed loses its
    /// history, because then nothing was replayed.
    catch_up: std::sync::atomic::AtomicU8,
}

impl Ctx {
    /// How the index caught up after the last restart
    /// (`docs/design.md` §3, rule 5 — "live-feed honesty").
    ///
    /// What this process did: the change journal was resumed at the saved
    /// watermark and the backlog applied (`Replayed`), or it was not
    /// (`Relisted` — the tree was walked, or the platform keeps no journal
    /// to resume).
    fn catch_up(&self) -> CatchUp {
        match self.catch_up.load(Ordering::Relaxed) {
            CATCH_UP_REPLAYED => CatchUp::Replayed,
            _ => CatchUp::Relisted,
        }
    }

    /// Record that the feed lost its history: nothing was replayed, so the
    /// tree was walked again and the answer has to change with it.
    fn set_relisted(&self) {
        self.catch_up.store(CATCH_UP_RELISTED, Ordering::Relaxed);
    }
}

/// Print usage and leave. `code` is the exit status: 0 when the user asked
/// for help, 2 for the rg convention's "bad invocation".
fn usage(code: i32) -> ! {
    rgy_engine::errln!(
        "searchd [--root <dir>] [--index <file>] [--reindex] [--max-age <dur>]\n         [--tv-dir <store>] [--listen stdio|uds:<path>|pipe:<name>] [--serve]\n\n\
         proto framing (default): one JSON request per line\n  \
           {{\"op\":\"search\",\"q\":\"...\",\"limit\":50}}\n  \
           {{\"op\":\"grep\",\"q\":\"...\"}}\n  \
           {{\"op\":\"status\"}}\n  \
         --serve: rgx compat — a JSON argv-array per line, answers framed\n  \
           by {{\"rgx\":\"begin\"}}/{{\"rgx\":\"end\"}} with rg --json match lines."
    );
    std::process::exit(code);
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
    // The console decides how the UTF-8 bytes are decoded, so it is put
    // into UTF-8 before anything is printed (parity row 4.7). The input
    // side too: in stdio mode the requests are typed by a person, and the
    // console encodes the keystrokes with the input code page. A
    // redirected stdout keeps its raw bytes -- a machine consumer wants
    // UTF-8, not a console's idea of it.
    rgy_engine::console::init_with_input();

    // Which view of an adapter-claimed file a search reads. rgy answers to
    // RGY_ADAPTER_VIEW and falls back to rgx's RGX_ADAPTER_VIEW, so a shell
    // configured for rgx is configured for rgy too.
    rgy_engine::adapter_view::apply();

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
                root = args.get(i).cloned().unwrap_or_else(|| usage(2));
            }
            "--index" => {
                i += 1;
                index = Some(args.get(i).cloned().unwrap_or_else(|| usage(2)));
            }
            "--reindex" => reindex = true,
            "--max-age" => {
                i += 1;
                max_age = Some(
                    args.get(i)
                        .and_then(|s| parse_age(s))
                        .unwrap_or_else(|| usage(2)),
                );
            }
            "--tv-dir" => {
                i += 1;
                tv_dir = Some(std::path::PathBuf::from(
                    args.get(i).cloned().unwrap_or_else(|| usage(2)),
                ));
            }
            "--listen" => {
                i += 1;
                listen = args.get(i).cloned().unwrap_or_else(|| usage(2));
            }
            "--serve" => serve = true,
            "-h" | "--help" => usage(0),
            other => {
                rgy_engine::errln!("searchd: unknown argument {other}");
                std::process::exit(2);
            }
        }
        i += 1;
    }

    // One spelling of the root, resolved once. The change feeds report
    // absolute paths (the macOS stream always does), and the live overlay
    // and the name index have to talk about the same file: one spelling of
    // the root is what lets them. A root that reaches the tree through a
    // symlink is resolved too, because the kernel reports the resolved
    // path.
    let root = absolute_root(&root);

    let startup = open_index(&root, index.as_deref(), reindex, max_age);
    let ctx = std::sync::Arc::new(Ctx {
        root: root.as_bytes().to_vec(),
        home: std::env::var("USERPROFILE")
            .or_else(|_| std::env::var("HOME"))
            .unwrap_or_default(),
        tv_dir,
        live: std::sync::RwLock::new(Live::with_root(startup.index, root.as_bytes())),
        catch_up: std::sync::atomic::AtomicU8::new(match startup.catch_up {
            CatchUp::Replayed => CATCH_UP_REPLAYED,
            CatchUp::Relisted => CATCH_UP_RELISTED,
        }),
    });

    // Keep the index fresh as the tree changes (the Watch thread); queries
    // keep answering on the last state while an apply is in flight.
    spawn_watch(&ctx, &startup.journal, startup.watermark);

    // Give the query cache back while nobody is typing (the trim thread).
    spawn_trim(&ctx);

    match listen.split_once(':') {
        None if listen == "stdio" => {
            let stdin = std::io::stdin();
            run_stream(stdin.lock(), rgy_engine::console::stdout(), &ctx, serve);
        }
        Some(("uds", path)) => serve_uds(path, &ctx, serve),
        Some(("pipe", name)) => serve_pipe(name, &ctx, serve),
        _ => usage(2),
    }
}

/// The root as the disk and the feeds spell it: absolute, symlinks
/// resolved.
///
/// The Windows canonical form carries a `\\?\` prefix. That prefix is not
/// what the user wrote and not what an answer should carry, so it comes off
/// again — except on a UNC path, where it is the only thing that says the
/// path is a UNC path.
fn absolute_root(root: &str) -> String {
    let path = std::path::Path::new(root);
    let absolute = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    let resolved = std::fs::canonicalize(&absolute).unwrap_or(absolute);
    let text = resolved.as_os_str().to_string_lossy().into_owned();
    if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
        return format!(r"\\{rest}");
    }
    text.strip_prefix(r"\\?\").unwrap_or(&text).to_string()
}

/// What the startup decided: the index to answer from, how this process
/// caught up with the disk, and where the change feed has to pick up.
struct Ready {
    /// The index to serve.
    index: Index,
    /// How this process caught up.
    catch_up: CatchUp,
    /// The platform's change journal, when there is one.
    journal: Journal,
    /// The change-feed position the index is current as of (0: none).
    ///
    /// On Windows this is the USN journal position the walk started at, and
    /// the position the next start resumes the journal from. It travels with
    /// the index in its header (`event_id`), so it is read back from disk
    /// rather than guessed.
    watermark: u64,
}

/// The platform's change journal, as the daemon needs it: where it is now,
/// how far back it reaches, and which journal it is.
///
/// Windows has one (the USN journal). The other platforms keep no history
/// to replay, and this says so rather than leaving the caller to guess from
/// a `cfg`: a daemon that cannot resume a journal has to walk the tree, and
/// has to say that out loud.
#[derive(Clone, Copy)]
struct Journal {
    /// The journal's identity (0: no journal). A journal that was deleted
    /// and recreated gets a new identity, and the records of the old one
    /// are not the records of the new one.
    ///
    /// Only the Windows feed reads it — it is what tells a recreated
    /// journal from the one the index was built against — so on the
    /// platforms without a journal the field is carried and never read.
    #[cfg_attr(not(windows), allow(dead_code))]
    id: u64,
    /// The position the journal is at now (0: no journal).
    head: u64,
    /// The oldest position the journal still covers (0: no journal).
    lowest: u64,
}

impl Journal {
    /// Does the journal still cover `position`?
    ///
    /// A journal that wrapped (its oldest record moved past the position)
    /// or was recreated (the positions went back to the beginning) cannot:
    /// the history between the position and the oldest record is gone, and
    /// a resume from there would skip it without saying so — the exact
    /// failure this row is about.
    fn covers(&self, position: u64) -> bool {
        self.head != 0 && position >= self.lowest && position <= self.head
    }

    /// What goes with an index this platform cannot bring up to date: a
    /// journal-less platform keeps no history, so the changes made since
    /// the index was built are not in it, and the reader has to be told.
    fn gap_note(&self) -> &'static str {
        if self.head == 0 {
            " — no change journal on this platform, so the changes made since then are not in it"
        } else {
            ""
        }
    }
}

#[cfg(windows)]
impl Journal {
    fn none() -> Journal {
        Journal {
            id: 0,
            head: 0,
            lowest: 0,
        }
    }

    /// The USN journal of the volume `root` is on, when this process can
    /// read it.
    ///
    /// The handle is opened and closed again here: the feed opens its own
    /// for the daemon's life, and a journal is a property of the volume, not
    /// of the handle.
    fn open(root: &[u8]) -> Journal {
        let Ok(path) = std::str::from_utf8(root) else {
            return Journal::none();
        };
        let Some(vol_root) = std::path::Path::new(path)
            .ancestors()
            .last()
            .and_then(|p| p.to_str())
        else {
            return Journal::none();
        };
        let Ok(volume) = rgy_discovery_windows::volume::Volume::open(vol_root) else {
            return Journal::none();
        };
        let Ok(j) = volume.query_usn_journal() else {
            return Journal::none();
        };
        Journal {
            id: j.usn_journal_id,
            head: j.next_usn.max(0) as u64,
            lowest: j.lowest_valid_usn.max(0) as u64,
        }
    }
}

#[cfg(not(windows))]
impl Journal {
    fn none() -> Journal {
        Journal {
            id: 0,
            head: 0,
            lowest: 0,
        }
    }

    /// No journal: inotify and kqueue keep no history to replay.
    fn open(_root: &[u8]) -> Journal {
        Journal::none()
    }
}

/// How old the index is, in seconds: the header's `synced_at` is the
/// wall-clock second the index was built at.
fn age_of(idx: &Index) -> u64 {
    (now_secs() as u64).saturating_sub(idx.synced_at as u64)
}

/// What the daemon does with a saved index the age policy accepted.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Startup {
    /// Reuse it and resume the change journal at the watermark: the changes
    /// that happened while the daemon was off are replayed.
    Replay(u64),
    /// Reuse it. There is no journal to replay the gap from, so the age
    /// policy is the only freshness rule there is — and it accepted this
    /// index.
    Reuse,
    /// Walk the tree again: the gap cannot be replayed, and an index that
    /// is missing the gap would answer wrongly.
    Rebuild,
}

/// The decision the startup makes about a saved index.
///
/// * the index carries no watermark → there is nothing to replay from;
/// * the platform has no journal to ask → the age policy decides, and it
///   already accepted this index;
/// * the journal covers the watermark → resume it there: the changes that
///   happened while the daemon was off are replayed;
/// * the journal does not cover it (it wrapped, or it was recreated) → the
///   gap cannot be replayed, so the tree is walked again.
fn startup_plan(watermark: u64, journal: &Journal) -> Startup {
    if watermark == 0 || journal.head == 0 {
        Startup::Reuse
    } else if journal.covers(watermark) {
        Startup::Replay(watermark)
    } else {
        Startup::Rebuild
    }
}

/// Load a usable index for this root, or walk and build one.
///
/// The startup path is where skipping the walk matters most: the daemon runs
/// exactly once per boot, so a saved index turns a full tree walk into an
/// mmap. A refusal (wrong root, expired, corrupt) always rebuilds and says so
/// rather than starting on a suspect index.
///
/// A saved index that the age policy accepts is reused only when the
/// platform's journal still covers the position it carries: then the feed
/// resumes there and the changes that happened while the daemon was off are
/// replayed. A journal that no longer covers it (it wrapped, or it was
/// recreated) cannot replay the gap, so the tree is walked again — and said
/// to be.
fn open_index(root: &str, index: Option<&str>, reindex: bool, max_age: Option<Duration>) -> Ready {
    let root_path = std::path::PathBuf::from(root);
    let index_path = match index {
        Some(p) => std::path::PathBuf::from(p),
        None => persist::default_index_path(&root_path),
    };
    let journal = Journal::open(root.as_bytes());

    if !reindex {
        match persist::load_for(&root_path, &index_path, max_age) {
            Ok(idx) => {
                // The index carries the change-feed position it is current as
                // of, in its header. It is the point the next start resumes
                // the journal from.
                let watermark = idx.event_id;
                match startup_plan(watermark, &journal) {
                    Startup::Replay(at) => {
                        // The journal is resumed at the watermark the index
                        // carries, and the backlog it delivers is applied to
                        // the live overlay. Each record's path is rebuilt
                        // from the MFT, so the folder to re-list is known.
                        // The age limit still applies to a resumed index:
                        // the journal says what changed, but the index is
                        // only as current as the walk that built it.
                        rgy_engine::errln!(
                            "searchd: reusing index {} ({} entries, built {}) — resuming the change journal at {}",
                            index_path.display(),
                            idx.n,
                            persist::age_text(age_of(&idx)),
                            at
                        );
                        idx.prefault();
                        return Ready {
                            index: idx,
                            catch_up: CatchUp::Replayed,
                            journal,
                            watermark: at,
                        };
                    }
                    Startup::Reuse => {
                        rgy_engine::errln!(
                            "searchd: reusing index {} ({} entries, built {}){}",
                            index_path.display(),
                            idx.n,
                            persist::age_text(age_of(&idx)),
                            journal.gap_note()
                        );
                        idx.prefault();
                        return Ready {
                            index: idx,
                            catch_up: CatchUp::Relisted,
                            journal,
                            watermark: 0,
                        };
                    }
                    Startup::Rebuild => {
                        // The journal does not reach back to the saved
                        // position: the gap cannot be replayed, so the tree
                        // is walked again.
                        rgy_engine::errln!(
                            "searchd: rebuilding — the change journal no longer covers the saved position ({watermark}); the tree is walked again"
                        );
                    }
                }
            }
            Err(why) => rgy_engine::errln!("searchd: rebuilding — {why}"),
        }
    }

    // The platform walk where one exists (docs/design.md §2.2),
    // the portable walk where none does or where the native one refuses.
    let source = rgy_discovery_dispatch::source_for(
        root.as_bytes().to_vec(),
        PortableSource::DEFAULT_THREADS,
    );
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
            rgy_engine::errln!("searchd: scanning… {}", interrupt::line(p));
        }
    }) {
        Ok(l) => l,
        // A daemon with no index has nothing to answer with, and a partial
        // one would answer wrongly. Exit rather than start.
        Err(Cancelled) => {
            rgy_engine::errln!("searchd: scan cancelled — no index written, exiting");
            std::process::exit(2);
        }
    };
    // The walk is current as of the position the journal was at when it
    // started: everything after it gets a higher position, and is replayed
    // by the next start. The index carries it, so the next daemon resumes
    // the journal there instead of at its head.
    //
    // The position moves with the walk, not with the daemon's life: a
    // checkpoint that advanced it while the daemon runs would have to save
    // the live state (base plus overlay) as the index, and the daemon saves
    // the base alone. Until it saves the overlay too, the watermark is the
    // position of the walk, and the age limit is what keeps the served index
    // honest.
    let watermark = journal.head;
    let idx = Index::build(listings, watermark, now_secs(), root.as_bytes());
    match persist::save(&idx, &root_path, &index_path) {
        Ok(saved) => rgy_engine::errln!(
            "searchd: indexed {} entries → {}",
            idx.n,
            saved.index.display()
        ),
        Err(e) => rgy_engine::errln!(
            "searchd: could not save index to {}: {e}",
            index_path.display()
        ),
    }
    Ready {
        index: idx,
        catch_up: CatchUp::Relisted,
        journal,
        watermark,
    }
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
        Request::Status => {
            // The live index, not the snapshot it was built from: a reader
            // asking how big the index is wants the answer as it stands.
            let live = ctx.live.read().unwrap();
            responses.push(Response::Status {
                docs: live.base.n - live.dead_count + live.over.len(),
                event_id: live.event_id,
                catch_up: ctx.catch_up(),
            });
        }
        Request::Search { q, limit } => match Alternatives::parse(&q, &ctx.home) {
            Err(e) => responses.push(Response::Error { message: e }),
            Ok(alts) => {
                for w in &alts.warnings {
                    responses.push(Response::Warning { message: w.clone() });
                }
                let live_guard = ctx.live.read().unwrap();
                let searcher = Searcher { live: &live_guard };
                // The merge is the searcher's, and the identity it keys on is
                // the one that matters: keying it on `Hit::idx` keeps one
                // entry and drops the rest, because every live entry shares
                // the `u32::MAX` sentinel. A query that matched three files
                // added since the last compaction answered with one of
                // them -- and the daemon's answer is what the CLI reads.
                let hits = searcher.search_alternatives(&alts.alts, limit);
                let mut buf = Vec::new();
                let wire: Vec<WireHit> = hits
                    .iter()
                    .map(|h| {
                        // An overlay hit already carries its own path: it is
                        // an entry the base index does not have, so reading it
                        // back from the base would index `parent[u32::MAX]`
                        // -- out of bounds -- and panic.
                        let full = match &h.over {
                            // The overlay's paths are the index's internal
                            // form (`/`); the answer is the platform's.
                            Some(path) => String::from_utf8_lossy(path)
                                .replace('/', std::path::MAIN_SEPARATOR_STR),
                            None => {
                                live_guard.base.path(h.idx as usize, &mut buf);
                                let rel = String::from_utf8_lossy(&buf).into_owned();
                                buf.clear();
                                let mut full = String::from_utf8_lossy(&ctx.root).into_owned();
                                let rel = rel.trim_start_matches(['/', '\\']);
                                if !rel.is_empty() {
                                    full.push(std::path::MAIN_SEPARATOR);
                                    full.push_str(rel);
                                }
                                full
                            }
                        };
                        WireHit {
                            path: full,
                            score: h.score(),
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
                    match rgy_engine::tv::grep_tv(dir, &pat) {
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
            let live_guard = ctx.live.read().unwrap();
            let searcher = Searcher { live: &live_guard };
            let mut jobs: Vec<(String, rgy_name_core::name_query::GrepMode, Query)> = Vec::new();
            for alt in &alts.alts {
                match &alt.grep {
                    Some(pat) => jobs.push((pat.clone(), alt.grep_mode, alt.clone_for_scan())),
                    None => jobs.push((
                        q.to_string(),
                        rgy_name_core::name_query::GrepMode::Literal,
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
            rgy_name_core::name_query::GrepMode::Literal
        } else {
            rgy_name_core::name_query::GrepMode::Regex
        };
        let mut matched = false;
        if let Some(dir) = &ctx.tv_dir {
            match rgy_engine::tv::grep_tv(dir, &pattern) {
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
            let live_guard = ctx.live.read().unwrap();
            let searcher = Searcher { live: &live_guard };
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
    let esc = rgy_proto::escape_json_string;
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
        .map(|e| format!(",\"error\":\"{}\"", rgy_proto::escape_json_string(e)))
        .unwrap_or_default();
    let _ = writeln!(
        out,
        "{{\"rgx\":\"end\",\"seq\":{seq},\"tag\":\"{tag}\",\"status\":{status},\"matched\":{matched}{err}}}"
    );
    let _ = out.flush();
}

// --- transports -----------------------------------------------------------

/// The platform live-change feed for the tree at `root`, or `None` where none
/// is available (unsupported OS, or the volume/journal could not be opened).
/// See the note on `Ctx::live` for the Windows chain and its lifetime.
///
/// `watermark` is the change-feed position the index is current as of. On
/// Windows the feed starts there, so the records between it and the
/// journal's head — the changes that happened while the daemon was off — are
/// delivered instead of being missed. It is 0 when the index carries no
/// position, and the feed then starts at the journal's head.
#[cfg(windows)]
fn open_watch(
    root: &[u8],
    journal: &Journal,
    watermark: u64,
) -> Option<Box<dyn rgy_discovery::Watch>> {
    let path = std::str::from_utf8(root).ok()?;
    let vol_root = std::path::Path::new(path).ancestors().last()?.to_str()?;
    // The volume lives for the process; leak it so the UsnWatch owns a
    // 'static borrow and can move into the update thread.
    let volume: &'static rgy_discovery_windows::volume::Volume = Box::leak(Box::new(
        rgy_discovery_windows::volume::Volume::open(vol_root).ok()?,
    ));
    let indexer = rgy_discovery_windows::indexer::Indexer::new(volume);
    let mut watch = rgy_discovery_windows::UsnWatch::new(indexer);
    if watermark != 0 && watermark <= i64::MAX as u64 {
        watch.start_usn = watermark as i64;
        // The journal's identity travels with it: a journal that was deleted
        // and recreated has a new id, and the feed reports that as a rescan
        // instead of reading the new journal as if it were the old one.
        watch.journal_id = journal.id;
    }
    Some(Box::new(watch))
}

#[cfg(target_os = "linux")]
fn open_watch(
    root: &[u8],
    _journal: &Journal,
    _watermark: u64,
) -> Option<Box<dyn rgy_discovery::Watch>> {
    use std::os::unix::ffi::OsStrExt;
    let path = std::path::PathBuf::from(std::ffi::OsStr::from_bytes(root));
    Some(Box::new(
        rgy_discovery_linux::InotifyWatch::new(&path).ok()?,
    ))
}

/// The macOS feed: FSEvents, pulled.
///
/// FSEvents is a callback feed — the stream pushes batches of events into a
/// channel — and the daemon speaks a pull, so the adapter in
/// `discovery-macos` sits between the two. The adapter owns the stream:
/// dropping the stream stops the feed, and a stopped feed is a daemon that
/// stopped noticing changes.
///
/// The stream is directory-granular (`Event.path` is the folder whose
/// contents changed); the adapter marks the record with the directory bit,
/// and the live overlay re-lists that folder — re-listing its parent would
/// see only the folder's own mtime and miss the edit inside.
#[cfg(target_os = "macos")]
fn open_watch(
    root: &[u8],
    _journal: &Journal,
    watermark: u64,
) -> Option<Box<dyn rgy_discovery::Watch>> {
    // Start where the index stands, and from now otherwise: an event id of
    // zero asks the framework for the whole history it still keeps, which
    // is a flood of folders to re-list at startup.
    let since = if watermark > 0 {
        watermark
    } else {
        rgy_discovery_macos::current_id()
    };
    Some(Box::new(rgy_discovery_macos::FseventsWatch::open(
        root,
        since,
        FSEVENTS_LATENCY,
    )))
}

/// How long FSEvents may wait to coalesce events into one batch.
///
/// The framework calls back once per window, so this is the delay between a
/// change on disk and the daemon seeing it. Half a second is short enough to
/// feel live and long enough that a build touching a thousand files does not
/// wake the daemon a thousand times.
#[cfg(target_os = "macos")]
const FSEVENTS_LATENCY: f64 = 0.5;

/// Every remaining target (FreeBSD and the long tail): no wired feed.
/// kqueue would be per-directory fd registration against a file-granular
/// batch vocabulary — the same granularity gap the macOS note above names —
/// so FreeBSD joins it until the seam grows a directory-level event. The
/// daemon answers on its snapshot rather than failing to start.
#[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
fn open_watch(
    _root: &[u8],
    _journal: &Journal,
    _watermark: u64,
) -> Option<Box<dyn rgy_discovery::Watch>> {
    None
}

/// Spawn the trim thread: give the query cache back while nobody is
/// typing.
///
/// The name table a broad query leaves behind is tens of megabytes on a
/// large index, and it is worth keeping only while the user is typing:
/// the next query after a pause reads the index again anyway, and the
/// table is what the pause leaves holding the memory. The daemon has no
/// idle moment of its own -- it blocks on the next request -- so the
/// trim is a thread of its own, ticking.
fn spawn_trim(ctx: &std::sync::Arc<Ctx>) {
    /// How long the cache has to be unused before it is dropped.
    const IDLE: std::time::Duration = std::time::Duration::from_secs(30);
    /// How often the thread looks.
    const TICK: std::time::Duration = std::time::Duration::from_secs(5);

    let ctx = ctx.clone();
    let _ = std::thread::Builder::new()
        .name("searchd-trim".into())
        .spawn(move || {
            loop {
                std::thread::sleep(TICK);
                // A read guard is enough: the trim only touches the cache's
                // own locks, and a query reads the table under them.
                if let Ok(live) = ctx.live.read() {
                    live.names_cache.trim_if_idle(IDLE);
                }
            }
        });
}

/// Spawn the live-update thread: build the platform `Watch` for `ctx.root` and
/// apply its change batches to `ctx.live`, taking the write lock *per batch*
/// so queries interleave (holding it across the feed would block them all).
/// If no feed opens, the thread just stops -- the daemon answers on its
/// snapshot rather than refusing to start.
fn spawn_watch(ctx: &std::sync::Arc<Ctx>, journal: &Journal, watermark: u64) {
    let ctx = ctx.clone();
    let root = ctx.root.clone();
    let journal = *journal;
    let _ = std::thread::Builder::new()
        .name("searchd-watch".into())
        .spawn(move || {
            let Some(mut watch) = open_watch(&root, &journal, watermark) else {
                return;
            };
            loop {
                match watch.next_batch() {
                    Ok(rgy_discovery::ChangeBatch::Events(events)) => {
                        let mut live = ctx.live.write().unwrap();
                        live.apply_events(&events);
                        // The feed's position is the watermark the index is
                        // current as of: it is what the next start resumes
                        // the journal from.
                        if let Some(pos) = watch.position() {
                            live.event_id = pos;
                        }
                    }
                    Ok(rgy_discovery::ChangeBatch::Rescan) => {
                        ctx.live.write().unwrap().apply_dir(&root, true);
                        // The feed lost its history, so nothing was replayed:
                        // the tree was walked again instead. The platform's
                        // usual answer no longer describes this index.
                        ctx.set_relisted();
                    }
                    Err(_) => break,
                }
            }
        });
}

#[cfg(unix)]
fn serve_uds(path: &str, ctx: &std::sync::Arc<Ctx>, serve: bool) {
    use std::os::unix::net::UnixListener;
    let _ = std::fs::remove_file(path);
    let listener = match UnixListener::bind(path) {
        Ok(l) => l,
        Err(e) => {
            rgy_engine::errln!("searchd: bind {path}: {e}");
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
    rgy_engine::errln!("searchd: uds listen is unix-only; use pipe:<name> or stdio");
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
                Err(e) => rgy_engine::errln!("searchd: pipe {path}: {e}"),
            }
        }
        Err(e) => rgy_engine::errln!("searchd: pipe {path}: {e}"),
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
    rgy_engine::errln!("searchd: pipe:{name} is windows-only; use uds:<path> or stdio");
    std::process::exit(2);
}

fn now_secs() -> u32 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as u32)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rgy_discovery::Source;

    /// The JSON escaper this repository uses and the one upstream's rgx
    /// uses must agree on the same input, byte for byte.
    ///
    /// They are two implementations of one rule — RFC 8259 §7 — and this
    /// daemon speaks to both kinds of client: the rgy protocol and the rgx
    /// compatibility front. If they ever disagree, a path or a grep line
    /// would be readable by one reader and not the other. The upstream
    /// escaper is upstream's and is not edited here; this test is what
    /// keeps the two honest.
    #[test]
    fn the_two_json_escapers_agree() {
        let cases = [
            "plain",
            "with \"quotes\"",
            "with \\ backslash",
            "with / solidus",
            "tab\there",
            "line\nbreak",
            "carriage\rreturn",
            "back\u{0008}space",
            "form\u{000C}feed",
            "nul\u{0000}byte",
            "unit\u{001F}separator",
            "del\u{007F}elete",
            "non-ascii \u{00e9}\u{4e2d}\u{1f600}",
        ];
        for c in cases {
            assert_eq!(
                rgy_proto::escape_json_string(c),
                rgx_core::djson::escape_json_string(c),
                "the two escapers disagree on {c:?}"
            );
        }
    }

    /// The root is one spelling: absolute, and the one the change feeds
    /// report. A relative root is resolved against the working directory,
    /// and the Windows canonical prefix comes off — it is not what the
    /// user wrote and not what an answer should carry.
    #[test]
    fn the_root_is_one_absolute_spelling() {
        let root = absolute_root(".");
        assert!(
            std::path::Path::new(&root).is_absolute(),
            "the root is absolute: {root}"
        );
        assert!(
            !root.starts_with(r"\\?\"),
            "the canonical prefix comes off: {root}"
        );
        assert_eq!(
            absolute_root(&root),
            root,
            "resolving twice changes nothing"
        );
    }

    /// A journal that reaches back to `lowest` and is at `head`.
    fn journal(id: u64, head: u64, lowest: u64) -> Journal {
        Journal { id, head, lowest }
    }

    /// The decision the startup makes about a saved index, as the pure
    /// function it is written as.
    ///
    /// The three answers are the three the daemon can give: resume the
    /// journal at the watermark, reuse the index as it is, or walk the tree
    /// again. The middle one is the case the age policy already decided;
    /// the last one is the case where the gap cannot be replayed and an
    /// index that misses it would answer wrongly.
    #[test]
    fn the_startup_decision_is_the_watermark_and_the_journal() {
        // No watermark: there is nothing to replay from.
        assert_eq!(startup_plan(0, &journal(7, 1_000, 500)), Startup::Reuse);

        // A journal that covers the watermark: resume it there.
        assert_eq!(
            startup_plan(700, &journal(7, 1_000, 500)),
            Startup::Replay(700)
        );

        // A journal that no longer reaches back to it (it wrapped, or it
        // was recreated): the gap cannot be replayed, so the tree is walked
        // again.
        assert_eq!(startup_plan(100, &journal(7, 1_000, 500)), Startup::Rebuild);
        // A watermark ahead of the journal's head is the same case: the
        // journal was recreated and its positions went back to the start.
        assert_eq!(
            startup_plan(5_000, &journal(7, 1_000, 500)),
            Startup::Rebuild
        );

        // No journal at all: the age policy is the only freshness rule
        // there is, and it has already accepted the index.
        assert_eq!(startup_plan(700, &journal(0, 0, 0)), Startup::Reuse);
    }

    /// The journal's window decides whether a watermark can be resumed
    /// from: a position inside the window is covered, one before it is not
    /// (the journal recycled those records), and one after it is not
    /// either (the journal was recreated).
    #[test]
    fn the_journal_window_decides_what_it_covers() {
        let j = journal(7, 1_000, 500);
        assert!(j.covers(500));
        assert!(j.covers(700));
        assert!(j.covers(1_000));
        assert!(!j.covers(499), "a recycled position is not covered");
        assert!(
            !j.covers(1_001),
            "a position ahead of the journal is not covered"
        );
        // No journal covers nothing.
        assert!(!journal(0, 0, 0).covers(0));
    }

    /// The status answer is what this process did, not what the platform
    /// could have done: a daemon that walked the tree says "relisted", and
    /// only a daemon that resumed the journal says "replayed".
    #[test]
    fn the_status_answer_reports_what_the_process_did() {
        let ctx = std::sync::Arc::new(Ctx {
            root: b"/tmp".to_vec(),
            home: String::new(),
            tv_dir: None,
            live: std::sync::RwLock::new(Live::new(Index::build(Vec::new(), 0, 0, b"/"))),
            catch_up: std::sync::atomic::AtomicU8::new(CATCH_UP_REPLAYED),
        });
        assert_eq!(ctx.catch_up(), CatchUp::Replayed);

        // A feed that loses its history turns a replay into a relist.
        ctx.set_relisted();
        assert_eq!(ctx.catch_up(), CatchUp::Relisted);
    }

    /// A file that appears after the daemon started has to be findable.
    ///
    /// The feed applies it to the live index and the answer names it. The
    /// entry is an overlay entry — it is not in the base index — so the
    /// answer has to come from the overlay's own path: reading it back from
    /// the base would index `parent[u32::MAX]`, out of bounds, and panic.
    ///
    /// This is the whole point of the live feed. A daemon that answers on
    /// its snapshot says "no such file" for a file that exists, which is
    /// the worst thing a search can do.
    #[test]
    fn a_live_change_is_answered_from_the_overlay() {
        let dir = std::env::temp_dir().join(format!(
            "rgy-searchd-live-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("hello.txt"), b"hello\n").unwrap();
        let root = dir.as_os_str().as_encoded_bytes().to_vec();

        // The index as it was before the file appeared.
        let source = PortableSource::new(root.clone(), 2);
        let mut live = Live::with_root(
            Index::build(source.enumerate().expect("the walk"), 0, 1, b"/"),
            &root,
        );

        // The file appears, and the feed says the folder whose contents
        // changed (what a directory-granular feed says).
        std::fs::write(dir.join("late.txt"), b"late\n").unwrap();
        live.apply_events(&[rgy_discovery::UsnRecord {
            file_reference: 0,
            parent_reference: 0,
            usn: 0,
            reason: 0,
            file_attributes: rgy_discovery::FILE_ATTRIBUTE_DIRECTORY,
            name: String::from_utf8_lossy(&root).into_owned(),
        }]);

        let ctx = std::sync::Arc::new(Ctx {
            root: root.clone(),
            home: String::new(),
            tv_dir: None,
            live: std::sync::RwLock::new(live),
            catch_up: std::sync::atomic::AtomicU8::new(CATCH_UP_RELISTED),
        });

        let responses = handle(
            Request::Search {
                q: "late".to_string(),
                limit: 10,
            },
            &ctx,
        );
        let hits = responses
            .iter()
            .find_map(|r| match r {
                Response::Hits { hits, .. } => Some(hits),
                _ => None,
            })
            .expect("a hits frame");
        assert!(
            hits.iter().any(|h| h.path.ends_with("late.txt")),
            "the new file must be answered; hits={hits:?}"
        );
    }

    /// Two files that appear after the daemon started both have to be
    /// found.
    ///
    /// The overlay holds the entries the base does not have, and they all
    /// carry the same `Hit::idx` -- the `u32::MAX` sentinel. A merge keyed
    /// on that key keeps one of them and drops the rest, so a query that
    /// matched three files added since the last compaction answered with
    /// one of them, and the daemon said the other two did not exist.
    ///
    /// The test above cannot see this: with one live entry there is
    /// nothing to collapse.
    #[test]
    fn every_live_change_is_answered() {
        let dir = std::env::temp_dir().join(format!(
            "rgy-searchd-live-many-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("hello.txt"), b"hello\n").unwrap();
        let root = dir.as_os_str().as_encoded_bytes().to_vec();

        // The index as it was before the files appeared.
        let source = PortableSource::new(root.clone(), 2);
        let mut live = Live::with_root(
            Index::build(source.enumerate().expect("the walk"), 0, 1, b"/"),
            &root,
        );

        // Two files appear, and the feed says the folder whose contents
        // changed (what a directory-granular feed says).
        std::fs::write(dir.join("late-one.txt"), b"one\n").unwrap();
        std::fs::write(dir.join("late-two.txt"), b"two\n").unwrap();
        live.apply_events(&[rgy_discovery::UsnRecord {
            file_reference: 0,
            parent_reference: 0,
            usn: 0,
            reason: 0,
            file_attributes: rgy_discovery::FILE_ATTRIBUTE_DIRECTORY,
            name: String::from_utf8_lossy(&root).into_owned(),
        }]);

        let ctx = std::sync::Arc::new(Ctx {
            root: root.clone(),
            home: String::new(),
            tv_dir: None,
            live: std::sync::RwLock::new(live),
            catch_up: std::sync::atomic::AtomicU8::new(CATCH_UP_RELISTED),
        });

        let responses = handle(
            Request::Search {
                q: "late".to_string(),
                limit: 10,
            },
            &ctx,
        );
        let hits = responses
            .iter()
            .find_map(|r| match r {
                Response::Hits { hits, .. } => Some(hits),
                _ => None,
            })
            .expect("a hits frame");
        assert!(
            hits.iter().any(|h| h.path.ends_with("late-one.txt")),
            "the first new file must be answered; hits={hits:?}"
        );
        assert!(
            hits.iter().any(|h| h.path.ends_with("late-two.txt")),
            "the second new file must be answered too, not collapsed into the first; hits={hits:?}"
        );
    }
}
