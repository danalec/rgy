// SPDX-License-Identifier: Apache-2.0

//! The client: the half that talks to a running daemon.
//!
//! Until now the protocol had a server and no client: `searchd` answered
//! requests and nothing in this repository ever asked one. A launcher that
//! opens in milliseconds cannot own an index — the walk, the load and the
//! ranking are the daemon's job — so the client is the piece that lets any
//! interface sit on top of the index that is already warm.
//!
//! The framing is the one the daemon already speaks: one JSON request per
//! line in, one JSON response per line out, and the response stream ends
//! with a terminator (`done`) or an `error`. Nothing here is negotiated and
//! nothing here is versioned beyond what the wire already is.
//!
//! # The transports
//!
//! | endpoint | transport |
//! |---|---|
//! | [`Endpoint::Stdio`] | the daemon's own stdin/stdout |
//! | [`Endpoint::Socket`] | a unix domain socket |
//! | [`Endpoint::Pipe`] | a Windows named pipe |
//!
//! The Windows named pipe is opened with the ordinary file API — the
//! `\\.\pipe\` namespace is a file namespace — and the one case that needs
//! care is `ERROR_PIPE_BUSY`: every instance of the pipe is in use. The
//! daemon keeps a pool of acceptors for exactly that reason, and the client
//! retries briefly rather than failing on the first busy moment.
//!
//! # The deadline
//!
//! Every request has one, and it is the deadline of the **whole** answer: a
//! daemon that dribbles a line every second runs out of time just the same.
//! A client that waits forever is a client that hangs the program it is in,
//! and the launcher asks the daemon from its own thread — so a daemon that
//! accepts the connection and then says nothing would freeze the window.
//!
//! The deadline belongs to the transport and not to the reader: a reader
//! cannot time out, and the three transports say "there is something to
//! read" in three different ways. The socket has a read timeout of its own;
//! the named pipe has a peek that says how many bytes are waiting without
//! taking them; and the standard input has neither, so its bytes arrive on
//! a thread.
//!
//! The window covers the **question** as well as the answer. A daemon that
//! takes the connection and then stops reading fills the pipe, and a write
//! that waits for that pipe is a write that never ends — so the question
//! goes out under the same window, and the two share it: the deadline is
//! computed once, and a caller that asks for two seconds waits two seconds
//! and not four.
//!
//! # The limits
//!
//! A line is bounded, and a document is bounded in its nesting. The peer is
//! another program: a line that never ends would make the client allocate
//! until it dies, and a document that nests deeper than the reader allows
//! would overflow the stack — and a stack overflow is an abort, not a
//! catchable panic, in a process that is meant to stay up. Both are refused
//! with a sentence that says what happened.

use alloc::boxed::Box;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use std::io::{Read, Write};
use std::time::{Duration, Instant};

use crate::endpoint::Endpoint;
use crate::{CatchUp, Feed, Hit, LineError, Request, Response};

/// The window a client waits for the daemon's answer.
///
/// A search over a warm index is a re-rank of the previous result set, and
/// that is microseconds; a fresh search over a large tree is the slow case,
/// and it is the daemon's work, not the client's. The timeout is generous on
/// purpose: a client that gave up on a slow answer would turn a slow answer
/// into a wrong one.
const READ_TIMEOUT: Duration = Duration::from_secs(30);

/// The longest answer line the client reads.
///
/// The content answer is the big one: it carries the matching lines of
/// every file the daemon read. The plain engine reports five lines a file
/// of four hundred bytes, so a fifty-file answer is a hundred kilobytes;
/// the pack store answers with all of them and is an order of magnitude
/// over that. The cap sits above every answer the daemon really sends and
/// below "the peer decides how much the client allocates" — a line that
/// never ends is a way to kill a resident process, and this client lives
/// inside one.
const MAX_LINE: usize = 8 * 1024 * 1024;

/// How much the client reads at a time.
///
/// It is the transport's own chunk: the socket and the pipe hand over what
/// they have, and the line reader puts the pieces together.
const CHUNK: usize = 16 * 1024;

/// The sentence for an answer that stopped at the limit.
///
/// The name layer and the content layer cut their answers at the same
/// place, and they say it the same way: the sentence lives here so that
/// two clients that read the same daemon say the same words about the
/// same answer, and a client that says nothing says the answer is whole.
pub const ANSWER_STOPS_AT_THE_LIMIT: &str = "the answer stops at the limit";

/// A connection to a daemon.
///
/// The client owns both halves of the stream, because the protocol is a
/// conversation and a half-open one is a client that blocks forever.
///
/// The write is under the caller's window and not only the read: a daemon
/// that takes the connection and then stops reading fills the pipe, and
/// the write is what waits for it.
pub struct Client {
    /// The bytes the daemon answers with.
    transport: Box<dyn Transport + Send>,
    /// Where the questions go.
    writer: Box<dyn Writer + Send>,
    /// Why the connection can no longer be used, when it cannot.
    ///
    /// A question that ran out of time on the way out leaves the
    /// connection in an unknown state — some of the bytes may have gone
    /// out — and a client that wrote another question into it would be
    /// talking into a request the daemon never got whole. The client
    /// says so instead of writing.
    ///
    /// The read has its own two. An answer that ran out of time leaves
    /// the question asked and its answer still to come — the daemon will
    /// write it after the caller has given up — and the next question
    /// into that connection would read the previous answer as its own.
    /// And an answer with a line the reader refuses has the rest of the
    /// answer still behind that line, for the same reason. The client
    /// says so instead of reading.
    broken: Option<Spent>,
    /// The session id the client sends, when it has one.
    ///
    /// The daemon keys its incremental-search sessions on this, and a
    /// client that keeps one id re-uses the previous result set when the
    /// query narrows — which is what a keystroke does. Two clients with two
    /// ids never disturb each other.
    session: Option<u64>,
}

impl Client {
    /// Connect to a daemon listening on `endpoint`.
    pub fn connect(endpoint: &Endpoint) -> std::io::Result<Client> {
        Client::connect_within(endpoint, None)
    }

    /// Connect, with the waiting bounded by the caller's deadline.
    ///
    /// The deadline is the budget of the whole answer, and the connect is
    /// part of the answer: a pipe that stayed busy for the whole window is
    /// a pipe the window was spent on, and the question that was never
    /// asked would have had no time left for it. `None` is the old
    /// behaviour: the pipe waits out the busy case on its own.
    pub fn connect_within(
        endpoint: &Endpoint,
        deadline: Option<Instant>,
    ) -> std::io::Result<Client> {
        let (transport, writer): (Box<dyn Transport + Send>, Box<dyn Writer + Send>) =
            match endpoint {
                // The standard output is the caller's own stream, and the
                // platform has no write timeout for it: the write goes on a
                // thread of its own, so that the caller can give up on it.
                Endpoint::Stdio => (
                    Box::new(StdioTransport::new()),
                    Box::new(ThreadWriter::new(Box::new(std::io::stdout()))),
                ),
                Endpoint::Socket(path) => socket_transport(path)?,
                Endpoint::Pipe(name) => pipe_transport_within(name, deadline)?,
            };
        Ok(Client {
            transport,
            writer,
            broken: None,
            session: None,
        })
    }

    /// The session id this client sends, if it has one.
    pub fn session(&self) -> Option<u64> {
        self.session
    }

    /// Use this session id from now on.
    ///
    /// A client that opens a window of results asks for a fresh id every
    /// time it opens one: the previous window's result set is not the answer
    /// to a new question.
    pub fn set_session(&mut self, id: Option<u64>) {
        self.session = id;
    }

    /// Send a request and read the answers until the stream ends.
    ///
    /// The stream ends on `done` or on `error`; a `warning` is part of the
    /// answer, not the end of it, because the daemon sends the warnings
    /// before the hits they belong to.
    pub fn request(&mut self, req: &Request) -> std::io::Result<Vec<Response>> {
        self.request_within(req, READ_TIMEOUT)
    }

    /// The same, with the window the caller asks for.
    ///
    /// The window is the whole answer's, not the chunk's: the deadline is
    /// computed once, here, and every read under it is measured against
    /// the same moment.
    pub fn request_within(
        &mut self,
        req: &Request,
        window: Duration,
    ) -> std::io::Result<Vec<Response>> {
        // A spent connection is refused before anything is written. The
        // refusal says which of the ways it was spent, and the kind is
        // the fact's: a daemon that did not take the question and one
        // that did not answer are both daemons that are there, and a
        // daemon that answered badly is one that is there too.
        if let Some(spent) = &self.broken {
            return Err(spent.refusal());
        }
        // The deadline is computed once, and the question and the answer
        // share it: a caller that asks for two seconds waits two seconds
        // and not four.
        let deadline = Instant::now() + window;
        let mut line = req.to_json();
        line.push('\n');
        self.write_question(line.as_bytes(), deadline)?;

        let mut out = Vec::new();
        loop {
            let Some(raw) = self.read_line(deadline)? else {
                // The daemon closed the connection. WHICH close is the
                // difference between two facts, and the kind is what the
                // caller reads.
                //
                // A close before anything was said is a daemon that is not
                // there: the pipe that was closed, and the user has to
                // start one. The kind says so, because a caller that says
                // "the daemon answered badly" about a daemon that is gone
                // hides the daemon the user has to start.
                //
                // A close after the daemon spoke is a daemon that IS there
                // and answered badly: it spoke, and then it stopped. The
                // answer that arrived is not an answer, and the caller
                // that says "no connection to the daemon" about it says
                // something that is not true -- and it is the launcher's
                // own rule, which says that only a daemon that could not
                // be REACHED is gone.
                //
                // A half answer is a different thing, and it is a different
                // kind: a caller that showed it would show "no results" for
                // a daemon that died mid-sentence.
                return Err(if out.is_empty() {
                    std::io::Error::new(
                        std::io::ErrorKind::ConnectionAborted,
                        "the daemon closed the connection before the answer",
                    )
                } else {
                    std::io::Error::new(
                        std::io::ErrorKind::UnexpectedEof,
                        "the daemon closed the connection in the middle of the answer",
                    )
                });
            };
            let line = raw.trim_end_matches(['\r']);
            if line.trim().is_empty() {
                continue;
            }
            let resp = match Response::from_json_checked(line) {
                Ok(resp) => resp,
                Err(e) => {
                    // The answer was refused, and the rest of it — the
                    // terminator included — is still to come: the next
                    // question into this connection would read its way in
                    // through the tail of this one.
                    self.broken = Some(Spent::UnreadAnswer);
                    return Err(unreadable_line(e));
                }
            };
            let terminal = ends_the_answer(req, &resp);
            out.push(resp);
            if terminal {
                break;
            }
        }
        // The count the terminator carries is checked here, where the wire
        // was read: every reader of the wire gets the check, and a reader
        // that used the raw frames gets it too.
        the_count_agrees(&out)?;
        Ok(out)
    }

