// SPDX-License-Identifier: Apache-2.0

//! searchd — the merged search daemon.
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

use rgy_discovery::walk::KIND_DIR;
use rgy_discovery::{PortableSource, interrupt, walk::Cancelled};
use rgy_engine::{keep_warm, warm_stop};
use rgy_name_core::content::{self, Grep};
use rgy_name_core::live::Live;
use rgy_name_core::name_index::Index;
use rgy_name_core::name_query::{Alternatives, Hit, Query, Searcher};
use rgy_name_core::persist;
use rgy_proto::{CatchUp, GrepFile, Hit as WireHit, Request, Response};

#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

/// How often the startup walk reports what it has covered.
const REPORT_EVERY: Duration = Duration::from_millis(500);

/// The two values the catch-up answer carries, as the atomic holds them.
const CATCH_UP_REPLAYED: u8 = 0;
const CATCH_UP_RELISTED: u8 = 1;

/// How long the startup waits for the live feed to be armed.
///
/// The feed has to be armed before the tree is walked again — a change that
/// happens while the walk runs must not be missed by both — and the status
/// answer has to know whether the tree is followed. The wait is bounded: a
/// feed that takes this long to open is a platform that is slow, and the
/// daemon answers rather than waits for it.
const FEED_ARM_WAIT: Duration = Duration::from_secs(5);

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
    /// The state of the live-change feed, as the status answer reports it.
    ///
    /// The protocol's status shape has no room for it, and a daemon whose
    /// feed died answers on a frozen index: a reader that is not told that
    /// the tree is no longer followed is told a tree that is.
    feed: std::sync::atomic::AtomicU8,
    /// The bell the content thread waits on.
    ///
    /// The content thread used to re-diff the whole index twenty times a
    /// second; the tree does not change twenty times a second. It waits
    /// here instead, and the watch thread rings the bell when the overlay
    /// moved.
    changed: Changed,
    /// The content index, when the daemon has built one.
    ///
    /// The trigram postings are what let a `grep:` rule most files out
    /// before they are read: with the index the search opens the
    /// candidates, without it the search opens everything the name filters
    /// name. The daemon builds it the first time a content search asks for
    /// it -- a daemon that is never asked to grep never pays for it.
    ///
    /// It is behind a lock of its own: the thread that builds it takes the
    /// lock for the cheap half (what should be indexed) and the search
    /// takes it for the postings.
    content: std::sync::Mutex<Option<content::Content>>,
    /// Set when a content search has been asked for: what tells the
    /// content thread there is something to build an index for.
    wants_content: std::sync::atomic::AtomicBool,
    /// The incremental search sessions, one per client.
    ///
    /// A session holds the last result set and re-ranks it when the query
    /// narrows, instead of walking the index again. The id is the client's
    /// own: the protocol carries it, so two clients (two windows of the
    /// TUI) do not share a session and neither one's keystrokes disturb
    /// the other's.
    ///
    /// The session is held behind an `Arc`, because the search takes it out
    /// of the map and runs with the map's lock dropped: the lock is held
    /// for the length of a search otherwise, and every client would wait
    /// for every other client's keystroke.
    sessions: std::sync::Mutex<
        std::collections::HashMap<u64, std::sync::Arc<rgy_name_core::session::Session>>,
    >,
    /// The ids the daemon names itself.
    ///
    /// The session id is the client's to keep, and the daemon's to hand
    /// out: a client that does not name one gets one from here, and the
    /// answer says which — that is the round trip that lets the next
    /// keystroke be answered from the previous answer instead of from the
    /// index.
    ///
    /// The ids the daemon names live above the ones the clients name: a
    /// client's id is a counter of its own (the launcher's starts at one),
    /// and the daemon's has the top bit set. The two spaces cannot meet,
    /// and a client that names an id the daemon would have named does not
    /// land on a session of somebody else's.
    sessions_next: std::sync::atomic::AtomicU64,
}

impl Ctx {
    /// The daemon's own session ids live above the clients' and below the
    /// wire's exactness limit.
    ///
    /// The two bounds are both real. The ids a client names are small
    /// counters, and the daemon's own are above them, so the two spaces
    /// cannot meet — a client that names a daemon id does not land on
    /// somebody else's session. And the wire carries a number as an
    /// `f64` (`rgy_proto`'s own JSON reader), so an integer above `2^53`
    /// does not survive the round trip: the daemon's id would reach the
    /// client as a different number, and the client would ask for the
    /// re-rank of the wrong session. `2^52` and `2^53` are the two ends
    /// of the space the daemon may name in.
    const DAEMON_SESSIONS: u64 = 1 << 52;
    /// The first id above the daemon's space: the wire's exactness limit.
    const SESSION_LIMIT: u64 = 1 << 53;

    /// A session id the daemon names.
    ///
    /// The id is the session the daemon keeps the answer under, and the
    /// answer says which one it is: a client that does not name a session
    /// gets one from here, sends it back with the next question, and the
    /// daemon re-ranks the answer it already has.
    ///
    /// **The id is unguessable, and that is what makes the round trip
    /// safe.** The id is a capability: a client that names it re-ranks the
    /// answer the daemon keeps under it, and a client that guesses it
    /// lands on somebody else's result set. The ids used to come out of a
    /// counter, and a counter cannot be a capability — the second client
    /// could name the first client's id, because the first client's id
    /// was the first number after the line — so the daemon refused every
    /// id in its own space, including the ones it had just issued. The
    /// answer it put on the wire was a name it would not take back, and
    /// the round trip the protocol promises never happened.
    ///
    /// The id comes from the process's own random seed instead, so a
    /// client that holds one is holding a capability and a client that
    /// does not hold one cannot reach the answer.
    ///
    /// The id is in the daemon's space, `[2^52, 2^53)`: above the ids a
    /// client names and below the wire's exactness limit, so the number
    /// the daemon sends is the number the daemon reads back.
    fn new_session(&self) -> Option<u64> {
        use std::collections::hash_map::RandomState;
        use std::hash::{BuildHasher, Hasher};

        // The entropy: the hash map's own hasher, which every process
        // seeds differently and every call re-keys. The counter goes in
        // so that two calls in the same process do not collide.
        let n = self.sessions_next.fetch_add(1, Ordering::Relaxed);
        let mut hasher = RandomState::new().build_hasher();
        hasher.write_u64(n);
        let raw = hasher.finish();

        // Into the daemon's space: the line itself, and the 52 bits below
        // it that the hash filled. The id is under the wire's limit, so it
        // survives the round trip as the very number that was sent.
        let width = Self::SESSION_LIMIT - Self::DAEMON_SESSIONS;
        let id = Self::DAEMON_SESSIONS | (raw & (width - 1));
        // The id enters the map here: the daemon issues it, and an id the
        // daemon issues is one the daemon takes back.
        //
        // The id used to enter the map only inside the search that used it,
        // and a query with alternatives (`|`) never got that far -- the
        // search returns before the session is looked up. So the daemon
        // handed out an id, put it on the wire, and then refused it: the
        // client's next keystroke named an id the daemon said was not
        // its own, and the answer came back with a different session. The
        // session is empty here, and that is right: it is the receipt for
        // the id, not the results.
        let session = std::sync::Arc::new(rgy_name_core::session::Session::new(id));
        self.sessions.lock().unwrap().insert(id, session);
        Some(id)
    }

    /// Whether the daemon has issued this session and still keeps it.
    ///
    /// The sessions that have been idle for too long are trimmed away, so
    /// an id the daemon issued can stop being one: the client that holds
    /// it gets a fresh session, and the answer it was re-ranking is gone.
    ///
    /// This is the whole of the daemon's answer to "is this id yours?".
    /// The ids are unguessable, so a client that names one of them is
    /// holding it — and a client that names an id the daemon never issued
    /// is refused, and given a fresh one.
    fn has_session(&self, id: u64) -> bool {
        self.sessions.lock().unwrap().contains_key(&id)
    }

    /// How the index caught up after the last restart (the live-feed
    /// honesty rule).
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

    /// The feed's state, as the status answer reports it.
    fn feed_state(&self) -> rgy_proto::Feed {
        match self.feed.load(Ordering::Relaxed) {
            FEED_LIVE => rgy_proto::Feed::Live,
            FEED_GONE => rgy_proto::Feed::Gone,
            _ => rgy_proto::Feed::None,
        }
    }

    /// The feed died: the tree is not followed any more, and the answer
    /// has to say so.
    fn set_feed_gone(&self) {
        self.feed.store(FEED_GONE, Ordering::Relaxed);
    }

    /// Search the alternatives through the client's session.
    ///
    /// The session is the incremental-search id: the
    /// daemon keeps one per id, and a query that narrows the previous one
    /// is answered by re-ranking the previous result set instead of
    /// walking the index again. The facet replay path is
    /// the same session: a facet selection that has not changed replays
    /// the previous answer instead of searching again.
    ///
    /// The answers are the same either way -- the session is an
    /// optimisation, not a different search -- so this is only about the
    /// work. An alternative (`|`) is searched on its own: a session holds
    /// one result set, and a query with alternatives is not one query.
    ///
    /// **A query that fails closed answers nothing on every route.** The
    /// engine refuses a query with an unknown `type:` and answers it with
    /// nothing. The session's path knows the flag too — it is a property of
    /// the query, not of one caller's routing — so the route that named a
    /// session answers with nothing like the route that did not. The
    /// session route used to break the rule, and the workaround that hid
    /// it is gone: the refusal now lives in the engine, where the fresh
    /// path keeps it, and the two cannot drift.
    fn search_with_session(
        &self,
        id: u64,
        searcher: &Searcher<'_>,
        alts: &[Query],
        limit: usize,
    ) -> Vec<rgy_name_core::name_query::Hit> {
        if alts.len() != 1 {
            return searcher.search_alternatives(alts, limit);
        }
        // The request's own cap becomes the query's cap, the way it does
        // without a session: the query's `limit:` is the query's, and the
        // request carries one too. The session path used to ignore the
        // request's, and answered with the query's default -- a request
        // for ten hits came back with fifty.
        let mut q = alts[0].clone();
        q.limit = limit;
        // The session is taken out of the map under the lock and the lock is
        // dropped before the search. The search is the slow part, and a lock
        // held across it makes every client wait for every other client's
        // keystroke -- which is the opposite of what a session is for.
        let session = {
            let mut sessions = self.sessions.lock().unwrap();
            sessions
                .entry(id)
                .or_insert_with(|| std::sync::Arc::new(rgy_name_core::session::Session::new(id)))
                .clone()
        };
        session.search_replaying_facets(searcher, &q)
    }

    /// Drop the sessions that have been idle for `idle`.
    ///
    /// The map grew by one entry for every id a client ever used, and a
    /// client that reconnects with a new id leaves one behind for every
    /// connection it ever made. A session nobody has used for a while is
    /// worth nothing: the next query reads the index again anyway.
    fn trim_sessions(&self, idle: std::time::Duration) {
        let mut sessions = self.sessions.lock().unwrap();
        sessions.retain(|_, s| !s.idle_for(idle));
    }
}

/// The feed's state, as the status answer reports it.
///
/// The protocol's status shape has no room for it, and a daemon whose
/// feed died answers on a frozen index: a reader that is not told that
/// the tree is no longer followed is told a tree that is.
const FEED_NONE: u8 = 0;
/// The feed runs: the tree is followed.
const FEED_LIVE: u8 = 1;
/// The feed died: the index is a snapshot from here on.
const FEED_GONE: u8 = 2;

/// The bell the content thread waits on.
///
/// The content thread used to re-diff the whole index twenty times a
/// second — a full pass over every path, a `Vec` per file and a sort —
/// and the tree does not change twenty times a second. It waits here
/// instead: the watch thread rings the bell when the overlay moved (the
/// feed's position and the overlay's own revision both move with the
/// tree), and the thread wakes for that and for nothing else.
struct Changed {
    /// How many times the tree has moved.
    times: std::sync::Mutex<u64>,
    /// Woken when it has.
    bell: std::sync::Condvar,
}

impl Changed {
    const fn new() -> Changed {
        Changed {
            times: std::sync::Mutex::new(0),
            bell: std::sync::Condvar::new(),
        }
    }

    /// The tree moved: wake the thread that waits for it.
    fn ring(&self) {
        let mut times = self.times.lock().unwrap();
        *times += 1;
        self.bell.notify_all();
    }

    /// How many times the tree has moved so far.
    fn count(&self) -> u64 {
        *self.times.lock().unwrap()
    }

    /// Wait until the tree has moved more than `after` times.
    fn wait(&self, after: u64) -> u64 {
        let mut times = self.times.lock().unwrap();
        while *times <= after {
            times = self.bell.wait(times).unwrap();
        }
        *times
    }
}

/// What the daemon answers with.
///
/// The frames of the protocol, and the one the daemon writes itself: the
/// status frame carries a fact the protocol's status shape has no room
/// for — whether the tree is still being followed — and the field is
/// additive, so a reader that does not know it reads the status as it
/// was before it existed.
#[derive(Debug)]
enum Answer {
    /// A frame of the protocol.
    Frame(Response),
    /// A line the daemon wrote.
    Line(String),
}

impl Answer {
    /// The line this answer is written as.
    fn to_json(&self) -> String {
        match self {
            Answer::Frame(r) => r.to_json(),
            Answer::Line(l) => l.clone(),
        }
    }
}

/// The status answer: the protocol's frame with the feed's state added.
///
/// The protocol's status shape is four fields, and the daemon knows a
/// fifth one: whether the tree is still being followed. The four fields
/// are the protocol's, byte for byte — the frame is the protocol's own —
/// and the fifth is added to it. The field is additive: a reader that
/// does not know it reads the status as it was before it existed.
fn status_frame(ctx: &Ctx) -> String {
    let live = ctx.live.read().unwrap();
    Response::Status {
        docs: live.base.n - live.dead_count + live.over.len(),
        event_id: live.event_id,
        catch_up: ctx.catch_up(),
        // Whether the content layer is answered by the pack store
        // or by a walk. A client that knows the difference can say
        // which one it is getting.
        tv: ctx.tv_dir.is_some(),
        // Whether the tree is still being followed. The field is the
        // protocol's: the daemon says what the feed is doing, and the
        // wire is where the things that go on it are spelled.
        feed: ctx.feed_state(),
    }
    .to_json()
}

/// Print the usage and leave. `code` is the exit status: 0 when the
/// user asked for help, 2 for the rg convention's "bad invocation".
///
/// The stream follows the status: the help is an answer, and an answer
/// goes to stdout — `searchd --help | less` shows the reader nothing
/// when the help goes to stderr. The complaint about a bad invocation
/// goes to stderr, because that one is not an answer.
fn usage(code: i32) -> ! {
    if code == 0 {
        rgy_engine::outln!("{}", usage_text());
    } else {
        rgy_engine::errln!("{}", usage_text());
    }
    std::process::exit(code);
}

