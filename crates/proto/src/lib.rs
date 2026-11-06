// SPDX-License-Identifier: Apache-2.0

//! JSON-lines protocol types shared by `searchd`, stdio, and `searchctl`
//!.
//!
//! One request per line in, one response per line out — identical over
//! named pipe (Windows), UDS (Unix), and stdio. Hand-rolled encode/decode:
//! the protocol is small enough that a dependency would be the bigger
//! surface. Field names follow the operation model; rgx `--serve` translates
//! to the same engine calls behind a compatibility front.
//!
//! The crate is `no_std`: the protocol is bytes on a wire, and a type that
//! only ever describes bytes has no business carrying a filesystem. The
//! one thing `std` was giving it — `f64::from_str` — is a float parser
//! this crate now owns ([`num_value`]), because the numbers the protocol
//! carries are integers anyway.

#![no_std]

extern crate alloc;
/// The standard library, when the machine is in play.
///
/// The crate is `no_std` at its root on purpose — the protocol is bytes on a
/// wire — and the `std` feature is what links the library back in for the
/// client, which is the only part that touches a socket. The test build
/// needs it too, for the macros.
#[cfg(any(test, feature = "std"))]
#[macro_use]
extern crate std;

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

pub mod endpoint;

#[cfg(feature = "std")]
pub mod client;

#[cfg(feature = "std")]
pub use client::{Client, DaemonStatus, GrepOutcome, Resolved, SearchOutcome};
#[cfg(feature = "std")]
pub use endpoint::Endpoint;

/// A request line: an op plus its arguments.
#[derive(Debug, Clone, PartialEq)]
pub enum Request {
    /// Name search: `{"op":"search","q":…,"limit":…}`.
    ///
    /// The `session` is the incremental-search id: the
    /// daemon keeps a session per id, and a query that narrows the
    /// previous one is answered by re-ranking the previous result set
    /// instead of walking the index again. It is optional and additive:
    /// a request without one is answered by a fresh search, exactly as
    /// before.
    Search {
        q: String,
        limit: usize,
        session: Option<u64>,
    },
    /// Content grep over the **whole tree**: `{"op":"grep","q":…,"limit":…}`
    /// (Phase 4 engine).
    ///
    /// The scoped content search is [`Request::Content`], and its op is
    /// `"content"`: the two are different questions, and a request that
    /// says `"content"` is the scoped one.
    Grep { q: String, limit: usize },
    /// Content search **over a scope**: `{"op":"content","q":…,"paths":[…]}`.
    ///
    /// The scope is the paths the caller already has — the answer of a name
    /// search — and the search reads only those. It is the second layer of
    /// an interface: the files are already chosen, and the question is what
    /// is inside them.
    ///
    /// The scope is what makes this cheap. A content search over the whole
    /// tree reads the whole tree; a content search over two hundred chosen
    /// files reads two hundred files, and the daemon's content index rules
    /// most of them out before a byte is read.
    ///
    /// `"grep-scoped"` is the same request under the name it had before,
    /// and it is the name the crate writes: a daemon that predates the
    /// fix reads `"content"` as a whole-tree search, and a scope that is
    /// dropped is a search that answers with the whole tree. Both names
    /// are read — and a request that names a scope it does not carry is
    /// refused, never widened into one.
    ///
    /// **The content route has no session.** The name search has one
    /// because the daemon keeps the previous result set and re-ranks it
    /// when the next query narrows it. The content answer is the files on
    /// the disk: it is read, not kept, and there is no result set to
    /// re-rank — the scope is the caller's, and the scope is already on
    /// the wire. A session id here was a promise the daemon could not
    /// keep, and a promise the wire makes is a promise the reader waits
    /// for: the field is gone rather than carried and ignored.
    Content {
        q: String,
        paths: Vec<String>,
        limit: usize,
    },
    /// Resolve a path through the lookup surface: `{"op":"resolve","path":…}`.
    ///
    /// The five indexes of the lookup answer it: the
    /// `file_path` and `folder_path` indexes give the path of an entry, the
    /// `file_name` and `folder_name` indexes the entries that carry a name,
    /// and the `folder_backend` index the volume that owns a folder.
    Resolve { path: String },
    /// `{"op":"status"}` — index size, event id, uptime.
    Status,
}

/// How the index caught up after the last restart.
///
/// Two values, and the difference is the point (the live-feed honesty
/// rule). The field carries what the daemon *did*, not what
/// the platform could have done:
///
/// * `replayed` — the daemon resumed the OS's journal at the watermark the
///   index carries and applied the changes that happened while it was off
///   (the USN journal on Windows, the FSEvents event id on macOS);
/// * `relisted` — it did not: the tree was walked again and diffed, either
///   because that is the only way this platform can catch up (inotify on
///   Linux, kqueue on FreeBSD) or because the journal no longer reaches
///   back to the watermark.
///
/// A string rather than a bool: "did it catch up" is not the question a
/// reader has — "replayed or relisted" is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CatchUp {
    /// The journal was resumed; the changes since the watermark were applied.
    Replayed,
    /// The tree was walked again and diffed; there was no journal to resume.
    Relisted,
}

impl CatchUp {
    /// The wire spelling. These two strings are the contract.
    pub fn as_str(&self) -> &'static str {
        match self {
            CatchUp::Replayed => "replayed",
            CatchUp::Relisted => "relisted",
        }
    }
}

/// Whether the tree is still being followed.
///
/// Three values, and the difference is the point: a daemon whose feed died
/// answers on a snapshot of the tree, and a reader that is not told that
/// the tree is no longer followed is told a tree that is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Feed {
    /// The feed runs: the tree is followed.
    Live,
    /// The feed died: the index is a snapshot from here on.
    Gone,
    /// There is no feed: nothing follows the tree.
    None,
}

impl Feed {
    /// The wire spelling. These three strings are the contract.
    pub fn as_str(&self) -> &'static str {
        match self {
            Feed::Live => "live",
            Feed::Gone => "gone",
            Feed::None => "none",
        }
    }
}

/// A response line.
#[derive(Debug, Clone, PartialEq)]
pub enum Response {
    /// `{"op":"hits","hits":[…],"took_us":…,"complete":…}` — one per request
    /// with matches.
    ///
    /// `complete` says the answer is whole. The name layer could not say it
    /// before: a list of hits looked like the whole answer, and a list that
    /// reached the limit was the same list as one that did not. The field
    /// is additive, like `tv` on the status and like `complete` on the
    /// content answer: a reader that does not know it keeps working, and a
    /// line without it is a line from a daemon that does not say, which
    /// reads as "not complete" — the answer that promises the least.
    Hits {
        hits: Vec<Hit>,
        took_us: u64,
        /// Whether the answer is whole.
        ///
        /// The rule is the number of hits: an answer with fewer hits than
        /// the limit is the whole answer, and one that reached the limit may
        /// have been cut short. It is the rule the content answer follows
        /// too, so a client reads one answer the way it reads the other.
        complete: bool,
        /// The session this answer belongs to.
        ///
        /// The session is the incremental-search id: the daemon keeps the
        /// answer under it, and the next request that carries the same id
        /// is answered by re-ranking this one instead of walking the index
        /// again. The id is on the answer because the client has to be able
        /// to send it back — a client that cannot name the session it is in
        /// cannot ask for the re-rank, and a session the client cannot name
        /// is a session the client cannot use.
        ///
        /// The daemon names the session when the request does not: the
        /// request may leave it out, and the answer says which session it
        /// was computed for. The field is additive in the way the others
        /// are: a line without it is a line from a daemon that does not say,
        /// and the reader keeps the session it had.
        ///
        /// And a session that is not one is no session at all: the rule is
        /// the request's own, and a peer that answers `-1` does not get to
        /// name the session `0` — which is in the client's own space, the
        /// space the daemon trusts.
        session: Option<u64>,
    },
    /// `{"op":"done","count":…}` — terminator for a hit stream.
    ///
    /// The count is the number of entries the answer carried, and it is the
    /// terminator's own count: the client counts what it got, which is the
    /// same number, and a reader of the wire sees the answer end with the
    /// size of the answer. The frame is the end of the stream — a client
    /// that reads it knows there is nothing more coming — and the count is
    /// what it says about the stream.
    ///
    /// The count is the one field of this frame that is not optional, and
    /// the reason is the reader's own: it has no way to say "not known" in
    /// a `usize`, and a zero here would be a claim that the answer carried
    /// nothing — a claim the peer never made. So a frame without the count
    /// is a frame the reader cannot use, and it refuses it, the way it
    /// refuses a hit without a path.
    ///
    /// And a count that cannot be true is not a count the reader can
    /// report: a negative one, or one the wire cannot carry exactly, is
    /// refused the same way.
    Done { count: usize },
    /// `{"op":"status","docs":…,"event_id":…,"catch_up":…,"tv":…,"feed":…}`.
    ///
    /// `catch_up` is additive: a reader that does not know the field keeps
    /// working, and the field is what tells a replayed catch-up from a
    /// relisted one.
    ///
    /// `event_id` is the journal position the daemon's index is current as
    /// of — the platform's own counter, which the tree does not control. It
    /// travels as an `f64`, like every number on the wire, so an integer
    /// above `2^53` is a number the reader reads back as a different one.
    /// The field is a report and nothing sends it back, so the number that
    /// arrives is a report and not a request; the limit is written here so
    /// that a reader knows what it is looking at.
    ///
    /// A negative position is not a position at all — the journal has none
    /// before its first entry — and the cast the reader used made it the
    /// start of the journal, a position the peer never claimed. So a
    /// negative one refuses the frame, the way a count that cannot be true
    /// refuses it.
    ///
    /// A missing position is additive: the field is optional, and the
    /// conservative reading of "no position" is `0` — which is exactly
    /// what a daemon that relisted the tree reports. That is the same
    /// reading the `catch_up` field's own default carries: a `status` line
    /// without a `catch_up` reads back as `relisted` — the conservative
    /// answer, the one that says the tree was walked again.
    ///
    /// `tv` says whether the daemon was started with a **pack store** —
    /// the corpus packed once and mmapped — and it is what tells a client
    /// that the content layer is answered by the store rather than by a
    /// walk. It is additive in the same way: a reader that does not know
    /// it reads "no store", which is the conservative answer.
    Status {
        /// The count of entries the daemon's index holds.
        ///
        /// The launcher draws the count, and a header that says "0" about
        /// an index of a thousand entries is a wrong fact — a wrong fact
        /// is worse than no fact. A count is a count of things: it is not
        /// negative, and it is a number the reader can carry and the wire
        /// can carry exactly. A count that is not one is not a count the
        /// reader can report, so the reader refuses the line — a daemon
        /// that cannot count its own index has answered badly, and the
        /// caller says so.
        ///
        /// The count is also the one field of this frame that is not
        /// optional: a line without it is a line the reader cannot use,
        /// because the reader has no way to say "not known" in a `usize`.
        docs: usize,
        /// The journal position the daemon's index is current as of.
        ///
        /// The position is a report — the daemon's own counter, which the
        /// tree does not control, and nothing sends it back — and the rule
        /// is the variant's own: a negative position refuses the frame, and
        /// a missing one is the start of the journal.
        event_id: u64,
        catch_up: CatchUp,
        /// Whether the tree is still being followed.
        ///
        /// Three values: `live` (the feed runs), `gone` (the feed died and
        /// the index is a snapshot from here on) and `none` (there is no
        /// feed). A daemon whose feed died answers on a frozen index, and a
        /// reader that is not told that the tree is no longer followed is
        /// told a tree that is.
        ///
        /// The field is additive in the same way `tv` is: a reader that does
        /// not know it reads the status as it was before it existed.
        feed: Feed,
        /// Whether the daemon has a pack store (`--tv-dir`).
        tv: bool,
    },
    /// `{"op":"grep-files","files":[…],"took_us":…,"complete":…,"skipped":…}`
    /// — content matches.
    ///
    /// `complete` says the answer is whole, and `skipped` says how many
    /// files the search did not read for a reason of its own -- a budget
    /// that ran out, or a cloud placeholder it refused to download. The
    /// files the limit left behind are not counted: the limit is the
    /// caller's, and a search that stopped because it already had the
    /// limit read what it was asked to read.
    ///
    /// A truncated search and a complete one are different answers, and the
    /// difference is on the wire: the two fields are additive, so a reader
    /// that does not know them keeps working — and a line without them is a line
    /// from a daemon that does not say, which reads as "not complete",
    /// the answer that promises the least.
    ///
    /// `skips` says **why** the files were not read, and it is additive in
    /// the same way: the count is what the protocol has always carried, and
    /// the count alone is a number the reader has to guess about — "skipped"
    /// leaves the user wondering whether the file was empty, locked, or
    /// never there. The reasons are what make the number useful, and the
    /// count stays where it is so that a reader that does not know the new
    /// field keeps working.
    GrepFiles {
        files: Vec<GrepFile>,
        took_us: u64,
        /// Whether the answer is whole.
        complete: bool,
        /// How many files the search did not read for a reason of its own.
        ///
        /// The count and the reasons are one fact written twice, and the
        /// protocol says the reasons add up to the count. The reader reads
        /// the count the reasons make — their sum — so the answer's own two
        /// numbers agree by construction, and the wire's count is the count
        /// only when there are no reasons to read it from.
        skipped: usize,
        /// Why the files were not read.
        ///
        /// The counts add up to `skipped`: every file the search did not
        /// read is in one of these groups, with the reason it was not read.
        /// Empty is a line from a daemon that does not say — the same
        /// conservative reading as every other additive field.
        skips: Vec<Skip>,
    },
    /// `{"op":"resolved","path":…,"folder":…,"backend":…}` — the answer of
    /// a `resolve` request.
    ///
    /// `folder` says whether the path is a folder, and `backend` which
    /// volume owns it (the `folder_backend` index of parity row I.1).
    Resolved {
        path: String,
        folder: bool,
        backend: u32,
    },
    /// `{"op":"error","message":…}` — the request failed; the stream lives.
    Error { message: String },
    /// `{"op":"warning","message":…}` — fail-closed query warning (unknown
    /// `type:`, bad `re:`), matching zen-rs's `warning:` lines.
    Warning { message: String },
}