    /// Put the question on the wire, under the window.
    ///
    /// A write that runs out of time is a timeout and not a broken
    /// connection: the daemon took the connection and did not read it,
    /// which is the same fact as a daemon that did not answer — the
    /// daemon is there, and it is slow. A caller that says "the daemon is
    /// gone" about a daemon that is up hides the daemon the user has to
    /// start.
    ///
    /// The connection is spent after that: some of the question may have
    /// gone out, and the answer the daemon sends would be the answer to a
    /// question it never got whole.
    fn write_question(&mut self, bytes: &[u8], deadline: Instant) -> std::io::Result<()> {
        match self.writer.write_until(bytes, deadline) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::TimedOut => {
                self.broken = Some(Spent::HalfQuestion);
                Err(e)
            }
            Err(e) => Err(e),
        }
    }

    /// Read one line, without its newline.
    ///
    /// `Ok(None)` is the end of the stream: the daemon closed it between
    /// two answers. Everything else is an error with a sentence in it —
    /// a timeout, a line cut in the middle, a line that never ends.
    fn read_line(&mut self, deadline: Instant) -> std::io::Result<Option<String>> {
        loop {
            let at = self.transport.pending().iter().position(|&b| b == b'\n');
            if let Some(at) = at {
                let line = self.transport.pending()[..at].to_vec();
                self.transport.consume(at + 1);
                return match String::from_utf8(line) {
                    Ok(line) => Ok(Some(line)),
                    Err(_) => {
                        // The answer is still coming behind this line,
                        // and the connection cannot be read from again.
                        self.broken = Some(Spent::UnreadAnswer);
                        Err(std::io::Error::new(
                            std::io::ErrorKind::InvalidData,
                            "the daemon sent a line that is not text",
                        ))
                    }
                };
            }
            // The line is not whole yet, and it must not grow without a
            // bound: a peer that never sends a newline would make the
            // client allocate until it dies.
            if self.transport.pending().len() > MAX_LINE {
                // The rest of the line is still coming, for the same
                // reason: the connection is spent.
                self.broken = Some(Spent::UnreadAnswer);
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("the daemon sent a line longer than {MAX_LINE} bytes"),
                ));
            }
            match self.transport.fill(deadline)? {
                Filled::Data => continue,
                Filled::Timeout => {
                    // The question went out whole, and the daemon will
                    // answer it after the caller has given up: the answer
                    // is still to come, and it must not be read as the
                    // next question's.
                    self.broken = Some(Spent::LateAnswer);
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::TimedOut,
                        "the daemon did not answer in time",
                    ));
                }
                Filled::Eof => {
                    if self.transport.pending().is_empty() {
                        return Ok(None);
                    }
                    // The stream ended in the middle of a line: the answer
                    // was cut, and what arrived is not an answer.
                    //
                    // The kind is the client's, and not the platform's: the
                    // errno that says "the peer is gone" is the platform's
                    // business — `ECONNRESET` on Linux, `EINVAL` on macOS —
                    // and the outcome a caller reads has to be the same
                    // everywhere. The transport maps both to the end of the
                    // stream, and the reader turns that into this one kind.
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::UnexpectedEof,
                        "the daemon closed the connection in the middle of the answer",
                    ));
                }
            }
        }
    }

    /// Search the index.
    ///
    /// The answers are the hits, the time the daemon took, and the warnings
    /// the query raised — a warning is a query that failed closed, and the
    /// caller is the one that has to say so.
    pub fn search(&mut self, q: &str, limit: usize) -> std::io::Result<SearchOutcome> {
        self.search_within(q, limit, READ_TIMEOUT)
    }

    /// The same, with the window the caller asks for.
    ///
    /// The launcher asks the daemon from the thread that draws the window,
    /// and a window that waits thirty seconds for an answer is a window
    /// that stops answering the keyboard. A caller that lives on that
    /// thread asks for a shorter window, and says so when the answer does
    /// not come.
    pub fn search_within(
        &mut self,
        q: &str,
        limit: usize,
        window: Duration,
    ) -> std::io::Result<SearchOutcome> {
        let session = self.session;
        let req = Request::Search {
            q: q.to_string(),
            limit,
            session,
        };
        let responses = self.request_within(&req, window)?;
        let mut out = SearchOutcome::default();
        // Whether a result frame has been read yet. The flag of the answer
        // is the AND of the flags of the frames: a frame that says "this
        // part is whole" cannot make the whole answer whole on its own.
        let mut whole = true;
        for r in responses {
            match r {
                Response::Hits {
                    hits,
                    took_us,
                    complete,
                    session,
                } => {
                    // The entries ACCUMULATE. One result frame per answer is
                    // the rule, and a peer that sends two does not lose the
                    // first one on the way in: the count that ends the
                    // answer says how big the answer is, and the reader
                    // compares it with what arrived.
                    out.hits.extend(hits);
                    out.took_us = took_us;
                    whole &= complete;
                    out.complete = whole;
                    // The session the answer was computed for. The client
                    // keeps it and sends it back with the next question,
                    // and the daemon re-ranks this answer instead of
                    // walking the index again: that is the whole point of
                    // the session, and it only works if the client knows
                    // which session it is in.
                    //
                    // A daemon that does not say keeps the session the
                    // client had: the answer is read as it was read
                    // before the field existed.
                    if let Some(id) = session {
                        self.session = Some(id);
                    }
                    out.session = session;
                }
                Response::Warning { message } => out.warnings.push(message),
                Response::Error { message } => return Err(std::io::Error::other(message)),
                _ => {}
            }
        }
        Ok(out)
    }

    /// The daemon's status: how many entries the live index holds, the event
    /// id it has caught up to, and whether it caught up by replaying the
    /// journal or by relisting the tree.
    pub fn status(&mut self) -> std::io::Result<DaemonStatus> {
        self.status_within(READ_TIMEOUT)
    }

    /// The same, with the window the caller asks for.
    ///
    /// The launcher asks who the daemon is from the thread that draws the
    /// window, and a daemon that takes the connection and then says
    /// nothing must not keep the window off the screen: the status is
    /// asked in the caller's window, like every other question.
    pub fn status_within(&mut self, window: Duration) -> std::io::Result<DaemonStatus> {
        let responses = self.request_within(&Request::Status, window)?;
        for r in responses {
            if let Response::Status {
                docs,
                event_id,
                catch_up,
                tv,
                feed,
            } = r
            {
                return Ok(DaemonStatus {
                    docs,
                    event_id,
                    catch_up,
                    tv,
                    feed,
                });
            }
            if let Response::Error { message } = r {
                return Err(std::io::Error::other(message));
            }
        }
        Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "the daemon answered the status with nothing that is a status",
        ))
    }

    /// Resolve a path through the daemon's lookup surface.
    ///
    /// The five indexes answer it: the `file_path` and `folder_path` indexes
    /// give the path of the entry, the `file_name` and `folder_name` indexes
    /// the entries that carry a name, and the `folder_backend` index the
    /// volume that owns a folder.
    ///
    /// The op was on the wire before any client could ask for it: the
    /// daemon answered it and the protocol read it, and the one thing that
    /// was missing was the branch that sends the question. This is that
    /// branch.
    pub fn resolve(&mut self, path: &str) -> std::io::Result<Resolved> {
        self.resolve_within(path, READ_TIMEOUT)
    }

    /// The same, with the window the caller asks for.
    pub fn resolve_within(&mut self, path: &str, window: Duration) -> std::io::Result<Resolved> {
        let req = Request::Resolve {
            path: path.to_string(),
        };
        let responses = self.request_within(&req, window)?;
        for r in responses {
            match r {
                Response::Resolved {
                    path,
                    folder,
                    backend,
                } => {
                    return Ok(Resolved {
                        path,
                        folder,
                        backend,
                    });
                }
                Response::Error { message } => return Err(std::io::Error::other(message)),
                _ => {}
            }
        }
        Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "the daemon answered the resolve with nothing that is a resolution",
        ))
    }

    /// Search the contents of the files.
    pub fn grep(&mut self, q: &str, limit: usize) -> std::io::Result<GrepOutcome> {
        self.grep_within(q, limit, READ_TIMEOUT)
    }

    /// The same, with the window the caller asks for.
    ///
    /// The content layer **reads files**: the answer is not the index's, it
    /// is the disk's, and a daemon that reads them slowly would keep the
    /// caller waiting. The launcher lives on the thread that draws the
    /// window, so the window is the launcher's, and the launcher says so
    /// when the answer does not come.
    pub fn grep_within(
        &mut self,
        q: &str,
        limit: usize,
        window: Duration,
    ) -> std::io::Result<GrepOutcome> {
        let req = Request::Grep {
            q: q.to_string(),
            limit,
        };
        let responses = self.request_within(&req, window)?;
        Self::grep_outcome(responses)
    }

    /// Search the contents of the files in `paths`.
    ///
    /// The scope is what makes this cheap: the daemon reads the files it is
    /// given and nothing else, and its content index rules most of them out
    /// before a byte is read.
    pub fn content(
        &mut self,
        q: &str,
        paths: &[String],
        limit: usize,
    ) -> std::io::Result<GrepOutcome> {
        self.content_within(q, paths, limit, READ_TIMEOUT)
    }

    /// The same, with the window the caller asks for.
    ///
    /// The window covers the **whole** answer and not each read of it: a
    /// daemon that sends a file every second and has a hundred files would
    /// keep a caller with a per-read window waiting a hundred times longer
    /// than the window it asked for.
    pub fn content_within(
        &mut self,
        q: &str,
        paths: &[String],
        limit: usize,
        window: Duration,
    ) -> std::io::Result<GrepOutcome> {
        // No session on the content request: the content route has none.
        // The name session holds the name hits, and the content answer is
        // the files on the disk — there is no result set to re-rank, and a
        // session id on this request would be a promise the daemon cannot
        // keep. The scope is the caller's and the scope is already here.
        let req = Request::Content {
            q: q.to_string(),
            paths: paths.to_vec(),
            limit,
        };
        let responses = self.request_within(&req, window)?;
        Self::grep_outcome(responses)
    }

    /// The content answer, out of the responses the daemon sent.
    ///
    /// One shape for the two searches: the whole tree and the scope are the
    /// same answer with a different scope, and a caller that reads them
    /// two ways would have two places to keep in step.
    fn grep_outcome(responses: Vec<Response>) -> std::io::Result<GrepOutcome> {
        let mut out = GrepOutcome::default();
        // Whether a result frame has been read yet. The flag of the answer
        // is the AND of the flags of the frames: a frame that says "this
        // part is whole" cannot make the whole answer whole on its own.
        let mut whole = true;
        for r in responses {
            match r {
                Response::GrepFiles {
                    files,
                    took_us,
                    complete,
                    skipped,
                    skips,
                } => {
                    // The entries ACCUMULATE, for the same reason the name
                    // answer's do: a peer that sends two result frames does
                    // not lose the first one on the way in, and the count
                    // that ends the answer says how big the answer is.
                    out.files.extend(files);
                    out.took_us = took_us;
                    whole &= complete;
                    out.complete = whole;
                    out.skipped += skipped;
                    out.skips.extend(skips);
                }
                Response::Warning { message } => out.warnings.push(message),
                Response::Error { message } => return Err(std::io::Error::other(message)),
                _ => {}
            }
        }
        Ok(out)
    }
}

/// What a search answered.
#[derive(Debug, Clone, Default)]
pub struct SearchOutcome {
    pub hits: Vec<Hit>,
    pub took_us: u64,
    pub warnings: Vec<String>,
    /// Whether the answer is whole.
    ///
    /// The answer is `false` until a daemon says otherwise: a search that
    /// does not say it is whole did not say it is whole.
    pub complete: bool,
    /// The session the answer belongs to.
    ///
    /// The daemon names the session when the request does not, and the
    /// client sends it back with the next question: that is what makes the
    /// next keystroke a re-rank of this answer instead of a walk of the
    /// index. `None` is an answer from a daemon that does not say — the
    /// client keeps the session it had.
    pub session: Option<u64>,
}

impl SearchOutcome {
    /// The sentence for an answer that is not whole.
    ///
    /// A name answer that stopped at the limit is a page of a longer list,
    /// and the client that shows it has to say so. It is the sentence the
    /// content path says — the two layers cut their answers at the same
    /// place, and two clients that read the same daemon have to say the
    /// same words about the same answer.
    pub fn partial(&self) -> Option<String> {
        if self.complete {
            return None;
        }
        Some(ANSWER_STOPS_AT_THE_LIMIT.to_string())
    }
}

/// What a path resolved to.
#[derive(Debug, Clone, PartialEq)]
pub struct Resolved {
    /// The path, as the daemon spells it.
    pub path: String,
    /// Whether it is a folder.
    ///
    /// The index knows and the path does not say: a folder's path is
    /// spelled like a file's.
    pub folder: bool,
    /// The volume that owns it.
    ///
    /// The `folder_backend` index's own id: a file has none of its own and
    /// inherits the folder's, and `0` is the volume the index does not
    /// name.
    pub backend: u32,
}

/// What a content search answered.
#[derive(Debug, Clone, Default)]
pub struct GrepOutcome {
    pub files: Vec<crate::GrepFile>,
    pub took_us: u64,
    pub warnings: Vec<String>,
    /// Whether the search read every candidate it was given.
    ///
    /// The answer is `false` until a daemon says otherwise: a search
    /// that does not say it read everything did not say it read
    /// everything.
    pub complete: bool,
    /// How many files the search did not read.
    ///
    /// The count is what the protocol has always carried, and it stays:
    /// a reader that does not know the reasons keeps working.
    pub skipped: usize,
    /// Why the search did not read them.
    ///
    /// The reasons are what make the count worth reading: a file that was
    /// not read because it is a cloud placeholder is a file the user can
    /// ask for, and a file the budget never reached is a file the user has
    /// to ask for again. Their counts add up to [`GrepOutcome::skipped`] —
    /// and the reader guarantees it: the count it reports is the count the
    /// reasons make, so the sentence above and the sentences below are the
    /// same number said twice.
    pub skips: Vec<crate::Skip>,
}