/// The usage text.
fn usage_text() -> String {
    "searchd [--root <dir>] [--index <file>] [--reindex] [--max-age <dur>]\n         [--tv-dir <store>] [--listen stdio|uds:<path>|pipe:<name>] [--serve]\n         [--threads <n>]\n\n\
         proto framing (default): one JSON request per line\n  \
           {\"op\":\"search\",\"q\":\"...\",\"limit\":50}\n  \
           {\"op\":\"grep\",\"q\":\"...\"}\n  \
           {\"op\":\"status\"}\n  \
         --serve: rgx compat — a JSON argv-array per line, answers framed\n  \
           by {\"rgx\":\"begin\"}/{\"rgx\":\"end\"} with rg --json match lines.\n\n\
         --threads: workers for the scan and the query (default: 4 for the\n\
         scan, the machine's cores for the query; RGY_NUM_THREADS sets\n\
         both).".to_string()
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
    // into UTF-8 before anything is printed. The input
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
    let mut threads: Option<usize> = None;
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
            "--threads" => {
                i += 1;
                threads = Some(
                    args.get(i)
                        .and_then(|s| s.parse::<usize>().ok())
                        .filter(|&n| n > 0)
                        .unwrap_or_else(|| usage(2)),
                );
            }
            "-h" | "--help" => usage(0),
            "-V" | "--version" => {
                rgy_engine::outln!("searchd {}", env!("CARGO_PKG_VERSION"));
                std::process::exit(0);
            }
            other => {
                rgy_engine::errln!("searchd: unknown argument {other}");
                std::process::exit(2);
            }
        }
        i += 1;
    }

    // A path that begins with `~` names the home directory. A POSIX shell
    // expands it before the command runs and Windows PowerShell does not,
    // so the tree expands it here: a daemon that held a root of `~` would
    // index a folder that does not exist, and answer about it as if it were
    // a tree.
    let root = rgy_engine::paths::expand_home(&root);
    let index = index.map(|p| rgy_engine::paths::expand_home(&p));
    let tv_dir = tv_dir.map(|p| {
        let expanded = rgy_engine::paths::expand_home(&p.to_string_lossy());
        std::path::PathBuf::from(expanded)
    });

    // One spelling of the root, resolved once. The change feeds report
    // absolute paths (the macOS stream always does), and the live overlay
    // and the name index have to talk about the same file: one spelling of
    // the root is what lets them. A root that reaches the tree through a
    // symlink is resolved too, because the kernel reports the resolved
    // path.
    let root = absolute_root(&root);

    // The width the scan and the query use. The flag wins; then the
    // environment (`RGY_NUM_THREADS`), which is the project's own setting
    // and says how wide everything is; then each one's own default -- four
    // for the scan (the config's default), and the machine's own core
    // count for the query, which is what the pool it replaced did.
    //
    // The environment is asked once, so the two widths cannot come from
    // two different answers.
    let set = threads.or_else(rgy_par::configured);
    let query_threads = set.unwrap_or_else(rgy_par::workers).max(1);
    let scan_threads = set.unwrap_or(PortableSource::DEFAULT_THREADS).max(1);
    // Pinned here so every parallel path in the daemon agrees on the width.
    rgy_par::set_workers(query_threads);

    let startup = open_index(&root, index.as_deref(), reindex, max_age, scan_threads);
    // The content index lives beside the name index: `<index>.content/`,
    // which is where the CLI looks for it.
    let content_dir = {
        let root_path = std::path::PathBuf::from(&root);
        let base = match index.as_deref() {
            Some(p) => std::path::PathBuf::from(p),
            None => persist::default_index_path(&root_path),
        };
        base.with_extension("content")
    };
    let live = Live::with_root(startup.index, root.as_bytes());
    let ctx = std::sync::Arc::new(Ctx {
        root: root.as_bytes().to_vec(),
        home: std::env::var("USERPROFILE")
            .or_else(|_| std::env::var("HOME"))
            .unwrap_or_default(),
        tv_dir,
        live: std::sync::RwLock::new(live),
        catch_up: std::sync::atomic::AtomicU8::new(match startup.catch_up {
            CatchUp::Replayed => CATCH_UP_REPLAYED,
            CatchUp::Relisted => CATCH_UP_RELISTED,
        }),
        feed: std::sync::atomic::AtomicU8::new(FEED_NONE),
        changed: Changed::new(),
        content: std::sync::Mutex::new(None),
        wants_content: std::sync::atomic::AtomicBool::new(false),
        sessions: std::sync::Mutex::new(std::collections::HashMap::new()),
        sessions_next: std::sync::atomic::AtomicU64::new(0),
    });

    // Keep the index fresh as the tree changes (the Watch thread); queries
    // keep answering on the last state while an apply is in flight.
    //
    // The feed is armed before the tree is walked again: a change that
    // happens while the walk runs must not be missed by both.
    let armed = spawn_watch(&ctx, &startup.journal, startup.watermark);
    // The feed is armed before the daemon answers: the tree may be walked
    // again (a change that happens while the walk runs must not be missed by
    // both), and the status answer says whether the tree is followed.
    if armed.recv_timeout(FEED_ARM_WAIT).is_err() {
        rgy_engine::errln!(
            "searchd: the live feed is not armed yet: a change may be missed while it opens"
        );
    }
    if startup.relist {
        // The tree is walked again and diffed, so "relisted" means what the
        // protocol says it means. The walk happens before the first answer:
        // a daemon that said "relisted" without having walked would be
        // answering on a snapshot and saying it had not.
        let listed = {
            let live = ctx.live.read().unwrap();
            list_all(&live, root.as_bytes())
        };
        {
            let mut live = ctx.live.write().unwrap();
            apply_all(&mut live, listed);
        }
        rgy_engine::errln!("searchd: the tree was walked again and diffed");
    }

    // Give the query cache back while nobody is typing (the trim thread).
    spawn_trim(&ctx);

    // Keep the content index fresh, once something asks for one (the
    // content thread).
    spawn_content(&ctx, content_dir);

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
    /// Whether the tree has to be walked again before the daemon answers.
    ///
    /// "Relisted" is the protocol's word for a tree that was walked again
    /// and diffed, so a daemon that says it has to have done it. A saved
    /// index is a snapshot: the files created since it was built are not
    /// in it, and the answer would be missing them.
    relist: bool,
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
    /// Why there is no journal (empty: there is one).
    ///
    /// The note the reader gets has to name the cause. "This platform keeps
    /// no history" and "this process was not allowed to open the volume"
    /// are different facts, and only one of them is the platform's: the old
    /// note said the platform, and on a machine where the journal is there
    /// and the process may not ask for it, that was the platform being
    /// blamed for a permission.
    reason: &'static str,
    /// The win32 error that stopped it (0: nothing failed, or there was no
    /// journal to ask).
    code: u32,
}

impl Journal {
    /// No journal, and the reason why.
    fn none_with(reason: &'static str, code: u32) -> Journal {
        Journal {
            id: 0,
            head: 0,
            lowest: 0,
            reason,
            code,
        }
    }

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
    ///
    /// The note names the cause, because the causes are not the same fact:
    /// a platform that keeps no history cannot be fixed by a different
    /// process, and a volume that would not open can. It also says what the
    /// daemon does about it -- nothing, while it runs -- because a reader
    /// who finds a stale index has to know whether to wait for it.
    fn gap_note(&self) -> String {
        if self.head != 0 {
            return String::new();
        }
        let mut note = String::from(" — no change journal: ");
        note.push_str(self.reason);
        if self.code != 0 {
            note.push_str(&format!(" (win32 error {})", self.code));
        }
        note.push_str(", so the changes made since the index was built are not in it");
        note
    }
}

#[cfg(windows)]
impl Journal {
    /// The USN journal of the volume `root` is on, when this process can
    /// read it.
    ///
    /// The handle is opened and closed again here: the feed opens its own
    /// for the daemon's life, and a journal is a property of the volume, not
    /// of the handle.
    ///
    /// Every way this can fail says which way it failed. The volume handle
    /// is opened with GENERIC_READ, and a process without the rights that
    /// needs is refused with ERROR_ACCESS_DENIED -- so a daemon with no
    /// journal said "no change journal on this platform", and on a machine
    /// where the journal is there and the process may not ask for it, that
    /// was the platform being blamed for a permission.
    fn open(root: &[u8]) -> Journal {
        let Ok(path) = std::str::from_utf8(root) else {
            return Journal::none_with("the root is not a UTF-8 path", 0);
        };
        let Some(vol_root) = std::path::Path::new(path)
            .ancestors()
            .last()
            .and_then(|p| p.to_str())
        else {
            return Journal::none_with("the root has no volume root to name", 0);
        };
        let volume = match rgy_discovery_windows::volume::Volume::open(vol_root) {
            Ok(volume) => volume,
            Err(e) => {
                return Journal::none_with(
                    "the volume would not open; a volume handle on Windows needs the rights an elevated process has",
                    volume_error_code(&e),
                );
            }
        };
        let j = match volume.query_usn_journal() {
            Ok(j) => j,
            Err(e) => {
                return Journal::none_with("the journal would not answer", volume_error_code(&e));
            }
        };
        Journal {
            id: j.usn_journal_id,
            head: j.next_usn.max(0) as u64,
            lowest: j.lowest_valid_usn.max(0) as u64,
            reason: "",
            code: 0,
        }
    }
}

/// The win32 error behind a volume failure.
///
/// The variants carry the code and the operation; the note the daemon prints
/// wants the code, because the code is the part a reader can look up and the
/// part that says which of the failures happened.
#[cfg(windows)]
fn volume_error_code(e: &rgy_discovery_windows::volume::VolumeError) -> u32 {
    use rgy_discovery_windows::volume::VolumeError;
    match e {
        VolumeError::Open { code, .. } => *code,
        VolumeError::IoControl { code, .. } => *code,
        VolumeError::JournalUnavailable { code } => *code,
    }
}