/// One hit: a path and its score.
///
/// The `span` is the character positions of the **name** that matched, and
/// it is what lets an interface say *which* part of the name the query
/// found. It is empty when the daemon could not say: a query with more
/// than one alternative does not say which alternative a hit came from, and
/// a span that guessed would be a lie in the place the user looks first.
#[derive(Debug, Clone, PartialEq)]
pub struct Hit {
    pub path: String,
    pub score: i32,
    /// The character offsets of the matched characters in the name, in
    /// order.
    ///
    /// The first character of the name is 0, and the offsets count the
    /// name as it is written: the engine folds the name to score it, and
    /// the positions it sends are the positions in the name the user
    /// sees.
    pub span: Vec<u32>,
    /// Whether the hit is a folder.
    ///
    /// The index knows and the path does not say: a folder's path is
    /// spelled like a file's, and an interface that shows a folder with a
    /// file's icon is showing the wrong picture of the thing the user is
    /// about to open.
    ///
    /// It is additive on the wire: a hit without the field is a file.
    pub folder: bool,
}

impl Hit {
    /// A hit with no span.
    pub fn new(path: String, score: i32) -> Hit {
        Hit {
            path,
            score,
            span: Vec::new(),
            folder: false,
        }
    }

    /// The same hit, and whether it is a folder.
    pub fn with_folder(mut self, folder: bool) -> Hit {
        self.folder = folder;
        self
    }
}

/// Whether a name answer is the whole answer.
///
/// The rule is the number of hits: an answer with fewer hits than the
/// limit is the whole answer, and one that reached the limit may have been
/// cut short. It is the rule the content answer follows too, and it costs
/// one comparison on a length the answer already has.
///
/// The rule lives here because this is where the meaning of the fact lives,
/// and every producer of a name answer has to agree about it: the daemon's
/// whole tree and its scope, the fresh search and the session's re-rank,
/// and the command line's own search when there is no daemon to ask.
pub fn name_answer_is_complete(hits: usize, limit: usize) -> bool {
    hits < limit
}

/// One file's content matches: path and the matching lines as
/// `(line number, text)` — the `grep:` result shape (rg's `path:line:text`).
///
/// The line numbers are **1-based**: the first line of a file is line 1, the
/// way `rg` prints them and the way the engine counts them. So a number that
/// is not a line is not a line, and the reader refuses a frame that carries
/// one — the answer must not name a line the file does not have.
#[derive(Debug, Clone, PartialEq)]
pub struct GrepFile {
    pub path: String,
    pub lines: Vec<(usize, String)>,
}

/// Why a file was not read.
///
/// The count of the files the search did not read is on the answer; the
/// reason is what makes the count worth reading. A file that was not read
/// because it is a cloud placeholder is a file the user can ask for, and a
/// file the search's budget never reached is a file the user has to ask for
/// again — and a count that does not say which of the two it was leaves the
/// reader guessing.
///
/// The files of one reason are one entry: a tree with a thousand placeholders
/// is one thing that happened, and a thousand entries would be a thousand
/// lines of wire for one fact.
#[derive(Debug, Clone, PartialEq)]
pub struct Skip {
    /// Why the files were not read.
    ///
    /// The daemon's own words: "a cloud placeholder; reading it would
    /// download it", "the search's budget ran out before it reached them".
    pub reason: String,
    /// How many files were not read for this reason.
    pub count: usize,
    /// One of the files, when the search knows which file it is.
    ///
    /// The budget stopped the search before it named the files it did not
    /// reach, so there is no file to name for that reason — and a path
    /// that was made up would be a path the user cannot open.
    pub path: Option<String>,
}

impl Request {
    /// How many hits the request asks for.
    ///
    /// The requests that carry hits carry a limit; the ones that do not
    /// (a resolve, a status) answer with one thing, and a limit on those
    /// is a limit on nothing.
    pub fn limit(&self) -> usize {
        match self {
            Request::Search { limit, .. } | Request::Grep { limit, .. } => *limit,
            Request::Content { limit, .. } => *limit,
            _ => 0,
        }
    }

    /// The session the request names, when it names one.
    ///
    /// The name search carries the session; the content route does not,
    /// because the content answer is read and not kept — there is no
    /// result set to re-rank.
    pub fn session(&self) -> Option<u64> {
        match self {
            Request::Search { session, .. } => *session,
            _ => None,
        }
    }

    /// Parse one JSON line. Returns `None` on anything malformed.
    pub fn from_json(line: &str) -> Option<Request> {
        let v = parse_json(line).ok()?;
        let obj = v.as_object()?;
        let op = obj.get("op")?.as_str()?;
        // The limit is how many hits the answer carries, and it is
        // clamped at both ends — the rule is written here, and not left
        // to a cast that saturates.
        //
        // The floor is zero and not one because a request for a
        // negative number of hits is a request for no hits. The daemon
        // answers it with an empty list and says it stopped at the
        // limit — the answer carries `complete: false`, because the
        // answer reached the limit — so the caller is not left thinking
        // there was nothing to find. A floor of one would answer with a
        // hit the caller did not ask for.
        //
        // The cap is the tree's own: the daemon holds the whole index,
        // and a request that asks for the whole index is a request to
        // make the daemon materialise it.
        //
        // A limit that is not a whole number is a limit that cannot be
        // true: the answer carries whole hits, and a request for 1.9
        // hits is a request for a number of hits that does not exist.
        // The rule for every other number on this wire is to refuse it
        // (`count_of`, `session_id`, `span_offset`), and the limit was
        // the only one that cast it into a *different* number instead:
        // 1.9 became 1, and the caller was told the answer was complete
        // for a limit it never asked for.
        let limit = match obj.get("limit").and_then(|l| l.as_num()) {
            Some(n) if n < 0.0 => 0,
            Some(n) if n >= MAX_LIMIT as f64 => MAX_LIMIT,
            Some(n) if !whole_number(n) => return None,
            Some(n) => n as usize,
            // No limit at all: the default.
            None => 50,
        };
        match op {
            "search" => Some(Request::Search {
                q: obj.get("q")?.as_str()?.to_string(),
                limit,
                // The session is the same rule on both sides of the wire,
                // and the rule lives in one place — a session that is
                // not one is no session at all.
                session: obj
                    .get("session")
                    .and_then(|s| s.as_num())
                    .and_then(session_id),
            }),
            "grep" => Some(Request::Grep {
                q: obj.get("q")?.as_str()?.to_string(),
                limit,
            }),
            // The scoped request, under both of its names. The scope is
            // read as it is written: a path that is not a string fails
            // the whole request, because a scope that is quietly
            // narrowed is a scope the caller believes it set and did
            // not — and a request that names no scope at all is
            // refused, never widened into a search of the whole tree.
            "content" | "grep-scoped" => {
                let mut paths = Vec::new();
                for p in obj.get("paths")?.as_array()? {
                    paths.push(p.as_str()?.to_string());
                }
                Some(Request::Content {
                    q: obj.get("q")?.as_str()?.to_string(),
                    paths,
                    limit,
                })
            }
            "resolve" => Some(Request::Resolve {
                path: obj.get("path")?.as_str()?.to_string(),
            }),
            "status" => Some(Request::Status),
            _ => None,
        }
    }

    pub fn to_json(&self) -> String {
        match self {
            Request::Search { q, limit, session } => {
                // The session id is additive: a request without one is the
                // request as it was before the field existed.
                match session {
                    Some(s) => format!(
                        r#"{{"op":"search","q":"{}","limit":{},"session":{}}}"#,
                        escape_json_string(q),
                        limit,
                        s
                    ),
                    None => format!(
                        r#"{{"op":"search","q":"{}","limit":{}}}"#,
                        escape_json_string(q),
                        limit
                    ),
                }
            }
            Request::Grep { q, limit } => {
                format!(
                    r#"{{"op":"grep","q":"{}","limit":{}}}"#,
                    escape_json_string(q),
                    limit
                )
            }
            Request::Content { q, paths, limit } => {
                let items: Vec<String> = paths
                    .iter()
                    .map(|p| format!("\"{}\"", escape_json_string(p)))
                    .collect();
                // The older spelling goes on the wire, and it is the one
                // every daemon has read: a daemon that predates the fix
                // reads "content" as a whole-tree search, and the scope
                // it drops is a search that answers with the whole tree.
                // The reader takes both.
                //
                // There is no session on this request: the content route
                // has none, and a request that carries one is read as the
                // request it was before the field existed.
                format!(
                    r#"{{"op":"grep-scoped","q":"{}","paths":[{}],"limit":{}}}"#,
                    escape_json_string(q),
                    items.join(","),
                    limit
                )
            }
            Request::Resolve { path } => {
                format!(
                    r#"{{"op":"resolve","path":"{}"}}"#,
                    escape_json_string(path)
                )
            }
            Request::Status => r#"{"op":"status"}"#.to_string(),
        }
    }
}

/// The deepest document the reader accepts.
///
/// The protocol's own answers nest three or four levels deep, and the cap
/// is far above that. It is there because the reader recurses once per
/// level: a document that nests deeper would overflow the stack, and a
/// stack overflow is an abort, not a panic — and this crate runs inside a
/// resident process, where an abort is the end of the program.
const MAX_DEPTH: usize = 64;

/// The most hits one request may ask for.
///
/// The limit is how many hits the answer carries, and the answer is a
/// list: the daemon holds the whole index, and a request that asks for
/// the whole index is a request to make the daemon materialise it. The
/// reader used to take the number as it came, and `1e30` became
/// `usize::MAX` — which reads as "no cap" and means the whole index in
/// one answer.
///
/// A clamped limit is not a lie: the answer says whether it was cut
/// short (`complete` is false when the list reached the limit), so the
/// client knows the list is the first N and not all of them.
pub const MAX_LIMIT: usize = 100_000;

/// The last integer the wire can carry exactly.
///
/// The protocol carries a number as an `f64`, and every integer above
/// `2^53` is a number the reader reads back as a different one. The
/// session ids live under it for that reason, and so does anything else
/// a client sends back: an id above the line would arrive as another
/// number, and the client would ask about the wrong one.
///
/// The limit is **inclusive**: `2^53` is a power of two, and a power of
/// two is exactly representable.
pub const WIRE_LIMIT: u64 = 1 << 53;

/// Whether a number is a whole one.
///
/// The cast truncates, and a peer that sent `1.5` would have been read as
/// the number `1` — a fact the peer never stated. A session id, a span
/// offset and a count are all counts of things, and half of one is not a
/// thing that exists.
///
/// The check is done with the cast itself and not with `fract()`: `fract`
/// is a `std` method, and this crate is `no_std` — the reader has to
/// refuse the same numbers with and without an operating system under it.
/// The cast saturates, so the number has to be inside the bound before it
/// is asked about; every caller checks its own bound first.
fn whole_number(n: f64) -> bool {
    (n as u64) as f64 == n
}

/// The session a number names, when the number names one.
///
/// The rule lives here because this is where the meaning of the fact
/// lives, and both sides of the wire have to agree about it: a request
/// that names no session is a fresh search, and an answer that names no
/// session leaves the client with the session it had.
///
/// The reader used to cast the number to `u64`, and a cast saturates: a
/// peer that sent `-1` was read as the id `0`, which is in the
/// *client's* space — the space the daemon trusts. Two clients, one
/// sending `-1` and one sending `0`, would have shared a result set,
/// and the answer side was worse: a peer that answered `-1` had its
/// answer read as the session `0`, which the client kept and sent back.
///
/// An id is a number the wire can carry exactly, and it is not a
/// negative one. A negative id is not an id, and an id above the wire's
/// limit is not an id the wire can carry: both are no session at all.
///
/// A fractional id is not an id either. The cast truncates, and a peer that
/// sent `1.5` would have been read as the id `1` — a session the peer
/// never named, in the daemon's space. The reader refuses a number that is
/// not a whole one, the same way it refuses a negative one.
fn session_id(n: f64) -> Option<u64> {
    if n >= 0.0 && n <= WIRE_LIMIT as f64 && whole_number(n) {
        Some(n as u64)
    } else {
        None
    }
}

/// The character offset a number names, when the number names one.
///
/// The offsets count the characters of the name from the first one, so
/// an offset is not negative — and a cast would have made `-1` the
/// first character, which is not what the peer said. An offset the
/// reader cannot hold is not an offset either.
///
/// The span is all of it or none of it, so an offset that is not an
/// offset takes the whole span with it: the answer then carries no
/// span at all, which is what the daemon says when it cannot say which
/// part of the name matched.
///
/// An offset counts characters, so it is a whole number. A cast truncates,
/// and a peer that sent `1.5` would have been read as the offset `1` —
/// a span that points into the middle of a name the peer never pointed at.
/// The reader refuses it, and the span goes with it.
fn span_offset(n: f64) -> Option<u32> {
    if n >= 0.0 && n <= u32::MAX as f64 && whole_number(n) {
        Some(n as u32)
    } else {
        None
    }
}

/// The count a number names, when the number names one.
///
/// A count is a count of things: it is not negative, and it is a number
/// the reader can carry. The two bounds are the wire's limit (`2^53`,
/// the last number the wire carries exactly) and the reader's own
/// `usize` — the smaller of the two is the rule, so the count the
/// reader reports is a count the reader can hold and the wire can
/// carry.
///
/// A count that is not one is not a count the reader can report. A zero
/// is a fact — "the index is empty", "it read every file" — and a wrong
/// fact is worse than no fact, so what the reader does with a count
/// that cannot be true is the frame's own business: a count the reader
/// cannot vouch for is refused, and a field that has no slot for "not
/// known" has no other honest reading.
///
/// A count is a count of things, so it is a whole number. A cast
/// truncates, and a peer that sent `1.5` would have been read as the
/// count `1` — a fact the peer never stated. One and a half files is
/// not a count that can be true, and the reader refuses it.
fn count_of(n: f64) -> Option<usize> {
    let limit = if (usize::MAX as f64) < (WIRE_LIMIT as f64) {
        usize::MAX as f64
    } else {
        WIRE_LIMIT as f64
    };
    if n >= 0.0 && n <= limit && whole_number(n) {
        Some(n as usize)
    } else {
        None
    }
}