impl GrepOutcome {
    /// The sentence for an answer that did not read everything.
    ///
    /// A search that did not read everything did not say it read
    /// everything, and an answer that does not say so is an answer that
    /// looks whole. The sentence lives with the answer: two clients that
    /// read the same daemon have to say the same thing about the same
    /// answer, and a client that says nothing says the answer is whole.
    pub fn partial(&self) -> Option<String> {
        if self.complete {
            return None;
        }
        if self.skipped > 0 {
            Some(format!(
                "the daemon did not read {} file(s): the answer is partial",
                self.skipped
            ))
        } else {
            // The daemon read what it was asked to read and the answer is
            // still not whole: the limit cut it, and a limit is the
            // caller's own. The sentence says the limit stopped the
            // answer, because that is what happened -- and it does not
            // say how many files are behind it, because the daemon does
            // not count the ones the limit left behind.
            Some(ANSWER_STOPS_AT_THE_LIMIT.to_string())
        }
    }

    /// The sentences about the files the search did not read.
    ///
    /// One sentence per reason, with the count and — when the daemon knows
    /// which file it is — one of the files. The count the answer carries
    /// says how many; these say why, and a reader that shows only the
    /// count leaves the user guessing whether the file was empty, locked,
    /// or never there.
    ///
    /// Nothing when the daemon did not say: the reasons are additive, and
    /// a daemon that does not say them is read as a daemon that does not
    /// have them.
    pub fn skip_reasons(&self) -> Vec<String> {
        self.skips
            .iter()
            .map(|s| {
                let file = match &s.path {
                    Some(p) => format!(" (first: {p})"),
                    None => String::new(),
                };
                format!(
                    "the daemon did not read {} file(s): {}{file}",
                    s.count, s.reason
                )
            })
            .collect()
    }
}

/// What the daemon says about itself.
#[derive(Debug, Clone)]
pub struct DaemonStatus {
    /// How many entries the daemon's live index holds.
    ///
    /// The daemon fills it — the status route counts the base index, minus
    /// the entries that died, plus the ones the overlay holds — and the
    /// client reads it here.
    ///
    /// The terminal launcher draws the count in its header, and the words
    /// that carry it are the launcher's (`crates/launcher/src/text.rs`): a
    /// sentence invented here would be English-only in a launcher that
    /// speaks two languages. The fact is read and kept here so that the
    /// client that grows the word has it, and the number is on the wire
    /// for whoever asks for it.
    ///
    /// The number is never zero that the daemon did not say: the reader
    /// refuses a status line without the count, because zero is a claim
    /// about an index that plainly has entries, and the launcher has its
    /// own "not known" for a count it was not given.
    pub docs: usize,
    /// The journal position the daemon's index is current as of.
    ///
    /// The daemon resumes the journal at this position when it restarts,
    /// and the position is what says where the index stopped: a reader
    /// that wants to know how fresh the index is compares it with the
    /// journal's own head.
    ///
    /// **No client shows it yet**, for the same reason as [`DaemonStatus::docs`]:
    /// the window has no word for a journal position.
    pub event_id: u64,
    /// `replayed` or `relisted` — the two are different facts about the same
    /// index and the daemon is honest about which one it is.
    ///
    /// The daemon fills it, and the client reads it here.
    ///
    /// **No client shows it yet**, for the same reason as
    /// [`DaemonStatus::docs`]: the window's vocabulary has no word for how
    /// the index caught up, and the feed — which has one — is a different
    /// fact. The fact is read and kept here so that the client that grows
    /// the word has it.
    ///
    /// It is the protocol's own vocabulary and not a string: the wire
    /// spells it, and a reader that compared strings would have to spell
    /// the spelling again — and a spelling that drifts is a warning that
    /// stops being said.
    pub catch_up: CatchUp,
    /// Whether the daemon has a pack store (`--tv-dir`) — the corpus packed
    /// once and mmapped, which is the fastest content answer there is.
    pub tv: bool,
    /// Whether the tree is still being followed.
    ///
    /// A feed that died leaves the daemon answering on a snapshot, and the
    /// reader has to know which of the two it is looking at.
    ///
    /// It is the protocol's own vocabulary and not a string, for the same
    /// reason as [`DaemonStatus::catch_up`].
    pub feed: Feed,
}

/// Whether this error is the daemon's own answer.
///
/// The daemon reports a failure with an `{"op":"error"}` frame, and the
/// client carries that message as an `io::Error` of the kind `Other`: a
/// message that came from the peer, and not from the pipe. A caller that
/// has to tell "the daemon said no" from "the daemon went away" asks
/// here — the exit code of a CLI is one of those two things, and the two
/// are not the same.
///
/// The kind is the distinction because it is the only one there is: the
/// frame and the transport are both failures of the request, and only the
/// frame carries the daemon's own words.
pub fn is_the_daemon_saying_no(e: &std::io::Error) -> bool {
    e.kind() == std::io::ErrorKind::Other
}

/// The response that ends the answer to `req`.
///
/// The daemon answers a search with the hits and a `done`, and a status
/// with the status alone: the client knows what it asked, so it knows what
/// ends the answer. The `done` always ends it — it is the terminator the
/// daemon writes — and an error always ends it, because the request
/// failed. The answer of a request that never ends is a timeout, not a
/// guess.
fn ends_the_answer(req: &Request, resp: &Response) -> bool {
    match resp {
        Response::Done { .. } | Response::Error { .. } => true,
        Response::Status { .. } => matches!(req, Request::Status),
        Response::Resolved { .. } => matches!(req, Request::Resolve { .. }),
        Response::Hits { .. } | Response::GrepFiles { .. } | Response::Warning { .. } => false,
    }
}

/// The count that ends the answer, against the entries that arrived.
///
/// The terminator says how many entries the answer carried, and the reader
/// counts what it got: the two are the same number, and the reader compares
/// them. The frame whose whole job is to say how big the answer was was
/// otherwise decoded and thrown away, and an answer that lost a frame in
/// the middle looks exactly like one that never lost anything.
///
/// The kind of the error is the data's: the daemon is there and said
/// something that is not true, and a caller that reads the kind reads the
/// same thing on every platform.
///
/// An answer that ends without a terminator is not this check's business:
/// the status route ends at its own frame, and the reader that asked for it
/// is reading a different thing.
fn the_count_agrees(responses: &[Response]) -> std::io::Result<()> {
    let Some(said) = responses.iter().find_map(|r| match r {
        Response::Done { count } => Some(*count),
        _ => None,
    }) else {
        return Ok(());
    };
    let arrived: usize = responses
        .iter()
        .map(|r| match r {
            Response::Hits { hits, .. } => hits.len(),
            Response::GrepFiles { files, .. } => files.len(),
            _ => 0,
        })
        .sum();
    if said == arrived {
        return Ok(());
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::InvalidData,
        format!("the answer says it carries {said} entries and {arrived} arrived"),
    ))
}

/// The sentence a line the client cannot read gets.
///
/// The three are different facts: a line that is not JSON is a broken
/// daemon, an answer the client does not know is a daemon that speaks
/// another protocol, and a document that nests too deep is a daemon that
/// sends something no reader should follow.
fn unreadable_line(e: LineError) -> std::io::Error {
    let message = match e {
        LineError::NotJson => "the daemon sent a line this client cannot read",
        LineError::NotAnAnswer => "the daemon sent an answer this client does not know",
        LineError::TooDeep => "the daemon sent a line that nests too deep to read",
    };
    std::io::Error::new(std::io::ErrorKind::InvalidData, message)
}

/// Why a connection was spent, and what the refusal of the next question
/// says about it.
///
/// The three are three facts, and the sentence and the kind are the
/// fact's: a write that ran out is a daemon that is there and did not
/// take the question, a read that ran out is a daemon that is there and
/// did not answer, and an answer that could not be read is a daemon that
/// answered badly. A caller that reads the kind says one of those three
/// and not a fourth thing that happened to none of them.
enum Spent {
    /// The question went out half: the write ran out of time.
    HalfQuestion,
    /// The answer did not come in the window: the question went out
    /// whole, and the daemon may still be writing the answer to it.
    LateAnswer,
    /// The answer held a line the reader refuses, and the rest of it is
    /// still behind that line.
    UnreadAnswer,
}

impl Spent {
    /// The error a question asked into a spent connection gets.
    fn refusal(&self) -> std::io::Error {
        match self {
            Spent::HalfQuestion => std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "the daemon did not take the question in time, and the connection cannot be used again",
            ),
            Spent::LateAnswer => std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "the daemon did not answer in time, and the connection cannot be used again",
            ),
            Spent::UnreadAnswer => std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "the daemon's answer could not be read, and the connection cannot be used again",
            ),
        }
    }
}

// --- the transports --------------------------------------------------------

/// What a read found.
enum Filled {
    /// Bytes arrived.
    Data,
    /// The window closed with nothing to read.
    Timeout,
    /// The other end closed the stream.
    Eof,
}

/// The bytes under the client.
///
/// The transport owns the bytes because the transport is what knows how
/// long to wait for them: a reader that cannot say "not yet" cannot be
/// the one that gives up.
trait Transport: Send {
    /// Read more bytes, waiting at most until `deadline`.
    fn fill(&mut self, deadline: Instant) -> std::io::Result<Filled>;
    /// The bytes read and not yet consumed.
    fn pending(&self) -> &[u8];
    /// Drop the first `n` bytes.
    fn consume(&mut self, n: usize);
}

/// The bytes a transport read, and the client has not consumed yet.
#[derive(Default)]
struct Pending {
    buf: Vec<u8>,
    /// Where the unconsumed bytes start.
    start: usize,
}

impl Pending {
    /// The bytes that were read and not consumed.
    fn bytes(&self) -> &[u8] {
        &self.buf[self.start..]
    }

    /// Add the bytes that were read.
    fn push(&mut self, bytes: &[u8]) {
        self.buf.extend_from_slice(bytes);
    }

    /// Drop the first `n` bytes.
    fn consume(&mut self, n: usize) {
        self.start += n;
        if self.start == self.buf.len() {
            // Nothing is left: the buffer starts over.
            self.buf.clear();
            self.start = 0;
        } else if self.start >= 64 * 1024 {
            // The dead part is worth more than the live one: the buffer is
            // compacted instead of growing for the rest of the answer.
            self.buf.drain(..self.start);
            self.start = 0;
        }
    }
}

/// The unix socket under the client.
///
/// The socket has a read timeout of its own, and the deadline is that
/// timeout: the read waits for what is left of the window and says that
/// nothing arrived when the window is over.
#[cfg(unix)]
struct SocketTransport {
    stream: std::os::unix::net::UnixStream,
    pending: Pending,
}

/// `EINVAL`, the errno macOS gives when the peer is gone.
///
/// The number is 22 on Linux and on the BSDs as well: `EINVAL` is the
/// same number on every platform this tree runs on.
#[cfg(unix)]
const EINVAL: i32 = 22;

/// The platform said the peer is gone, in the platform's own words.
///
/// macOS says it with `EINVAL`, and it says it from two places: from
/// `setsockopt` on a socket whose peer has already disconnected — the
/// option is an invalid argument on a dead socket there — and from a
/// read on a socket whose peer closed with bytes still in flight.
/// Linux says the second thing with `ECONNRESET`, which Rust already
/// names `ConnectionReset`.
///
/// The errno is the platform's and the kind is Rust's: `EINVAL` is 22
/// everywhere this tree runs, and Rust names it `InvalidInput`.
#[cfg(unix)]
fn the_peer_is_gone(e: &std::io::Error) -> bool {
    e.kind() == std::io::ErrorKind::InvalidInput && e.raw_os_error() == Some(EINVAL)
}

/// The socket cannot take the question: the peer is gone.
///
/// The write says it with `EPIPE` on every platform, and macOS says it
/// with `EINVAL` as well, where the socket options were refused before
/// the write was even tried.
#[cfg(unix)]
fn the_connection_is_closed(e: &std::io::Error) -> bool {
    e.kind() == std::io::ErrorKind::BrokenPipe || the_peer_is_gone(e)
}

#[cfg(unix)]
impl SocketTransport {
    /// The transport over an open socket.
    fn new(stream: std::os::unix::net::UnixStream) -> SocketTransport {
        SocketTransport {
            stream,
            pending: Pending::default(),
        }
    }
}