#[cfg(not(windows))]
impl Journal {
    /// No journal: inotify and kqueue keep no history to replay.
    fn open(_root: &[u8]) -> Journal {
        Journal::none_with("this platform keeps no change history", 0)
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
fn open_index(
    root: &str,
    index: Option<&str>,
    reindex: bool,
    max_age: Option<Duration>,
    threads: usize,
) -> Ready {
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
                            // The journal is resumed: the backlog it
                            // delivers is the gap, and nothing has to be
                            // walked.
                            relist: false,
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
                            // The tree is walked again: the saved index is
                            // a snapshot, and the files created since it
                            // was built are not in it. "Relisted" means the
                            // tree was walked again and diffed, so the walk
                            // has to happen before the daemon answers.
                            relist: true,
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

    // The platform walk where one exists,
    // the portable walk where none does or where the native one refuses.
    let source = rgy_discovery_dispatch::source_for(root.as_bytes().to_vec(), threads);
    // Which backend walks the tree: the MFT where the platform has one,
    // the portable walk where it does not or where the native one
    // refuses. The pick is the dispatch crate's whole reason to exist,
    // and a reader that cannot see it cannot tell a fast walk from a
    // slow one.
    rgy_engine::errln!(
        "searchd: walking the tree with the {} backend",
        source.backend_name()
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
    // The user's priority rules, beside the index: `<index>.rules`. No file
    // is not an error -- the defaults are the rules. The message the rules
    // file comes back with says what is in use: the defaults when the file
    // could not be read, the parsed rules when a line of it did not parse.
    let (rules, why) = rgy_name_core::rules::Rules::beside(&index_path);
    if let Some(why) = why {
        rgy_engine::errln!("searchd: {why}");
    }
    let mut idx = Index::build_with_rules(listings, watermark, now_secs(), root.as_bytes(), &rules);
    match persist::save(&mut idx, &root_path, &index_path) {
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
        // The tree was just walked: there is nothing left to walk.
        relist: false,
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
        // A client that writes a UTF-8 BOM before its first request would
        // lose that request: the BOM is not JSON, so the answer would be
        // "malformed request" and the client's first keystroke would go
        // unanswered.
        let line = line.trim_start_matches('\u{feff}');
        if line.trim().is_empty() {
            continue;
        }
        let responses = match Request::from_json(line) {
            None => vec![Answer::Frame(Response::Error {
                message: "malformed request".into(),
            })],
            Some(req) => handle(req, ctx),
        };
        for r in responses {
            let _ = writeln!(out, "{}", r.to_json());
        }
        let _ = out.flush();
    }
}

fn handle(req: Request, ctx: &Ctx) -> Vec<Answer> {
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
            //
            // The frame is the daemon's own: the protocol's status shape
            // has no room for the feed's state, and a daemon whose feed
            // died answers on a frozen index.
            responses.push(Answer::Line(status_frame(ctx)));
        }
        Request::Search { q, limit, session } => match Alternatives::parse(&q, &ctx.home) {
            Err(e) => responses.push(Answer::Frame(Response::Error { message: e })),
            Ok(alts) => {
                for w in &alts.warnings {
                    responses.push(Answer::Frame(Response::Warning { message: w.clone() }));
                }
                let live_guard = ctx.live.read().unwrap();
                let searcher = Searcher { live: &live_guard };
                // The merge is the searcher's, and the identity it keys on is
                // the one that matters: keying it on `Hit::idx` keeps one
                // entry and drops the rest, because every live entry shares
                // the `u32::MAX` sentinel. A query that matched three files
                // added since the last compaction answered with one of
                // them -- and the daemon's answer is what the CLI reads.
                //
                // With a session the answer is the same one, computed from
                // the session's previous result set: a
                // query that narrows the previous one is re-ranked instead
                // of walked again.
                //
                // The session is the client's to name and the daemon's to
                // hand out: a request that names none gets one from here,
                // and the answer carries it. That is the round trip that
                // makes the next keystroke a re-rank of this answer — a
                // client that cannot name the session it is in cannot ask
                // for that.
                //
                // The daemon's own space is above the clients' — the ids a
                // client names are small counters, and the daemon hands
                // out ids above them. A client that named one above the
                // line would meet a session the daemon handed to somebody
                // else, and the two would share a result set. So the
                // daemon does not trust the client on this: a name from
                // the daemon's own space is not a session, it is a claim
                // on somebody else's, and the daemon names a fresh one
                // instead. The answer says which session it was computed
                // for, so the client learns the name it has to use.
                //
                // **Unless the daemon issued it.** The ids the daemon
                // hands out are unguessable, so a client that names one
                // of them is holding it — that is the round trip the
                // protocol promises, and it is the re-rank that makes the
                // next keystroke cheap. The daemon used to refuse these
                // ids too, and the answer it put on the wire was a name it
                // would not take back: the ids came out of a counter, so
                // a client could name somebody else's, and the daemon
                // refused all of them to keep that from happening. The
                // ids are random now, and the daemon takes back the ones
                // it issued and only those.
                //
                // And both spaces stay under the wire's exactness limit,
                // because the protocol carries a number as an `f64`: an
                // id above `2^53` would reach the client as a different
                // number, and the client would ask for the re-rank of the
                // wrong session.
                let session = match session {
                    Some(id) if id >= Ctx::DAEMON_SESSIONS => {
                        if ctx.has_session(id) {
                            Some(id)
                        } else {
                            ctx.new_session()
                        }
                    }
                    Some(id) => Some(id),
                    None => ctx.new_session(),
                };
                let hits = match session {
                    Some(id) => ctx.search_with_session(id, &searcher, &alts.alts, limit),
                    // No session to name: the answer is a whole search, and
                    // it carries no session, so the client knows there is
                    // nothing to re-rank.
                    None => searcher.search_alternatives(&alts.alts, limit),
                };
                let mut buf = Vec::new();
                // The characters the query matched: the span the score came
                // from, and the thing an interface needs to say *which* part
                // of the name matched. With more than one alternative a hit
                // does not say which alternative it came from, so the span
                // would be a guess and it is left out — the same rule the
                // CLI's `--json` follows, so both say the same thing.
                let single = match alts.alts.as_slice() {
                    [one] => Some(&one.tokens),
                    _ => None,
                };
                // The root is spelled once for the whole answer and not once
                // per hit: it is the same path every time, and spelling it
                // again is spelling it again — the console counts the paths
                // it could not spell, and a root spelled a hundred times is
                // one path.
                let root_text = rgy_engine::console::spell(&ctx.root);
                let wire: Vec<WireHit> = hits
                    .iter()
                    .map(|h| {
                        // An overlay hit already carries its own path: it is
                        // an entry the base index does not have, so reading it
                        // back from the base would index `parent[u32::MAX]`
                        // -- out of bounds -- and panic.
                        let (full, name) = match &h.over {
                            // The overlay's paths are the index's internal
                            // form (`/`); the answer is the platform's.
                            Some(path) => {
                                // The path is spelled the way the tree spells it,
                                // and a path that is not UTF-8 is spelled with a
                                // replacement character — and the count of those
                                // is said out loud (console::spell).
                                let text = rgy_engine::console::spell(path);
                                let name =
                                    text.rsplit(['/', '\\']).next().unwrap_or(&text).to_string();
                                (text.replace('/', std::path::MAIN_SEPARATOR_STR), name)
                            }
                            None => {
                                live_guard.base.path(h.idx as usize, &mut buf);
                                let rel = rgy_engine::console::spell(&buf);
                                buf.clear();
                                let name =
                                    rel.rsplit(['/', '\\']).next().unwrap_or(&rel).to_string();
                                let mut full = root_text.clone();
                                let rel = rel.trim_start_matches(['/', '\\']);
                                if !rel.is_empty() {
                                    full.push(std::path::MAIN_SEPARATOR);
                                    full.push_str(rel);
                                }
                                (full, name)
                            }
                        };
                        let span = match single {
                            Some(tokens) => {
                                rgy_name_core::name_query::matched_chars(name.as_bytes(), tokens)
                            }
                            None => Vec::new(),
                        };
                        // Whether the hit is a folder, from the index's own
                        // kind. The path does not say: a folder's path is
                        // spelled like a file's, and an interface that
                        // shows a folder with a file's icon shows the
                        // wrong picture of the thing the user is about to
                        // open.
                        //
                        // An overlay hit is not in the base index, so the
                        // base's kind byte is not its kind — the overlay
                        // carries a kind of its own, and that is where the
                        // answer comes from. The overlay holds folders as
                        // well as files: a folder created since the last
                        // compaction is an overlay entry, and answering it
                        // as a file would draw a file's icon over a folder.
                        let folder = is_folder(&live_guard, h);
                        WireHit {
                            path: full,
                            score: h.score(),
                            span,
                            folder,
                        }
                    })
                    .collect();
                let count = wire.len();
                let took = t0.elapsed().as_micros() as u64;
                responses.push(Answer::Frame(Response::Hits {
                    hits: wire,
                    took_us: took,
                    // The answer says whether it is whole: a list shorter
                    // than the limit is the whole answer, and one that
                    // reached the limit may have been cut short. The rule
                    // is the one every route that answers a name query
                    // follows — the whole tree and the scope, the fresh
                    // search and the session's re-rank.
                    complete: rgy_proto::name_answer_is_complete(count, limit),
                    // The session the answer belongs to. The client sends
                    // it back with the next question, and the daemon
                    // re-ranks this answer instead of walking the index
                    // again. It is absent when the daemon could not name
                    // one, and an answer with no session on it says the
                    // client has nothing to re-rank.
                    session,
                }));
                responses.push(Answer::Frame(Response::Done { count }));
            }
        },
        Request::Grep { q, limit } => responses.extend(grep(&q, limit, ctx)),
        // The content route has no session: the name session holds the
        // name hits and the content answer is the files on the disk, so
        // there is nothing to re-rank. The request does not carry one, and
        // a request that does is read as the request it was before the
        // field existed.
        Request::Content { q, paths, limit } => {
            responses.extend(scoped_content(&q, &paths, limit, ctx))
        }
        Request::Resolve { path } => responses.push(resolve(&path, ctx)),
    }
    warm_stop();
    responses
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

/// A content search over a scope: the files a name search already chose.
///
/// This is the second layer of an interface, and **the scope is what makes
/// it affordable**. A content search over the tree reads the tree; a
/// content search over the two hundred files a name search chose reads two
/// hundred files.
///
/// Three ways to answer, and the daemon takes the cheapest one it has:
///
/// 1. **the pack store** (`--tv-dir`): the corpus is packed once and
///    mmap'd, and the scope is applied to its answer — the fastest answer
///    there is, and the scope costs nothing on top of it;
/// 2. **the adapters**: a PDF, a `.docx`, a zip, a mail file — the bytes on
///    disk are not searchable text, and an adapter is what turns them into
///    text. This is the rgx layer, and it is the reason the vendored
///    adapters exist at all;
/// 3. **the files**: the plain path, which is rgx's own regex engine over
///    raw bytes, read by the daemon's read threads.
///
/// The pattern is the query language's: `grep:` is literal and `regex:` is
/// a regex, and a bare word is literal — the same words the rest of rgy
/// answers with, so the second layer speaks the first layer's language.
fn scoped_content(q: &str, paths: &[String], limit: usize, ctx: &Ctx) -> Vec<Answer> {
    let t0 = Instant::now();
    let home = ctx.home.clone();
    let alts = match Alternatives::parse(q, &home) {
        Ok(a) => a,
        Err(e) => return vec![Answer::Frame(Response::Error { message: e })],
    };
    // The warnings travel with the answer: a query that failed closed is
    // part of what the client has to show.
    let mut out: Vec<Answer> = alts
        .warnings
        .iter()
        .map(|w| Answer::Frame(Response::Warning { message: w.clone() }))
        .collect();
    // The pattern and its mode, from the first alternative that carries
    // one. A bare word is a literal, and `regex:` asks for the engine.
    //
    // A rejected alternative carries no pattern the search may use: the
    // query language refused it -- an unknown `type:` or a `re:` that does
    // not compile -- and the reason was said above, so it matches nothing.
    // The same rule the store route follows, so the same query is answered
    // the same way by every route.
    //
    // And when every alternative was refused, the answer is no files at
    // all: the query failed closed, and a search that ran anyway would be
    // the "searches everything" the refusal exists to prevent.
    let refused_every = !alts.alts.is_empty() && alts.alts.iter().all(|a| a.never);
    if refused_every {
        out.push(Answer::Frame(Response::GrepFiles {
            files: Vec::new(),
            took_us: t0.elapsed().as_micros() as u64,
            // Nothing was read, and nothing was left out: the answer is
            // whole because it is empty.
            complete: true,
            skipped: 0,
            skips: Vec::new(),
        }));
        out.push(Answer::Frame(Response::Done { count: 0 }));
        return out;
    }
    let (pattern, mode) = alts
        .alts
        .iter()
        .filter(|a| !a.never)
        .find_map(|a| a.grep.as_ref().map(|p| (p.clone(), a.grep_mode)))
        .unwrap_or_else(|| (q.to_string(), rgy_name_core::name_query::GrepMode::Literal));

    // The scope, in the bytes the filesystem speaks.
    let scope: Vec<Vec<u8>> = paths.iter().map(|p| p.as_bytes().to_vec()).collect();
    if scope.is_empty() {
        out.push(Answer::Frame(Response::GrepFiles {
            files: Vec::new(),
            took_us: t0.elapsed().as_micros() as u64,
            // Nothing was to be read, and nothing was left out.
            complete: true,
            skipped: 0,
            skips: Vec::new(),
        }));
        out.push(Answer::Frame(Response::Done { count: 0 }));
        return out;
    }

    // 1. The pack store.
    if let Some(dir) = &ctx.tv_dir {
        return match rgy_engine::tv::grep_tv(dir, &pattern) {
            Ok(fs) => {
                // The scope filters the store's answer and the limit cuts
                // what is left — the store searched the corpus whole, the
                // scope says which of those files the caller asked about,
                // and the limit says how many of them the caller gets.
                //
                // The order is the contract: the limit bounds the ANSWER,
                // so it comes after the scope and not before it. A limit
                // that came first would cut the corpus-wide answer, and the
                // scope would then filter what was left — a different
                // answer, and one that is missing the scope's files.
                //
                // The scope is ALSO the same scope in the spelling the
                // filesystem resolves it to. The store indexes its corpus
                // under the canonical root — the root it resolved — and the
                // caller's scope may spell that root the way the caller
                // was given it. On Windows the two spellings are the same
                // file and different bytes: `C:\Users\RUNNER~1\AppData\...`
                // and `C:\Users\runneradmin\AppData\...` are one folder,
                // and a scope that carried the first one matched nothing
                // in a store that holds the second. The bytes are
                // compared the cheap way first, and the resolved spelling
                // is the fallback: the answer is the same file either
                // way, and a scope that names a file that is not there
                // resolves to nothing and falls back to its own bytes.
                let resolved: Vec<Vec<u8>> = scope
                    .iter()
                    .map(|p| rgy_discovery::file::canonicalize(p).unwrap_or_else(|| p.clone()))
                    .collect();
                let files: Vec<GrepFile> = fs
                    .into_iter()
                    .filter(|f| {
                        let path = f.path.as_bytes();
                        scope.iter().any(|s| same_path(s, path))
                            || resolved.iter().any(|s| same_path(s, path))
                    })
                    .take(limit)
                    .map(|f| GrepFile {
                        path: platform_path(f.path.as_bytes()),
                        lines: f.lines.into_iter().map(|(n, t)| (n as usize, t)).collect(),
                    })
                    .collect();
                let count = files.len();
                out.push(Answer::Frame(Response::GrepFiles {
                    files,
                    took_us: t0.elapsed().as_micros() as u64,
                    // The store searched the corpus whole, and the scope is
                    // what its answer was filtered by: every file in the
                    // scope was read. **That is not what `complete` says.**
                    // The flag is about the limit — the answer may have been
                    // cut short by it — so it is the rule the other routes
                    // follow: an answer with fewer files than the limit is
                    // the whole answer, and one that reached the limit is a
                    // page.
                    complete: rgy_proto::name_answer_is_complete(count, limit),
                    skipped: 0,
                    skips: Vec::new(),
                }));
                out.push(Answer::Frame(Response::Done { count }));
                out
            }
            Err(e) => vec![Answer::Frame(Response::Error {
                message: format!("tv store: {e}"),
            })],
        };
    }

    let g = match Grep::new(&pattern, mode) {
        Ok(g) => g,
        Err(e) => {
            // A pattern the engine refuses is a warning and not the end of
            // the answer: the whole-tree route says it that way, and the
            // manual says the warning is part of the answer, not the end
            // of it. This route used to answer `{"op":"error"}` and
            // nothing else — the one path that stopped instead of
            // answering with nothing and saying why.
            out.push(Answer::Frame(Response::Warning { message: e }));
            out.push(Answer::Frame(Response::GrepFiles {
                files: Vec::new(),
                took_us: t0.elapsed().as_micros() as u64,
                // Nothing was read, and nothing was left out: the answer is
                // whole because it is empty.
                complete: true,
                skipped: 0,
                skips: Vec::new(),
            }));
            out.push(Answer::Frame(Response::Done { count: 0 }));
            return out;
        }
    };

    // 2. The files: the ones the adapters claim are read through them, and
    // the rest are read as the bytes they are.
    //
    // The reader is the registry, and the registry is built ONCE, on the
    // first search that needs it, and kept: it is ten families of adapters,
    // and building it again for every keystroke of the second layer would
    // cost more than the search it serves.
    let res = content::verify_paths_with(&g, &scope, limit, Some(&rgy_adapters::Adapters));
    if let Some(why) = res.error.clone() {
        // A lane panicked: the answer is what the other lanes found, and it
        // says so rather than pretending.
        return vec![Answer::Frame(Response::Error { message: why })];
    }
    // What the search did not read, said on the wire: a truncated search
    // and a complete one are different answers. The files are the ones a
    // budget stopped the search from reaching, and the placeholders it
    // refused to download: the files the limit left behind are not among
    // them, because the limit is the caller's own.
    let scanned_complete = res.complete;
    let (skipped, skips) = unread_files(scope.len(), res.read, scanned_complete, &res.skipped);
    let mut files: Vec<GrepFile> = Vec::new();
    for f in res.files {
        files.push(GrepFile {
            path: platform_path(&f.path),
            lines: f.lines,
        });
    }

    let count = files.len();
    // The answer is whole when the search read everything it was given and
    // the limit did not cut it short -- the same rule the whole-tree grep
    // follows, so the two routes say the same thing about the same answer.
    //
    // A file that was not read is a file the answer does not cover: the
    // search was given it and could not open it, and an answer that says
    // "whole" about a file nobody read is a wrong fact. The count of the
    // files that were not read is on the wire next to the flag, and the
    // flag has to agree with it.
    let complete = content_answer_is_complete(count, limit, scanned_complete, skipped);
    out.push(Answer::Frame(Response::GrepFiles {
        files,
        took_us: t0.elapsed().as_micros() as u64,
        complete,
        skipped,
        skips,
    }));
    out.push(Answer::Frame(Response::Done { count }));
    out
}

/// A path, spelled the way the platform spells it.
///
/// The tree's own form uses `/` everywhere — that is the tree's form, not
/// the platform's — and the name layer's answers are already spelled with
/// the platform's separator. The content layer's answers came out with the
/// tree's form, so one daemon said one file two ways, and a window that
/// shows both layers showed the same file twice.
fn platform_path(path: &[u8]) -> String {
    rgy_engine::console::spell(path).replace('/', std::path::MAIN_SEPARATOR_STR)
}

/// Two paths are the same file when they are the same bytes, or when they
/// are the same bytes once the separators are spelled the same way.
///
/// The scope comes from the name index and the answer from the pack store,
/// and the two do not always spell the separators the same.
fn same_path(a: &[u8], b: &[u8]) -> bool {
    if a == b {
        return true;
    }
    let norm = |p: &[u8]| {
        let s = rgy_engine::console::spell(p).replace('\\', "/");
        let s = s.trim_start_matches("./").trim_end_matches('/');
        // The case is folded only where the filesystem folds it: two names
        // that differ in case are two files on Linux and macOS, and one file
        // on Windows.
        if cfg!(windows) {
            s.to_ascii_lowercase()
        } else {
            s.to_owned()
        }
    };
    norm(a) == norm(b)
}

/// Resolve a path through the lookup surface.
///
/// The five indexes answer it: the `file_path` and `folder_path` indexes
/// give the path of the entry, and the `folder_backend` index the volume
/// that owns the folder.
fn resolve(path: &str, ctx: &Ctx) -> Answer {
    let live_guard = ctx.live.read().unwrap();
    let index = &live_guard.base;
    let lookup = rgy_name_core::lookup::Lookup::new(index);
    let Some(entry) = index.lookup(path.as_bytes()) else {
        return Answer::Frame(Response::Error {
            message: format!("no entry at {path}"),
        });
    };
    let kind = index.kind()[entry as usize] & 3;
    let folder = kind == rgy_discovery::walk::KIND_DIR;
    // The backend is the volume that owns the folder; a file has none of
    // its own, so it inherits the folder's.
    let backend = if folder {
        lookup.folder_backend(entry).map_or(0, |b| b.id())
    } else {
        // The file's folder is one up: the entry's parent.
        match index.parent()[entry as usize] {
            0 => 0,
            d => lookup
                .folder_backend(index.dir_entry()[d as usize])
                .map_or(0, |b| b.id()),
        }
    };
    // The answer is spelled the way every path the daemon says is: the
    // index's own form is root-relative with `/`, and the platform's is
    // the root and the platform's separator -- one daemon does not say
    // one file two ways. The spelling is `platform_path`'s, because a
    // second copy of the rule is a second way to drift.
    let rel = rgy_engine::console::spell(&lookup.file_path(entry));
    let rel = rel.trim_start_matches(['/', '\\']);
    let mut full = rgy_engine::console::spell(&ctx.root);
    full.push('/');
    full.push_str(rel);
    Answer::Frame(Response::Resolved {
        path: platform_path(full.as_bytes()),
        folder,
        backend,
    })
}

/// The files the search did not read for a reason of its own, and why.
///
/// The ones a budget stopped it from reaching, and the placeholders it
/// refused to download. The files the limit left behind are not among
/// them: the limit is the caller's, and a search that stopped because it
/// already had enough read what it was asked to read.
///
/// The count is what the protocol has always carried, and it stays: a
/// reader that does not know the reasons keeps working. The reasons are
/// what make the count worth reading — "skipped" alone leaves the user
/// wondering whether the file was empty, locked, or never there.
///
/// The files fall into two kinds, and the kinds are different facts:
///
/// * the files the search named and refused — a cloud placeholder it would
///   have to download — carry their path and their reason;
/// * the files the budget stopped the search from reaching were never
///   named, so they carry the reason and no path: a path made up would be
///   a path the user cannot open.
///
/// The whole-tree route and the scoped route both answer this, and the
/// rule lives here so that the two cannot drift.
fn unread_files(
    candidates: usize,
    read: usize,
    complete: bool,
    skipped: &[rgy_name_core::content::Skipped],
) -> (usize, Vec<rgy_proto::Skip>) {
    let never_reached = if complete {
        0
    } else {
        candidates.saturating_sub(read)
    };
    // The files the search named, grouped by the reason: a tree with a
    // thousand placeholders is one thing that happened, and a thousand
    // entries would be a thousand lines of wire for one fact.
    let mut groups: Vec<(String, usize, Option<String>)> = Vec::new();
    for s in skipped {
        let path = Some(platform_path(&s.path));
        match groups.iter_mut().find(|(reason, _, _)| reason == s.reason) {
            Some((_, count, first)) => {
                *count += 1;
                if first.is_none() {
                    *first = path;
                }
            }
            None => groups.push((s.reason.to_string(), 1, path)),
        }
    }
    let mut out: Vec<rgy_proto::Skip> = groups
        .into_iter()
        .map(|(reason, count, path)| rgy_proto::Skip {
            reason,
            count,
            path,
        })
        .collect();
    if never_reached > 0 {
        out.push(rgy_proto::Skip {
            reason: "the search's budget ran out before it reached them".to_string(),
            count: never_reached,
            path: None,
        });
    }
    (never_reached + skipped.len(), out)
}

/// Whether the content answer is whole.
///
/// The answer is whole when the limit did not cut it and every candidate
/// was read. A file that was not read is a file the answer does not
/// cover: the search was given it and could not open it, and an answer
/// that says "whole" about a file nobody read is a wrong fact. The count
/// of the files that were not read is on the wire next to the flag, and
/// the flag has to agree with it.
///
/// The rule lives here so that the scoped route and the whole-tree route
/// cannot drift: they answered the same search two different ways when
/// the flag was computed in two places.
fn content_answer_is_complete(
    count: usize,
    limit: usize,
    scanned_complete: bool,
    skipped: usize,
) -> bool {
    rgy_proto::name_answer_is_complete(count, limit) && scanned_complete && skipped == 0
}

fn grep(q: &str, limit: usize, ctx: &Ctx) -> Vec<Answer> {
    let t0 = Instant::now();
    let mut files: Vec<GrepFile> = Vec::new();
    // TV pack store answers when the daemon was given one: the corpus
    // bytes are packed once and mmap'd (plan 4.3); the direct engine
    // below stays for trees without a store.
    if let Some(dir) = &ctx.tv_dir {
        let mut out: Vec<Answer> = Vec::new();
        match Alternatives::parse(q, &ctx.home) {
            Ok(alts) => {
                // The warnings travel with the answer, as they do on the
                // route below: a query that failed closed is part of what
                // the client has to show. The store route used to drop
                // them, so the same daemon answered the same query two
                // different ways depending on whether it had a store.
                for w in &alts.warnings {
                    out.push(Answer::Frame(Response::Warning { message: w.clone() }));
                }
                for alt in &alts.alts {
                    // A rejected alternative matches nothing, and the
                    // reason was said above: the store is not asked.
                    if alt.never {
                        continue;
                    }
                    // The limit is the limit. The store answers with every
                    // file that matched, and a launcher that draws fifty
                    // thousand rows is a launcher that froze.
                    if files.len() >= limit {
                        break;
                    }
                    let pat = alt.grep.clone().unwrap_or_else(|| q.to_string());
                    match rgy_engine::tv::grep_tv(dir, &pat) {
                        Ok(fs) => {
                            for f in fs {
                                if files.len() >= limit {
                                    break;
                                }
                                files.push(GrepFile {
                                    path: platform_path(f.path.as_bytes()),
                                    lines: f
                                        .lines
                                        .into_iter()
                                        .map(|(n, t)| (n as usize, t))
                                        .collect(),
                                });
                            }
                        }
                        Err(e) => {
                            return vec![Answer::Frame(Response::Error {
                                message: format!("tv store: {e}"),
                            })];
                        }
                    }
                }
            }
            Err(e) => return vec![Answer::Frame(Response::Error { message: e })],
        }
        let count = files.len();
        let took = t0.elapsed().as_micros() as u64;
        out.push(Answer::Frame(Response::GrepFiles {
            files,
            took_us: took,
            // The store searched the corpus whole; the limit is what
            // can cut the answer short.
            complete: rgy_proto::name_answer_is_complete(count, limit),
            skipped: 0,
            // The store read every file it was asked to read: there is
            // nothing it did not read.
            skips: Vec::new(),
        }));
        out.push(Answer::Frame(Response::Done { count }));
        return out;
    }
    match Alternatives::parse(q, &ctx.home) {
        Err(e) => vec![Answer::Frame(Response::Error { message: e })],
        Ok(alts) => {
            let mut out = Vec::new();
            for w in &alts.warnings {
                out.push(Answer::Frame(Response::Warning { message: w.clone() }));
            }
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
            // What the search did not read, said on the wire: the facts
            // of every alternative are the facts of the answer.
            let mut complete = true;
            let mut skipped = 0usize;
            // Why the search did not read them, said the same way: the
            // count alone left the user wondering whether the file was
            // empty, locked, or never there.
            let mut skips: Vec<rgy_proto::Skip> = Vec::new();
            // A pattern the engine refuses is said, not dropped. The
            // alternative it belonged to matches nothing, and the answer
            // says so: a query that answered with the other alternatives
            // and said nothing would be a query that quietly searched less
            // than it was asked for.
            let mut refused: Vec<String> = Vec::new();
            for (pat, mode, mut filt) in jobs {
                let g = match Grep::new(&pat, mode) {
                    Ok(g) => g,
                    Err(e) => {
                        refused.push(e);
                        continue;
                    }
                };
                // The request's limit is the limit. The candidates the scan
                // names and the files the index found are both counted
                // against it, so a query with three alternatives answers
                // with the limit and not three times the limit: the name
                // path merges its alternatives and truncates
                // (name_query::merge_hits), and the content path has to say
                // the same thing.
                filt.limit = limit;
                // The content index, when the daemon has one: the postings
                // rule most files out before they are read, and the files
                // the index cannot vouch for are read anyway. The union is
                // the scan's exact answer either way.
                //
                // The first content search is what asks for an index: the
                // thread that builds it starts then, and a daemon that is
                // never asked to grep never pays for one.
                ctx.wants_content.store(true, Ordering::Relaxed);
                // The plan is the only part that needs the live index: it
                // says which candidates there are. The reads below are I/O
                // — up to the whole budget — and the feed's writer must not
                // be held off for them, so the lock covers the plan and not
                // the reads.
                let (covered, bare, mut r) = {
                    let index = ctx.content.lock().unwrap();
                    let live = ctx.live.read().unwrap();
                    content::plan(
                        &g,
                        &filt,
                        &ctx.root,
                        &live,
                        ctx.home.as_bytes(),
                        index.as_ref(),
                    )
                };
                r.files.retain(|f| covered.contains(&f.path));
                // The room the answer has left, across the alternatives.
                let room = limit.saturating_sub(files.len() + r.files.len());
                // The same reader the scoped route uses: a file the
                // adapters claim is read through them, whichever route the
                // query took.
                let rest =
                    content::verify_paths_with(&g, &bare, room, Some(&rgy_adapters::Adapters));
                if let Some(why) = r.error.clone().or_else(|| rest.error.clone()) {
                    // A lane panicked: the answer is what the other lanes
                    // found, and it says so rather than pretending.
                    return vec![Answer::Frame(Response::Error { message: why })];
                }
                let index_candidates = if r.complete { 0 } else { r.candidates };
                let index_read = if r.complete { 0 } else { r.read };
                let bare_candidates = if rest.complete { 0 } else { bare.len() };
                let bare_read = if rest.complete { 0 } else { rest.read };
                r.files.extend(rest.files);
                r.read += rest.read;
                // The placeholders the index's own scan refused, and the
                // ones this scan refused: both are files the search did
                // not read. They are one list here so that the reasons
                // come out of one place, and the count is the count it
                // has always been.
                let mut not_read = std::mem::take(&mut r.skipped);
                not_read.extend(rest.skipped);
                r.candidates = covered.len() + bare.len();
                // The scan read everything it was given: that is this
                // alternative's own fact, across both the index's scan and
                // the bare scan. Whether the *answer* is whole is the
                // total's fact -- the alternatives make one answer
                // together, and one alternative's own count says nothing
                // about it -- so the limit is settled at the end.
                r.complete = r.complete && rest.complete;
                complete = complete && r.complete;
                let (n, groups) = unread_files(
                    index_candidates + bare_candidates,
                    index_read + bare_read,
                    r.complete,
                    &not_read,
                );
                skipped += n;
                skips.extend(groups);
                for f in r.files {
                    files.push(GrepFile {
                        path: platform_path(&f.path),
                        lines: f.lines,
                    });
                }
            }
            // The limit is the limit, across the alternatives: the request
            // asked for ten files and ten files is what it gets. The room
            // above keeps the reads inside it; this keeps the index's own
            // matches inside it too.
            files.truncate(limit);
            let count = files.len();
            let took = t0.elapsed().as_micros() as u64;
            // The warning comes before the answer it belongs to, so a reader
            // that reads the frames in order hears the refusal first.
            for w in &refused {
                out.push(Answer::Frame(Response::Warning { message: w.clone() }));
            }
            out.push(Answer::Frame(Response::GrepFiles {
                files,
                took_us: took,
                // The answer is whole when the limit did not cut it and
                // every scan read everything it was given. The same rule
                // the other two routes follow, so the three routes say the
                // same thing about the same answer.
                //
                // A file that was not read is a file the answer does not
                // cover, and the count of them is on the wire next to the
                // flag: the flag says "whole" only when it agrees with it.
                complete: content_answer_is_complete(count, limit, complete, skipped),
                skipped,
                skips,
            }));
            out.push(Answer::Frame(Response::Done { count }));
            out
        }
    }
}

// --- the rgx --serve compatibility front -----------------------------------

/// How many files the compatibility front answers with.
const SERVE_LIMIT: usize = 100;

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
        let (pattern, fixed, scope) = match serve_pattern(&argv) {
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
                            &platform_path(f.path.as_bytes()),
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
            // The scope: the files the caller named, or the whole tree when
            // it named none. A caller that already chose its files must not
            // pay for a tree it did not ask about — that is the difference
            // between a search and a walk.
            let paths: Vec<Vec<u8>> = if scope.is_empty() {
                // The candidates the scan names are the request's limit
                // worth, and the hard cap is the ceiling: the whole tree is
                // not enumerated to answer a hundred files.
                let q = Query {
                    limit: SERVE_LIMIT,
                    ..Default::default()
                };
                content::scan_paths(&ctx.root, searcher.live, q)
            } else {
                scope.iter().map(|p| p.as_bytes().to_vec()).collect()
            };
            let res = content::verify(&g, &paths, SERVE_LIMIT);
            if let Some(why) = res.error.clone() {
                // A lane panicked: the answer is what the other lanes found,
                // and it says so rather than pretending.
                serve_end(&mut out, seq, &tag, 2, false, Some(&why));
                continue;
            }
            for f in &res.files {
                matched = true;
                serve_file(&mut out, &platform_path(&f.path), &f.lines);
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
/// `-F`/`--fixed-strings` makes it literal; the positionals after the
/// pattern are the **scope** — the files to search, and not the tree.
fn serve_pattern(argv: &[String]) -> Option<(String, bool, Vec<String>)> {
    let mut fixed = false;
    let mut pattern = None;
    let mut scope = Vec::new();
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
            a => scope.push(a.to_string()),
        }
        i += 1;
    }
    pattern.map(|p| (p, fixed, scope))
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

/// A live-change feed, and the name of the platform it comes from.
///
/// The name is for the daemon's own line: a reader who watches the index
/// change wants to know what is watching it, and a silent thread says
/// nothing.
struct Feed {
    name: &'static str,
    watch: Box<dyn rgy_discovery::Watch>,
}

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
fn open_watch(root: &[u8], journal: &Journal, watermark: u64) -> Option<Feed> {
    // The journal first: it has the history, and the changes that happened
    // while the daemon was off are in it.
    if let Some(feed) = usn_watch(root, journal, watermark) {
        return Some(feed);
    }
    // The fallback: the tree's own directory notifications. They have no
    // history -- the gap the journal would have replayed stays a gap -- but
    // they need no privileges, and a volume handle needs the rights an
    // elevated process has. So the daemon follows the tree from the moment
    // it starts, where the journal is out of reach.
    let watch = rgy_discovery_windows::RdcwWatch::new(root).ok()?;
    Some(Feed {
        name: "the tree's directory notifications",
        watch: Box::new(watch),
    })
}

/// The journal's feed, where this process may read the journal.
#[cfg(windows)]
fn usn_watch(root: &[u8], journal: &Journal, watermark: u64) -> Option<Feed> {
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
    Some(Feed {
        name: "the USN journal",
        watch: Box::new(watch),
    })
}

#[cfg(target_os = "linux")]
fn open_watch(root: &[u8], _journal: &Journal, _watermark: u64) -> Option<Feed> {
    // The root is the bytes the filesystem gave, and the watch takes them
    // the same way: a Unix path is not a UTF-8 string.
    let watch = rgy_discovery_linux::InotifyWatch::new(root).ok()?;
    Some(Feed {
        name: "inotify",
        watch: Box::new(watch),
    })
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
fn open_watch(root: &[u8], _journal: &Journal, watermark: u64) -> Option<Feed> {
    // Start where the index stands, and from now otherwise: an event id of
    // zero asks the framework for the whole history it still keeps, which
    // is a flood of folders to re-list at startup.
    let since = if watermark > 0 {
        watermark
    } else {
        rgy_discovery_macos::current_id()
    };
    let watch = rgy_discovery_macos::FseventsWatch::open(root, since, FSEVENTS_LATENCY);
    Some(Feed {
        name: "FSEvents",
        watch: Box::new(watch),
    })
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
fn open_watch(_root: &[u8], _journal: &Journal, _watermark: u64) -> Option<Feed> {
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
                // The sessions go with it: a client that reconnects with a
                // new id would otherwise leave a session behind for every
                // connection it ever made.
                ctx.trim_sessions(IDLE);
            }
        });
}

/// Spawn the content thread: keep the content index fresh, so a `grep:`
/// reads less.
///
/// The index is the trigram postings of the tree's files. With it, the
/// grams rule most files out before they are read; without it, the search
/// reads every candidate the name filters name. The daemon is the only
/// process that lives long enough to pay for building it, so it is the one
/// that builds it -- but only once a content search has asked for one: a
/// daemon that is never asked to grep never pays for the index either.
///
/// The build reads the files it indexes, so it goes in batches: a burst
/// that reads the whole disk would make every keystroke of a search running
/// beside it slower.
fn spawn_content(ctx: &std::sync::Arc<Ctx>, dir: std::path::PathBuf) {
    /// How many files a batch indexes.
    const BATCH: usize = 512;
    /// How long the thread waits for a content search to ask for an index.
    const TICK: std::time::Duration = std::time::Duration::from_millis(50);

    let ctx = ctx.clone();
    let _ = std::thread::Builder::new()
        .name("searchd-content".into())
        .spawn(move || {
            // Nothing to build until a content search asks for one.
            while !ctx.wants_content.load(Ordering::Relaxed) {
                std::thread::sleep(TICK);
            }
            // The diff: what should be indexed there, against what is indexed
            // now.
            //
            // It is the expensive half — it walks the whole index, rebuilds
            // every path and sorts them — and the tree does not change twenty
            // times a second. So it runs when the tree moved, and the build
            // works through what it returned instead of asking again after
            // every batch.
            let diff = || {
                let live = ctx.live.read().unwrap();
                // What should be indexed there: the cheap half,
                // over the metadata the live index already holds.
                // The root's tree, so a file that appeared while
                // the daemon was off is in the batch too.
                let wants = content::wants(
                    &live,
                    ctx.home.as_bytes(),
                    &[],
                    std::slice::from_ref(&ctx.root),
                    &ctx.root,
                );
                drop(live);
                let mut g = ctx.content.lock().unwrap();
                let c = g.get_or_insert_with(|| content::Content::open(dir.clone()));
                c.diff(wants)
            };
            // The docs to index, and how far the build got through them.
            // `None` until the first diff: the content index is empty then,
            // and everything in the tree is missing from it.
            let mut todo: Option<content::Docs> = None;
            let mut at = 0usize;
            // The tree as it was when the todo was made.
            let mut seen = ctx.changed.count();
            loop {
                let done = match &todo {
                    None => true,
                    Some(docs) => at >= docs.len(),
                };
                if done {
                    if todo.is_some() {
                        // Nothing left to build: wait for the tree to move.
                        // The watch thread rings the bell when the overlay
                        // moved, and the tree does not move on its own.
                        seen = ctx.changed.wait(seen);
                    }
                    todo = Some(diff());
                    at = 0;
                    continue;
                }
                if ctx.changed.count() != seen {
                    // The tree moved while the batch was being built: the diff
                    // runs again, so the build works on what the tree holds
                    // now.
                    seen = ctx.changed.count();
                    todo = Some(diff());
                    at = 0;
                    continue;
                }
                let Some(docs) = todo.as_ref() else { continue };
                let n = (docs.len() - at).min(BATCH);
                // Each batch is a segment of its own, and the segment's id
                // comes from the index that will hold it.
                let id = {
                    let mut g = ctx.content.lock().unwrap();
                    let c = g.get_or_insert_with(|| content::Content::open(dir.clone()));
                    c.alloc_id()
                };
                // The build reads the files, so it happens with
                // the lock released: a search that runs beside it
                // must not wait for a read of the whole batch.
                if let Some(seg) = content::build_segment(&dir, id, docs, at..at + n) {
                    // The segments pile up: a batch of files is a
                    // segment, and a thousand batches are a
                    // thousand segments to read on every query. The
                    // plan says when enough of them have gathered
                    // to be worth merging into one.
                    let (ids, segs, mid) = {
                        let mut g = ctx.content.lock().unwrap();
                        let Some(c) = g.as_mut() else { continue };
                        c.push(seg);
                        let Some(ids) = c.merge_plan() else { continue };
                        // The segments are cloned (the postings are
                        // a shared mmap) so the merge can run with
                        // the lock released: it reads eight
                        // segments and writes a new one, and a
                        // search running beside it must not wait.
                        let segs: Vec<content::Segment> =
                            c.segments(&ids).into_iter().cloned().collect();
                        let mid = c.alloc_id();
                        (ids, segs, mid)
                    };
                    let refs: Vec<&content::Segment> = segs.iter().collect();
                    if let Some(merged) = content::merge(&dir, mid, &refs) {
                        let mut g = ctx.content.lock().unwrap();
                        if let Some(c) = g.as_mut() {
                            c.replace(&ids, merged);
                        }
                    }
                }
                at += n;
            }
        });
}

/// Spawn the live-update thread: apply the platform `Watch`'s change batches
/// to `ctx.live`, taking the write lock *per batch* so queries interleave
/// (holding it across the feed would block them all).
///
/// The feed is opened before the thread starts: the tree is walked again
/// after that, and a change that happens during the walk must not be missed
/// by both. If there is no feed, the thread just stops -- the daemon answers
/// on its snapshot rather than refusing to start.
fn spawn_watch(
    ctx: &std::sync::Arc<Ctx>,
    journal: &Journal,
    watermark: u64,
) -> std::sync::mpsc::Receiver<()> {
    // The feed is armed in this thread and not in the caller: the watch is
    // not `Send`, and the caller has to know when the feed is armed anyway.
    // The receiver is the signal, and it is sent once the feed is open (or
    // once it is known that there is none).
    let (armed, signal) = std::sync::mpsc::sync_channel(1);
    let ctx = ctx.clone();
    let root = ctx.root.clone();
    let journal = *journal;
    let _ = std::thread::Builder::new()
        .name("searchd-watch".into())
        .spawn(move || {
            let Some(feed) = open_watch(&root, &journal, watermark) else {
                rgy_engine::errln!(
                    "searchd: no live feed for this tree: the index is a snapshot until the next rebuild"
                );
                // The tree is not followed, and the startup has to know it:
                // the walk that may follow has no feed to cover it.
                let _ = armed.send(());
                return;
            };
            ctx.feed.store(FEED_LIVE, Ordering::Relaxed);
            rgy_engine::errln!("searchd: following the tree with {}", feed.name);
            // The feed is armed: the tree may be walked again now, and a
            // change that happens during the walk is the feed's to deliver.
            let _ = armed.send(());
            let mut watch = feed.watch;
            loop {
                match watch.next_batch() {
                    Ok(rgy_discovery::ChangeBatch::Events(events)) => {
                        {
                            let mut live = ctx.live.write().unwrap();
                            live.apply_events(&events);
                            // The feed's position is the watermark the index is
                            // current as of: it is what the next start resumes
                            // the journal from.
                            if let Some(pos) = watch.position() {
                                live.event_id = pos;
                            }
                        }
                        // The tree moved: the content index has to be diffed
                        // against it again.
                        ctx.changed.ring();
                    }
                    Ok(rgy_discovery::ChangeBatch::Rescan) => {
                        // The feed lost its history, so nothing was replayed:
                        // the tree is walked again and diffed. The daemon
                        // does the walk itself: the overlay's own re-list of
                        // the root says "rebuild" for a root that is the whole
                        // disk and leaves the tree alone, which is a silent
                        // no-op.
                        let listed = {
                            let live = ctx.live.read().unwrap();
                            list_all(&live, &root)
                        };
                        {
                            let mut live = ctx.live.write().unwrap();
                            apply_all(&mut live, listed);
                        }
                        // The platform's usual answer no longer describes
                        // this index.
                        ctx.set_relisted();
                        ctx.changed.ring();
                    }
                    Err(e) => {
                        // The feed died, and its cause is the only thing that
                        // says why: the daemon keeps answering from the index
                        // it has, and a reader that is not told the tree is no
                        // longer followed is told a tree that is.
                        rgy_engine::errln!(
                            "searchd: the live feed stopped: {e} — the index is a snapshot from here on"
                        );
                        ctx.set_feed_gone();
                        break;
                    }
                }
            }
        });
    signal
}

/// The tree, walked again: every folder of the index listed from the disk.
///
/// The listing is the slow half and it only reads, so it is done on its
/// own: the queries keep answering while the disk is read, and only the
/// diff waits for the write lock.
fn list_all(live: &Live, root: &[u8]) -> Vec<rgy_name_core::live::Fetched> {
    folders_of(&live.base, root)
        .iter()
        .map(|f| live.fetch(f, false))
        .collect()
}

/// The diff: what the listing says, applied to the index.
///
/// The listing is the tree as it is now and the index is the tree as it
/// was; what changed lands in the overlay and the base keeps the shape the
/// search is fast on. A folder the index does not have is found through its
/// parent — the parent's listing names it, and the new folder is added with
/// its subtree — so the walk reaches the whole tree.
fn apply_all(live: &mut Live, listed: Vec<rgy_name_core::live::Fetched>) {
    for f in listed {
        live.apply(f);
    }
}

/// The folders the index holds, as the disk spells them.
///
/// The index holds the tree's paths relative to its root ("/a/b") and the
/// disk spells them from the root down ("<root>/a/b"): one crossing, and the
/// daemon needs it to name the folders it walks.
fn folders_of(base: &Index, root: &[u8]) -> Vec<Vec<u8>> {
    let mut buf = Vec::new();
    let mut out = Vec::with_capacity(base.d);
    for &entry in base.dir_entry() {
        base.path(entry as usize, &mut buf);
        out.push(on_disk(root, &buf));
    }
    out
}

/// The path on the disk of a path of the index.
///
/// The index's paths are relative to its root and the disk's are not, and the
/// separators are the index's (`/`): the disk spells a path with `\` on
/// Windows and this tree speaks `/` everywhere.
fn on_disk(root: &[u8], rel: &[u8]) -> Vec<u8> {
    let mut full: Vec<u8> = root
        .iter()
        .map(|&b| if b == b'\\' { b'/' } else { b })
        .collect();
    let rel = rel.strip_prefix(b"/").unwrap_or(rel);
    if !full.is_empty() && !full.ends_with(b"/") && !rel.is_empty() {
        full.push(b'/');
    }
    full.extend_from_slice(rel);
    full
}

/// Bind a unix socket and make it the user's alone.
///
/// The mode is set **after** the bind, where the socket exists to be set:
/// a socket another local user can connect to is a daemon that answers
/// somebody else's questions, and the path the caller asked for need not
/// be private — `uds:/tmp/rgy.sock` is in a directory every user can
/// write, and the manual showed exactly that spelling. The cache
/// directory the registered daemon uses is the user's (`~/.cache/rgy`),
/// but a path the caller names is the caller's to choose.
///
/// A mode that could not be set is said out loud and is **not fatal**: the
/// daemon is bound and answering, and a warning is the truth about a
/// socket that is still shared. A daemon that refused to start would be a
/// launcher with no daemon at all, which is worse than a socket the user
/// has to narrow.
#[cfg(unix)]
fn bind_private_socket(
    path: &std::path::Path,
) -> std::io::Result<std::os::unix::net::UnixListener> {
    use std::os::unix::fs::PermissionsExt;
    let listener = std::os::unix::net::UnixListener::bind(path)?;
    if let Err(e) = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)) {
        rgy_engine::errln!(
            "searchd: the socket {} could not be made private (0600): {e}",
            path.display()
        );
    }
    Ok(listener)
}