/// Why a line is not what the reader was looking for.
///
/// The reason is part of the answer. A line that is not JSON, a line that
/// is JSON but not an answer, and a line that nests deeper than the
/// reader allows are three different failures, and a reader that says
/// "nothing" about all three is a reader that turns a broken daemon into
/// an empty one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LineError {
    /// The line is not JSON: the reader refused it.
    NotJson,
    /// The line is JSON but it is not an answer: it has no `op`, or an
    /// `op` this reader does not know, or a field the reader cannot read.
    NotAnAnswer,
    /// The line nests deeper than the reader allows.
    TooDeep,
}

impl Response {
    pub fn to_json(&self) -> String {
        match self {
            Response::Hits {
                hits,
                took_us,
                complete,
                session,
            } => {
                let items: Vec<String> = hits
                    .iter()
                    .map(|h| {
                        // The fields in the order a person reads them, and
                        // the optional ones only when they say something:
                        // the common hit — a file, with a span — is two
                        // fields shorter than it could be.
                        let mut fields = alloc::vec![
                            format!(r#""path":"{}""#, escape_json_string(&h.path)),
                            format!(r#""score":{}"#, h.score),
                        ];
                        if !h.span.is_empty() {
                            let span: Vec<String> = h.span.iter().map(|c| c.to_string()).collect();
                            fields.push(format!(r#""span":[{}]"#, span.join(",")));
                        }
                        if h.folder {
                            fields.push(alloc::string::String::from(r#""folder":true"#));
                        }
                        format!("{{{}}}", fields.join(","))
                    })
                    .collect();
                // The session is the last field and it is there only when
                // there is one: the session is what the client sends back,
                // and a client that has no session to send is answered the
                // way it was answered before the field existed.
                let session = match session {
                    Some(s) => format!(r#","session":{s}"#),
                    None => String::new(),
                };
                format!(
                    r#"{{"op":"hits","hits":[{}],"took_us":{},"complete":{}{}}}"#,
                    items.join(","),
                    took_us,
                    complete,
                    session
                )
            }
            Response::Done { count } => format!(r#"{{"op":"done","count":{count}}}"#),
            Response::Status {
                docs,
                event_id,
                catch_up,
                tv,
                feed,
            } => {
                format!(
                    r#"{{"op":"status","docs":{docs},"event_id":{event_id},"catch_up":"{}","tv":{tv},"feed":"{}"}}"#,
                    catch_up.as_str(),
                    feed.as_str()
                )
            }
            Response::GrepFiles {
                files,
                took_us,
                complete,
                skipped,
                skips,
            } => {
                let items: Vec<String> = files
                    .iter()
                    .map(|f| {
                        let ls: Vec<String> = f
                            .lines
                            .iter()
                            .map(|(n, t)| format!("[{n},\"{}\"]", escape_json_string(t)))
                            .collect();
                        format!(
                            r#"{{"path":"{}","lines":[{}]}}"#,
                            escape_json_string(&f.path),
                            ls.join(",")
                        )
                    })
                    .collect();
                // The reasons are groups: how many files, why, and one of
                // them when the search knows which file it is. The path is
                // left out when there is none -- the budget stopped the
                // search before it named the files it did not reach.
                let reasons: Vec<String> = skips
                    .iter()
                    .map(|s| {
                        let path = match &s.path {
                            Some(p) => format!(r#","path":"{}""#, escape_json_string(p)),
                            None => String::new(),
                        };
                        format!(
                            r#"{{"reason":"{}","count":{}{}}}"#,
                            escape_json_string(&s.reason),
                            s.count,
                            path
                        )
                    })
                    .collect();
                format!(
                    r#"{{"op":"grep-files","files":[{}],"took_us":{},"complete":{},"skipped":{},"skips":[{}]}}"#,
                    items.join(","),
                    took_us,
                    complete,
                    skipped,
                    reasons.join(",")
                )
            }
            Response::Resolved {
                path,
                folder,
                backend,
            } => {
                format!(
                    r#"{{"op":"resolved","path":"{}","folder":{},"backend":{}}}"#,
                    escape_json_string(path),
                    folder,
                    backend
                )
            }
            Response::Error { message } => {
                format!(
                    r#"{{"op":"error","message":"{}"}}"#,
                    escape_json_string(message)
                )
            }
            Response::Warning { message } => {
                format!(
                    r#"{{"op":"warning","message":"{}"}}"#,
                    escape_json_string(message)
                )
            }
        }
    }

    /// Read one answer line, saying why it is not one.
    ///
    /// The mirror of [`Response::to_json`], and the reason the protocol can
    /// have a client at all: a daemon answers in exactly the shape it
    /// serialises, and a reader that only knew how to write would be a
    /// reader that could not read.
    ///
    /// A field that is missing is a field that is optional: a `hits` line
    /// without a `span` reads back as a hit without one, and a `status` line
    /// without a `catch_up` reads back as `relisted` — the conservative
    /// answer, the one that says the tree was walked again.
    ///
    /// The count of a `status` line is the exception, because the reader has
    /// no slot for "not known": the count is the size of the index, and a
    /// zero is a claim about an index that plainly has entries. So a line
    /// without the count is not an answer the reader can use, and it refuses
    /// it — the same way it refuses a hit without a path.
    ///
    /// The reason is part of the answer: a client that cannot read the line
    /// and a client that read a line it does not know are two different
    /// failures, and a caller that shows one of them as "no results" is
    /// showing a lie.
    pub fn from_json_checked(line: &str) -> Result<Response, LineError> {
        let v = parse_json(line)?;
        let obj = v.as_object().ok_or(LineError::NotAnAnswer)?;
        let op = obj
            .get("op")
            .and_then(|o| o.as_str())
            .ok_or(LineError::NotAnAnswer)?;
        Ok(match op {
            "hits" => {
                let mut hits = Vec::new();
                for it in obj
                    .get("hits")
                    .and_then(|h| h.as_array())
                    .ok_or(LineError::NotAnAnswer)?
                {
                    // A hit the protocol cannot read is not a hit that is
                    // quietly left out: the count the client shows is
                    // the count of what came back, and a hit that
                    // vanished makes that count a lie.
                    let o = it.as_object().ok_or(LineError::NotAnAnswer)?;
                    let path = o
                        .get("path")
                        .and_then(|p| p.as_str())
                        .ok_or(LineError::NotAnAnswer)?
                        .to_string();
                    let score = o.get("score").and_then(|n| n.as_num()).unwrap_or(0.0) as i32;
                    // The span is optional, and it is all of it or none
                    // of it: a span that guessed would be a lie in the
                    // place the user looks first.
                    //
                    // An offset that is not an offset takes the whole
                    // span with it — the rule is the span's own, and a
                    // cast would have made a negative offset the first
                    // character of the name.
                    let span = match o.get("span").and_then(|s| s.as_array()) {
                        Some(items) => items
                            .iter()
                            .map(|n| n.as_num().and_then(span_offset))
                            .collect::<Option<Vec<u32>>>()
                            .unwrap_or_default(),
                        None => Vec::new(),
                    };
                    // The folder flag is additive: an older daemon's
                    // hit has no field, and a hit with no field is a
                    // file.
                    let folder = o.get("folder").and_then(|f| f.as_bool()).unwrap_or(false);
                    hits.push(Hit {
                        path,
                        score,
                        span,
                        folder,
                    });
                }
                let took_us = obj.get("took_us").and_then(|n| n.as_num()).unwrap_or(0.0) as u64;
                // The fact is additive: a line from a daemon that does not
                // say is a line that promises the least.
                let complete = match obj.get("complete") {
                    Some(Json::Bool(b)) => *b,
                    _ => false,
                };
                // The session is additive too: a line without it is a line
                // from a daemon that does not name the session, and the
                // reader keeps the session it had.
                //
                // And a session that is not one is no session at all: the
                // rule is the same on both sides of the wire, and a peer
                // that answers `-1` does not get to name the session `0`
                // — which is in the client's own space, the space the
                // daemon trusts.
                let session = obj
                    .get("session")
                    .and_then(|n| n.as_num())
                    .and_then(session_id);
                Response::Hits {
                    hits,
                    took_us,
                    complete,
                    session,
                }
            }
            "done" => {
                // The count of this frame is the size of the answer: the
                // client counts what it got, which is the same number, and
                // the terminator says so about the stream it ends.
                //
                // The count is the one field of the frame that is not
                // optional, and the reason is the reader's own: it has no
                // way to say "not known" in a `usize`, and a zero here
                // would be a claim that the answer carried nothing — a
                // claim the peer never made.
                //
                // And a count that cannot be true is not a count the
                // reader can report: a negative one, or one the wire
                // cannot carry exactly, is refused the same way.
                let count = match obj.get("count").and_then(|n| n.as_num()) {
                    Some(n) => count_of(n).ok_or(LineError::NotAnAnswer)?,
                    None => return Err(LineError::NotAnAnswer),
                };
                Response::Done { count }
            }
            "status" => {
                // The count is the size of the daemon's index, and the
                // launcher draws it: a header that says "0" about an index
                // of a thousand entries is a wrong fact, and a wrong fact
                // is worse than no fact.
                //
                // The count is one of the four fields of the status frame
                // and the daemon writes it on every status, so a line
                // without it is a line the reader cannot use — and the
                // reader cannot say "not known" in a `usize`: the
                // launcher's own slot for "not known" is on the far side
                // of the count, and a zero here would be a claim about an
                // index that plainly has entries.
                //
                // A count that cannot be true is not a count the reader
                // can report either: a negative one, or one the wire
                // cannot carry exactly, is refused the same way.
                let docs = match obj.get("docs").and_then(|n| n.as_num()) {
                    Some(n) => count_of(n).ok_or(LineError::NotAnAnswer)?,
                    None => return Err(LineError::NotAnAnswer),
                };
                let event_id = match obj.get("event_id").and_then(|n| n.as_num()) {
                    // The position is a report, and a negative one is not a
                    // position at all: the journal has none before its
                    // first entry. The cast would have made it the start
                    // of the journal, a position the peer never claimed —
                    // so the frame is refused, the way a count that cannot
                    // be true refuses it.
                    Some(n) if n < 0.0 => return Err(LineError::NotAnAnswer),
                    // A position above the wire's limit stays: the field
                    // travels as an `f64`, so an integer above `2^53` is a
                    // number the reader reads back as a different one, and
                    // that is written down in the variant's own
                    // documentation. A report is allowed to be off.
                    Some(n) => n as u64,
                    // A missing position is additive: the field is
                    // optional, and the conservative reading of "no
                    // position" is `0` — which is exactly what a daemon
                    // that relisted the tree reports. That is the same
                    // reading the `catch_up` field's own default carries.
                    None => 0,
                };
                let catch_up = match obj.get("catch_up").and_then(|s| s.as_str()) {
                    Some("replayed") => CatchUp::Replayed,
                    _ => CatchUp::Relisted,
                };
                // A daemon that does not say is a daemon whose feed nobody
                // knows: the conservative answer is the one that says
                // there is no feed.
                let feed = match obj.get("feed").and_then(|s| s.as_str()) {
                    Some("live") => Feed::Live,
                    Some("gone") => Feed::Gone,
                    _ => Feed::None,
                };
                // A status line without the field is a status line from a
                // daemon that has no pack store — which is the answer that
                // does not promise anything.
                let tv = match obj.get("tv") {
                    Some(Json::Bool(b)) => *b,
                    _ => false,
                };
                Response::Status {
                    docs,
                    event_id,
                    catch_up,
                    tv,
                    feed,
                }
            }
            "grep-files" => {
                let mut files = Vec::new();
                for it in obj
                    .get("files")
                    .and_then(|f| f.as_array())
                    .ok_or(LineError::NotAnAnswer)?
                {
                    // The same rule as the hits: a file the protocol
                    // cannot read is not a file that is quietly left
                    // out.
                    let o = it.as_object().ok_or(LineError::NotAnAnswer)?;
                    let path = o
                        .get("path")
                        .and_then(|p| p.as_str())
                        .ok_or(LineError::NotAnAnswer)?
                        .to_string();
                    let lines = match o.get("lines").and_then(|l| l.as_array()) {
                        Some(ls) => {
                            let mut lines = Vec::with_capacity(ls.len());
                            for l in ls {
                                let pair = l.as_array().ok_or(LineError::NotAnAnswer)?;
                                // The line number is the line the text is
                                // on, and the line numbers are 1-based: the
                                // first line of a file is line 1, the way
                                // `rg` prints them. A number that is not a
                                // line is not a line — the reader used to
                                // cast it, and the cast made `-1` the
                                // line `0`, which no file has.
                                //
                                // The pair is one fact about one line: a
                                // number without its text and a text
                                // without its number are not a match. The
                                // reader already refuses the frame for the
                                // half it cannot read, and it refuses it
                                // here too — a pair left out would make
                                // the file's matches fewer than they were,
                                // which is the lie the hits arm refuses to
                                // tell.
                                let n = pair
                                    .first()
                                    .and_then(|n| n.as_num())
                                    .and_then(count_of)
                                    .filter(|line| *line >= 1)
                                    .ok_or(LineError::NotAnAnswer)?;
                                let t = pair
                                    .get(1)
                                    .and_then(|t| t.as_str())
                                    .ok_or(LineError::NotAnAnswer)?
                                    .to_string();
                                lines.push((n, t));
                            }
                            lines
                        }
                        None => Vec::new(),
                    };
                    files.push(GrepFile { path, lines });
                }
                let took_us = obj.get("took_us").and_then(|n| n.as_num()).unwrap_or(0.0) as u64;
                // The two facts are additive: a line from a daemon that
                // does not say is a line that promises the least.
                let complete = match obj.get("complete") {
                    Some(Json::Bool(b)) => *b,
                    _ => false,
                };
                // The count the wire carries, read as a count: a count that
                // cannot be true is not a count the reader can report, and
                // the sentence the count makes ("the daemon did not read N
                // file(s)") would name a number that cannot be true.
                //
                // A frame that does not say the count is a frame from a
                // daemon that does not say: the count is zero, the
                // conservative reading, and `complete` carries the rest.
                let wire_count = match obj.get("skipped").and_then(|n| n.as_num()) {
                    Some(n) => count_of(n).ok_or(LineError::NotAnAnswer)?,
                    None => 0,
                };
                // The reasons are additive: a line without them is a line
                // from a daemon that does not say why, and the count it
                // does carry still reads.
                let mut skips = Vec::new();
                // What the reasons make of the count: the count and the
                // reasons are one fact written twice, and the protocol says
                // the reasons add up to the count.
                let mut counted = 0usize;
                if let Some(items) = obj.get("skips").and_then(|s| s.as_array()) {
                    for it in items {
                        // A group that is not an object is not a group, and
                        // the reader does not drop it: the reasons add up to
                        // the count, so a group left out of the sum would
                        // make the count smaller than the truth -- and the
                        // count the reader reports would be the reasons'
                        // sum, not the wire's number. The `files` array
                        // refuses an element that is not an object for the
                        // same reason, and this refuses it the same way.
                        let o = match it.as_object() {
                            Some(o) => o,
                            None => return Err(LineError::NotAnAnswer),
                        };
                        // The reason is the whole point of the group: a
                        // group with no reason says nothing about why the
                        // files were not read, and a reader that counted it
                        // would be counting a cause that is not there.
                        let reason = o
                            .get("reason")
                            .and_then(|r| r.as_str())
                            .ok_or(LineError::NotAnAnswer)?
                            .to_string();
                        // The count is the group's number, and a group
                        // without one is a group the reader cannot count:
                        // the sentence it makes would name "0 file(s)" for
                        // files that were not read, and a group left out of
                        // the sum would make the count smaller than the
                        // truth.
                        let count = o
                            .get("count")
                            .and_then(|n| n.as_num())
                            .and_then(count_of)
                            .ok_or(LineError::NotAnAnswer)?;
                        counted = counted.saturating_add(count);
                        // The path is there when the search knows which
                        // file it is, and absent when it does not.
                        let path = o
                            .get("path")
                            .and_then(|p| p.as_str())
                            .map(|p| p.to_string());
                        skips.push(Skip {
                            reason,
                            count,
                            path,
                        });
                    }
                }
                // The count the reader reports is the count the reasons
                // make: the two writings of the one fact are read as one,
                // so the answer's own numbers agree by construction, and
                // the sentences they make are the same number twice. The
                // wire's count is the count only when there are no reasons
                // to read it from.
                let skipped = if skips.is_empty() {
                    wire_count
                } else {
                    counted
                };
                Response::GrepFiles {
                    files,
                    took_us,
                    complete,
                    skipped,
                    skips,
                }
            }
            "resolved" => {
                let path = obj
                    .get("path")
                    .and_then(|p| p.as_str())
                    .ok_or(LineError::NotAnAnswer)?
                    .to_string();
                let folder = match obj.get("folder") {
                    Some(Json::Bool(b)) => *b,
                    _ => false,
                };
                // The backend is the `folder_backend` index's own id, and
                // `0` is the volume the index does not name: the daemon
                // writes `0` for a file it cannot place, and a negative id
                // lands on the same value, which is the safe reading of a
                // number that is not one.
                //
                // The reader cannot tell which ids the daemon's index has:
                // the ids are the daemon's own, and the index is not on the
                // wire. And nothing in the tree acts on the id — the
                // client keeps it and no caller reads it — so the field is
                // a report, and a report may be wrong without misleading
                // anyone. It is read as it arrives, and a reader of the
                // code will know that it was a decision.
                let backend = obj.get("backend").and_then(|n| n.as_num()).unwrap_or(0.0) as u32;
                Response::Resolved {
                    path,
                    folder,
                    backend,
                }
            }
            "error" => Response::Error {
                message: obj
                    .get("message")
                    .and_then(|m| m.as_str())
                    .ok_or(LineError::NotAnAnswer)?
                    .to_string(),
            },
            "warning" => Response::Warning {
                message: obj
                    .get("message")
                    .and_then(|m| m.as_str())
                    .ok_or(LineError::NotAnAnswer)?
                    .to_string(),
            },
            _ => return Err(LineError::NotAnAnswer),
        })
    }

    /// Parse one JSON line. Returns `None` on anything malformed.
    pub fn from_json(line: &str) -> Option<Response> {
        Self::from_json_checked(line).ok()
    }
}

/// Escape the *contents* of a JSON string — the text that goes between the
/// quotation marks.
///
/// RFC 8259 §7: the quotation mark, the reverse solidus, and every C0
/// control (U+0000 through U+001F) MUST be escaped. Every other character
/// may go out raw, because the line is UTF-8 (RFC 8259 §8.1). The five
/// short forms — `\b`, `\f`, `\n`, `\r`, `\t` — are used where they
/// exist, and the rest of the C0 range becomes `\u00XX`. The solidus and
/// DEL are left alone: both are legal raw, and escaping them would change
/// bytes for no gain.
///
/// This is the one escaper in this repository's crates. `searchd` and
/// `searchctl` call it, so a path, a query or a grep line reaches the
/// wire as a single line of JSON whatever bytes it carries, and the line
/// framing cannot be broken by the content. `crates/rgx-core` has its own
/// (`djson::escape_json_string`); that one is upstream's, and this one
/// does not fork it — the two agree on the same input because both follow
/// RFC 8259 §7, down to the uppercase hex digits.
pub fn escape_json_string(s: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let bytes = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    // Byte-indexed: the loop only ever hands the writer an untouched slice
    // of the input or an ASCII escape literal, so multi-byte UTF-8 is
    // carried through verbatim and never round-tripped character by
    // character. Slicing is safe because the slice boundaries are always
    // the positions of ASCII bytes.
    let mut start = 0usize;
    for (i, &b) in bytes.iter().enumerate() {
        if b < 0x20 && !matches!(b, 0x08 | 0x0C | b'\n' | b'\r' | b'\t') {
            // A C0 control with no short form.
            out.push_str(&s[start..i]);
            out.push_str("\\u00");
            out.push(char::from(HEX[(b >> 4) as usize]));
            out.push(char::from(HEX[(b & 0x0F) as usize]));
            start = i + 1;
            continue;
        }
        let esc: &str = match b {
            b'"' => "\\\"",
            b'\\' => "\\\\",
            0x08 => "\\b",
            0x0C => "\\f",
            b'\n' => "\\n",
            b'\r' => "\\r",
            b'\t' => "\\t",
            _ => continue,
        };
        out.push_str(&s[start..i]);
        out.push_str(esc);
        start = i + 1;
    }
    out.push_str(&s[start..]);
    out
}

// --- a deliberately small JSON value reader (objects/strings/numbers) -----

#[derive(Debug, Clone, PartialEq)]
pub enum Json {
    Str(String),
    Num(f64),
    Bool(bool),
    Null,
    Arr(Vec<Json>),
    Obj(Vec<(String, Json)>),
}

impl Json {
    /// The string this value is, when it is one.
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Json::Str(s) => Some(s),
            _ => None,
        }
    }

    /// The number this value is, when it is one.
    pub fn as_num(&self) -> Option<f64> {
        match self {
            Json::Num(n) => Some(*n),
            _ => None,
        }
    }

    /// The truth this value is, when it is one.
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Json::Bool(b) => Some(*b),
            _ => None,
        }
    }

    /// The object this value is, when it is one.
    pub fn as_object(&self) -> Option<&Vec<(String, Json)>> {
        match self {
            Json::Obj(o) => Some(o),
            _ => None,
        }
    }

    /// The array this value is, when it is one.
    pub fn as_array(&self) -> Option<&Vec<Json>> {
        match self {
            Json::Arr(a) => Some(a),
            _ => None,
        }
    }

    /// The field of an object, by name.
    ///
    /// `None` when this value is not an object, and `None` when the
    /// object does not have the field — a field that is missing is a
    /// field that is optional, and the caller decides what its absence
    /// means.
    pub fn get(&self, key: &str) -> Option<&Json> {
        match self {
            Json::Obj(fields) => fields.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }
}

/// Read one JSON line into a value.
///
/// The protocol's own reader is enough for the protocol, and this is the
/// same reader — the rgx front answers in rg's JSON shape, and a client
/// that had to bring a second JSON parser for it would carry two
/// parsers for one wire.
pub fn read_json(line: &str) -> Option<Json> {
    parse_json(line).ok()
}

trait ObjGet {
    fn get(&self, k: &str) -> Option<&Json>;
}
impl ObjGet for Vec<(String, Json)> {
    fn get(&self, k: &str) -> Option<&Json> {
        self.iter().find(|(key, _)| key == k).map(|(_, v)| v)
    }
}

fn parse_json(s: &str) -> Result<Json, LineError> {
    let b: Vec<char> = s.chars().collect();
    let (v, i) = value(&b, 0, 0)?;
    // Trailing whitespace only.
    if b[i..].iter().any(|c| !c.is_whitespace()) {
        return Err(LineError::NotJson);
    }
    Ok(v)
}

fn value(b: &[char], i: usize, depth: usize) -> Result<(Json, usize), LineError> {
    // The depth is the nesting the reader is already inside: a document
    // that asks for more than the reader allows is refused, and refusing
    // it is the whole point — the alternative is the stack.
    if depth > MAX_DEPTH {
        return Err(LineError::TooDeep);
    }
    let mut i = skip_ws(b, i);
    match b.get(i).ok_or(LineError::NotJson)? {
        '"' => {
            let (s, j) = string(b, i)?;
            Ok((Json::Str(s), j))
        }
        '{' => {
            i += 1;
            let mut out = Vec::new();
            i = skip_ws(b, i);
            if b.get(i) == Some(&'}') {
                return Ok((Json::Obj(out), i + 1));
            }
            loop {
                i = skip_ws(b, i);
                let (k, j) = string(b, i)?;
                i = skip_ws(b, j);
                if b.get(i) != Some(&':') {
                    return Err(LineError::NotJson);
                }
                let (v, j) = value(b, i + 1, depth + 1)?;
                out.push((k, v));
                i = skip_ws(b, j);
                match b.get(i).ok_or(LineError::NotJson)? {
                    ',' => i += 1,
                    '}' => return Ok((Json::Obj(out), i + 1)),
                    _ => return Err(LineError::NotJson),
                }
            }
        }
        '[' => {
            i += 1;
            let mut out = Vec::new();
            i = skip_ws(b, i);
            if b.get(i) == Some(&']') {
                return Ok((Json::Arr(out), i + 1));
            }
            loop {
                let (v, j) = value(b, i, depth + 1)?;
                out.push(v);
                i = skip_ws(b, j);
                match b.get(i).ok_or(LineError::NotJson)? {
                    ',' => i += 1,
                    ']' => return Ok((Json::Arr(out), i + 1)),
                    _ => return Err(LineError::NotJson),
                }
            }
        }
        't' if b[i..].starts_with(&['t', 'r', 'u', 'e']) => Ok((Json::Bool(true), i + 4)),
        'f' if b[i..].starts_with(&['f', 'a', 'l', 's', 'e']) => Ok((Json::Bool(false), i + 5)),
        'n' if b[i..].starts_with(&['n', 'u', 'l', 'l']) => Ok((Json::Null, i + 4)),
        _ => num(b, i),
    }
}

fn string(b: &[char], i: usize) -> Result<(String, usize), LineError> {
    if b.get(i) != Some(&'"') {
        return Err(LineError::NotJson);
    }
    let mut s = String::new();
    let mut i = i + 1;
    while let Some(&c) = b.get(i) {
        match c {
            '"' => return Ok((s, i + 1)),
            '\\' => {
                let e = *b.get(i + 1).ok_or(LineError::NotJson)?;
                i += 2;
                match e {
                    '"' => s.push('"'),
                    '\\' => s.push('\\'),
                    '/' => s.push('/'),
                    'b' => s.push('\u{0008}'),
                    'f' => s.push('\u{000C}'),
                    'n' => s.push('\n'),
                    'r' => s.push('\r'),
                    't' => s.push('\t'),
                    'u' => {
                        let (cp, j) = unicode_escape(b, i).ok_or(LineError::NotJson)?;
                        s.push(char::from_u32(cp).ok_or(LineError::NotJson)?);
                        i = j;
                    }
                    // An escape RFC 8259 does not define is not JSON. The
                    // reader does not guess what was meant.
                    _ => return Err(LineError::NotJson),
                }
            }
            // A control character that arrived raw is not JSON either
            // (RFC 8259 §7): the writer never emits one, and a line that
            // carries one did not come from a JSON writer.
            c if (c as u32) < 0x20 => return Err(LineError::NotJson),
            other => {
                s.push(other);
                i += 1;
            }
        }
    }
    Err(LineError::NotJson)
}

/// One `\uXXXX` escape, and the surrogate pair that may follow it.
///
/// Returns the code point and the index just past the escape.
fn unicode_escape(b: &[char], i: usize) -> Option<(u32, usize)> {
    let hi = hex4(b, i)?;
    let j = i + 4;
    if (0xD800..=0xDBFF).contains(&hi) {
        // A high surrogate is half a character, and the other half has to be
        // there: `\uD83D\uDE00` is one character, U+1F600. A half that
        // arrives alone is not a character at all, and RFC 8259 §8.2 says
        // what an implementation does with it is unpredictable — so it is
        // refused rather than turned into a replacement character.
        if b.get(j) != Some(&'\\') || b.get(j + 1) != Some(&'u') {
            return None;
        }
        let lo = hex4(b, j + 2)?;
        if !(0xDC00..=0xDFFF).contains(&lo) {
            return None;
        }
        let cp = 0x10000 + ((hi - 0xD800) << 10) + (lo - 0xDC00);
        Some((cp, j + 6))
    } else if (0xDC00..=0xDFFF).contains(&hi) {
        // A low surrogate with no high surrogate before it.
        None
    } else {
        Some((hi, j))
    }
}

/// The four hexadecimal digits of a `\uXXXX` escape.
fn hex4(b: &[char], i: usize) -> Option<u32> {
    let mut v = 0u32;
    for k in 0..4 {
        v = v * 16 + b.get(i + k)?.to_digit(16)?;
    }
    Some(v)
}

fn num(b: &[char], i: usize) -> Result<(Json, usize), LineError> {
    let start = i;
    let mut i = i;
    while matches!(b.get(i), Some(c) if c.is_ascii_digit() || matches!(c, '-' | '+' | '.' | 'e' | 'E'))
    {
        i += 1;
    }
    if i == start {
        return Err(LineError::NotJson);
    }
    let s: String = b[start..i].iter().collect();
    Ok((Json::Num(num_value(&s).ok_or(LineError::NotJson)?), i))
}

/// The number in the text, as the protocol's `f64`.
///
/// `f64::from_str` is `std`-only, so this is the crate's own parser. It
/// is not a float library: it reads the JSON number grammar of RFC 8259
/// §6 -- a sign, an integer part, a fraction, an exponent -- and nothing
/// else. `inf`, `NaN` and a leading `+` are not JSON, and are refused.
///
/// An integer is accumulated digit by digit, so every integer a JSON line
/// can carry (up to 2^53) survives the trip exactly; a number with a
/// fraction or an exponent is scaled by multiplication, because `powi` and
/// `powf` are `std` as well.
fn num_value(s: &str) -> Option<f64> {
    let b = s.as_bytes();
    let mut i = 0usize;
    let neg = match b.first() {
        Some(b'-') => {
            i = 1;
            true
        }
        _ => false,
    };
    let mut mant = 0.0f64;
    let mut digits = false;
    while let Some(&d) = b.get(i) {
        if !d.is_ascii_digit() {
            break;
        }
        mant = mant * 10.0 + f64::from(d - b'0');
        i += 1;
        digits = true;
    }
    let mut exp = 0i32;
    if b.get(i) == Some(&b'.') {
        i += 1;
        while let Some(&d) = b.get(i) {
            if !d.is_ascii_digit() {
                break;
            }
            mant = mant * 10.0 + f64::from(d - b'0');
            exp -= 1;
            i += 1;
            digits = true;
        }
    }
    if !digits {
        return None;
    }
    if matches!(b.get(i), Some(b'e') | Some(b'E')) {
        i += 1;
        let esign: i32 = match b.get(i) {
            Some(b'+') => {
                i += 1;
                1
            }
            Some(b'-') => {
                i += 1;
                -1
            }
            _ => 1,
        };
        let mut e = 0i32;
        let mut edigits = false;
        while let Some(&d) = b.get(i) {
            if !d.is_ascii_digit() {
                break;
            }
            e = e.saturating_mul(10).saturating_add(i32::from(d - b'0'));
            i += 1;
            edigits = true;
        }
        if !edigits {
            return None;
        }
        exp = exp.saturating_add(esign.saturating_mul(e));
    }
    if i != b.len() {
        return None;
    }
    // The scaling is a loop, so the exponent is bounded: a number that
    // asks for 10^400 is already past the end of `f64`, and the loop that
    // would take it there is not worth running.
    let mut v = mant;
    let mut e = exp;
    while e > 0 {
        if e > 400 {
            return Some(if neg {
                f64::NEG_INFINITY
            } else {
                f64::INFINITY
            });
        }
        v *= 10.0;
        e -= 1;
    }
    while e < 0 {
        if e < -400 {
            return Some(if neg { -0.0 } else { 0.0 });
        }
        v /= 10.0;
        e += 1;
    }
    Some(if neg { -v } else { v })
}

fn skip_ws(b: &[char], mut i: usize) -> usize {
    while matches!(b.get(i), Some(c) if c.is_whitespace()) {
        i += 1;
    }
    i
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Golden wire format: the exact bytes each variant emits. If one of these
    /// fails, the on-the-wire JSON changed -- update it deliberately (and any
    /// client), never by regenerating the expectation.
    #[test]
    fn wire_format_is_pinned() {
        assert_eq!(
            Request::Search {
                q: "a".into(),
                limit: 1,
                session: None
            }
            .to_json(),
            r#"{"op":"search","q":"a","limit":1}"#
        );
        // The session id is additive: the request without it is the request
        // as it was before the field existed.
        assert_eq!(
            Request::Search {
                q: "a".into(),
                limit: 1,
                session: Some(7)
            }
            .to_json(),
            r#"{"op":"search","q":"a","limit":1,"session":7}"#
        );
        assert_eq!(
            Request::Resolve {
                path: "/a/b".into()
            }
            .to_json(),
            r#"{"op":"resolve","path":"/a/b"}"#
        );
        assert_eq!(
            Request::Grep {
                q: "g".into(),
                limit: 2
            }
            .to_json(),
            r#"{"op":"grep","q":"g","limit":2}"#
        );
        assert_eq!(Request::Status.to_json(), r#"{"op":"status"}"#);
        assert_eq!(
            Response::Hits {
                hits: vec![Hit {
                    path: "p".into(),
                    score: 5,
                    span: Vec::new(),
                    folder: false,
                }],
                took_us: 6,
                complete: true,
                session: None,
            }
            .to_json(),
            r#"{"op":"hits","hits":[{"path":"p","score":5}],"took_us":6,"complete":true}"#
        );
        // The session is on the answer when there is one: the client sends
        // it back, and a client that cannot name the session cannot ask
        // for the re-rank.
        assert_eq!(
            Response::Hits {
                hits: vec![Hit::new("p".into(), 5)],
                took_us: 6,
                complete: true,
                session: Some(7),
            }
            .to_json(),
            r#"{"op":"hits","hits":[{"path":"p","score":5}],"took_us":6,"complete":true,"session":7}"#
        );
        assert_eq!(
            Response::Done { count: 7 }.to_json(),
            r#"{"op":"done","count":7}"#
        );
        assert_eq!(
            Response::Status {
                docs: 3,
                event_id: 9,
                catch_up: CatchUp::Relisted,
                tv: false,
                feed: Feed::Live,
            }
            .to_json(),
            r#"{"op":"status","docs":3,"event_id":9,"catch_up":"relisted","tv":false,"feed":"live"}"#
        );
        // The other value, spelled the way the rules spells it.
        assert_eq!(
            Response::Status {
                docs: 3,
                event_id: 9,
                catch_up: CatchUp::Replayed,
                tv: false,
                feed: Feed::None,
            }
            .to_json(),
            r#"{"op":"status","docs":3,"event_id":9,"catch_up":"replayed","tv":false,"feed":"none"}"#
        );
        assert_eq!(
            Response::GrepFiles {
                files: vec![GrepFile {
                    path: "f".into(),
                    lines: vec![(1, "t".into())]
                }],
                took_us: 8,
                complete: true,
                skipped: 0,
                skips: Vec::new(),
            }
            .to_json(),
            r#"{"op":"grep-files","files":[{"path":"f","lines":[[1,"t"]]}],"took_us":8,"complete":true,"skipped":0,"skips":[]}"#
        );
        // The reasons are on the answer: the count says how many files were
        // not read, and the reason says why.
        assert_eq!(
            Response::GrepFiles {
                files: Vec::new(),
                took_us: 8,
                complete: false,
                skipped: 3,
                skips: vec![Skip {
                    reason: "a cloud placeholder".into(),
                    count: 2,
                    path: Some("C:/a.txt".into()),
                }],
            }
            .to_json(),
            r#"{"op":"grep-files","files":[],"took_us":8,"complete":false,"skipped":3,"skips":[{"reason":"a cloud placeholder","count":2,"path":"C:/a.txt"}]}"#
        );
        assert_eq!(
            Response::Error {
                message: "e".into()
            }
            .to_json(),
            r#"{"op":"error","message":"e"}"#
        );
        assert_eq!(
            Response::Warning {
                message: "w".into()
            }
            .to_json(),
            r#"{"op":"warning","message":"w"}"#
        );
    }

    /// The catch-up value is a string with exactly two spellings, and they
    /// are the ones the rules uses: a journal-backed platform
    /// "replayed", a journal-less one "relisted".
    #[test]
    fn the_catch_up_spellings_are_the_two_the_design_doc_uses() {
        assert_eq!(CatchUp::Replayed.as_str(), "replayed");
        assert_eq!(CatchUp::Relisted.as_str(), "relisted");
    }

    #[test]
    fn request_round_trips() {
        let r = Request::Search {
            q: "report type:pdf".into(),
            limit: 25,
            session: None,
        };
        let line = r.to_json();
        assert_eq!(Request::from_json(&line), Some(r));
        assert_eq!(
            Request::from_json(r#"{"op":"status"}"#),
            Some(Request::Status)
        );
        assert!(Request::from_json("{not json").is_none());
        assert!(Request::from_json(r#"{"op":"unknown"}"#).is_none());
    }

    #[test]
    fn response_lines_encode() {
        let r = Response::Hits {
            hits: vec![Hit {
                path: "C:\\a\"b.txt".into(),
                score: 12,
                span: Vec::new(),
                folder: false,
            }],
            took_us: 40,
            complete: false,
            session: None,
        };
        let line = r.to_json();
        // C:\a"b.txt → escaped backslash, escaped quote.
        assert!(line.contains("a\\\"b.txt"), "quote escaped: {line}");
        assert!(line.contains("C:\\\\a"), "backslash escaped: {line}");
        assert!(line.starts_with(r#"{"op":"hits""#));
        assert_eq!(
            Response::Error {
                message: "boom".into()
            }
            .to_json(),
            r#"{"op":"error","message":"boom"}"#
        );
    }

    #[test]
    fn nested_arrays_parse() {
        let v = parse_json(r#"{"op":"search","q":"x","limit":5}"#).unwrap();
        let o = v.as_object().unwrap();
        assert_eq!(o.get("q").unwrap().as_str(), Some("x"));
    }

    /// RFC 8259 §7: the quotation mark, the reverse solidus, and every C0
    /// control (U+0000 through U+001F) MUST be escaped. Everything else may
    /// go out raw, because the line is UTF-8 (RFC 8259 §8.1).
    #[test]
    fn every_character_that_must_be_escaped_is() {
        assert_eq!(escape_json_string("a\"b"), r#"a\"b"#);
        assert_eq!(escape_json_string("a\\b"), r#"a\\b"#);
        assert_eq!(escape_json_string("a\nb"), r#"a\nb"#);
        assert_eq!(escape_json_string("a\rb"), r#"a\rb"#);
        assert_eq!(escape_json_string("a\tb"), r#"a\tb"#);
        assert_eq!(escape_json_string("a\u{0008}b"), r#"a\bb"#);
        assert_eq!(escape_json_string("a\u{000C}b"), r#"a\fb"#);
        assert_eq!(escape_json_string("a\u{0000}b"), r#"a\u0000b"#);
        assert_eq!(escape_json_string("a\u{001F}b"), r#"a\u001Fb"#);
        // Legal raw, and left raw: the solidus, DEL, and non-ASCII.
        assert_eq!(escape_json_string("a/b"), "a/b");
        assert_eq!(escape_json_string("a\u{007F}b"), "a\u{007F}b");
        assert_eq!(escape_json_string("a\u{00e9}b"), "a\u{00e9}b");
    }

    /// The line a response makes is one line. A path with a tab in it, or a
    /// grep line out of a TSV file, must not be able to break the framing.
    #[test]
    fn a_response_with_awkward_text_stays_one_line() {
        let line = Response::GrepFiles {
            files: vec![GrepFile {
                path: "C:\\dir\there".into(),
                lines: vec![(1, "a\tb\nc\u{0001}d".into())],
            }],
            took_us: 3,
            complete: true,
            skipped: 0,
            skips: Vec::new(),
        }
        .to_json();
        assert!(!line.contains('\n'), "the line carries a newline: {line:?}");
        assert!(!line.contains('\t'), "the line carries a tab: {line:?}");
        assert_eq!(
            line,
            r#"{"op":"grep-files","files":[{"path":"C:\\dir\there","lines":[[1,"a\tb\nc\u0001d"]]}],"took_us":3,"complete":true,"skipped":0,"skips":[]}"#
        );
    }

    /// The reader and the writer agree: what the writer escapes, the reader
    /// reads back, and a string with every awkward byte in it survives the
    /// trip unchanged — and the line it travels in is valid JSON.
    #[test]
    fn strings_survive_the_round_trip() {
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
            "combining e\u{0301}",
        ];
        for c in cases {
            let req = Request::Search {
                q: c.into(),
                limit: 7,
                session: None,
            };
            let line = req.to_json();
            assert!(
                !line.bytes().any(|b| b < 0x20),
                "the line carries a raw control character: {c:?} -> {line:?}"
            );
            assert_eq!(
                Request::from_json(&line),
                Some(req),
                "round trip: {c:?} -> {line}"
            );
        }
    }

    /// The reader knows every escape RFC 8259 defines, and refuses the ones
    /// it does not.
    #[test]
    fn the_reader_understands_every_escape() {
        let q = |s: &str| Request::Search {
            q: s.into(),
            limit: 1,
            session: None,
        };
        assert_eq!(
            Request::from_json(r#"{"op":"search","q":"A\u00e9","limit":1}"#),
            Some(q("A\u{00e9}"))
        );
        assert_eq!(
            Request::from_json(r#"{"op":"search","q":"\uD83D\uDE00","limit":1}"#),
            Some(q("\u{1f600}"))
        );
        assert_eq!(
            Request::from_json(r#"{"op":"search","q":"a\/b","limit":1}"#),
            Some(q("a/b"))
        );
        assert_eq!(
            Request::from_json(r#"{"op":"search","q":"a\u0000b","limit":1}"#),
            Some(q("a\u{0000}b"))
        );
        // Not JSON: an unknown escape, a short \u, a lone surrogate, and a
        // control character that arrived unescaped.
        assert!(Request::from_json(r#"{"op":"search","q":"a\xb","limit":1}"#).is_none());
        assert!(Request::from_json(r#"{"op":"search","q":"a\u00","limit":1}"#).is_none());
        assert!(Request::from_json(r#"{"op":"search","q":"\uD800","limit":1}"#).is_none());
        assert!(Request::from_json("{\"op\":\"search\",\"q\":\"a\tb\",\"limit\":1}").is_none());
    }

    #[test]
    fn a_hit_carries_the_span_it_matched() {
        let h = Hit {
            path: "C:/notes/relatorio.txt".into(),
            score: 9,
            span: vec![8, 9, 10],
            folder: false,
        };
        let line = Response::Hits {
            hits: vec![h.clone()],
            took_us: 3,
            complete: true,
            session: None,
        }
        .to_json();
        assert!(
            line.contains(r#""span":[8,9,10]"#),
            "span on the wire: {line}"
        );
        // And it reads back the same.
        let back = Response::from_json(&line).expect("the line reads back");
        assert_eq!(
            back,
            Response::Hits {
                hits: vec![h],
                took_us: 3,
                complete: true,
                session: None,
            }
        );
    }

    #[test]
    fn a_hit_without_a_span_still_reads() {
        // The span is additive: an older daemon's line has no span, and a
        // client that meets one must not fall over on it. The same line has
        // no `complete` either, and it reads as "not whole" — the answer
        // that promises the least.
        let back =
            Response::from_json(r#"{"op":"hits","hits":[{"path":"p","score":5}],"took_us":6}"#)
                .expect("the line reads back");
        assert_eq!(
            back,
            Response::Hits {
                hits: vec![Hit::new("p".into(), 5)],
                took_us: 6,
                complete: false,
                session: None,
            }
        );
    }

    /// The rule that says whether a name answer is whole.
    ///
    /// A list shorter than the limit is the whole answer, and a list that
    /// reached the limit may have been cut short: the rule is the number
    /// of hits, and it costs one comparison.
    #[test]
    fn the_name_answer_rule_is_the_number_of_hits() {
        assert!(name_answer_is_complete(0, 1), "an empty answer is whole");
        assert!(
            name_answer_is_complete(9, 10),
            "a list under the limit is whole"
        );
        assert!(
            !name_answer_is_complete(10, 10),
            "a list at the limit may have been cut short"
        );
        assert!(
            !name_answer_is_complete(11, 10),
            "a list over the limit was cut short"
        );
        assert!(
            !name_answer_is_complete(0, 0),
            "a limit of zero cuts every answer"
        );
    }

    /// The name answer says whether it is whole.
    ///
    /// A list of hits looks like the whole answer, and a list that reached
    /// the limit is the same list as one that did not: the answer has to
    /// say which it is, or the reader cannot tell a whole answer from a
    /// page of a longer one.
    #[test]
    fn a_hits_line_says_whether_the_answer_is_whole() {
        let whole = Response::Hits {
            hits: vec![Hit::new("p".into(), 1)],
            took_us: 2,
            complete: true,
            session: None,
        };
        let cut = Response::Hits {
            hits: vec![Hit::new("p".into(), 1)],
            took_us: 2,
            complete: false,
            session: None,
        };
        assert!(
            whole.to_json().contains(r#""complete":true"#),
            "a whole answer says so: {}",
            whole.to_json()
        );
        assert!(
            cut.to_json().contains(r#""complete":false"#),
            "a cut answer says so: {}",
            cut.to_json()
        );
        // And the fact survives the round trip in both directions.
        for answer in [whole, cut] {
            let back = Response::from_json(&answer.to_json()).expect("the line reads back");
            assert_eq!(back, answer);
        }
    }

    /// A folder says it is a folder, and a file does not have to say it.
    ///
    /// The flag is additive: a hit without the field is a file, and an
    /// older daemon's line reads without falling over.
    #[test]
    fn a_folder_says_it_is_a_folder() {
        let folder = Hit::new("C:/docs".into(), 7).with_folder(true);
        let line = Response::Hits {
            hits: vec![folder.clone()],
            took_us: 1,
            complete: true,
            session: None,
        }
        .to_json();
        assert!(
            line.contains(r#""folder":true"#),
            "the flag on the wire: {line}"
        );
        let back = Response::from_json(&line).expect("the line reads back");
        assert_eq!(
            back,
            Response::Hits {
                hits: vec![folder],
                took_us: 1,
                complete: true,
                session: None,
            }
        );

        // A file does not carry the field at all: the common hit is the
        // short one.
        let file = Hit::new("C:/a/notes.txt".into(), 2);
        let line = Response::Hits {
            hits: vec![file.clone()],
            took_us: 1,
            complete: true,
            session: None,
        }
        .to_json();
        assert!(!line.contains("folder"), "a file says nothing: {line}");
        let back = Response::from_json(&line).expect("the line reads back");
        assert_eq!(
            back,
            Response::Hits {
                hits: vec![file],
                took_us: 1,
                complete: true,
                session: None,
            }
        );
    }

    #[test]
    fn every_response_reads_back_from_its_own_writing() {
        // The round trip is the contract: a client can only exist if the
        // daemon's own words are readable.
        let responses = vec![
            Response::Hits {
                hits: vec![Hit::new("p".into(), 1)],
                took_us: 2,
                complete: true,
                session: Some(11),
            },
            Response::Done { count: 3 },
            Response::Status {
                docs: 4,
                event_id: 5,
                catch_up: CatchUp::Replayed,
                tv: true,
                feed: Feed::Live,
            },
            Response::GrepFiles {
                files: vec![GrepFile {
                    path: "f".into(),
                    lines: vec![(7, "line".into())],
                }],
                took_us: 8,
                complete: true,
                skipped: 2,
                skips: vec![Skip {
                    reason: "a cloud placeholder".into(),
                    count: 2,
                    path: Some("f".into()),
                }],
            },
            Response::Resolved {
                path: "p".into(),
                folder: true,
                backend: 9,
            },
            Response::Error {
                message: "boom".into(),
            },
            Response::Warning {
                message: "careful".into(),
            },
        ];
        for r in responses {
            let line = r.to_json();
            assert_eq!(Response::from_json(&line), Some(r), "the line is: {line}");
        }
    }

    #[test]
    fn a_catch_up_the_client_does_not_know_is_the_conservative_one() {
        // A missing `catch_up` says the tree was walked again — the answer
        // that claims the least.
        let back = Response::from_json(r#"{"op":"status","docs":1,"event_id":2}"#)
            .expect("the line reads back");
        assert_eq!(
            back,
            Response::Status {
                docs: 1,
                event_id: 2,
                catch_up: CatchUp::Relisted,
                tv: false,
                feed: Feed::None,
            }
        );
    }

    #[test]
    fn a_scoped_content_request_round_trips_with_its_scope() {
        let req = Request::Content {
            q: "tesla".to_string(),
            paths: vec!["C:/a.txt".to_string(), "C:/b\".txt".to_string()],
            limit: 10,
        };
        let line = req.to_json();
        assert!(line.contains(r#""op":"grep-scoped""#), "the op is: {line}");
        assert_eq!(Request::from_json(&line), Some(req));
    }

    /// The content route has no session, and a request that carries one is
    /// read as the request it was before the field existed.
    ///
    /// The field was a promise the daemon could not keep: the content answer
    /// is the files on the disk, and there is no result set to re-rank. A
    /// reader that meets an old client's request with a session on it reads
    /// the scope and the query, and the session is ignored — which is what
    /// the daemon did with it all along.
    #[test]
    fn a_scoped_content_request_with_a_session_is_the_request_as_it_was() {
        let line =
            r#"{"op":"grep-scoped","q":"tesla","paths":["C:/a.txt"],"limit":10,"session":7}"#;
        assert_eq!(
            Request::from_json(line),
            Some(Request::Content {
                q: "tesla".to_string(),
                paths: vec!["C:/a.txt".to_string()],
                limit: 10,
            })
        );
        // And the request the crate writes does not carry one: the wire
        // does not promise what the daemon cannot deliver.
        let req = Request::Content {
            q: "tesla".to_string(),
            paths: vec!["C:/a.txt".to_string()],
            limit: 10,
        };
        assert!(!req.to_json().contains("session"), "the session is absent");
    }

    #[test]
    fn the_whole_tree_grep_keeps_its_own_op() {
        // The scoped op is additive: the old one still means the old thing.
        let req = Request::Grep {
            q: "tesla".to_string(),
            limit: 10,
        };
        assert_eq!(Request::from_json(&req.to_json()), Some(req));
        assert_eq!(
            Request::from_json(r#"{"op":"grep","q":"x","limit":1}"#),
            Some(Request::Grep {
                q: "x".to_string(),
                limit: 1
            })
        );
    }

    /// The scoped request, spelled the way the documentation spells it.
    ///
    /// The spelling is the contract: a peer that follows the docs and
    /// says `"content"` must get the scoped answer — and not the whole
    /// tree with its scope quietly dropped.
    #[test]
    fn a_scoped_request_spelled_the_way_the_documentation_spells_it_keeps_its_scope() {
        let line = r#"{"op":"content","q":"tesla","paths":["C:/a.txt","C:/b.txt"],"limit":10}"#;
        assert_eq!(
            Request::from_json(line),
            Some(Request::Content {
                q: "tesla".to_string(),
                paths: vec!["C:/a.txt".to_string(), "C:/b.txt".to_string()],
                limit: 10,
            })
        );
    }

    /// The older spelling is the same request.
    #[test]
    fn the_older_spelling_of_the_scoped_request_is_the_same_request() {
        let line = r#"{"op":"grep-scoped","q":"tesla","paths":["C:/a.txt"],"limit":10}"#;
        assert_eq!(
            Request::from_json(line),
            Some(Request::Content {
                q: "tesla".to_string(),
                paths: vec!["C:/a.txt".to_string()],
                limit: 10,
            })
        );
    }

    /// A scoped request that carries no scope is refused, and never
    /// widened into a search of the whole tree.
    ///
    /// The whole-tree grep is `"grep"`. A request that says `"content"`
    /// and names no paths is a request the daemon cannot honour, and the
    /// honest answer to it is an error, not a search of everything.
    #[test]
    fn a_scoped_request_without_a_scope_is_refused_and_not_widened() {
        assert_eq!(
            Request::from_json(r#"{"op":"content","q":"x","limit":1}"#),
            None
        );
        assert_eq!(
            Request::from_json(r#"{"op":"grep-scoped","q":"x","limit":1}"#),
            None
        );
    }

    /// A scope that cannot be read fails the request.
    ///
    /// A path that is not a string is not a path the daemon can read,
    /// and a scope that is quietly narrowed is a scope the caller
    /// believes it set and did not.
    #[test]
    fn a_scope_that_cannot_be_read_fails_the_request() {
        assert_eq!(
            Request::from_json(r#"{"op":"content","q":"x","paths":["C:/a.txt",7],"limit":1}"#),
            None
        );
        assert_eq!(
            Request::from_json(r#"{"op":"content","q":"x","paths":"C:/a.txt","limit":1}"#),
            None
        );
    }

    /// A hit the protocol cannot read fails the line.
    ///
    /// The count the client shows is the count of what came back, and a
    /// hit that vanishes makes that count a lie: a reader that says
    /// "nothing" about a broken line is a reader that turns a broken
    /// daemon into an empty one.
    #[test]
    fn a_hit_the_protocol_cannot_read_fails_the_line() {
        // A hit with no path.
        assert_eq!(
            Response::from_json_checked(r#"{"op":"hits","hits":[{"score":5}],"took_us":6}"#),
            Err(LineError::NotAnAnswer)
        );
        // A hit whose path is not a string.
        assert_eq!(
            Response::from_json_checked(
                r#"{"op":"hits","hits":[{"path":7,"score":5}],"took_us":6}"#
            ),
            Err(LineError::NotAnAnswer)
        );
        // And the good line still reads.
        let good = Response::from_json_checked(
            r#"{"op":"hits","hits":[{"path":"p","score":5}],"took_us":6}"#,
        );
        assert!(good.is_ok(), "the good line reads: {good:?}");
    }

    /// The content answer says what it did not read.
    ///
    /// A truncated search and a complete one are different answers, and
    /// the difference is on the wire: the two fields are additive, so a
    /// reader that does not know them keeps working.
    #[test]
    fn the_content_answer_says_what_it_did_not_read() {
        let r = Response::GrepFiles {
            files: vec![GrepFile {
                path: "f".into(),
                lines: vec![(1, "t".into())],
            }],
            took_us: 8,
            complete: false,
            skipped: 3,
            skips: Vec::new(),
        };
        let line = r.to_json();
        assert!(line.contains(r#""complete":false"#), "the flag: {line}");
        assert!(line.contains(r#""skipped":3"#), "the count: {line}");
        assert_eq!(Response::from_json(&line), Some(r));
    }

    /// The content answer says **why** it did not read them.
    ///
    /// The count is what the protocol has always carried, and it stays: a
    /// reader that does not know the reasons keeps working. The reasons are
    /// what make the count worth reading — "skipped" alone leaves the user
    /// wondering whether the file was empty, locked, or never there — and
    /// their counts add up to the count the answer already carried.
    #[test]
    fn the_content_answer_says_why_it_did_not_read_them() {
        let r = Response::GrepFiles {
            files: Vec::new(),
            took_us: 8,
            complete: false,
            skipped: 3,
            skips: vec![
                Skip {
                    reason: "a cloud placeholder; reading it would download it".into(),
                    count: 2,
                    path: Some("C:/a.txt".into()),
                },
                Skip {
                    // The budget stopped the search before it named the
                    // files it did not reach, so there is no path to say.
                    reason: "the search's budget ran out before it reached them".into(),
                    count: 1,
                    path: None,
                },
            ],
        };
        let line = r.to_json();
        assert!(line.contains(r#""skipped":3"#), "the count: {line}");
        assert!(line.contains(r#""count":2"#), "the group's count: {line}");
        assert!(line.contains(r#""path":"C:/a.txt""#), "the file: {line}");
        assert_eq!(Response::from_json(&line), Some(r));
        // The counts add up to the count: every file the search did not
        // read is in one of the groups.
        let back = Response::from_json(&line).expect("the line reads back");
        let Response::GrepFiles { skipped, skips, .. } = back else {
            panic!("the line is a content answer");
        };
        assert_eq!(
            skips.iter().map(|s| s.count).sum::<usize>(),
            skipped,
            "the reasons account for every file"
        );
    }

    /// A group that is not a group fails the line.
    ///
    /// The reasons add up to the count, so a group the reader cannot read
    /// is a group it cannot add: leaving it out would make the count the
    /// reader reports smaller than the count the daemon sent, and the
    /// sentence would say "3 files" for a daemon that said 5. The `files`
    /// array refuses an element it cannot read for the same reason, and
    /// this refuses it the same way.
    #[test]
    fn a_skip_group_that_is_not_a_group_fails_the_line() {
        assert_eq!(
            Response::from_json_checked(
                r#"{"op":"grep-files","files":[],"took_us":8,"complete":false,"skipped":5,"skips":[{"reason":"it is a cloud placeholder","count":3},"oops"]}"#
            ),
            Err(LineError::NotAnAnswer)
        );
    }

    /// A count that is not a whole number is not a count.
    ///
    /// One and a half files were not read: the number cannot be true, and a
    /// cast would have read it as `1` — a count the peer never stated. The
    /// same rule holds for the count of the answer, for the count of the
    /// files that were not read, and for the session the answer names.
    #[test]
    fn a_fractional_number_is_refused() {
        // The count of the answer.
        assert_eq!(
            Response::from_json_checked(r#"{"op":"done","count":1.5}"#),
            Err(LineError::NotAnAnswer)
        );
        // The count of the files that were not read.
        assert_eq!(
            Response::from_json_checked(
                r#"{"op":"grep-files","files":[],"took_us":8,"complete":false,"skipped":1.5}"#
            ),
            Err(LineError::NotAnAnswer)
        );
        // The session: a fractional one is no session at all, and the
        // reader keeps the session it had.
        let back = Response::from_json_checked(
            r#"{"op":"hits","hits":[],"took_us":1,"complete":true,"session":1.5}"#,
        )
        .expect("the line reads back");
        let Response::Hits { session, .. } = back else {
            panic!("the line is a hits answer");
        };
        assert_eq!(session, None, "a fractional session is no session");
    }

    /// A fractional offset takes the whole span with it.
    ///
    /// The span is all of it or none of it: an offset that is not an offset
    /// is not a character of the name, and a cast would have pointed the
    /// highlight into the middle of a name the peer never pointed at.
    #[test]
    fn a_fractional_offset_takes_the_span_with_it() {
        let back = Response::from_json_checked(
            r#"{"op":"hits","hits":[{"path":"C:/a.txt","score":1,"span":[1.5]}],"took_us":1}"#,
        )
        .expect("the line reads back");
        let Response::Hits { hits, .. } = back else {
            panic!("the line is a hits answer");
        };
        assert!(
            hits[0].span.is_empty(),
            "the span is gone: {:?}",
            hits[0].span
        );
    }

    /// A daemon that does not say the reasons does not promise the count is
    /// the whole story — and the count it does say still reads.
    #[test]
    fn a_grep_files_line_without_the_reasons_reads_the_count_it_carries() {
        let back = Response::from_json(
            r#"{"op":"grep-files","files":[],"took_us":8,"complete":true,"skipped":2}"#,
        )
        .expect("the line reads back");
        assert_eq!(
            back,
            Response::GrepFiles {
                files: Vec::new(),
                took_us: 8,
                complete: true,
                skipped: 2,
                skips: Vec::new(),
            }
        );
    }

    /// A daemon that does not say is a daemon that promises the least.
    #[test]
    fn a_grep_files_line_without_the_two_facts_does_not_promise_completeness() {
        let back = Response::from_json(r#"{"op":"grep-files","files":[],"took_us":8}"#)
            .expect("the line reads back");
        assert_eq!(
            back,
            Response::GrepFiles {
                files: Vec::new(),
                took_us: 8,
                complete: false,
                skipped: 0,
                skips: Vec::new(),
            }
        );
    }

    /// The answer names the session it belongs to.
    ///
    /// The session is the incremental-search id: the client sends it back
    /// and the daemon re-ranks the answer instead of walking the index
    /// again. A client that cannot name the session it is in cannot ask for
    /// that, so the id is on the answer — and the field is additive, so a
    /// daemon that does not say it is read as an answer with no session.
    #[test]
    fn a_hits_line_names_the_session_the_answer_belongs_to() {
        let r = Response::Hits {
            hits: vec![Hit::new("p".into(), 1)],
            took_us: 2,
            complete: true,
            session: Some(7),
        };
        let line = r.to_json();
        assert!(line.contains(r#""session":7"#), "the session is: {line}");
        assert_eq!(Response::from_json(&line), Some(r));
        // And a line without one is read as an answer with no session: the
        // reader keeps the session it had.
        let back =
            Response::from_json(r#"{"op":"hits","hits":[{"path":"p","score":1}],"took_us":2}"#)
                .expect("the line reads back");
        assert_eq!(
            back,
            Response::Hits {
                hits: vec![Hit::new("p".into(), 1)],
                took_us: 2,
                complete: false,
                session: None,
            }
        );
    }

    /// A session id survives the wire, and the wire's limit is where the
    /// two spaces end.
    ///
    /// The reader carries a number as an `f64`, so an integer above
    /// `2^53` does not survive the round trip: the id that arrives is a
    /// different id. The daemon's own sessions and the ids a client names
    /// both live under that limit for exactly that reason — the daemon's
    /// in `[2^52, 2^53)`, the clients' below `2^52` — and this test is
    /// what holds the line.
    #[test]
    fn a_session_id_survives_the_wire() {
        for id in [0u64, 1, 7, (1 << 52) - 1, 1 << 52, (1 << 53) - 1, 1 << 53] {
            let r = Response::Hits {
                hits: vec![],
                took_us: 2,
                complete: true,
                session: Some(id),
            };
            let line = r.to_json();
            let back = Response::from_json(&line);
            // Every integer up to `2^53` survives: the limit is inclusive,
            // because `2^53` is a power of two and a power of two is
            // exactly representable.
            assert_eq!(back, Some(r), "the session {id} did not survive: {line}");
        }
        // And the one above it does not — which is why nothing names one.
        let r = Response::Hits {
            hits: vec![],
            took_us: 2,
            complete: true,
            session: Some((1 << 53) + 1),
        };
        let line = r.to_json();
        assert_ne!(
            Response::from_json(&line),
            Some(r),
            "an id above the wire's limit cannot survive, which is why nothing names one"
        );
    }

    /// A limit the tree cannot honour is clamped, and a session that is
    /// not a session is not one.
    ///
    /// The limit is how many hits the answer carries, and the answer is a
    /// list: the daemon holds the whole index, and a request that asks for
    /// the whole index is a request to make the daemon materialise it.
    /// `1e30` became `usize::MAX`, which reads as "no cap" and means the
    /// whole index in one answer. The reader clamps now, and the answer
    /// says whether the list reached the limit, so a clamped limit is
    /// visible and not a lie.
    ///
    /// The session is a number the wire can carry exactly, and it is not a
    /// negative one. The reader used to cast it to `u64`, and a cast
    /// saturates: a client that sent `-1` got the id `0`, which is in the
    /// *client's* space — the space the daemon trusts. Two clients, one
    /// sending `-1` and one sending `0`, would have shared a result set.
    #[test]
    fn a_limit_is_clamped_and_a_session_that_is_not_one_is_not_one() {
        let huge = Request::from_json(r#"{"op":"search","q":"x","limit":1e30}"#)
            .expect("the request is read");
        assert_eq!(huge.limit(), MAX_LIMIT, "a limit beyond the cap is the cap");

        let normal = Request::from_json(r#"{"op":"search","q":"x","limit":20}"#)
            .expect("the request is read");
        assert_eq!(normal.limit(), 20, "a limit under the cap is the limit");

        // A negative id is not an id.
        let negative = Request::from_json(r#"{"op":"search","q":"x","session":-1}"#)
            .expect("the request is read");
        assert_eq!(negative.session(), None, "a negative id is no session");

        // And an id above the wire's limit is not one the wire can carry.
        let over = Request::from_json(r#"{"op":"search","q":"x","session":9007199254740994}"#)
            .expect("the request is read");
        assert_eq!(over.session(), None, "an id above the limit is no session");

        // The limit itself is inclusive: `2^53` is a power of two, and a
        // power of two is exactly representable.
        let at = Request::from_json(r#"{"op":"search","q":"x","session":9007199254740992}"#)
            .expect("the request is read");
        assert_eq!(at.session(), Some(WIRE_LIMIT), "the limit is inclusive");

        // And an ordinary id in the client's space is the client's to name.
        let seven = Request::from_json(r#"{"op":"search","q":"x","session":7}"#)
            .expect("the request is read");
        assert_eq!(seven.session(), Some(7), "an id under the line is read");
    }

    /// The session an answer names, when the answer names one.
    fn session_of(r: &Response) -> Option<u64> {
        match r {
            Response::Hits { session, .. } => *session,
            _ => panic!("the answer is a hits answer"),
        }
    }

    /// The span of the first hit of an answer.
    fn span_of(r: &Response) -> Vec<u32> {
        match r {
            Response::Hits { hits, .. } => match hits.first() {
                Some(hit) => hit.span.clone(),
                None => Vec::new(),
            },
            _ => panic!("the answer is a hits answer"),
        }
    }

    /// The answer side has the session rule the request side has.
    ///
    /// The reader used to cast the number to `u64`, and a cast saturates: a
    /// peer that answered `-1` was read as the session `0` — which is in
    /// the *client's* space, the space the daemon trusts — so the client
    /// kept it and sent it back on the next question. The rule is now the
    /// request's own, and it is in one place.
    ///
    /// An answer that names no session is an answer that names no session:
    /// the client keeps the session it had.
    #[test]
    fn an_answer_names_a_session_only_when_the_number_is_one() {
        let negative = Response::from_json(
            r#"{"op":"hits","hits":[],"took_us":1,"complete":true,"session":-1}"#,
        )
        .expect("the answer is read");
        assert_eq!(
            session_of(&negative),
            None,
            "a negative id is no session on the answer side either"
        );

        let over = Response::from_json(
            r#"{"op":"hits","hits":[],"took_us":1,"complete":true,"session":9007199254740994}"#,
        )
        .expect("the answer is read");
        assert_eq!(
            session_of(&over),
            None,
            "an id above the wire's limit is no session"
        );

        // The limit itself is inclusive: `2^53` is a power of two, and a
        // power of two is exactly representable.
        let at = Response::from_json(
            r#"{"op":"hits","hits":[],"took_us":1,"complete":true,"session":9007199254740992}"#,
        )
        .expect("the answer is read");
        assert_eq!(
            session_of(&at),
            Some(WIRE_LIMIT),
            "the limit is inclusive on the answer side too"
        );
    }

    /// The limit is clamped at both ends, and the floor is zero.
    ///
    /// The reader used to cast the number to `usize`, and a cast saturates:
    /// a negative limit became zero by accident, and the code did not say
    /// so. The rule is written down now — below zero it is zero hits, and
    /// above the cap it is the cap.
    ///
    /// The floor is zero because a request for a negative number of hits is
    /// a request for no hits: the daemon answers with an empty list and
    /// says it stopped at the limit, so the caller is not left thinking
    /// there was nothing to find.
    #[test]
    fn a_limit_below_zero_is_zero_hits_and_a_limit_above_the_cap_is_the_cap() {
        let negative = Request::from_json(r#"{"op":"search","q":"x","limit":-1}"#)
            .expect("the request is read");
        assert_eq!(negative.limit(), 0, "a negative limit is no hits");

        let over = Request::from_json(r#"{"op":"search","q":"x","limit":100001}"#)
            .expect("the request is read");
        assert_eq!(over.limit(), MAX_LIMIT, "a limit above the cap is the cap");

        let ordinary = Request::from_json(r#"{"op":"search","q":"x","limit":20}"#)
            .expect("the request is read");
        assert_eq!(ordinary.limit(), 20, "an ordinary limit is itself");

        // And no limit at all is the default.
        let none = Request::from_json(r#"{"op":"search","q":"x"}"#).expect("the request is read");
        assert_eq!(none.limit(), 50, "no limit is the default");
    }

    /// A limit that is not a whole number is a request that cannot be true.
    ///
    /// The answer carries whole hits, and a request for 1.9 of them is a
    /// request for a number of hits that does not exist. The reader used
    /// to cast it, and a cast truncates: 1.9 became 1, 0.5 became 0, and
    /// the caller was told the answer was complete for a limit it never
    /// asked for. Every other number on this wire is refused when it
    /// cannot be true — the count, the session, the span — and the limit
    /// is now the same.
    #[test]
    fn a_limit_that_is_not_a_whole_number_is_refused() {
        assert_eq!(
            Request::from_json(r#"{"op":"search","q":"x","limit":1.9}"#),
            None,
            "a limit of 1.9 hits is not a limit"
        );
        assert_eq!(
            Request::from_json(r#"{"op":"search","q":"x","limit":0.5}"#),
            None,
            "a limit of half a hit is not a limit"
        );
        assert_eq!(
            Request::from_json(r#"{"op":"grep","q":"x","limit":99.999}"#),
            None,
            "and the same on the grep route"
        );
        // A whole number written as a decimal is still a whole number.
        let whole = Request::from_json(r#"{"op":"search","q":"x","limit":20.0}"#)
            .expect("the request is read");
        assert_eq!(whole.limit(), 20, "20.0 is 20");
    }

    /// A request that names a session and a request that names none.
    ///
    /// The session `0` is a session: the rule that keeps a negative id out
    /// does not keep the first id out, and the id `0` is in the client's
    /// own space — the space the daemon trusts.
    #[test]
    fn a_request_that_names_a_session_and_one_that_names_none() {
        let zero = Request::from_json(r#"{"op":"search","q":"x","session":0}"#)
            .expect("the request is read");
        assert_eq!(zero.session(), Some(0), "the first id is a session");

        let none = Request::from_json(r#"{"op":"search","q":"x"}"#).expect("the request is read");
        assert_eq!(none.session(), None, "no id is no session");
    }

    /// A span with an offset that is not an offset is no span.
    ///
    /// The offsets count the characters of the name from the first one, so
    /// an offset is not negative — and the reader used to cast it, and the
    /// cast made `-1` the first character of the name, which is not what
    /// the peer said. The span is all of it or none of it, so an offset
    /// that is not one takes the whole span with it.
    #[test]
    fn a_span_with_an_offset_that_is_not_an_offset_is_no_span() {
        let negative = Response::from_json(
            r#"{"op":"hits","hits":[{"path":"a.txt","score":1,"span":[-1]}],"took_us":1,"complete":true}"#,
        )
        .expect("the answer is read");
        assert_eq!(
            span_of(&negative),
            Vec::new(),
            "a negative offset is not an offset, and the span is none of it"
        );

        // And an ordinary span is the span the peer sent.
        let ordinary = Response::from_json(
            r#"{"op":"hits","hits":[{"path":"a.txt","score":1,"span":[0,2]}],"took_us":1,"complete":true}"#,
        )
        .expect("the answer is read");
        assert_eq!(span_of(&ordinary), vec![0, 2], "an ordinary span is read");
    }

    /// A status whose count is not a count is not an answer.
    ///
    /// The count is the size of the daemon's index and the launcher draws
    /// it: a header that says "0" about an index of a thousand entries is
    /// a wrong fact, and a wrong fact is worse than no fact. A count that
    /// cannot be true is not a count the reader can report, so the reader
    /// refuses the line — a daemon that cannot count its own index has
    /// answered badly, and the caller says so.
    #[test]
    fn a_status_whose_count_is_not_a_count_is_not_an_answer() {
        let negative = Response::from_json_checked(
            r#"{"op":"status","docs":-1,"event_id":0,"catch_up":"replayed","tv":false,"feed":"live"}"#,
        );
        assert_eq!(
            negative,
            Err(LineError::NotAnAnswer),
            "a negative count is not a count"
        );

        let over = Response::from_json_checked(
            r#"{"op":"status","docs":9007199254740994,"event_id":0,"catch_up":"replayed","tv":false,"feed":"live"}"#,
        );
        assert_eq!(
            over,
            Err(LineError::NotAnAnswer),
            "a count above the wire's limit is not a count"
        );

        // And an ordinary count is the count.
        let ordinary = Response::from_json(
            r#"{"op":"status","docs":1226,"event_id":0,"catch_up":"replayed","tv":false,"feed":"live"}"#,
        )
        .expect("the answer is read");
        match ordinary {
            Response::Status { docs, .. } => {
                assert_eq!(docs, 1226, "the count is the count");
            }
            _ => panic!("the answer is a status"),
        }
    }

    /// A status without the count is not a status.
    ///
    /// The count is the size of the index and the launcher draws it, and a
    /// zero is a fact: a header that says "0" about an index of a thousand
    /// entries is a wrong fact. The reader has no way to say "not known" in
    /// a `usize`, so a line without the count is a line it cannot use — and
    /// it refuses it, the same way it refuses a hit without a path.
    #[test]
    fn a_status_without_the_count_is_not_a_status() {
        let absent = Response::from_json_checked(
            r#"{"op":"status","event_id":0,"catch_up":"replayed","tv":false,"feed":"live"}"#,
        );
        assert_eq!(
            absent,
            Err(LineError::NotAnAnswer),
            "a status without the count is not an answer the reader can use"
        );

        // And a count is read as the count.
        let present = Response::from_json(
            r#"{"op":"status","docs":1226,"event_id":0,"catch_up":"replayed","tv":false,"feed":"live"}"#,
        )
        .expect("the answer is read");
        match present {
            Response::Status { docs, .. } => {
                assert_eq!(docs, 1226, "the count is the count");
            }
            _ => panic!("the answer is a status"),
        }
    }

    /// A done whose count is not a count is not an answer.
    ///
    /// The count of the terminator is the size of the answer: the client
    /// counts what it got, which is the same number, and the terminator
    /// says so about the stream it ends. A count that cannot be true is
    /// not a count the reader can report — a negative one, or one the wire
    /// cannot carry exactly — so the reader refuses the frame, the same
    /// way it refuses a count the status frame carries.
    #[test]
    fn a_done_whose_count_is_not_a_count_is_not_an_answer() {
        let negative = Response::from_json_checked(r#"{"op":"done","count":-1}"#);
        assert_eq!(
            negative,
            Err(LineError::NotAnAnswer),
            "a negative count is not a count"
        );

        let over = Response::from_json_checked(r#"{"op":"done","count":9007199254740994}"#);
        assert_eq!(
            over,
            Err(LineError::NotAnAnswer),
            "a count above the wire's limit is not a count"
        );

        // And an ordinary count is the count.
        let ordinary =
            Response::from_json(r#"{"op":"done","count":7}"#).expect("the answer is read");
        match ordinary {
            Response::Done { count } => {
                assert_eq!(count, 7, "the count is the count");
            }
            _ => panic!("the answer is a terminator"),
        }
    }

    /// A done without the count is not a done.
    ///
    /// The count of the terminator is the size of the answer, and a zero
    /// is a fact: a frame that says "0" about an answer that carried a
    /// thousand entries is a wrong fact. The reader has no way to say "not
    /// known" in a `usize`, so a frame without the count is a frame it
    /// cannot use — and it refuses it, the way it refuses a hit without a
    /// path.
    #[test]
    fn a_done_without_the_count_is_not_a_done() {
        let absent = Response::from_json_checked(r#"{"op":"done"}"#);
        assert_eq!(
            absent,
            Err(LineError::NotAnAnswer),
            "a terminator without the count is not an answer the reader can use"
        );
    }

    /// A status whose journal position is not a position is not an answer.
    ///
    /// The position is the daemon's own counter and it is a report: a
    /// negative one is not a position at all, and the cast the reader used
    /// made it the start of the journal — a position the peer never
    /// claimed. So the frame is refused, the way a count that cannot be
    /// true refuses it.
    ///
    /// A position above the wire's limit stays, though: the field is a
    /// report and it travels as an `f64`, so an integer above `2^53` is a
    /// number the reader reads back as a different one. That is written
    /// down in the variant's own documentation, and a report is allowed to
    /// be off.
    #[test]
    fn a_status_whose_journal_position_is_not_a_position_is_not_an_answer() {
        let negative = Response::from_json_checked(
            r#"{"op":"status","docs":1226,"event_id":-1,"catch_up":"replayed","tv":false,"feed":"live"}"#,
        );
        assert_eq!(
            negative,
            Err(LineError::NotAnAnswer),
            "a negative position is not a position"
        );

        // And a position above the wire's limit is a report, and a report
        // is read as it arrives.
        let over = Response::from_json(
            r#"{"op":"status","docs":1226,"event_id":9007199254740994,"catch_up":"replayed","tv":false,"feed":"live"}"#,
        )
        .expect("the answer is read");
        match over {
            Response::Status { event_id, .. } => {
                assert_eq!(
                    event_id, 9_007_199_254_740_994,
                    "a report above the wire's limit is read, not refused"
                );
            }
            _ => panic!("the answer is a status"),
        }
    }

    /// A status without the journal position reads back as the start of the
    /// journal.
    ///
    /// The position is a report and the field is optional: a line without
    /// it is a line from a daemon that does not say, and the conservative
    /// reading of "no position" is `0` — which is exactly what a daemon
    /// that relisted the tree reports. That is the same reading the
    /// `catch_up` field's own default carries: a `status` line without a
    /// `catch_up` reads back as `relisted` — the conservative answer, the
    /// one that says the tree was walked again.
    #[test]
    fn a_status_without_the_journal_position_reads_back_as_the_start_of_the_journal() {
        let absent = Response::from_json(
            r#"{"op":"status","docs":1226,"catch_up":"replayed","tv":true,"feed":"live"}"#,
        )
        .expect("the answer is read");
        match absent {
            Response::Status {
                docs,
                event_id,
                catch_up,
                tv,
                feed,
            } => {
                assert_eq!(
                    event_id, 0,
                    "a line that does not say reads as the start of the journal"
                );
                assert_eq!(docs, 1226, "the count is the count");
                assert_eq!(catch_up, CatchUp::Replayed, "the catch-up is the catch-up");
                assert!(tv, "the store is the store");
                assert_eq!(feed, Feed::Live, "the feed is the feed");
            }
            _ => panic!("the answer is a status"),
        }
    }

    /// A line number that is not a line refuses the frame.
    ///
    /// The line numbers are 1-based — the first line of a file is line 1,
    /// the way `rg` prints them and the way the engine counts them — so a
    /// zero names a line no file has, and the reader used to cast the
    /// number: `-1` became the line `0`.
    ///
    /// The pair is one fact about one line, and the reader already refuses
    /// the frame for the half it cannot read. It refuses it here too: a
    /// pair left out would make the file's matches fewer than they were,
    /// which is the lie the hits arm refuses to tell.
    #[test]
    fn a_line_number_that_is_not_a_line_refuses_the_frame() {
        let zero = Response::from_json_checked(
            r#"{"op":"grep-files","files":[{"path":"a.txt","lines":[[0,"text"]]}],"took_us":1,"complete":true}"#,
        );
        assert_eq!(
            zero,
            Err(LineError::NotAnAnswer),
            "the line 0 is a line no file has"
        );

        let negative = Response::from_json_checked(
            r#"{"op":"grep-files","files":[{"path":"a.txt","lines":[[-1,"text"]]}],"took_us":1,"complete":true}"#,
        );
        assert_eq!(
            negative,
            Err(LineError::NotAnAnswer),
            "a negative line is not a line"
        );

        // And an ordinary line is the line.
        let ordinary = Response::from_json(
            r#"{"op":"grep-files","files":[{"path":"a.txt","lines":[[7,"text"]]}],"took_us":1,"complete":true}"#,
        )
        .expect("the answer is read");
        match ordinary {
            Response::GrepFiles { files, .. } => {
                assert_eq!(
                    files[0].lines,
                    vec![(7, "text".to_string())],
                    "the line is the line"
                );
            }
            _ => panic!("the answer is a grep answer"),
        }
    }

    /// A count that cannot be true refuses the frame.
    ///
    /// The counts are the reason the reasons are on the wire: they are what
    /// makes the sentence "the daemon did not read N file(s)" true. A count
    /// that cannot be true would make the sentence name a number that
    /// cannot be true, and the reader has no way to say "not known" in a
    /// `usize` — a zero is a fact ("it read every file").
    #[test]
    fn a_skip_count_that_cannot_be_true_refuses_the_frame() {
        let negative = Response::from_json_checked(
            r#"{"op":"grep-files","files":[],"took_us":1,"complete":false,"skipped":-1}"#,
        );
        assert_eq!(
            negative,
            Err(LineError::NotAnAnswer),
            "a negative count is not a count"
        );

        let group = Response::from_json_checked(
            r#"{"op":"grep-files","files":[],"took_us":1,"complete":false,"skipped":2,"skips":[{"reason":"a cloud placeholder","count":-1}]}"#,
        );
        assert_eq!(
            group,
            Err(LineError::NotAnAnswer),
            "a group's negative count is not a count"
        );

        // And a group without a count is a group the reader cannot count:
        // the sentence it makes would name "0 file(s)" for files that were
        // not read, and a group left out of the sum would make the count
        // smaller than the truth.
        let without = Response::from_json_checked(
            r#"{"op":"grep-files","files":[],"took_us":1,"complete":false,"skipped":2,"skips":[{"reason":"a cloud placeholder"}]}"#,
        );
        assert_eq!(
            without,
            Err(LineError::NotAnAnswer),
            "a group without a count is a group the reader cannot count"
        );
    }

    /// The count the reader reports is the count the reasons make.
    ///
    /// The count and the reasons are one fact written twice, and the
    /// protocol says the reasons add up to the count. The reader reads the
    /// count the reasons make, so the answer's own two numbers agree by
    /// construction — and the sentences they make are the same number said
    /// twice.
    #[test]
    fn the_count_the_reader_reports_is_the_count_the_reasons_make() {
        let frame = Response::from_json(
            r#"{"op":"grep-files","files":[],"took_us":1,"complete":false,"skipped":7,"skips":[{"reason":"a cloud placeholder","count":2},{"reason":"the search's budget ran out before it reached them","count":1}]}"#,
        )
        .expect("the answer is read");
        match frame {
            Response::GrepFiles { skipped, skips, .. } => {
                assert_eq!(skipped, 3, "the count is the count the reasons make");
                assert_eq!(
                    skips.iter().map(|s| s.count).sum::<usize>(),
                    skipped,
                    "the reasons add up to the count"
                );
            }
            _ => panic!("the answer is a grep answer"),
        }

        // And a frame with no reasons is the count the wire carries.
        let bare = Response::from_json(
            r#"{"op":"grep-files","files":[],"took_us":1,"complete":false,"skipped":4}"#,
        )
        .expect("the answer is read");
        match bare {
            Response::GrepFiles { skipped, .. } => {
                assert_eq!(
                    skipped, 4,
                    "the wire's count is the count when there are no reasons"
                );
            }
            _ => panic!("the answer is a grep answer"),
        }
    }

    /// The backend is a report, and it is read as it arrives.
    ///
    /// The ids are the daemon's own index's, and the reader cannot tell
    /// which ones exist — the index is not on the wire. Nothing in the tree
    /// acts on the id: the client keeps it and no caller reads it. So the
    /// field is a report, and a report may be wrong without misleading
    /// anyone. A negative id lands on `0`, which is the volume the index
    /// does not name — the safe reading of a number that is not one.
    #[test]
    fn the_backend_is_a_report_and_is_read_as_it_arrives() {
        let negative = Response::from_json(
            r#"{"op":"resolved","path":"C:/a.txt","folder":false,"backend":-1}"#,
        )
        .expect("the answer is read");
        match negative {
            Response::Resolved { backend, .. } => {
                assert_eq!(
                    backend, 0,
                    "a negative id is the volume the index does not name"
                );
            }
            _ => panic!("the answer is a resolution"),
        }

        let big = Response::from_json(
            r#"{"op":"resolved","path":"C:/a.txt","folder":false,"backend":9007199254740994}"#,
        )
        .expect("the answer is read");
        match big {
            Response::Resolved { backend, .. } => {
                assert_eq!(backend, u32::MAX, "a report is read as it arrives");
            }
            _ => panic!("the answer is a resolution"),
        }
    }

    /// A document that nests deeper than the reader allows is refused.
    ///
    /// The reader recurses once per level, and a stack overflow is an
    /// abort, not a panic: the document has to be refused, not read. The
    /// depth the protocol's own answers nest in is nothing at all.
    #[test]
    fn a_document_that_nests_too_deep_is_refused() {
        let deep = format!("{}{}", "[".repeat(200), "]".repeat(200));
        assert_eq!(read_json(&deep), None, "the reader refuses it");
        assert_eq!(
            Response::from_json_checked(&deep),
            Err(LineError::TooDeep),
            "and it says why"
        );
        let shallow = format!("{}{}", "[".repeat(8), "]".repeat(8));
        assert!(read_json(&shallow).is_some(), "eight levels are nothing");
    }
}