#[cfg(unix)]
impl Transport for SocketTransport {
    fn fill(&mut self, deadline: Instant) -> std::io::Result<Filled> {
        let now = Instant::now();
        if now >= deadline {
            return Ok(Filled::Timeout);
        }
        let left = (deadline - now).max(Duration::from_millis(1));
        // The socket's own timeout is the rest of the window, rounded up
        // to the millisecond: a timeout of zero is not "no time left" on a
        // socket, it is "wait forever".
        //
        // macOS refuses the option on a socket whose peer has already
        // gone: `setsockopt` on a dead socket is an invalid argument
        // there. The refusal is not the end of the stream — the bytes
        // that arrived before the peer went away are still in the
        // buffer, and the read below is the one that has them — so the
        // read goes on, and it goes on without a timeout of its own. A
        // socket that is dead answers at once: what is left in the
        // buffer, or the end of the stream.
        match self.stream.set_read_timeout(Some(left)) {
            Ok(()) => {}
            Err(e) if the_peer_is_gone(&e) => {}
            Err(e) => return Err(e),
        }
        let mut chunk = [0u8; CHUNK];
        match self.stream.read(&mut chunk) {
            Ok(0) => Ok(Filled::Eof),
            Ok(n) => {
                self.pending.push(&chunk[..n]);
                Ok(Filled::Data)
            }
            // The socket's timeout fired: the window is over.
            Err(e)
                if e.kind() == std::io::ErrorKind::WouldBlock
                    || e.kind() == std::io::ErrorKind::TimedOut =>
            {
                Ok(Filled::Timeout)
            }
            // The other end went away with bytes still in flight. That is
            // the same fact as the end of the stream — the peer is gone —
            // and the reader decides what it means: an answer that
            // stopped in the middle, or a clean end between two answers.
            //
            // A socket that is reset says so with its own errno, and the
            // errno is the platform's business, not the client's: the
            // sentence the caller reads has to be the same everywhere.
            //
            // The two errnos are the platform's, and they are the two
            // names for the same fact:
            //
            // * `ECONNRESET` on Linux, when the peer reset the socket
            //   while bytes were still in flight;
            // * `EINVAL` on macOS, where a read on a socket whose peer
            //   closed with data still in the receive buffer is an invalid
            //   argument.
            //
            // Both mean the peer is gone, and both are the end of the
            // stream: the reader is the one that decides whether that was
            // a cut answer or a clean end, and the caller reads one error
            // on every platform.
            Err(e) if e.kind() == std::io::ErrorKind::ConnectionReset => Ok(Filled::Eof),
            Err(e) if the_peer_is_gone(&e) => Ok(Filled::Eof),
            Err(e) => Err(e),
        }
    }

    fn pending(&self) -> &[u8] {
        self.pending.bytes()
    }

    fn consume(&mut self, n: usize) {
        self.pending.consume(n)
    }
}

/// The unix socket, and the half that writes to it.
#[cfg(unix)]
fn socket_transport(
    path: &std::path::Path,
) -> std::io::Result<(Box<dyn Transport + Send>, Box<dyn Writer + Send>)> {
    // The path the caller asked for is not always a path the platform can
    // carry: `sun_path` is 104 bytes on macOS and 108 on Linux, and a
    // deeper cache directory is a client that never reaches the daemon.
    // The rule that shortens it lives in `endpoint`, because the daemon
    // shortens the same path the same way — and a client that looked
    // somewhere else than the daemon bound would wait for a socket that is
    // not there.
    let path = crate::endpoint::socket_path(path)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e.to_string()))?;
    let stream = std::os::unix::net::UnixStream::connect(&path)?;
    let write = stream.try_clone()?;
    Ok((
        Box::new(SocketTransport::new(stream)),
        Box::new(SocketWriter::new(write)),
    ))
}

/// The unix socket is not a transport everywhere.
#[cfg(not(unix))]
fn socket_transport(
    path: &std::path::Path,
) -> std::io::Result<(Box<dyn Transport + Send>, Box<dyn Writer + Send>)> {
    let _ = path;
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "unix sockets are not available on this platform; use a named pipe",
    ))
}

/// The Windows named pipe under the client.
///
/// A blocking read on a named pipe cannot time out: the pipe has no read
/// timeout, and `SetNamedPipeHandleState` does not give it one. The way
/// out is to look before reading — `PeekNamedPipe` says how many bytes are
/// waiting without taking them, and it answers right away even when there
/// are none — and to read only what is there.
#[cfg(windows)]
struct PipeTransport {
    file: std::fs::File,
    pending: Pending,
}

#[cfg(windows)]
impl PipeTransport {
    /// The transport over an open pipe.
    fn new(file: std::fs::File) -> PipeTransport {
        PipeTransport {
            file,
            pending: Pending::default(),
        }
    }
}

#[cfg(windows)]
impl Transport for PipeTransport {
    fn fill(&mut self, deadline: Instant) -> std::io::Result<Filled> {
        loop {
            match peek(&self.file)? {
                Peek::Bytes(waiting) => {
                    let mut chunk = [0u8; CHUNK];
                    let want = waiting.min(chunk.len());
                    let got = self.file.read(&mut chunk[..want])?;
                    if got == 0 {
                        return Ok(Filled::Eof);
                    }
                    self.pending.push(&chunk[..got]);
                    return Ok(Filled::Data);
                }
                Peek::Closed => return Ok(Filled::Eof),
                Peek::None => {
                    let now = Instant::now();
                    if now >= deadline {
                        return Ok(Filled::Timeout);
                    }
                    // Look again in a moment: the peek is what turns a
                    // blocking pipe into a deadline.
                    std::thread::sleep((deadline - now).min(Duration::from_millis(5)));
                }
            }
        }
    }

    fn pending(&self) -> &[u8] {
        self.pending.bytes()
    }

    fn consume(&mut self, n: usize) {
        self.pending.consume(n)
    }
}

/// The named pipe, and the half that writes to it.
///
/// A pipe that nobody reads fills up, and the write that fills it waits,
/// and the wait has no timeout: a pipe opened without
/// `FILE_FLAG_OVERLAPPED` cannot be given one. The write goes on a thread
/// of its own, and the thread is stopped when the client gives up on it.
///
/// The waiting for a busy pipe is bounded by the caller's deadline, when
/// there is one.
#[cfg(windows)]
fn pipe_transport_within(
    name: &str,
    deadline: Option<Instant>,
) -> std::io::Result<(Box<dyn Transport + Send>, Box<dyn Writer + Send>)> {
    let file = open_pipe_within(name, deadline)?;
    let write = file.try_clone()?;
    Ok((
        Box::new(PipeTransport::new(file)),
        Box::new(ThreadWriter::new(Box::new(write))),
    ))
}

/// The named pipe is not a transport everywhere.
#[cfg(not(windows))]
fn pipe_transport_within(
    name: &str,
    _deadline: Option<Instant>,
) -> std::io::Result<(Box<dyn Transport + Send>, Box<dyn Writer + Send>)> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        format!("named pipes are not available on this platform: {name}"),
    ))
}

/// What the pipe has waiting.
#[cfg(windows)]
enum Peek {
    /// This many bytes are waiting.
    Bytes(usize),
    /// The pipe is empty.
    None,
    /// The other end closed the pipe.
    Closed,
}

/// The bytes waiting in the pipe, without taking them.
///
/// `PeekNamedPipe` answers right away — blocking or not — and it says how
/// many bytes are there without removing them. The one-byte buffer is the
/// price of the answer: the count is what the client wants, and the byte
/// the peek copies is still in the pipe afterwards.
#[cfg(windows)]
fn peek(file: &std::fs::File) -> std::io::Result<Peek> {
    use std::os::windows::io::AsRawHandle;
    unsafe extern "system" {
        fn PeekNamedPipe(
            pipe: *mut core::ffi::c_void,
            buffer: *mut core::ffi::c_void,
            size: u32,
            read: *mut u32,
            total: *mut u32,
            left: *mut u32,
        ) -> i32;
    }
    /// The other end closed the pipe.
    const ERROR_BROKEN_PIPE: i32 = 109;
    /// The pipe has nobody on the other end.
    const ERROR_PIPE_NOT_CONNECTED: i32 = 233;
    /// The message in the pipe is bigger than the buffer.
    const ERROR_MORE_DATA: i32 = 234;

    let mut one = [0u8; 1];
    let mut total = 0u32;
    let ok = unsafe {
        PeekNamedPipe(
            file.as_raw_handle(),
            one.as_mut_ptr() as *mut core::ffi::c_void,
            one.len() as u32,
            core::ptr::null_mut(),
            &mut total,
            core::ptr::null_mut(),
        )
    };
    if ok == 0 {
        let err = std::io::Error::last_os_error();
        return match err.raw_os_error() {
            Some(ERROR_BROKEN_PIPE) | Some(ERROR_PIPE_NOT_CONNECTED) => Ok(Peek::Closed),
            // A message bigger than the buffer is still a message that is
            // there: the count is unknown, but the bytes are not.
            Some(ERROR_MORE_DATA) => Ok(Peek::Bytes(1)),
            _ => Err(err),
        };
    }
    Ok(if total == 0 {
        Peek::None
    } else {
        Peek::Bytes(total as usize)
    })
}

/// The daemon's own standard input.
///
/// The standard input cannot say whether it has bytes: a console is not a
/// pipe and a file is not a socket, and there is no `PeekNamedPipe` for
/// either. The thread is what turns "nothing yet" into something the
/// deadline can measure — the bytes arrive on their own, and the client
/// waits for them with a clock.
struct StdioTransport {
    /// The bytes the thread read.
    rx: std::sync::mpsc::Receiver<std::io::Result<Vec<u8>>>,
    pending: Pending,
}

impl StdioTransport {
    /// The transport over the standard input.
    ///
    /// The thread reads the standard input and hands the bytes over. It
    /// stops when the input is over, or when the client is gone: a send
    /// that fails means nobody is listening any more.
    fn new() -> StdioTransport {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut stdin = std::io::stdin();
            let mut chunk = vec![0u8; CHUNK];
            loop {
                match stdin.read(&mut chunk) {
                    Ok(0) => break,
                    Ok(n) => {
                        if tx.send(Ok(chunk[..n].to_vec())).is_err() {
                            break;
                        }
                    }
                    Err(e) => {
                        let _ = tx.send(Err(e));
                        break;
                    }
                }
            }
        });
        StdioTransport {
            rx,
            pending: Pending::default(),
        }
    }
}

impl Transport for StdioTransport {
    fn fill(&mut self, deadline: Instant) -> std::io::Result<Filled> {
        let left = deadline.saturating_duration_since(Instant::now());
        match self.rx.recv_timeout(left) {
            Ok(Ok(bytes)) => {
                self.pending.push(&bytes);
                Ok(Filled::Data)
            }
            Ok(Err(e)) => Err(e),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => Ok(Filled::Timeout),
            // The thread is gone and the standard input is over.
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => Ok(Filled::Eof),
        }
    }

    fn pending(&self) -> &[u8] {
        self.pending.bytes()
    }

    fn consume(&mut self, n: usize) {
        self.pending.consume(n)
    }
}

/// Open a Windows named pipe, waiting out the busy case.
///
/// `ERROR_PIPE_BUSY` means every instance of the pipe has a client on it.
/// The daemon keeps a pool of acceptors precisely so this is rare, and a
/// client that waited a moment is more useful than one that failed.
///
/// The waiting is bounded by the caller's deadline, when there is one: the
/// wait for a busy pipe is a wait like any other, and the caller's window
/// is the budget of the whole answer. A deadline that passes while the pipe
/// is busy is a timeout, and a timeout says the daemon is there and slow --
/// which is what it is.
#[cfg(windows)]
fn open_pipe_within(name: &str, deadline: Option<Instant>) -> std::io::Result<std::fs::File> {
    use std::fs::OpenOptions;
    const ERROR_PIPE_BUSY: i32 = 231;
    let path = format!(r"\\.\pipe\{name}");
    let mut attempts = 0;
    loop {
        match OpenOptions::new().read(true).write(true).open(&path) {
            Ok(f) => return Ok(f),
            Err(e) => {
                if e.raw_os_error() != Some(ERROR_PIPE_BUSY) {
                    return Err(e);
                }
                match deadline {
                    // The wait ends when the caller's window does, and it
                    // says so: a pipe that stayed busy until the window ran
                    // out is a daemon that is there and slow, and the caller
                    // has to be able to tell the two apart.
                    Some(deadline) => {
                        if Instant::now() >= deadline {
                            return Err(std::io::Error::new(
                                std::io::ErrorKind::TimedOut,
                                "the named pipe stayed busy until the window ran out",
                            ));
                        }
                    }
                    // Without a deadline the wait is the old one: a fixed
                    // number of moments, and then the error the pipe gave.
                    None => {
                        attempts += 1;
                        if attempts >= 80 {
                            return Err(e);
                        }
                    }
                }
                std::thread::sleep(Duration::from_millis(25));
            }
        }
    }
}