#[cfg(unix)]
fn serve_uds(path: &str, ctx: &std::sync::Arc<Ctx>, serve: bool) {
    // The path the caller asked for is not always a path the platform can
    // carry: `sun_path` is 104 bytes on macOS and 108 on Linux, and a
    // deeper cache directory is a daemon that cannot start at all. The
    // rule that shortens it lives in `rgy_proto::endpoint`, because the
    // client shortens the same path the same way — and a daemon that
    // bound one path and a client that looked for another is a client
    // that waits for a socket that is not there.
    let path = match rgy_proto::endpoint::socket_path(std::path::Path::new(path)) {
        Ok(p) => p,
        Err(e) => {
            rgy_engine::errln!("searchd: {e}");
            std::process::exit(2);
        }
    };
    // The socket's directory has to be there before the bind: a bind on a
    // missing directory is a daemon that exits and is restarted for ever.
    // The short path lives in the user's own cache, which the daemon may
    // not have made yet.
    //
    // The two conditions are one: the folder is made when there is one to
    // make, and the failure is the failure of making it. The two
    // clippy versions in this house read the nested form differently --
    // the older one asks for the collapsed one -- and the collapsed one
    // is the one both of them accept.
    if let Some(dir) = path.parent()
        && let Err(e) = std::fs::create_dir_all(dir)
    {
        rgy_engine::errln!("searchd: create {}: {e}", dir.display());
        std::process::exit(2);
    }
    let _ = std::fs::remove_file(&path);
    let listener = match bind_private_socket(&path) {
        Ok(l) => l,
        Err(e) => {
            rgy_engine::errln!("searchd: bind {}: {e}", path.display());
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

    /// The socket is the user's alone.
    ///
    /// A socket in a folder another local user can reach is a daemon that
    /// answers somebody else's questions, and the manual's own example,
    /// `uds:/tmp/rgy.sock`, is in exactly such a folder. Measured on the
    /// CachyOS box before the mode was set: `srwxr-xr-x`.
    #[cfg(unix)]
    #[test]
    fn the_socket_is_private_to_the_user() {
        use std::os::unix::fs::PermissionsExt;
        let path = std::env::temp_dir().join(format!(
            "rgy-searchd-socket-mode-{}.sock",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        let listener = super::bind_private_socket(&path).expect("the socket binds");
        let mode = std::fs::metadata(&path)
            .expect("the socket exists")
            .permissions()
            .mode()
            & 0o777;
        drop(listener);
        let _ = std::fs::remove_file(&path);
        assert_eq!(mode, 0o600, "the socket is the user's alone");
    }

    /// A tree in the temporary directory that takes itself away.
    ///
    /// A test that leaves a folder behind is a test that fills the
    /// temporary directory, and that directory is the one place every
    /// test on the machine shares. The tree is removed when it is
    /// dropped, so a test that panics takes its folder with it.
    struct TempTree(std::path::PathBuf);

    impl TempTree {
        /// A tree of its own, named after what it is for.
        fn new(tag: &str) -> TempTree {
            let dir = std::env::temp_dir().join(format!(
                "{tag}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            std::fs::create_dir_all(&dir).unwrap();
            TempTree(dir)
        }
    }

    impl std::ops::Deref for TempTree {
        type Target = std::path::Path;

        fn deref(&self) -> &std::path::Path {
            &self.0
        }
    }

    impl Drop for TempTree {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
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
        Journal {
            id,
            head,
            lowest,
            reason: "",
            code: 0,
        }
    }

    /// The note a reader gets when the daemon has no journal.
    ///
    /// It has to name the cause: the causes are not the same fact, and a
    /// reader told "no change journal on this platform" on a machine whose
    /// platform has one is left looking in the wrong place.
    #[test]
    fn the_note_names_the_cause_and_not_the_platform() {
        let refused = Journal::none_with(
            "the volume would not open; a volume handle on Windows needs the rights an elevated process has",
            5,
        );
        let note = refused.gap_note();
        assert!(note.contains("the volume would not open"), "{note}");
        assert!(note.contains("win32 error 5"), "{note}");
        assert!(
            !note.contains("on this platform"),
            "the platform is not the cause: {note}"
        );
    }

    /// A journal that is there says nothing about a missing one.
    #[test]
    fn a_journal_that_is_there_has_no_note() {
        assert_eq!(journal(7, 1_000, 500).gap_note(), "");
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
            feed: std::sync::atomic::AtomicU8::new(FEED_NONE),
            changed: Changed::new(),
            content: std::sync::Mutex::new(None),
            wants_content: std::sync::atomic::AtomicBool::new(false),
            sessions: std::sync::Mutex::new(std::collections::HashMap::new()),
            sessions_next: std::sync::atomic::AtomicU64::new(0),
        });
        assert_eq!(ctx.catch_up(), CatchUp::Replayed);

        // A feed that loses its history turns a replay into a relist.
        ctx.set_relisted();
        assert_eq!(ctx.catch_up(), CatchUp::Relisted);
    }

    /// The lookup surface answers a path.
    #[test]
    fn a_path_resolves_through_the_lookup_surface() {
        let dir = std::env::temp_dir().join(format!(
            "rgy-searchd-lookup-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(dir.join("docs")).unwrap();
        std::fs::write(dir.join("docs/hello.txt"), b"hello\n").unwrap();
        let root = dir.as_os_str().as_encoded_bytes().to_vec();

        let source = PortableSource::new(root.clone(), 2);
        let live = Live::with_root(
            Index::build(source.enumerate().expect("the walk"), 0, 1, b"/"),
            &root,
        );
        let ctx = std::sync::Arc::new(Ctx {
            root: root.clone(),
            home: String::new(),
            tv_dir: None,
            live: std::sync::RwLock::new(live),
            catch_up: std::sync::atomic::AtomicU8::new(CATCH_UP_RELISTED),
            feed: std::sync::atomic::AtomicU8::new(FEED_NONE),
            changed: Changed::new(),
            content: std::sync::Mutex::new(None),
            wants_content: std::sync::atomic::AtomicBool::new(false),
            sessions: std::sync::Mutex::new(std::collections::HashMap::new()),
            sessions_next: std::sync::atomic::AtomicU64::new(0),
        });

        // The file resolves, and it is a file.
        let responses = handle(
            Request::Resolve {
                path: "/docs/hello.txt".to_string(),
            },
            &ctx,
        );
        let r = responses.iter().find_map(|r| match r {
            Answer::Frame(Response::Resolved {
                path,
                folder,
                backend,
            }) => Some((path.clone(), *folder, *backend)),
            _ => None,
        });
        assert!(r.is_some(), "the path resolves: {responses:?}");
        let (path, folder, backend) = r.expect("a resolved frame");
        assert!(
            path.ends_with("hello.txt"),
            "the path is the file's: {path}"
        );
        // The spelling is the platform's, root and all: the `search`
        // answer is spelled that way, and the resolve answer does not
        // say the same file another way.
        let root_text = String::from_utf8(root.clone()).expect("the temp root is text");
        assert_eq!(
            path,
            format!(
                "{root_text}{}docs{}hello.txt",
                std::path::MAIN_SEPARATOR,
                std::path::MAIN_SEPARATOR
            ),
            "the path is spelled the way the platform spells it"
        );
        assert!(!folder, "it is a file, not a folder");
        assert_eq!(backend, 0, "the volume that owns it");

        // The folder resolves too, and it is a folder.
        let responses = handle(
            Request::Resolve {
                path: "/docs".to_string(),
            },
            &ctx,
        );
        let folder = responses.iter().find_map(|r| match r {
            Answer::Frame(Response::Resolved { folder, .. }) => Some(*folder),
            _ => None,
        });
        assert_eq!(folder, Some(true), "the folder is a folder");

        // A path nobody has is an error, not a wrong answer.
        let responses = handle(
            Request::Resolve {
                path: "/nosuchpath".to_string(),
            },
            &ctx,
        );
        assert!(
            responses
                .iter()
                .any(|r| matches!(r, Answer::Frame(Response::Error { .. }))),
            "a path that is not there is an error"
        );
        let _ = std::fs::remove_dir_all(&dir);
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
        let dir = TempTree::new("rgy-searchd-live");
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
            name: rgy_engine::console::spell(&root),
        }]);

        let ctx = std::sync::Arc::new(Ctx {
            root: root.clone(),
            home: String::new(),
            tv_dir: None,
            live: std::sync::RwLock::new(live),
            catch_up: std::sync::atomic::AtomicU8::new(CATCH_UP_RELISTED),
            feed: std::sync::atomic::AtomicU8::new(FEED_NONE),
            changed: Changed::new(),
            content: std::sync::Mutex::new(None),
            wants_content: std::sync::atomic::AtomicBool::new(false),
            sessions: std::sync::Mutex::new(std::collections::HashMap::new()),
            sessions_next: std::sync::atomic::AtomicU64::new(0),
        });

        let responses = handle(
            Request::Search {
                q: "late".to_string(),
                limit: 10,
                session: None,
            },
            &ctx,
        );
        let hits = responses
            .iter()
            .find_map(|r| match r {
                Answer::Frame(Response::Hits { hits, .. }) => Some(hits),
                _ => None,
            })
            .expect("a hits frame");
        assert!(
            hits.iter().any(|h| h.path.ends_with("late.txt")),
            "the new file must be answered; hits={hits:?}"
        );
    }

    /// A folder that appeared after the snapshot is a folder on the wire.
    ///
    /// The overlay holds the entries the base index does not have, and it
    /// holds their kind: a folder created since the last compaction is an
    /// overlay entry, and its kind says it is a folder. The base's kind
    /// byte cannot say it — the base has no entry for it at all — so the
    /// answer has to come from the overlay's own entry.
    ///
    /// The daemon used to answer every overlay hit as a file, on the
    /// ground that the overlay holds files. It holds folders too, and an
    /// interface that draws a file's icon over a folder shows the user
    /// the wrong picture of the thing they are about to open.
    #[test]
    fn an_overlay_folder_is_a_folder_on_the_wire() {
        let dir = TempTree::new("rgy-searchd-overlay-folder");
        std::fs::write(dir.join("hello.txt"), b"hello\n").unwrap();
        let root = dir.as_os_str().as_encoded_bytes().to_vec();

        // The index as it was before the folder appeared.
        let source = PortableSource::new(root.clone(), 2);
        let mut live = Live::with_root(
            Index::build(source.enumerate().expect("the walk"), 0, 1, b"/"),
            &root,
        );

        // The folder appears, and the feed says the folder whose contents
        // changed (what a directory-granular feed says).
        std::fs::create_dir(dir.join("freshfolder")).unwrap();
        live.apply_events(&[rgy_discovery::UsnRecord {
            file_reference: 0,
            parent_reference: 0,
            usn: 0,
            reason: 0,
            file_attributes: rgy_discovery::FILE_ATTRIBUTE_DIRECTORY,
            name: rgy_engine::console::spell(&root),
        }]);

        let ctx = std::sync::Arc::new(Ctx {
            root: root.clone(),
            home: String::new(),
            tv_dir: None,
            live: std::sync::RwLock::new(live),
            catch_up: std::sync::atomic::AtomicU8::new(CATCH_UP_RELISTED),
            feed: std::sync::atomic::AtomicU8::new(FEED_NONE),
            changed: Changed::new(),
            content: std::sync::Mutex::new(None),
            wants_content: std::sync::atomic::AtomicBool::new(false),
            sessions: std::sync::Mutex::new(std::collections::HashMap::new()),
            sessions_next: std::sync::atomic::AtomicU64::new(0),
        });

        let responses = handle(
            Request::Search {
                q: "freshfolder".to_string(),
                limit: 10,
                session: None,
            },
            &ctx,
        );
        let hits = responses
            .iter()
            .find_map(|r| match r {
                Answer::Frame(Response::Hits { hits, .. }) => Some(hits),
                _ => None,
            })
            .expect("a hits frame");
        assert!(
            hits.iter()
                .any(|h| h.path.ends_with("freshfolder") && h.folder),
            "the folder in the overlay must be answered as a folder; hits={hits:?}"
        );
    }

    /// An overlay hit without an entry falls back to the path.
    ///
    /// The overlay's map is consulted by the path the hit carries, and a
    /// hit can name a path the map does not hold: the entry was not there
    /// when the answer was built. Then the path is all that is left, and a
    /// path that ends in the separator is a folder's path.
    ///
    /// The rule is the CLI's, and the two sides of the tree have to agree:
    /// the same hit, the same path, the same answer.
    #[test]
    fn an_overlay_hit_without_its_entry_falls_back_to_the_path() {
        let live = Live::new(Index::build(Vec::new(), 0, 0, b"/"));
        let folder = Hit {
            key: rgy_name_core::rank::RankKey::default(),
            idx: u32::MAX,
            mtime: 0,
            over: Some(b"/tree/gone/".to_vec()),
        };
        assert!(
            is_folder(&live, &folder),
            "a path that ends in the separator is a folder"
        );
        let file = Hit {
            key: rgy_name_core::rank::RankKey::default(),
            idx: u32::MAX,
            mtime: 0,
            over: Some(b"/tree/gone.txt".to_vec()),
        };
        assert!(
            !is_folder(&live, &file),
            "a path that does not end in the separator is not a folder"
        );
    }

    /// A folder is a folder on the wire.
    ///
    /// A folder's path is spelled like a file's, and the icon beside a
    /// result is the icon of the thing: the daemon says which hits are
    /// folders, and it says it from the index's own kind byte — not from
    /// a guess about the spelling.
    #[test]
    fn a_folder_is_a_folder_on_the_wire() {
        let dir = TempTree::new("rgy-searchd-folder");
        std::fs::create_dir_all(dir.join("docs")).unwrap();
        std::fs::write(dir.join("docs/notes.txt"), b"notes\n").unwrap();
        std::fs::write(dir.join("top.txt"), b"top\n").unwrap();
        let root = dir.as_os_str().as_encoded_bytes().to_vec();

        let source = PortableSource::new(root.clone(), 2);
        let live = Live::with_root(
            Index::build(source.enumerate().expect("the walk"), 0, 1, b"/"),
            &root,
        );
        let ctx = std::sync::Arc::new(Ctx {
            root: root.clone(),
            home: String::new(),
            tv_dir: None,
            live: std::sync::RwLock::new(live),
            catch_up: std::sync::atomic::AtomicU8::new(CATCH_UP_RELISTED),
            feed: std::sync::atomic::AtomicU8::new(FEED_NONE),
            changed: Changed::new(),
            content: std::sync::Mutex::new(None),
            wants_content: std::sync::atomic::AtomicBool::new(false),
            sessions: std::sync::Mutex::new(std::collections::HashMap::new()),
            sessions_next: std::sync::atomic::AtomicU64::new(0),
        });

        let hits_of = |q: &str| {
            let responses = handle(
                Request::Search {
                    q: q.to_string(),
                    limit: 10,
                    session: None,
                },
                &ctx,
            );
            responses
                .iter()
                .find_map(|r| match r {
                    Answer::Frame(Response::Hits { hits, .. }) => Some(hits.clone()),
                    _ => None,
                })
                .expect("a hits frame")
        };

        let folders = hits_of("docs");
        assert!(
            folders.iter().any(|h| h.path.ends_with("docs") && h.folder),
            "the folder is a folder on the wire: {folders:?}"
        );
        let files = hits_of("top");
        assert!(
            files
                .iter()
                .any(|h| h.path.ends_with("top.txt") && !h.folder),
            "the file is a file on the wire: {files:?}"
        );
    }

    /// The name answer says whether it is whole, and every route says it
    /// the same way.
    ///
    /// A list of hits looks like the whole answer, and a list that reached
    /// the limit is the same list as one that did not: the answer has to
    /// say which it is, or the reader cannot tell a whole answer from a
    /// page of a longer one.
    ///
    /// The routes are the whole tree, the scope and the session's re-rank,
    /// and they are the same question with different starting points — so
    /// they must not disagree about the same answer.
    #[test]
    fn a_name_answer_says_whether_it_is_whole() {
        let dir = TempTree::new("rgy-searchd-complete");
        // The scope is a folder of the tree, and its name is not a name the
        // working directory has: the scope is a path in the index, and a
        // path that resolved against the working directory would be a
        // different question.
        std::fs::create_dir_all(dir.join("rgy-scope")).unwrap();
        for i in 0..3 {
            std::fs::write(dir.join(format!("rgy-scope/hello{i}.txt")), b"hello\n").unwrap();
        }
        let root = dir.as_os_str().as_encoded_bytes().to_vec();

        let source = PortableSource::new(root.clone(), 2);
        let live = Live::with_root(
            Index::build(source.enumerate().expect("the walk"), 0, 1, b"/"),
            &root,
        );
        let ctx = std::sync::Arc::new(Ctx {
            root: root.clone(),
            home: String::new(),
            tv_dir: None,
            live: std::sync::RwLock::new(live),
            catch_up: std::sync::atomic::AtomicU8::new(CATCH_UP_RELISTED),
            feed: std::sync::atomic::AtomicU8::new(FEED_NONE),
            changed: Changed::new(),
            content: std::sync::Mutex::new(None),
            wants_content: std::sync::atomic::AtomicBool::new(false),
            sessions: std::sync::Mutex::new(std::collections::HashMap::new()),
            sessions_next: std::sync::atomic::AtomicU64::new(0),
        });

        // The answer of a route: how many hits it carried, and whether it
        // said it was whole.
        let answer = |q: &str, limit: usize, session: Option<u64>| {
            let responses = handle(
                Request::Search {
                    q: q.to_string(),
                    limit,
                    session,
                },
                &ctx,
            );
            responses
                .iter()
                .find_map(|r| match r {
                    Answer::Frame(Response::Hits { hits, complete, .. }) => {
                        Some((hits.len(), *complete))
                    }
                    _ => None,
                })
                .expect("a hits frame")
        };

        // The routes that answer a name query.
        let scope = "in:rgy-scope hello".to_string();
        let routes: [(&str, &str, Option<u64>); 3] = [
            ("the whole tree", "hello", None),
            ("the scope", scope.as_str(), None),
            ("the session", "hello", Some(7)),
        ];

        for (what, q, session) in routes {
            // Below the limit: the whole answer.
            let (hits, complete) = answer(q, 10, session);
            assert_eq!(hits, 3, "{what} found the three files: {hits}");
            assert!(complete, "{what} says a whole answer is not whole");

            // At the limit: the answer may have been cut short.
            let (hits, complete) = answer(q, 3, session);
            assert_eq!(hits, 3, "{what} found the three files: {hits}");
            assert!(!complete, "{what} says a cut answer is whole");
        }
    }

    /// The answer names the session it belongs to.
    ///
    /// The session is the incremental-search id: the daemon keeps the answer
    /// under it and the next request that carries the same id is answered
    /// from that answer instead of from the index. A client that cannot
    /// name the session it is in cannot ask for that — so the id is on the
    /// answer, and the daemon names one when the request does not.
    #[test]
    fn a_name_answer_names_the_session_it_belongs_to() {
        let dir = TempTree::new("rgy-searchd-session");
        std::fs::write(dir.join("hello.txt"), b"hello\n").unwrap();
        let root = dir.as_os_str().as_encoded_bytes().to_vec();

        let source = PortableSource::new(root.clone(), 2);
        let live = Live::with_root(
            Index::build(source.enumerate().expect("the walk"), 0, 1, b"/"),
            &root,
        );
        let ctx = std::sync::Arc::new(Ctx {
            root: root.clone(),
            home: String::new(),
            tv_dir: None,
            live: std::sync::RwLock::new(live),
            catch_up: std::sync::atomic::AtomicU8::new(CATCH_UP_RELISTED),
            feed: std::sync::atomic::AtomicU8::new(FEED_NONE),
            changed: Changed::new(),
            content: std::sync::Mutex::new(None),
            wants_content: std::sync::atomic::AtomicBool::new(false),
            sessions: std::sync::Mutex::new(std::collections::HashMap::new()),
            sessions_next: std::sync::atomic::AtomicU64::new(0),
        });

        // The session the answer was computed for.
        let session_of = |session: Option<u64>| {
            handle(
                Request::Search {
                    q: "hello".to_string(),
                    limit: 10,
                    session,
                },
                &ctx,
            )
            .iter()
            .find_map(|r| match r {
                Answer::Frame(Response::Hits { session, .. }) => Some(*session),
                _ => None,
            })
            .expect("a hits frame")
        };

        // A request that names no session gets one from the daemon: the
        // client has no way to name a session it was never given.
        let first = session_of(None);
        assert!(first.is_some(), "the daemon names the session");
        // And the id it names is its own: the clients' ids are small
        // counters, and the two spaces must not meet.
        assert!(
            first.unwrap() >= Ctx::DAEMON_SESSIONS,
            "the daemon's own ids live above the clients': {first:?}"
        );
        // And under the wire's limit: the protocol carries a number as an
        // `f64`, and an id above `2^53` would reach the client as a
        // different number — the client would then ask for the re-rank of
        // somebody else's session.
        assert!(
            first.unwrap() < Ctx::SESSION_LIMIT,
            "the daemon's own ids live under the wire's limit: {first:?}"
        );
        // A second request that names no session gets a session of its own:
        // two clients that never name a session do not share one.
        let second = session_of(None);
        assert_ne!(first, second, "each answer names its own session");
        // A request that names a session gets that session back: the
        // client's id is the session's id, and the answer says which.
        assert_eq!(session_of(Some(7)), Some(7));
    }

    /// The daemon takes back the session it issued, alternatives or not.
    ///
    /// A query with a `|` is searched on its own: the session holds one
    /// result set, and a query with alternatives is not one query. So the
    /// search returns before the session is looked up — and the id the
    /// daemon issued used to enter the map only there. The daemon put an
    /// id on the wire and then refused it: the client's next keystroke
    /// named an id the daemon said was not its own, and the answer came
    /// back with a different session. The answers were right, so nothing
    /// looked broken; the session was simply never the same one twice.
    #[test]
    fn the_daemon_takes_back_the_session_it_issued_for_a_query_with_alternatives() {
        let dir = TempTree::new("rgy-searchd-session-alts");
        std::fs::write(dir.join("hello.txt"), b"hello\n").unwrap();
        let root = dir.as_os_str().as_encoded_bytes().to_vec();

        let source = PortableSource::new(root.clone(), 2);
        let live = Live::with_root(
            Index::build(source.enumerate().expect("the walk"), 0, 1, b"/"),
            &root,
        );
        let ctx = std::sync::Arc::new(Ctx {
            root: root.clone(),
            home: String::new(),
            tv_dir: None,
            live: std::sync::RwLock::new(live),
            catch_up: std::sync::atomic::AtomicU8::new(CATCH_UP_RELISTED),
            feed: std::sync::atomic::AtomicU8::new(FEED_NONE),
            changed: Changed::new(),
            content: std::sync::Mutex::new(None),
            wants_content: std::sync::atomic::AtomicBool::new(false),
            sessions: std::sync::Mutex::new(std::collections::HashMap::new()),
            sessions_next: std::sync::atomic::AtomicU64::new(0),
        });

        let session_of = |session: Option<u64>| {
            handle(
                Request::Search {
                    q: "hello | world".to_string(),
                    limit: 10,
                    session,
                },
                &ctx,
            )
            .iter()
            .find_map(|r| match r {
                Answer::Frame(Response::Hits { session, .. }) => Some(*session),
                _ => None,
            })
            .expect("a hits frame")
        };

        // The daemon names a session, and the answer carries it.
        let first = session_of(None);
        assert!(first.is_some(), "the daemon names the session");
        // And the next request that names it gets it back: the id the
        // daemon issued is the daemon's own, and it takes back the ones it
        // issued and only those.
        assert_eq!(
            session_of(Some(first.unwrap())),
            Some(first.unwrap()),
            "the daemon takes back the id it issued, alternatives or not"
        );
    }

    /// A query the engine refuses answers nothing, on every route.
    ///
    /// The unknown `type:` fails closed: the engine refuses the whole
    /// alternative and the answer is empty. The session's path knows the
    /// flag too — the refusal is a property of the query, not of one
    /// caller's routing — so the route that named a session answers with
    /// nothing like the route that did not. The session route used to
    /// answer with hits where the route without a session answered with
    /// nothing, and the daemon worked around it by routing around the
    /// session; the workaround is gone and the rule is in the engine.
    #[test]
    fn a_query_that_fails_closed_answers_nothing_with_a_session_too() {
        let dir = TempTree::new("rgy-searchd-never");
        std::fs::write(dir.join("hello.txt"), b"hello\n").unwrap();
        let root = dir.as_os_str().as_encoded_bytes().to_vec();

        let source = PortableSource::new(root.clone(), 2);
        let live = Live::with_root(
            Index::build(source.enumerate().expect("the walk"), 0, 1, b"/"),
            &root,
        );
        let ctx = std::sync::Arc::new(Ctx {
            root: root.clone(),
            home: String::new(),
            tv_dir: None,
            live: std::sync::RwLock::new(live),
            catch_up: std::sync::atomic::AtomicU8::new(CATCH_UP_RELISTED),
            feed: std::sync::atomic::AtomicU8::new(FEED_NONE),
            changed: Changed::new(),
            content: std::sync::Mutex::new(None),
            wants_content: std::sync::atomic::AtomicBool::new(false),
            sessions: std::sync::Mutex::new(std::collections::HashMap::new()),
            sessions_next: std::sync::atomic::AtomicU64::new(0),
        });

        // The query the engine refuses, and the query it does not: the
        // refused one is the whole difference.
        let hits_of = |q: &str, session: Option<u64>| {
            handle(
                Request::Search {
                    q: q.to_string(),
                    limit: 10,
                    session,
                },
                &ctx,
            )
            .iter()
            .find_map(|r| match r {
                Answer::Frame(Response::Hits { hits, .. }) => Some(hits.len()),
                _ => None,
            })
            .expect("a hits frame")
        };

        assert_eq!(hits_of("hello", None), 1, "the query that is asked for");
        assert_eq!(
            hits_of("hello", Some(7)),
            1,
            "the same query, with a session"
        );
        // And the refused one answers nothing on both routes.
        assert_eq!(
            hits_of("hello type:banana", None),
            0,
            "the refused query answers nothing"
        );
        assert_eq!(
            hits_of("hello type:banana", Some(7)),
            0,
            "the refused query answers nothing with a session too"
        );
    }

    /// The daemon takes back the sessions it issued, and only those.
    ///
    /// The ids the daemon hands out live above the top bit, and the ids a
    /// client names are supposed to be small counters. A client that named
    /// one above the line would meet a session the daemon handed to
    /// somebody else, and the two would share a result set — one client's
    /// answer re-ranked against another's.
    ///
    /// The ids are unguessable, so a client that names one of them is
    /// holding it: that is the round trip the protocol promises, and it
    /// is the re-rank that makes the next keystroke cheap. The daemon
    /// takes back the ids it issued and refuses the ones it did not, and
    /// a client that does not hold one is given a fresh session.
    ///
    /// The daemon used to refuse all of them — the ids came out of a
    /// counter then, so a client could name somebody else's — and the
    /// answer it put on the wire was a name it would not take back. The
    /// round trip never happened, and the failure was silent: the same
    /// answers, slower.
    #[test]
    fn the_daemon_takes_back_the_sessions_it_issued() {
        let dir = TempTree::new("rgy-searchd-session-space");
        std::fs::write(dir.join("hello.txt"), b"hello\n").unwrap();
        let root = dir.as_os_str().as_encoded_bytes().to_vec();

        let source = PortableSource::new(root.clone(), 2);
        let live = Live::with_root(
            Index::build(source.enumerate().expect("the walk"), 0, 1, b"/"),
            &root,
        );
        let ctx = std::sync::Arc::new(Ctx {
            root: root.clone(),
            home: String::new(),
            tv_dir: None,
            live: std::sync::RwLock::new(live),
            catch_up: std::sync::atomic::AtomicU8::new(CATCH_UP_RELISTED),
            feed: std::sync::atomic::AtomicU8::new(FEED_NONE),
            changed: Changed::new(),
            content: std::sync::Mutex::new(None),
            wants_content: std::sync::atomic::AtomicBool::new(false),
            sessions: std::sync::Mutex::new(std::collections::HashMap::new()),
            sessions_next: std::sync::atomic::AtomicU64::new(0),
        });

        let session_of = |session: Option<u64>| {
            handle(
                Request::Search {
                    q: "hello".to_string(),
                    limit: 10,
                    session,
                },
                &ctx,
            )
            .iter()
            .find_map(|r| match r {
                Answer::Frame(Response::Hits { session, .. }) => Some(*session),
                _ => None,
            })
            .expect("a hits frame")
        };

        // The daemon names one when the client names none.
        let named = session_of(None).expect("the daemon names a session");
        assert!(
            (Ctx::DAEMON_SESSIONS..Ctx::SESSION_LIMIT).contains(&named),
            "the daemon's own id lives in its own space and under the wire's limit: {named}"
        );

        // **The round trip.** The client sends it back and the answer is
        // computed for the same session: that is the re-rank, and it is
        // what the protocol promises.
        assert_eq!(
            session_of(Some(named)),
            Some(named),
            "the daemon takes back the session it issued"
        );

        // And an id the daemon never issued is refused, and a fresh one
        // is named instead: a client that guesses cannot reach somebody
        // else's answer.
        let guessed = named.wrapping_add(1_000_003) % (Ctx::SESSION_LIMIT - Ctx::DAEMON_SESSIONS)
            + Ctx::DAEMON_SESSIONS;
        assert_ne!(
            session_of(Some(guessed)),
            Some(guessed),
            "a client cannot take a session the daemon never issued"
        );

        // The small counters are still the client's to name.
        assert_eq!(session_of(Some(7)), Some(7));

        // And two sessions the daemon named are two different sessions:
        // the ids are not a counter, so one client cannot walk into
        // another's by naming the next number.
        let first = session_of(None).expect("a session");
        let second = session_of(None).expect("a session");
        assert_ne!(first, second, "the daemon's own ids are not a counter");
    }

    /// The reader and the daemon, proven together.
    ///
    /// The bug this pairs with lived in the space BETWEEN the two: the
    /// reader cast the session number to `u64`, and a cast saturates — a
    /// client that sent `-1` was read as the session `0`, which is in the
    /// *client's* space, the space the daemon trusts. The test that paired
    /// with it built its request in Rust, where a negative id is not even
    /// representable, so the reader was not in the path and the bug was
    /// invisible to it.
    ///
    /// So this one goes through the wire: the JSON line is built, the
    /// protocol's own reader parses it, and the request the reader made is
    /// the request the daemon answers. A `-1` on the wire must not reach
    /// the daemon as the session `0`.
    #[test]
    fn the_reader_never_hands_the_daemon_a_session_it_would_trust() {
        let dir = TempTree::new("rgy-searchd-wire-session");
        std::fs::write(dir.join("hello.txt"), b"hello\n").unwrap();
        let root = dir.as_os_str().as_encoded_bytes().to_vec();

        let source = PortableSource::new(root.clone(), 2);
        let live = Live::with_root(
            Index::build(source.enumerate().expect("the walk"), 0, 1, b"/"),
            &root,
        );
        let ctx = std::sync::Arc::new(Ctx {
            root: root.clone(),
            home: String::new(),
            tv_dir: None,
            live: std::sync::RwLock::new(live),
            catch_up: std::sync::atomic::AtomicU8::new(CATCH_UP_RELISTED),
            feed: std::sync::atomic::AtomicU8::new(FEED_NONE),
            changed: Changed::new(),
            content: std::sync::Mutex::new(None),
            wants_content: std::sync::atomic::AtomicBool::new(false),
            sessions: std::sync::Mutex::new(std::collections::HashMap::new()),
            sessions_next: std::sync::atomic::AtomicU64::new(0),
        });

        // The line a client sends, and the reader that reads it.
        let on_the_wire = |session: &str| {
            let line = format!(r#"{{"op":"search","q":"hello","limit":10,"session":{session}}}"#);
            rgy_proto::Request::from_json(&line).expect("the line reads back")
        };
        let answered = |req: &Request| {
            handle(req.clone(), &ctx)
                .iter()
                .find_map(|r| match r {
                    Answer::Frame(Response::Hits { session, .. }) => Some(*session),
                    _ => None,
                })
                .expect("a hits frame")
        };

        // A negative id is not an id: the reader refuses it, and what the
        // daemon is handed is a request that names no session at all.
        let refused = on_the_wire("-1");
        assert_eq!(
            refused.session(),
            None,
            "a negative id is not a session: the reader refuses it"
        );
        let named = answered(&refused);
        assert_ne!(
            named,
            Some(0),
            "a `-1` on the wire must not become the session `0` in the daemon's hand"
        );
        assert!(
            named.is_some_and(|id| id >= Ctx::DAEMON_SESSIONS),
            "a request that names no session is answered with one of the daemon's own: {named:?}"
        );

        // And the session a client legitimately holds still survives the
        // wire: the reader does not mangle what it can carry, and the
        // daemon takes the id back.
        let held = on_the_wire("7");
        assert_eq!(held.session(), Some(7), "the session reaches the daemon");
        assert_eq!(
            answered(&held),
            Some(7),
            "the daemon takes back the session the client holds"
        );
    }

    /// A content search answers the same with a content index as without it.
    ///
    /// The index is the trigram postings: with it the search opens the
    /// candidates, without it the search opens everything the name filters
    /// name. The union is the scan's exact answer either way, and that is
    /// the property that has to hold when the daemon reads its own index.
    #[test]
    fn a_content_search_answers_the_same_with_an_index() {
        let dir = std::env::temp_dir().join("rgy-searchd-content-test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("hello.txt"), b"hello world\n").unwrap();
        let root = dir.to_string_lossy().into_owned().into_bytes();

        let source = PortableSource::new(root.clone(), 2);
        let live = Live::with_root(
            Index::build(source.enumerate().expect("the walk"), 0, 1, b"/"),
            &root,
        );
        let ctx = std::sync::Arc::new(Ctx {
            root: root.clone(),
            home: String::new(),
            tv_dir: None,
            live: std::sync::RwLock::new(live),
            catch_up: std::sync::atomic::AtomicU8::new(CATCH_UP_RELISTED),
            feed: std::sync::atomic::AtomicU8::new(FEED_NONE),
            changed: Changed::new(),
            content: std::sync::Mutex::new(None),
            wants_content: std::sync::atomic::AtomicBool::new(false),
            sessions: std::sync::Mutex::new(std::collections::HashMap::new()),
            sessions_next: std::sync::atomic::AtomicU64::new(0),
        });
        let names = |rs: Vec<Answer>| -> Vec<String> {
            rs.iter()
                .filter_map(|r| match r {
                    Answer::Frame(Response::GrepFiles { files, .. }) => {
                        Some(files.iter().map(|f| f.path.clone()).collect::<Vec<_>>())
                    }
                    _ => None,
                })
                .flatten()
                .collect()
        };

        let before = names(grep("hello", 10, &ctx));
        assert!(
            before.iter().any(|p| p.ends_with("hello.txt")),
            "{before:?}"
        );

        // Now the daemon has an index: build one over the same tree and
        // answer again.
        let mut c = rgy_name_core::content::Content::open(dir.join("content"));
        let n = c.sync(
            &ctx.live.read().unwrap(),
            b"",
            &[],
            std::slice::from_ref(&root),
            &root,
        );
        assert!(n > 0, "the index holds the file");
        *ctx.content.lock().unwrap() = Some(c);

        let after = names(grep("hello", 10, &ctx));
        assert_eq!(
            before, after,
            "the index changes what is read, not what is answered"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The rule for the number, in one place.
    ///
    /// A search that stopped because it already had the limit read what
    /// it was asked to read: nothing is missing, and the number is zero.
    /// A budget that ran out is a hole, and the hole is the files the
    /// search never reached.
    #[test]
    fn the_files_the_search_did_not_read_are_the_ones_it_never_reached() {
        // It read everything it was asked to read: the limit stopped it,
        // and the limit is the caller's.
        assert_eq!(unread_files(100, 100, true, &[]).0, 0);
        // The placeholders are files it did not read, whoever stopped it.
        assert_eq!(unread_files(100, 100, true, &placeholders(2)).0, 2);
        // A budget that ran out: the files it never reached are missing.
        assert_eq!(unread_files(100, 40, false, &[]).0, 60);
        assert_eq!(unread_files(100, 40, false, &placeholders(3)).0, 63);
    }

    /// The answer is whole only when every file was read.
    ///
    /// A file that could not be opened was not read, and an answer that
    /// says "whole" about a file nobody read is a wrong fact: the client
    /// reads the flag as "the search read every candidate it was given",
    /// and the count of the files it did not read is on the wire next to
    /// it.
    #[test]
    fn an_answer_that_left_a_file_unread_is_not_whole() {
        // Everything was read: the answer is whole.
        assert!(content_answer_is_complete(3, 10, true, 0));
        // The limit cut it short: a page, not the whole answer.
        assert!(!content_answer_is_complete(10, 10, true, 0));
        // A file that could not be opened was not read.
        assert!(!content_answer_is_complete(3, 10, true, 1));
        // The budget ran out before it reached them all.
        assert!(!content_answer_is_complete(3, 10, false, 0));
    }

    /// The count is not enough: the answer says why.
    ///
    /// A file that was not read because it is a cloud placeholder is a file
    /// the user can ask for, and a file the budget never reached is a file
    /// the user has to ask for again — and a count that does not say which
    /// of the two it was leaves the reader guessing.
    #[test]
    fn the_answer_says_why_the_files_were_not_read() {
        // The placeholders the search named carry their path and their
        // reason.
        let (n, skips) = unread_files(100, 100, true, &placeholders(2));
        assert_eq!(n, 2);
        assert_eq!(skips.len(), 1, "one thing happened: {skips:?}");
        assert_eq!(skips[0].count, 2);
        assert_eq!(
            skips[0].reason, "a cloud placeholder",
            "the reason is the daemon's own: {skips:?}"
        );
        // The path is spelled the way the platform spells it: the answer's
        // own files are spelled that way, and one file spelled two ways is
        // two files to the reader.
        let spelled = platform_path(b"C:/placeholder0.txt");
        assert_eq!(skips[0].path.as_deref(), Some(spelled.as_str()), "the file");

        // The files the budget stopped the search from reaching were never
        // named, so they carry the reason and no path: a path made up would
        // be a path the user cannot open.
        let (n, skips) = unread_files(100, 40, false, &[]);
        assert_eq!(n, 60);
        assert_eq!(skips.len(), 1);
        assert_eq!(skips[0].count, 60);
        assert_eq!(skips[0].path, None, "no file was named");

        // The counts add up to the count: every file the search did not
        // read is in one of the groups.
        let (n, skips) = unread_files(100, 40, false, &placeholders(3));
        assert_eq!(n, 63);
        assert_eq!(
            skips.iter().map(|s| s.count).sum::<usize>(),
            n,
            "the reasons account for every file: {skips:?}"
        );
    }

    /// The placeholders a search refused, for the rule above.
    fn placeholders(n: usize) -> Vec<rgy_name_core::content::Skipped> {
        (0..n)
            .map(|i| rgy_name_core::content::Skipped {
                path: format!("C:/placeholder{i}.txt").into_bytes(),
                reason: "a cloud placeholder",
            })
            .collect()
    }

    /// An answer cut short by the limit names no files.
    ///
    /// The daemon read what it was asked to read and the answer is still
    /// not whole: the limit cut it. The wire's `skipped` says how many
    /// files the search did not read for a reason of its own, and the
    /// limit is not a reason of its own -- so the number is zero, and a
    /// sentence that said "did not read 0 file(s)" would be naming a
    /// number that says nothing and sounds like it does.
    #[test]
    fn an_answer_cut_short_by_the_limit_names_no_files() {
        let dir = std::env::temp_dir().join("rgy-searchd-limit-test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for i in 0..3 {
            std::fs::write(dir.join(format!("hello{i}.txt")), b"hello world\n").unwrap();
        }
        let root = dir.to_string_lossy().into_owned().into_bytes();

        let source = PortableSource::new(root.clone(), 2);
        let live = Live::with_root(
            Index::build(source.enumerate().expect("the walk"), 0, 1, b"/"),
            &root,
        );
        let ctx = std::sync::Arc::new(Ctx {
            root: root.clone(),
            home: String::new(),
            tv_dir: None,
            live: std::sync::RwLock::new(live),
            catch_up: std::sync::atomic::AtomicU8::new(CATCH_UP_RELISTED),
            feed: std::sync::atomic::AtomicU8::new(FEED_NONE),
            changed: Changed::new(),
            content: std::sync::Mutex::new(None),
            wants_content: std::sync::atomic::AtomicBool::new(false),
            sessions: std::sync::Mutex::new(std::collections::HashMap::new()),
            sessions_next: std::sync::atomic::AtomicU64::new(0),
        });

        let answer = grep("hello", 1, &ctx);
        let (files, complete, skipped) = answer
            .iter()
            .find_map(|r| match r {
                Answer::Frame(Response::GrepFiles {
                    files,
                    complete,
                    skipped,
                    ..
                }) => Some((files.len(), *complete, *skipped)),
                _ => None,
            })
            .expect("the answer carries the files");
        assert_eq!(files, 1, "the limit is the limit");
        assert!(!complete, "an answer cut short is not whole");
        assert_eq!(
            skipped, 0,
            "the daemon read what it was asked to read, so it skipped nothing"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The pack store's answer is a page when the limit cuts it.
    ///
    /// The store searched the corpus whole and the scope filtered its
    /// answer — every file in the scope was read, and that is true — but
    /// the limit cuts what the caller gets, and the answer has to say so.
    /// The route used to ignore the limit and to claim the answer was
    /// whole: a caller that asked for one file got every file in the scope
    /// and was told it had seen them all.
    #[test]
    fn the_pack_store_answer_is_a_page_when_the_limit_cuts_it() {
        // A tree of three files that all carry the needle.
        let tree = TempTree::new("rgy-searchd-tv-scope");
        let mut scope = Vec::new();
        for name in ["a.txt", "b.txt", "c.txt"] {
            let path = tree.join(name);
            std::fs::write(&path, b"the needle is here\n").unwrap();
            scope.push(path.to_string_lossy().into_owned());
        }
        // And the pack store over it.
        let store = TempTree::new("rgy-searchd-tv-store");
        rgx_core::tv_runtime::builder::build_store(
            &tree,
            &store,
            &rgx_core::tv_runtime::builder::BuildConfig::default(),
        )
        .expect("the pack store is built");

        let ctx = std::sync::Arc::new(Ctx {
            root: tree.as_os_str().as_encoded_bytes().to_vec(),
            home: String::new(),
            tv_dir: Some(store.0.clone()),
            live: std::sync::RwLock::new(Live::new(Index::build(Vec::new(), 0, 0, b"/"))),
            catch_up: std::sync::atomic::AtomicU8::new(CATCH_UP_RELISTED),
            feed: std::sync::atomic::AtomicU8::new(FEED_NONE),
            changed: Changed::new(),
            content: std::sync::Mutex::new(None),
            wants_content: std::sync::atomic::AtomicBool::new(false),
            sessions: std::sync::Mutex::new(std::collections::HashMap::new()),
            sessions_next: std::sync::atomic::AtomicU64::new(0),
        });

        let answer = |limit: usize| {
            scoped_content("grep:needle", &scope, limit, &ctx)
                .iter()
                .find_map(|r| match r {
                    Answer::Frame(Response::GrepFiles {
                        files, complete, ..
                    }) => Some((files.len(), *complete)),
                    _ => None,
                })
                .expect("the answer carries the files")
        };

        // The scope holds three files and the caller asks for one: the
        // answer is a page, and it says so.
        let (files, complete) = answer(1);
        assert_eq!(files, 1, "the limit is the limit");
        assert!(!complete, "an answer cut short is not whole");

        // And the whole scope is whole when the limit is above it: the
        // store read every file in the scope, and the limit left the
        // answer alone.
        let (files, complete) = answer(10);
        assert_eq!(files, 3, "every file in the scope is in the answer");
        assert!(complete, "an answer under the limit is the whole answer");
    }

    /// A pattern the engine refuses is a warning, and the answer goes on.
    ///
    /// The whole-tree route answers a refusal with a warning frame and an
    /// empty answer, and the manual says the warning is part of the answer
    /// and not the end of it. The scoped route used to answer the same
    /// refusal with an `{"op":"error"}` frame and nothing else — the one
    /// path that stopped instead of answering with nothing and saying why.
    #[test]
    fn a_refused_pattern_is_a_warning_on_the_scoped_route_too() {
        let tree = TempTree::new("rgy-searchd-refused");
        let path = tree.join("a.txt");
        std::fs::write(&path, b"the needle is here\n").unwrap();
        let scope = vec![path.to_string_lossy().into_owned()];

        let ctx = std::sync::Arc::new(Ctx {
            root: tree.as_os_str().as_encoded_bytes().to_vec(),
            home: String::new(),
            tv_dir: None,
            live: std::sync::RwLock::new(Live::new(Index::build(Vec::new(), 0, 0, b"/"))),
            catch_up: std::sync::atomic::AtomicU8::new(CATCH_UP_RELISTED),
            feed: std::sync::atomic::AtomicU8::new(FEED_NONE),
            changed: Changed::new(),
            content: std::sync::Mutex::new(None),
            wants_content: std::sync::atomic::AtomicBool::new(false),
            sessions: std::sync::Mutex::new(std::collections::HashMap::new()),
            sessions_next: std::sync::atomic::AtomicU64::new(0),
        });

        // The pattern the engine refuses: an unclosed character class.
        let answers = scoped_content("regex:[", &scope, 10, &ctx);
        assert!(
            answers
                .iter()
                .any(|a| matches!(a, Answer::Frame(Response::Warning { .. }))),
            "the refusal is said out loud: {answers:?}"
        );
        assert!(
            !answers
                .iter()
                .any(|a| matches!(a, Answer::Frame(Response::Error { .. }))),
            "and it is not the end of the answer: {answers:?}"
        );
        assert!(
            answers
                .iter()
                .any(|a| matches!(a, Answer::Frame(Response::GrepFiles { .. }))),
            "the answer is still there, and it is empty: {answers:?}"
        );
    }

    /// The scope names the file however the path is spelled.
    ///
    /// The store indexes its corpus under the root it resolved, and the
    /// caller spells the scope the way the caller was given it. Two
    /// spellings of one folder — a link, an alias, a short name — are
    /// bytes that differ, and a scope that did not resolve matched
    /// nothing: the answer came back empty, and it said it was whole.
    ///
    /// The Windows runner is where this bites by itself: its temporary
    /// directory is spelled the short way (`C:\Users\RUNNER~1\...`) and the
    /// store holds the long one (`C:\Users\runneradmin\...`), because the
    /// store resolves the root and the caller does not.
    #[test]
    fn the_scope_matches_the_store_however_the_path_is_spelled() {
        // A tree of two files that carry the needle.
        let tree = TempTree::new("rgy-searchd-tv-spelling");
        let mut scope = Vec::new();
        for name in ["a.txt", "b.txt"] {
            let path = tree.join(name);
            std::fs::write(&path, b"the needle is here\n").unwrap();
            scope.push(path.to_string_lossy().into_owned());
        }
        // And the pack store over it.
        let store = TempTree::new("rgy-searchd-tv-spelling-store");
        rgx_core::tv_runtime::builder::build_store(
            &tree,
            &store,
            &rgx_core::tv_runtime::builder::BuildConfig::default(),
        )
        .expect("the pack store is built");

        let ctx = std::sync::Arc::new(Ctx {
            root: tree.as_os_str().as_encoded_bytes().to_vec(),
            home: String::new(),
            tv_dir: Some(store.0.clone()),
            live: std::sync::RwLock::new(Live::new(Index::build(Vec::new(), 0, 0, b"/"))),
            catch_up: std::sync::atomic::AtomicU8::new(CATCH_UP_RELISTED),
            feed: std::sync::atomic::AtomicU8::new(FEED_NONE),
            changed: Changed::new(),
            content: std::sync::Mutex::new(None),
            wants_content: std::sync::atomic::AtomicBool::new(false),
            sessions: std::sync::Mutex::new(std::collections::HashMap::new()),
            sessions_next: std::sync::atomic::AtomicU64::new(0),
        });

        let count = |scope: &[String]| {
            scoped_content("grep:needle", scope, 10, &ctx)
                .iter()
                .find_map(|r| match r {
                    Answer::Frame(Response::GrepFiles { files, .. }) => Some(files.len()),
                    _ => None,
                })
                .expect("the answer carries the files")
        };

        // The scope as the caller spells it.
        assert_eq!(count(&scope), 2, "the scope names the files it lists");

        // And the same scope through a link to the tree: the same folder,
        // spelled another way. The store holds the tree it resolved, and
        // the scope holds the link.
        #[cfg(unix)]
        {
            let link = tree.with_extension("link");
            let _ = std::fs::remove_dir_all(&link);
            // The tree is handed over as the path it is: the tree borrows
            // a path, and `&TempTree` is not a path the operating system
            // is asked to make a link to.
            std::os::unix::fs::symlink(&*tree, &link).expect("a link to the tree");
            let through = vec![link.join("a.txt").to_string_lossy().into_owned()];
            assert_eq!(
                count(&through),
                1,
                "the scope names the file through the link to it"
            );
        }
    }

    /// An answer that several alternatives filled is not whole.
    ///
    /// The completeness used to be decided by each alternative's own
    /// count: two alternatives that each found fewer files than the limit
    /// could fill the limit between them, and the answer said it was
    /// whole. The limit is the total's fact.
    #[test]
    fn an_answer_filled_by_several_alternatives_is_not_whole() {
        let dir = std::env::temp_dir().join("rgy-searchd-alts-test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for i in 0..3 {
            std::fs::write(dir.join(format!("a{i}.txt")), b"alpha\n").unwrap();
            std::fs::write(dir.join(format!("b{i}.txt")), b"beta\n").unwrap();
        }
        let root = dir.to_string_lossy().into_owned().into_bytes();

        let source = PortableSource::new(root.clone(), 2);
        let live = Live::with_root(
            Index::build(source.enumerate().expect("the walk"), 0, 1, b"/"),
            &root,
        );
        let ctx = std::sync::Arc::new(Ctx {
            root: root.clone(),
            home: String::new(),
            tv_dir: None,
            live: std::sync::RwLock::new(live),
            catch_up: std::sync::atomic::AtomicU8::new(CATCH_UP_RELISTED),
            feed: std::sync::atomic::AtomicU8::new(FEED_NONE),
            changed: Changed::new(),
            content: std::sync::Mutex::new(None),
            wants_content: std::sync::atomic::AtomicBool::new(false),
            sessions: std::sync::Mutex::new(std::collections::HashMap::new()),
            sessions_next: std::sync::atomic::AtomicU64::new(0),
        });

        // Three files match the first alternative and one the second: the
        // limit of four is filled by the two of them together.
        let answer = grep("grep:alpha|grep:beta", 4, &ctx);
        let (files, complete) = answer
            .iter()
            .find_map(|r| match r {
                Answer::Frame(Response::GrepFiles {
                    files, complete, ..
                }) => Some((files.len(), *complete)),
                _ => None,
            })
            .expect("the answer carries the files");
        assert_eq!(files, 4, "the limit is the limit");
        assert!(
            !complete,
            "the limit cut the answer, so the answer is not whole"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The tree, walked again: a file the index does not have is found.
    ///
    /// A saved index is a snapshot, and "relisted" is the protocol's word for
    /// a tree that was walked again and diffed — so the walk has to reach the
    /// file the snapshot does not have. A daemon that says "relisted"
    /// without having walked answers "no such file" for a file that exists,
    /// which is the worst thing a search can do.
    #[test]
    fn the_relist_finds_a_file_the_index_does_not_have() {
        let dir = TempTree::new("rgy-searchd-relist");
        std::fs::write(dir.join("old.txt"), b"old\n").unwrap();
        let root = dir.as_os_str().as_encoded_bytes().to_vec();

        // The index as it was before the file appeared.
        let source = PortableSource::new(root.clone(), 2);
        let mut live = Live::with_root(
            Index::build(source.enumerate().expect("the walk"), 0, 1, b"/"),
            &root,
        );

        // The file appears while the daemon is off, and the tree is walked
        // again and diffed.
        std::fs::write(dir.join("new.txt"), b"new\n").unwrap();
        let listed = list_all(&live, &root);
        apply_all(&mut live, listed);

        // The answer names it.
        let searcher = Searcher { live: &live };
        let q = Query::parse("new", "").expect("the query parses");
        let hits = searcher.search_alternatives(&[q], 10);
        assert!(
            hits.iter()
                .any(|h| h.over.as_ref().is_some_and(|p| p.ends_with(b"new.txt"))),
            "the file the index did not have is in the answer; {} hits",
            hits.len()
        );
    }

    /// The limit is the limit, across the alternatives.
    ///
    /// A query with two alternatives used to answer with two times the limit:
    /// each alternative read its own limit and the answers were
    /// concatenated. The name path merges its alternatives and truncates
    /// (name_query::merge_hits), and the content path has to say the same
    /// thing.
    #[test]
    fn the_limit_is_the_limit_across_the_alternatives() {
        let dir = TempTree::new("rgy-searchd-limit");
        for i in 0..4 {
            std::fs::write(dir.join(format!("alpha{i}.txt")), b"alpha alpha\n").unwrap();
            std::fs::write(dir.join(format!("multi{i}.txt")), b"multi multi\n").unwrap();
        }
        let root = dir.as_os_str().as_encoded_bytes().to_vec();
        let source = PortableSource::new(root.clone(), 2);
        let live = Live::with_root(
            Index::build(source.enumerate().expect("the walk"), 0, 1, b"/"),
            &root,
        );
        let ctx = std::sync::Arc::new(Ctx {
            root: root.clone(),
            home: String::new(),
            tv_dir: None,
            live: std::sync::RwLock::new(live),
            catch_up: std::sync::atomic::AtomicU8::new(CATCH_UP_RELISTED),
            feed: std::sync::atomic::AtomicU8::new(FEED_NONE),
            changed: Changed::new(),
            content: std::sync::Mutex::new(None),
            wants_content: std::sync::atomic::AtomicBool::new(false),
            sessions: std::sync::Mutex::new(std::collections::HashMap::new()),
            sessions_next: std::sync::atomic::AtomicU64::new(0),
        });

        let count = |q: &str, limit: usize| {
            grep(q, limit, &ctx)
                .iter()
                .filter_map(|r| match r {
                    Answer::Frame(Response::GrepFiles { files, .. }) => Some(files.len()),
                    _ => None,
                })
                .sum::<usize>()
        };
        // Two alternatives and a limit of two: two files, not four.
        assert_eq!(
            count("grep:alpha|grep:multi", 2),
            2,
            "the limit is the limit, across the alternatives"
        );
        // Eight files match, and a limit of ten keeps all eight.
        assert_eq!(
            count("grep:alpha|grep:multi", 10),
            8,
            "every match is answered"
        );
    }

    /// A pattern the engine refuses is said, not dropped.
    ///
    /// The whole-tree grep used to skip an alternative whose pattern the
    /// engine would not compile, and answer with the others and no word
    /// about it. A query that searched less than it was asked for, and
    /// said nothing, is a query whose answer cannot be trusted.
    ///
    /// The pattern is a Unicode property: the byte engine has no answer for
    /// one. A lookahead would do as well, but a word with a `?` in it is a
    /// name glob before it is a pattern, so it never reaches this layer.
    #[test]
    fn a_refused_pattern_is_said_and_not_dropped() {
        let dir = TempTree::new("rgy-searchd-refused");
        std::fs::write(dir.join("alpha.txt"), b"alpha\n").unwrap();
        let root = dir.as_os_str().as_encoded_bytes().to_vec();
        let source = PortableSource::new(root.clone(), 2);
        let live = Live::with_root(
            Index::build(source.enumerate().expect("the walk"), 0, 1, b"/"),
            &root,
        );
        let ctx = std::sync::Arc::new(Ctx {
            root: root.clone(),
            home: String::new(),
            tv_dir: None,
            live: std::sync::RwLock::new(live),
            catch_up: std::sync::atomic::AtomicU8::new(CATCH_UP_RELISTED),
            feed: std::sync::atomic::AtomicU8::new(FEED_NONE),
            changed: Changed::new(),
            content: std::sync::Mutex::new(None),
            wants_content: std::sync::atomic::AtomicBool::new(false),
            sessions: std::sync::Mutex::new(std::collections::HashMap::new()),
            sessions_next: std::sync::atomic::AtomicU64::new(0),
        });

        let warnings = |answers: &[Answer]| -> Vec<String> {
            answers
                .iter()
                .filter_map(|r| match r {
                    Answer::Frame(Response::Warning { message }) => Some(message.clone()),
                    _ => None,
                })
                .collect()
        };
        let files = |answers: &[Answer]| -> usize {
            answers
                .iter()
                .filter_map(|r| match r {
                    Answer::Frame(Response::GrepFiles { files, .. }) => Some(files.len()),
                    _ => None,
                })
                .sum::<usize>()
        };

        // The refused alternative alone: the refusal is said, and nothing
        // is claimed to have been found.
        let answers = grep(r"regex:\p{L}+", 10, &ctx);
        let said = warnings(&answers);
        assert_eq!(said.len(), 1, "the refusal is said: {answers:?}");
        assert!(
            said[0].contains("unicode property"),
            "the reason is said: {}",
            said[0]
        );
        assert_eq!(files(&answers), 0, "a refused pattern finds nothing");

        // And beside an alternative that runs: the answer is the one that
        // could be given, and the refusal is still said.
        let answers = grep(r"grep:alpha|regex:\p{L}+", 10, &ctx);
        assert_eq!(
            files(&answers),
            1,
            "the alternative that could be run is answered"
        );
        assert_eq!(
            warnings(&answers).len(),
            1,
            "the refusal is said: {answers:?}"
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
        let dir = TempTree::new("rgy-searchd-live-many");
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
            name: rgy_engine::console::spell(&root),
        }]);

        let ctx = std::sync::Arc::new(Ctx {
            root: root.clone(),
            home: String::new(),
            tv_dir: None,
            live: std::sync::RwLock::new(live),
            catch_up: std::sync::atomic::AtomicU8::new(CATCH_UP_RELISTED),
            feed: std::sync::atomic::AtomicU8::new(FEED_NONE),
            changed: Changed::new(),
            content: std::sync::Mutex::new(None),
            wants_content: std::sync::atomic::AtomicBool::new(false),
            sessions: std::sync::Mutex::new(std::collections::HashMap::new()),
            sessions_next: std::sync::atomic::AtomicU64::new(0),
        });

        let responses = handle(
            Request::Search {
                q: "late".to_string(),
                limit: 10,
                session: None,
            },
            &ctx,
        );
        let hits = responses
            .iter()
            .find_map(|r| match r {
                Answer::Frame(Response::Hits { hits, .. }) => Some(hits),
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