// --- the write ----------------------------------------------------------------

/// The half that writes the question to the daemon.
///
/// The write has a deadline of its own, and the reason is the read's: a
/// daemon that takes the connection and then stops reading fills the
/// pipe, and a write that waits for that pipe is a write that never
/// ends. The window the caller asks for is the window the write is
/// measured against.
trait Writer: Send {
    /// The bytes go out, and the call gives up at the deadline.
    ///
    /// A write that runs out of time is `ErrorKind::TimedOut`: the
    /// daemon is there and did not take the question.
    fn write_until(&mut self, bytes: &[u8], deadline: Instant) -> std::io::Result<()>;
}

/// The sentence a write that ran out of time gets.
///
/// It says that the daemon did not take the question, and not that it did
/// not answer: the daemon took the connection — it is there — and the two
/// are different facts. The kind says the same: a timeout is a daemon that
/// is up and slow, and a connection error is a daemon that is gone.
fn write_timeout() -> std::io::Error {
    std::io::Error::new(
        std::io::ErrorKind::TimedOut,
        "the daemon did not take the question in time",
    )
}

/// The sentence a connection that is closed gets.
fn closed_connection() -> std::io::Error {
    std::io::Error::new(
        std::io::ErrorKind::BrokenPipe,
        "the connection to the daemon is closed",
    )
}

/// The unix socket, and the half that writes to it.
///
/// The socket has a write timeout of its own, and the deadline is that
/// timeout: the write waits for what is left of the window and gives up
/// when the window is over — the same shape as the read's.
#[cfg(unix)]
struct SocketWriter {
    stream: std::os::unix::net::UnixStream,
}

#[cfg(unix)]
impl SocketWriter {
    /// The writer over an open socket.
    fn new(stream: std::os::unix::net::UnixStream) -> SocketWriter {
        SocketWriter { stream }
    }
}

#[cfg(unix)]
impl Writer for SocketWriter {
    fn write_until(&mut self, bytes: &[u8], deadline: Instant) -> std::io::Result<()> {
        // The question goes out in pieces, and each piece is given what
        // is left of the window: the socket's own timeout is the rest of
        // it, and the rest is what is left of it. A write that was given
        // the whole window on every piece would take as many windows as
        // it has pieces, and the client would wait longer than it asked
        // for — long enough for the daemon to close the connection under
        // a write that was only late.
        let mut sent = 0;
        while sent < bytes.len() {
            let now = Instant::now();
            if now >= deadline {
                return Err(write_timeout());
            }
            // The timeout is rounded up to the millisecond: a timeout of
            // zero is not "no time left" on a socket, it is "wait
            // forever".
            let left = (deadline - now).max(Duration::from_millis(1));
            // The same refusal macOS makes on the read: `setsockopt` on
            // a socket whose peer has gone is an invalid argument there.
            // The write below is the one that says what happened, so
            // the refusal is not the end of it.
            match self.stream.set_write_timeout(Some(left)) {
                Ok(()) => {}
                Err(e) if the_peer_is_gone(&e) => {}
                Err(e) => return Err(e),
            }
            match self.stream.write(&bytes[sent..]) {
                Ok(0) => return Err(closed_connection()),
                Ok(n) => sent += n,
                // The socket's timeout fired: the daemon is there and is
                // not reading.
                Err(e)
                    if e.kind() == std::io::ErrorKind::WouldBlock
                        || e.kind() == std::io::ErrorKind::TimedOut =>
                {
                    return Err(write_timeout());
                }
                // The write was interrupted and nothing went out: it goes
                // on.
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                // The peer is gone: the socket cannot take the question.
                // That is the product's fact — the daemon is not there —
                // and not the platform's errno that says it, so the
                // sentence a caller reads is the same everywhere.
                Err(e) if the_connection_is_closed(&e) => return Err(closed_connection()),
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }
}

/// The write that goes on a thread of its own.
///
/// A write that blocks cannot be stopped from the thread that made it:
/// the write is inside a syscall, and the thread is inside the write. The
/// write goes somewhere else and the client waits for the word, under the
/// window — and when the window runs out, the write is given up on and the
/// thread ends with the connection.
struct ThreadWriter {
    /// Where the question goes.
    ///
    /// It is gone once the writer gave up: the thread's wait for the next
    /// question ends with it.
    tx: Option<std::sync::mpsc::Sender<Vec<u8>>>,
    /// Where the word about the write comes back.
    rx: std::sync::mpsc::Receiver<std::io::Result<()>>,
    /// The thread that owns the write half.
    thread: Option<std::thread::JoinHandle<()>>,
}

impl ThreadWriter {
    /// The writer over a stream, on a thread of its own.
    fn new(mut stream: Box<dyn Write + Send>) -> ThreadWriter {
        let (tx, rx) = std::sync::mpsc::channel::<Vec<u8>>();
        let (word_tx, word_rx) = std::sync::mpsc::channel::<std::io::Result<()>>();
        let thread = std::thread::spawn(move || {
            while let Ok(bytes) = rx.recv() {
                let written = stream.write_all(&bytes).and_then(|()| stream.flush());
                let failed = written.is_err();
                // The word goes back even when the write failed: the
                // client is waiting for it, and a client that waits for a
                // word that never comes waits for the window instead of
                // the answer.
                if word_tx.send(written).is_err() {
                    break;
                }
                if failed {
                    break;
                }
            }
        });
        ThreadWriter {
            tx: Some(tx),
            rx: word_rx,
            thread: Some(thread),
        }
    }

    /// Give up on a write that is stuck.
    ///
    /// The thread's write is stopped — the platform's way of saying
    /// "stop" — and the channel goes with it, so that the thread's wait
    /// for the next question ends too. A thread that waits for a pipe
    /// nobody reads is a thread that never ends.
    fn give_up(&mut self) {
        stop_the_write(self.thread.as_ref());
        self.tx = None;
    }
}

impl Writer for ThreadWriter {
    fn write_until(&mut self, bytes: &[u8], deadline: Instant) -> std::io::Result<()> {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(write_timeout());
        }
        let Some(tx) = self.tx.as_ref() else {
            return Err(closed_connection());
        };
        if tx.send(bytes.to_vec()).is_err() {
            // The thread is gone: the connection is closed.
            return Err(closed_connection());
        }
        match self.rx.recv_timeout(left) {
            Ok(Ok(())) => Ok(()),
            Ok(Err(e)) => Err(e),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                // The daemon took the connection and is not reading it.
                self.give_up();
                Err(write_timeout())
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => Err(closed_connection()),
        }
    }
}

impl Drop for ThreadWriter {
    fn drop(&mut self) {
        // The connection is dropped: the thread that writes to it ends
        // with it, and a write that is still inside a syscall is stopped
        // so that the thread does not outlive the connection.
        self.give_up();
    }
}

/// Stop a write that is stuck, on a thread of its own.
///
/// Windows has one way of saying "stop": `CancelSynchronousIo` cancels
/// the pending I/O of the thread that issued it, and the write comes
/// back with an error. The write is synchronous — a pipe opened without
/// `FILE_FLAG_OVERLAPPED` — which is the only kind a pipe that is read
/// with a peek can be.
#[cfg(windows)]
fn stop_the_write(thread: Option<&std::thread::JoinHandle<()>>) {
    use std::os::windows::io::AsRawHandle;
    unsafe extern "system" {
        fn CancelSynchronousIo(thread: *mut core::ffi::c_void) -> i32;
    }
    let Some(thread) = thread else {
        return;
    };
    // A thread that is not writing has nothing to cancel, and the
    // platform says so with a failure that is not an error here.
    unsafe { CancelSynchronousIo(thread.as_raw_handle()) };
}

/// Stop a write that is stuck, on a thread of its own.
///
/// A unix socket does not need this: the socket's own write timeout ends
/// the write. A standard output cannot be stopped at all — the platform
/// has no way to interrupt a write that is inside a syscall — and the
/// thread ends when the write does.
#[cfg(not(windows))]
fn stop_the_write(_thread: Option<&std::thread::JoinHandle<()>>) {}

/// The window the client waits for, named so the caller can read it.
pub fn read_timeout() -> Duration {
    READ_TIMEOUT
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pipe_is_not_a_socket() {
        // The two are different transports and the client says which one it
        // cannot use rather than failing later, mid-conversation.
        let e = Client::connect(&Endpoint::Socket("/tmp/rgy-test.sock".into())).err();
        if cfg!(unix) {
            // On unix the socket is the supported one; the error is then the
            // connection failing, not the transport being unsupported.
            assert!(e.is_some(), "a socket that is not there must not connect");
        } else {
            let e = e.expect("a socket on windows is unsupported");
            assert_eq!(e.kind(), std::io::ErrorKind::Unsupported);
        }
    }

    #[test]
    fn the_read_timeout_is_a_window_and_not_a_deadline() {
        assert!(read_timeout() >= Duration::from_secs(1));
    }

    // --- the daemon that does not answer, and the daemon that dies -------

    /// The window the tests give an answer that is not coming.
    ///
    /// It is short because the case under test is the wait, not the
    /// answer: the production window is thirty seconds, and a test that
    /// waited half a minute to prove a timeout would be a test nobody
    /// runs.
    const SHORT: Duration = Duration::from_millis(500);

    /// A name no other test uses.
    fn unique() -> String {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        format!(
            "rgy-proto-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        )
    }

    /// The test's own end of the transport.
    ///
    /// The client speaks a socket on unix and a named pipe on Windows, and
    /// the test speaks the same one: a stand-in that spoke something else
    /// would prove nothing about the transport the client really uses.
    enum Peer {
        #[cfg(unix)]
        Socket(std::os::unix::net::UnixStream),
        #[cfg(windows)]
        Pipe(std::fs::File),
    }

    impl Peer {
        /// Send bytes to the client.
        fn send(&mut self, bytes: &[u8]) -> std::io::Result<()> {
            match self {
                #[cfg(unix)]
                Peer::Socket(s) => s.write_all(bytes),
                #[cfg(windows)]
                Peer::Pipe(f) => f.write_all(bytes),
            }
        }

        /// Read the client's request, one line of it.
        ///
        /// A daemon reads the request before it answers it. A daemon that
        /// answers first and closes the connection can close it under the
        /// client's own write, and the client then fails on the write —
        /// with a broken pipe — instead of on the answer it was reading.
        fn read_request(&mut self) -> std::io::Result<()> {
            let mut byte = [0u8; 1];
            loop {
                let read = match self {
                    #[cfg(unix)]
                    Peer::Socket(s) => s.read(&mut byte)?,
                    #[cfg(windows)]
                    Peer::Pipe(f) => f.read(&mut byte)?,
                };
                if read == 0 {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::UnexpectedEof,
                        "the client closed the connection before the request",
                    ));
                }
                if byte[0] == b'\n' {
                    return Ok(());
                }
            }
        }

        /// Read the client's request, one line of it, and say what it said.
        ///
        /// The line is the question: a test that says what the client asked
        /// has to be able to read it, and the read is the same read
        /// `read_request` does — one byte at a time, until the newline.
        fn read_request_line(&mut self) -> std::io::Result<String> {
            let mut line: Vec<u8> = Vec::new();
            let mut byte = [0u8; 1];
            loop {
                let read = match self {
                    #[cfg(unix)]
                    Peer::Socket(s) => s.read(&mut byte)?,
                    #[cfg(windows)]
                    Peer::Pipe(f) => f.read(&mut byte)?,
                };
                if read == 0 {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::UnexpectedEof,
                        "the client closed the connection before the request",
                    ));
                }
                if byte[0] == b'\n' {
                    return Ok(String::from_utf8_lossy(&line).into_owned());
                }
                line.push(byte[0]);
            }
        }

        /// Read what is left of the connection, and say whether it ended.
        ///
        /// The end is what the client dropping the connection looks like:
        /// the read comes back empty, or with an error, once the bytes
        /// that were on their way are gone.
        fn read_to_the_end(&mut self) -> bool {
            let mut chunk = [0u8; 4096];
            loop {
                let read = match self {
                    #[cfg(unix)]
                    Peer::Socket(s) => s.read(&mut chunk),
                    #[cfg(windows)]
                    Peer::Pipe(f) => f.read(&mut chunk),
                };
                match read {
                    Ok(0) => return true,
                    Ok(_) => continue,
                    Err(_) => return true,
                }
            }
        }
    }

    /// A daemon of the test's own, on an endpoint of the test's own.
    struct Server {
        endpoint: Endpoint,
        thread: std::thread::JoinHandle<()>,
    }

    impl Server {
        /// Wait for the daemon to be done.
        fn join(self) {
            let _ = self.thread.join();
        }
    }

    /// A server that takes one client and does what the test says.
    #[cfg(unix)]
    fn serve(script: impl FnOnce(Peer) + Send + 'static) -> Server {
        let path = std::env::temp_dir().join(format!("{}.sock", unique()));
        let listener = std::os::unix::net::UnixListener::bind(&path).expect("the socket binds");
        let socket = path.clone();
        let thread = std::thread::spawn(move || {
            if let Ok((stream, _)) = listener.accept() {
                script(Peer::Socket(stream));
            }
            let _ = std::fs::remove_file(&socket);
        });
        Server {
            endpoint: Endpoint::Socket(path),
            thread,
        }
    }

    /// A server that takes one client and does what the test says.
    #[cfg(windows)]
    fn serve(script: impl FnOnce(Peer) + Send + 'static) -> Server {
        use std::os::windows::io::AsRawHandle;
        unsafe extern "system" {
            fn ConnectNamedPipe(
                pipe: *mut core::ffi::c_void,
                overlapped: *mut core::ffi::c_void,
            ) -> i32;
        }
        let name = unique();
        let file = create_pipe(&name);
        let thread = std::thread::spawn(move || {
            // The instance is already there: the only thing left to wait
            // for is the client.
            unsafe { ConnectNamedPipe(file.as_raw_handle(), core::ptr::null_mut()) };
            script(Peer::Pipe(file));
        });
        Server {
            endpoint: Endpoint::Pipe(name),
            thread,
        }
    }

    /// The pipe instance the test's server waits on.
    #[cfg(windows)]
    fn create_pipe(name: &str) -> std::fs::File {
        use std::os::windows::io::FromRawHandle;
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
        }
        const PIPE_ACCESS_DUPLEX: u32 = 0x3;
        const PIPE_TYPE_BYTE: u32 = 0x0;
        const PIPE_UNLIMITED_INSTANCES: u32 = 255;
        let path = format!(r"\\.\pipe\{name}");
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
                core::ptr::null_mut(),
            )
        };
        assert!(!handle.is_null(), "the pipe instance was created");
        // The file owns the handle: dropping it closes the pipe, which is
        // what tells the client the stream is over.
        unsafe { std::fs::File::from_raw_handle(handle) }
    }

    /// A daemon that takes the connection and says nothing.
    fn says_nothing(peer: Peer) {
        // The connection stays open: the client is waiting for an answer
        // that is not coming, and that is the case under test.
        let _peer = peer;
        std::thread::sleep(Duration::from_millis(1000));
    }

    /// A daemon that takes the connection and never reads it.
    ///
    /// The pipe fills and the client's write waits for it: this is the
    /// case the write's window is for.
    fn never_reads(peer: Peer) {
        // The connection stays open: a daemon that hung up would break
        // the write instead of filling it, and the client would fail on
        // a broken pipe instead of on the window.
        let _peer = peer;
        std::thread::sleep(Duration::from_millis(1000));
    }

    /// A question big enough to fill the pipe.
    ///
    /// The write under test has to block, and a pipe only blocks when it
    /// is full: a question of a few bytes goes into the buffer and the
    /// client learns nothing. A megabyte of question fills the pipe on
    /// every platform — the socket's buffer and the pipe's buffer are
    /// both smaller than that.
    fn big_request() -> Request {
        Request::Search {
            q: "x".repeat(1024 * 1024),
            limit: 10,
            session: None,
        }
    }

    /// The sentence says which kind of not-whole the answer is.
    ///
    /// Two facts on the wire, two sentences: the files the daemon did not
    /// read are a hole in the search, and an answer that stopped at the
    /// limit is a page of a longer list. A sentence that named "0
    /// file(s)" for the second would be a number that says nothing and
    /// sounds like it does.
    #[test]
    fn the_answer_says_which_partial_it_is() {
        let whole = GrepOutcome {
            complete: true,
            skipped: 0,
            ..Default::default()
        };
        assert!(
            whole.partial().is_none(),
            "a whole answer says nothing about being whole"
        );

        let hole = GrepOutcome {
            complete: false,
            skipped: 3,
            ..Default::default()
        };
        let said = hole.partial().expect("an answer that is not whole says so");
        assert!(said.contains("3 file(s)"), "the count is the count: {said}");
        assert!(
            said.contains("partial"),
            "the answer is called partial: {said}"
        );

        let cut = GrepOutcome {
            complete: false,
            skipped: 0,
            ..Default::default()
        };
        let said = cut.partial().expect("an answer that is not whole says so");
        assert!(
            !said.contains("0 file(s)"),
            "the sentence names no files it did not read: {said}"
        );
        assert!(said.contains("limit"), "the limit is what cut it: {said}");
    }

    /// The name answer says the same words the content answer says.
    ///
    /// The name layer could not say it at all before and the content layer
    /// could: two clients that read the same daemon have to say the same
    /// thing about the same answer, and the two layers cut their answers
    /// at the same place.
    #[test]
    fn a_name_answer_says_the_same_words_the_content_answer_says() {
        let whole = SearchOutcome {
            complete: true,
            ..Default::default()
        };
        assert!(
            whole.partial().is_none(),
            "a whole answer says nothing about being whole"
        );

        let cut = SearchOutcome {
            complete: false,
            ..Default::default()
        };
        let said = cut.partial().expect("an answer that is not whole says so");
        assert!(said.contains("limit"), "the limit is what cut it: {said}");

        // The same words, from the two layers.
        let content = GrepOutcome {
            complete: false,
            skipped: 0,
            ..Default::default()
        };
        assert_eq!(
            cut.partial(),
            content.partial(),
            "the two layers say the same thing about the same answer"
        );
    }

    /// The fact comes off the wire.
    ///
    /// The daemon says whether the answer is whole and the client has to
    /// hear it: an answer that does not say it is whole did not say it is
    /// whole.
    #[test]
    fn a_search_reads_whether_the_answer_is_whole() {
        let server = serve(|mut peer: Peer| {
            let _ = peer.read_request();
            let _ = peer.send(
                b"{\"op\":\"hits\",\"hits\":[{\"path\":\"a\",\"score\":1}],\"took_us\":2,\"complete\":false}\n",
            );
            let _ = peer.send(b"{\"op\":\"done\",\"count\":1}\n");
        });
        let mut client = Client::connect(&server.endpoint).expect("the client connects");
        let out = client.search("a", 10).expect("the daemon answers");
        assert!(
            !out.complete,
            "the answer that was cut at the limit is not whole"
        );
        assert_eq!(
            out.partial().as_deref(),
            Some(ANSWER_STOPS_AT_THE_LIMIT),
            "the client says the words the content path says"
        );
        server.join();
    }

    /// The client keeps the session the answer names.
    ///
    /// The session is the incremental-search id: the daemon keeps the answer
    /// under it, and the next question that carries the same id is answered
    /// from that answer instead of from the index. The client has to send it
    /// back, so the client has to keep it — and the id has to reach the
    /// client, which is why the answer names it.
    #[test]
    fn the_client_keeps_the_session_the_answer_names() {
        let server = serve(|mut peer: Peer| {
            // The first question asks with no session: the client has none
            // to send.
            let first = peer.read_request_line().expect("the request is read");
            assert!(
                !first.contains("session"),
                "the first question carries no session: {first}"
            );
            let _ = peer.send(
                b"{\"op\":\"hits\",\"hits\":[{\"path\":\"a\",\"score\":1}],\"took_us\":2,\"complete\":true,\"session\":7}\n",
            );
            let _ = peer.send(b"{\"op\":\"done\",\"count\":1}\n");
            // The second question carries the session the first answer named.
            let second = peer.read_request_line().expect("the request is read");
            assert!(
                second.contains("\"session\":7"),
                "the second question carries the session: {second}"
            );
            let _ = peer.send(
                b"{\"op\":\"hits\",\"hits\":[{\"path\":\"a\",\"score\":1}],\"took_us\":2,\"complete\":true,\"session\":7}\n",
            );
            let _ = peer.send(b"{\"op\":\"done\",\"count\":1}\n");
        });
        let mut client = Client::connect(&server.endpoint).expect("the client connects");
        assert_eq!(client.session(), None, "the client has no session yet");
        let out = client.search("a", 10).expect("the daemon answers");
        assert_eq!(out.session, Some(7), "the answer names the session");
        assert_eq!(client.session(), Some(7), "the client keeps it");
        let _ = client.search("ab", 10).expect("the daemon answers again");
        server.join();
    }

    /// A daemon that does not name the session leaves the client with the
    /// session it had.
    ///
    /// The field is additive, and a daemon that does not say is a daemon that
    /// does not change anything: the client that had a session keeps it, and
    /// the client that had none has none.
    #[test]
    fn a_daemon_that_does_not_name_the_session_leaves_the_client_alone() {
        let server = serve(|mut peer: Peer| {
            let _ = peer.read_request();
            let _ = peer.send(
                b"{\"op\":\"hits\",\"hits\":[{\"path\":\"a\",\"score\":1}],\"took_us\":2,\"complete\":true}\n",
            );
            let _ = peer.send(b"{\"op\":\"done\",\"count\":1}\n");
        });
        let mut client = Client::connect(&server.endpoint).expect("the client connects");
        client.set_session(Some(3));
        let out = client.search("a", 10).expect("the daemon answers");
        assert_eq!(out.session, None, "the answer names no session");
        assert_eq!(client.session(), Some(3), "the client keeps its own");
        server.join();
    }

    /// A daemon that names a session which is not one leaves the client
    /// with the session it had.
    ///
    /// The rule is the same on both sides of the wire: a session that is
    /// not one is no session at all. The reader used to cast the number,
    /// and the cast made `-1` the session `0` — which is in the client's
    /// own space, the space the daemon trusts — so the client kept it and
    /// sent it back on the next question.
    #[test]
    fn a_daemon_that_names_a_session_which_is_not_one_leaves_the_client_alone() {
        let server = serve(|mut peer: Peer| {
            let _ = peer.read_request();
            let _ = peer.send(
                b"{\"op\":\"hits\",\"hits\":[{\"path\":\"a\",\"score\":1}],\"took_us\":2,\"complete\":true,\"session\":-1}\n",
            );
            let _ = peer.send(b"{\"op\":\"done\",\"count\":1}\n");
        });
        let mut client = Client::connect(&server.endpoint).expect("the client connects");
        client.set_session(Some(3));
        let out = client.search("a", 10).expect("the daemon answers");
        assert_eq!(out.session, None, "the answer names no session");
        assert_eq!(client.session(), Some(3), "the client keeps its own");
        server.join();
    }

    /// The count and the reasons are the same number.
    ///
    /// The count and the reasons are one fact written twice, and the
    /// protocol says the reasons add up to the count — so the reader reads
    /// the count the reasons make. A daemon whose own two numbers disagree
    /// does not make the client say two different things about the same
    /// answer.
    #[test]
    fn the_count_and_the_reasons_are_the_same_number() {
        let server = serve(|mut peer: Peer| {
            let _ = peer.read_request();
            let _ = peer.send(
                b"{\"op\":\"grep-files\",\"files\":[],\"took_us\":3,\"complete\":false,\"skipped\":9,\"skips\":[{\"reason\":\"a cloud placeholder\",\"count\":2},{\"reason\":\"the search's budget ran out before it reached them\",\"count\":1}]}\n",
            );
            let _ = peer.send(b"{\"op\":\"done\",\"count\":0}\n");
        });
        let mut client = Client::connect(&server.endpoint).expect("the client connects");
        let outcome = client.grep("a", 10).expect("the daemon answers");
        assert_eq!(
            outcome.skipped, 3,
            "the count is the count the reasons make"
        );
        assert_eq!(
            outcome.skips.iter().map(|s| s.count).sum::<usize>(),
            outcome.skipped,
            "the reasons add up to the count"
        );
        let said = outcome.partial().expect("the answer is not whole");
        assert!(
            said.contains("3 file(s)"),
            "the sentence names the count: {said}"
        );
        server.join();
    }

    /// The client says why the files were not read.
    ///
    /// The count is what the protocol has always carried, and the reasons
    /// are what make it worth reading: "skipped" alone leaves the user
    /// wondering whether the file was empty, locked, or never there.
    #[test]
    fn the_client_says_why_the_files_were_not_read() {
        let server = serve(|mut peer: Peer| {
            let _ = peer.read_request();
            let _ = peer.send(
                b"{\"op\":\"grep-files\",\"files\":[],\"took_us\":3,\"complete\":false,\"skipped\":3,\"skips\":[{\"reason\":\"a cloud placeholder\",\"count\":2,\"path\":\"C:/a.txt\"},{\"reason\":\"the search's budget ran out before it reached them\",\"count\":1}]}\n",
            );
            let _ = peer.send(b"{\"op\":\"done\",\"count\":0}\n");
        });
        let mut client = Client::connect(&server.endpoint).expect("the client connects");
        let out = client.grep("hello", 10).expect("the daemon answers");
        assert_eq!(out.skipped, 3, "the count is the count");
        assert_eq!(out.skips.len(), 2, "the reasons are the reasons");
        let said = out.skip_reasons();
        assert_eq!(said.len(), 2, "one sentence per reason: {said:?}");
        assert!(
            said[0].contains("a cloud placeholder") && said[0].contains("2 file(s)"),
            "the reason and the count: {said:?}"
        );
        assert!(
            said[0].contains("C:/a.txt"),
            "the file the daemon named: {said:?}"
        );
        assert!(
            said[1].contains("budget") && said[1].contains("1 file(s)"),
            "the reason and the count: {said:?}"
        );
        server.join();
    }

    /// The daemon's own refusal is told apart from a broken pipe.
    ///
    /// The daemon reports a failure with an `{"op":"error"}` frame, and the
    /// client carries the message as an error. A pipe that broke is a
    /// failure too, and the two are not the same: the exit code of a CLI
    /// is one of them or the other, and the code that says "the daemon
    /// reported an error" must not be given for a pipe that died.
    #[test]
    fn the_daemons_own_refusal_is_told_apart_from_a_broken_pipe() {
        let server = serve(|mut peer: Peer| {
            let _ = peer.read_request();
            let _ =
                peer.send(b"{\"op\":\"error\",\"message\":\"the pattern `[` does not compile\"}\n");
        });
        let mut client = Client::connect(&server.endpoint).expect("the client connects");
        let err = client.grep("hello", 10).expect_err("the daemon said no");
        assert!(
            is_the_daemon_saying_no(&err),
            "the daemon's own words are the daemon's: {err}"
        );
        assert!(
            err.to_string().contains("does not compile"),
            "and the message is the daemon's: {err}"
        );
        server.join();

        // A pipe that broke is not the daemon saying no.
        let broken = std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "the pipe closed");
        assert!(
            !is_the_daemon_saying_no(&broken),
            "a broken pipe is not a report: {broken}"
        );
    }

    /// The resolve op has a branch now.
    ///
    /// The daemon answered the question and the protocol read the answer
    /// before any client could ask it: the one thing that was missing was
    /// the branch that sends the question, and a message a client can
    /// receive but cannot ask for is a message nobody receives.
    #[test]
    fn a_resolve_asks_the_question_and_reads_the_answer() {
        let server = serve(|mut peer: Peer| {
            let request = peer.read_request_line().expect("the request is read");
            assert!(
                request.contains(r#""op":"resolve""#),
                "the question is a resolve: {request}"
            );
            let _ = peer.send(
                b"{\"op\":\"resolved\",\"path\":\"C:/a.txt\",\"folder\":false,\"backend\":3}\n",
            );
        });
        let mut client = Client::connect(&server.endpoint).expect("the client connects");
        let out = client.resolve("C:/a.txt").expect("the daemon answers");
        assert_eq!(out.path, "C:/a.txt", "the path is the path");
        assert!(!out.folder, "a file is not a folder");
        assert_eq!(out.backend, 3, "the volume is the volume");
        server.join();
    }

    /// A daemon that hangs up.
    fn hangs_up(peer: Peer) {
        let mut peer = peer;
        // The request is read first, and then nothing: the peer is
        // dropped and the connection closes between two answers.
        let _ = peer.read_request();
    }

    /// A daemon that dies in the middle of the answer.
    fn says_half(peer: Peer) {
        let mut peer = peer;
        // The request is read first: a daemon that answers without reading
        // can close the connection under the client's own write, and the
        // client then fails on the write and never sees the answer that
        // was cut.
        let _ = peer.read_request();
        let _ = peer.send(br#"{"op":"hits","hits":[{"path":"a""#);
        // The peer is dropped here: the connection closes with the line
        // unfinished.
    }

    /// A daemon that says a whole frame and then hangs up.
    ///
    /// The frame is whole -- it ends with a newline -- and the daemon stops
    /// after it. The answer started and it did not finish, and that is a
    /// different thing from a daemon that hung up between two answers: this
    /// one was there.
    fn says_a_frame_and_hangs_up(peer: Peer) {
        let mut peer = peer;
        let _ = peer.read_request();
        let _ = peer.send(
            br#"{"op":"hits","hits":[{"path":"a","score":1}],"took_us":1,"complete":true}
"#,
        );
        // The peer is dropped here: the connection closes at the frame's
        // boundary, with the answer unfinished.
    }

    /// A daemon whose count does not match what it sent.
    ///
    /// One hit on the wire and a count of five: the answer says it carries
    /// five entries and one arrived.
    fn counts_wrong(peer: Peer) {
        let mut peer = peer;
        let _ = peer.read_request();
        let _ = peer.send(
            br#"{"op":"hits","hits":[{"path":"a","score":1}],"took_us":1,"complete":true}
{"op":"done","count":5}
"#,
        );
    }

    /// A daemon that sends a line that never ends.
    fn says_too_much(peer: Peer) {
        let mut peer = peer;
        // The request is read first: a daemon that answers without
        // reading can close the connection under the client's own write,
        // and the client then fails on the write and never sees the line
        // that never ends.
        let _ = peer.read_request();
        let chunk = vec![b'x'; CHUNK];
        for _ in 0..(MAX_LINE / CHUNK + 2) {
            if peer.send(&chunk).is_err() {
                // The client gave up and closed the connection: the rest
                // of the line is not going anywhere.
                break;
            }
        }
    }

    /// A daemon that sends a document that nests deeper than the reader
    /// allows.
    fn says_too_deep(peer: Peer) {
        let mut peer = peer;
        let _ = peer.read_request();
        let deep = format!("{}{}", "[".repeat(500), "]".repeat(500));
        let _ = peer.send(deep.as_bytes());
        let _ = peer.send(b"\n");
    }

    /// A daemon that answers in another protocol.
    fn says_something_else(peer: Peer) {
        let mut peer = peer;
        let _ = peer.read_request();
        let _ = peer.send(b"{\"op\":\"rgx\",\"ready\":true}\n");
    }

    /// A daemon that says nothing must not hang the client.
    ///
    /// This is the whole point of the deadline: a daemon that accepts the
    /// connection and never answers used to freeze the launcher, because
    /// the launcher asks the daemon from its own thread.
    #[test]
    fn a_daemon_that_says_nothing_times_the_client_out() {
        let server = serve(says_nothing);
        let mut client = Client::connect(&server.endpoint).expect("the client connects");
        let started = Instant::now();
        let err = client
            .request_within(&Request::Status, SHORT)
            .expect_err("a silent daemon has no answer");
        assert_eq!(
            err.kind(),
            std::io::ErrorKind::TimedOut,
            "the wait ran out: {err}"
        );
        assert!(
            err.to_string().contains("did not answer in time"),
            "the reason says what happened: {err}"
        );
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "the client waited for the window, not forever"
        );
        server.join();
    }

    /// A search with the caller's own window times out with that window.
    ///
    /// The launcher asks the daemon from the thread that draws the
    /// window, so the window the caller asks for is the one that decides
    /// how long the launcher stops answering the keyboard.
    #[test]
    fn a_search_with_the_callers_window_times_out_with_it() {
        let server = serve(says_nothing);
        let mut client = Client::connect(&server.endpoint).expect("the client connects");
        let started = Instant::now();
        let err = client
            .search_within("invoice", 10, SHORT)
            .expect_err("a silent daemon has no answer");
        assert_eq!(
            err.kind(),
            std::io::ErrorKind::TimedOut,
            "the wait ran out: {err}"
        );
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "the client waited for the window, not forever"
        );
        server.join();
    }

    /// A content search with the caller's own window times out with that
    /// window.
    ///
    /// The content layer reads files, and the launcher that waits for the
    /// answer lives on the thread that draws the window: the window the
    /// caller asks for is the one that decides how long the launcher stops
    /// answering the keyboard.
    #[test]
    fn a_content_search_with_the_callers_window_times_out_with_it() {
        let server = serve(says_nothing);
        let mut client = Client::connect(&server.endpoint).expect("the client connects");
        let started = Instant::now();
        let err = client
            .grep_within("total", 10, SHORT)
            .expect_err("a silent daemon has no answer");
        assert_eq!(
            err.kind(),
            std::io::ErrorKind::TimedOut,
            "the wait ran out: {err}"
        );
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "the client waited for the window, not forever"
        );
        server.join();
    }

    /// The scoped content search honours the caller's window too: the
    /// scope is what makes it cheap, not what makes it quick.
    #[test]
    fn a_scoped_content_search_with_the_callers_window_times_out_with_it() {
        let server = serve(says_nothing);
        let mut client = Client::connect(&server.endpoint).expect("the client connects");
        let started = Instant::now();
        let err = client
            .content_within("total", &[String::from("/a/notes.txt")], 10, SHORT)
            .expect_err("a silent daemon has no answer");
        assert_eq!(
            err.kind(),
            std::io::ErrorKind::TimedOut,
            "the wait ran out: {err}"
        );
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "the client waited for the window, not forever"
        );
        server.join();
    }

    /// The status, with the caller's own window, times out with it.
    ///
    /// The launcher asks who the daemon is from the thread that draws the
    /// window: a daemon that takes the connection and then says nothing
    /// must not hold the window off the screen, and the window it was
    /// given is the one that decides how long the launcher waits.
    #[test]
    fn a_status_with_the_callers_window_times_out_with_it() {
        let server = serve(says_nothing);
        let mut client = Client::connect(&server.endpoint).expect("the client connects");
        let started = Instant::now();
        let err = client
            .status_within(SHORT)
            .expect_err("a silent daemon has no status");
        assert_eq!(
            err.kind(),
            std::io::ErrorKind::TimedOut,
            "the wait ran out: {err}"
        );
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "the client waited for the window, not forever"
        );
        server.join();
    }

    /// The platform's word for "the peer is gone" is one number.
    ///
    /// macOS says it with `EINVAL`, and it says it from the socket
    /// options as well as from the read: `setsockopt` on a socket whose
    /// peer has already disconnected is an invalid argument there. The
    /// client has to know that number, because the errno it sees is the
    /// platform's and the sentence the caller reads is the product's.
    #[test]
    #[cfg(unix)]
    fn the_peer_is_gone_is_einval() {
        assert!(the_peer_is_gone(&std::io::Error::from_raw_os_error(EINVAL)));
        // A broken pipe is the write's word for the same fact, and the
        // write's rule is the wider one: a broken pipe is the peer gone
        // too, and the client says so in its own words.
        let broken = std::io::Error::from_raw_os_error(32);
        assert!(!the_peer_is_gone(&broken));
        assert!(the_connection_is_closed(&broken));
        // And an error that is not the platform's is not the peer's.
        assert!(!the_peer_is_gone(&std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "not the platform's",
        )));
    }

    /// A daemon that dies in the middle of the answer is an error, and not
    /// an answer with nothing in it.
    ///
    /// The kind is the product's own, and not one platform's errno: the
    /// peer that goes away mid-answer says so with `ECONNRESET` on Linux
    /// and with `EINVAL` on macOS, and the client maps both to the end of
    /// the stream, which the reader turns into this one kind. A caller
    /// that reads the kind reads the same thing on every platform.
    ///
    /// The macOS word comes from two places, and the client has to hear
    /// both of them: the read on a socket whose peer closed with bytes
    /// still in flight, and the socket options, which a dead socket
    /// refuses on that platform. The half answer that arrived before the
    /// peer went away is still in the buffer, and the client has to read
    /// it: a client that threw it away would report a clean end where
    /// the answer was cut.
    #[test]
    fn an_answer_cut_in_the_middle_is_an_error() {
        let server = serve(says_half);
        let mut client = Client::connect(&server.endpoint).expect("the client connects");
        let err = client
            .request_within(
                &Request::Search {
                    q: "a".into(),
                    limit: 10,
                    session: None,
                },
                SHORT,
            )
            .expect_err("half an answer is not an answer");
        assert_eq!(
            err.kind(),
            std::io::ErrorKind::UnexpectedEof,
            "the stream was cut: {err}"
        );
        assert!(
            err.to_string().contains("in the middle of the answer"),
            "the reason says what happened: {err}"
        );
        server.join();
    }

    /// A daemon that says a frame and then hangs up is a daemon that is
    /// there.
    ///
    /// The frame was whole, and the daemon stopped after it. The answer
    /// started and it did not finish, and that is not the same thing as a
    /// daemon that hung up between two answers: this one spoke, and the
    /// launcher reads the kind to know whether to say "no daemon" or "the
    /// daemon answered badly". A daemon that answered badly is a daemon the
    /// user has to look at; a daemon that is gone is a daemon the user has
    /// to start.
    #[test]
    fn a_daemon_that_stops_after_a_frame_is_not_a_daemon_that_is_gone() {
        let server = serve(says_a_frame_and_hangs_up);
        let mut client = Client::connect(&server.endpoint).expect("the client connects");
        let err = client
            .request_within(
                &Request::Search {
                    q: "a".into(),
                    limit: 10,
                    session: None,
                },
                SHORT,
            )
            .expect_err("half an answer is not an answer");
        assert_eq!(
            err.kind(),
            std::io::ErrorKind::UnexpectedEof,
            "the answer was cut: {err}"
        );
        assert!(
            err.to_string().contains("in the middle of the answer"),
            "the reason says what happened: {err}"
        );
        server.join();
    }

    /// A count that does not match the entries is not an answer.
    ///
    /// The count is how big the answer said it was, and the reader counts
    /// what arrived. The two are the same number, and the reader compares
    /// them: a frame whose whole job is to say how big the answer was was
    /// decoded and thrown away, and an answer that lost a frame in the
    /// middle looked exactly like one that never lost anything.
    #[test]
    fn a_count_that_does_not_match_the_entries_fails_the_answer() {
        let server = serve(counts_wrong);
        let mut client = Client::connect(&server.endpoint).expect("the client connects");
        let err = client
            .request_within(
                &Request::Search {
                    q: "a".into(),
                    limit: 10,
                    session: None,
                },
                SHORT,
            )
            .expect_err("a count that does not match is not an answer");
        assert_eq!(
            err.kind(),
            std::io::ErrorKind::InvalidData,
            "the answer lied about its size: {err}"
        );
        assert!(
            err.to_string().contains("carries 5 entries"),
            "the reason says what did not agree: {err}"
        );
        server.join();
    }

    /// A daemon that hangs up is a daemon that is gone.
    ///
    /// The connection closed between two answers: the daemon is not
    /// there. The kind says so, because the launcher reads the kind to
    /// know whether to say "no daemon" or "the daemon answered badly".
    #[test]
    fn a_daemon_that_hangs_up_is_a_daemon_that_is_gone() {
        let server = serve(hangs_up);
        let mut client = Client::connect(&server.endpoint).expect("the client connects");
        let err = client
            .request_within(&Request::Status, SHORT)
            .expect_err("there is no answer");
        assert_eq!(
            err.kind(),
            std::io::ErrorKind::ConnectionAborted,
            "the daemon hung up: {err}"
        );
        server.join();
    }

    /// A line that never ends is refused before it fills the memory.
    #[test]
    fn a_line_that_never_ends_is_refused() {
        let server = serve(says_too_much);
        let mut client = Client::connect(&server.endpoint).expect("the client connects");
        // The deadline is a backstop, and it is not what this test proves:
        // what it proves is that the client refuses the line itself. A
        // deadline short enough to be reached on a slow machine turns the
        // refusal into a timeout and the test fails about the machine --
        // the line is eight megabytes, and reading them takes a while
        // where the bytes are emulated rather than read.
        let err = client
            .request_within(&Request::Status, Duration::from_secs(120))
            .expect_err("a line that never ends is not an answer");
        assert_eq!(
            err.kind(),
            std::io::ErrorKind::InvalidData,
            "the line was refused: {err}"
        );
        assert!(
            err.to_string().contains("longer than"),
            "the reason says what happened: {err}"
        );
        // The client stops reading the moment it refuses the line, so the
        // daemon is still writing into a socket nobody drains any more.
        // The client lets go of its end BEFORE the test waits for the
        // daemon: a daemon that writes into a closed socket fails and
        // stops, and the test's daemon does exactly that -- but a daemon
        // that writes into an open socket nobody reads blocks in the
        // write, and the test waits for that daemon.
        //
        // This is not a macOS quirk; it is a socket whose buffer was big
        // enough to hold the rest of the line. Linux's held 208 KiB and
        // the daemon finished anyway; the macOS runner's did not, and the
        // test sat in that join until the job's timeout killed the whole
        // run, an hour and a half later. The test is what was wrong: a
        // test that waits for a peer it has stopped talking to.
        drop(client);
        server.join();
    }

    /// A document that nests deeper than the reader allows is refused.
    #[test]
    fn a_line_that_nests_too_deep_is_refused() {
        let server = serve(says_too_deep);
        let mut client = Client::connect(&server.endpoint).expect("the client connects");
        let err = client
            .request_within(&Request::Status, SHORT)
            .expect_err("a document too deep to read is not an answer");
        assert_eq!(
            err.kind(),
            std::io::ErrorKind::InvalidData,
            "the document was refused: {err}"
        );
        assert!(
            err.to_string().contains("nests too deep"),
            "the reason says what happened: {err}"
        );
        server.join();
    }

    /// An answer the client does not know is an error, and not a line to
    /// skip on the way to the next one.
    #[test]
    fn an_answer_the_client_does_not_know_is_an_error() {
        let server = serve(says_something_else);
        let mut client = Client::connect(&server.endpoint).expect("the client connects");
        let err = client
            .request_within(&Request::Status, SHORT)
            .expect_err("an answer in another protocol is not an answer");
        assert_eq!(
            err.kind(),
            std::io::ErrorKind::InvalidData,
            "the answer was refused: {err}"
        );
        assert!(
            err.to_string().contains("does not know"),
            "the reason says what happened: {err}"
        );
        server.join();
    }

    /// The daemon's own answers are read, and the answer ends where the
    /// daemon ends it.
    #[test]
    fn the_daemons_own_answers_are_read() {
        let server = serve(|mut peer: Peer| {
            // The request is read first, like every other daemon in this
            // file: a daemon that answers without reading can close the
            // connection under the client's own write, and the client
            // then fails on the write instead of on the answer it was
            // reading.
            let _ = peer.read_request();
            let _ = peer
                .send(b"{\"op\":\"hits\",\"hits\":[{\"path\":\"a\",\"score\":1}],\"took_us\":2}\n");
            let _ = peer.send(b"{\"op\":\"done\",\"count\":1}\n");
        });
        let mut client = Client::connect(&server.endpoint).expect("the client connects");
        let out = client.search("a", 10).expect("the daemon answers");
        assert_eq!(out.hits.len(), 1);
        assert_eq!(out.hits[0].path, "a");
        server.join();
    }

    // --- the daemon that does not read ------------------------------------

    /// A daemon that takes the connection and never reads it: the
    /// question does not go out, and the client gives up on it under the
    /// window.
    ///
    /// The daemon is there — it took the connection — so the failure is a
    /// timeout and not a broken connection: a caller that says "the
    /// daemon is gone" about a daemon that is up hides the daemon the
    /// user has to start.
    #[test]
    fn a_daemon_that_never_reads_times_the_write_out() {
        let server = serve(never_reads);
        let mut client = Client::connect(&server.endpoint).expect("the client connects");
        let started = Instant::now();
        let err = client
            .request_within(&big_request(), SHORT)
            .expect_err("a daemon that never reads has no answer");
        assert_eq!(
            err.kind(),
            std::io::ErrorKind::TimedOut,
            "the write ran out: {err}"
        );
        assert!(
            err.to_string().contains("did not take the question"),
            "the reason says what happened: {err}"
        );
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "the client waited for the window, not forever"
        );
        drop(client);
        server.join();
    }

    /// A client whose write ran out of time cannot be used again.
    ///
    /// Some of the question may have gone out, and the answer the daemon
    /// sends would be the answer to a question it never got whole. The
    /// client refuses the next question at once, and says why.
    #[test]
    fn a_client_that_timed_out_on_the_write_refuses_the_next_question() {
        let server = serve(never_reads);
        let mut client = Client::connect(&server.endpoint).expect("the client connects");
        let err = client
            .request_within(&big_request(), SHORT)
            .expect_err("the write runs out");
        assert_eq!(
            err.kind(),
            std::io::ErrorKind::TimedOut,
            "the write ran out: {err}"
        );
        // The next question is refused at once: the connection is spent.
        // The refusal carries the timeout's kind, because it reports the
        // same fact — the daemon is there and did not take the question.
        let started = Instant::now();
        let err = client
            .request_within(&Request::Status, SHORT)
            .expect_err("a spent connection is refused");
        assert!(
            started.elapsed() < SHORT / 2,
            "the client refused at once, not after a window: {started:?}"
        );
        assert_eq!(
            err.kind(),
            std::io::ErrorKind::TimedOut,
            "the refusal is the same fact: {err}"
        );
        assert!(
            err.to_string().contains("cannot be used again"),
            "the reason says what happened: {err}"
        );
        drop(client);
        server.join();
    }

    /// A client whose answer ran out of time cannot be used again either.
    ///
    /// The question went out whole, and the daemon will still answer it:
    /// the answer is behind the window, and a client that asked another
    /// question into the connection would read the previous answer as
    /// this one's. The client refuses at once, and says why.
    #[test]
    fn a_client_that_timed_out_on_the_answer_refuses_the_next_question() {
        let server = serve(|mut peer: Peer| {
            // The question is read, like every daemon in this file, and
            // the answer comes -- after the client's window.
            let _ = peer.read_request();
            std::thread::sleep(Duration::from_millis(1500));
            let _ = peer.send(b"{\"op\":\"hits\",\"hits\":[],\"took_us\":1}\n");
            let _ = peer.send(b"{\"op\":\"done\",\"count\":0}\n");
        });
        let mut client = Client::connect(&server.endpoint).expect("the client connects");
        let err = client
            .request_within(&Request::Status, SHORT)
            .expect_err("the answer does not come in the window");
        assert_eq!(
            err.kind(),
            std::io::ErrorKind::TimedOut,
            "the read ran out: {err}"
        );
        // The next question is refused at once: the answer to the first
        // one may still be coming, and it must not be read as this one's.
        let started = Instant::now();
        let err = client
            .request_within(&Request::Status, SHORT)
            .expect_err("a spent connection is refused");
        assert!(
            started.elapsed() < SHORT / 2,
            "the client refused at once, not after a window: {started:?}"
        );
        assert_eq!(
            err.kind(),
            std::io::ErrorKind::TimedOut,
            "the refusal is the same fact: {err}"
        );
        assert!(
            err.to_string().contains("cannot be used again"),
            "the reason says what happened: {err}"
        );
        drop(client);
        server.join();
    }

    /// The write that gave up does not outlive the connection.
    ///
    /// The thread that writes to the pipe is stopped with the
    /// connection: a thread that waits for a pipe nobody reads is a
    /// thread that never ends, and a client that gave up must not leave
    /// one behind.
    #[test]
    fn a_write_that_gave_up_does_not_outlive_the_connection() {
        let (tx, rx) = std::sync::mpsc::channel();
        let server = serve(move |mut peer: Peer| {
            // The read comes after the client's window: a daemon that
            // read during the write would drain the pipe and the write
            // would go through, and the case under test would not be the
            // case under test.
            std::thread::sleep(Duration::from_millis(1500));
            let _ = tx.send(peer.read_to_the_end());
        });
        {
            let mut client = Client::connect(&server.endpoint).expect("the client connects");
            let err = client
                .request_within(&big_request(), SHORT)
                .expect_err("the write runs out");
            assert_eq!(
                err.kind(),
                std::io::ErrorKind::TimedOut,
                "the write ran out: {err}"
            );
            // The client is dropped here: the connection ends with it.
        }
        match rx.recv_timeout(Duration::from_secs(10)) {
            Ok(ended) => {
                assert!(ended, "the connection ended with the client");
                server.join();
            }
            Err(_) => panic!("the daemon was still waiting: the write outlived the connection"),
        }
    }
}
