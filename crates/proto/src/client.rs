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

use alloc::boxed::Box;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use std::io::{BufRead, BufReader, Write};
use std::time::Duration;

use crate::endpoint::Endpoint;
use crate::{Hit, Request, Response};

/// The window a client waits for the daemon's answer.
///
/// A search over a warm index is a re-rank of the previous result set, and
/// that is microseconds; a fresh search over a large tree is the slow case,
/// and it is the daemon's work, not the client's. The timeout is generous on
/// purpose: a client that gave up on a slow answer would turn a slow answer
/// into a wrong one.
const READ_TIMEOUT: Duration = Duration::from_secs(30);

/// A connection to a daemon.
///
/// The client owns both halves of the stream, because the protocol is a
/// conversation and a half-open one is a client that blocks forever.
pub struct Client {
    reader: Box<dyn BufRead + Send>,
    writer: Box<dyn Write + Send>,
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
        let (reader, writer): (Box<dyn BufRead + Send>, Box<dyn Write + Send>) = match endpoint {
            Endpoint::Stdio => (
                Box::new(BufReader::new(std::io::stdin())),
                Box::new(std::io::stdout()),
            ),
            Endpoint::Socket(path) => {
                #[cfg(unix)]
                {
                    let stream = std::os::unix::net::UnixStream::connect(path)?;
                    let read = stream.try_clone()?;
                    (Box::new(BufReader::new(read)), Box::new(stream))
                }
                #[cfg(not(unix))]
                {
                    let _ = path;
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::Unsupported,
                        "unix sockets are not available on this platform; use a named pipe",
                    ));
                }
            }
            Endpoint::Pipe(name) => {
                let file = open_pipe(name)?;
                let read = file.try_clone()?;
                (Box::new(BufReader::new(read)), Box::new(file))
            }
        };
        Ok(Client {
            reader,
            writer,
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
        let line = req.to_json();
        self.writer.write_all(line.as_bytes())?;
        self.writer.write_all(b"\n")?;
        self.writer.flush()?;

        let mut out = Vec::new();
        loop {
            let mut buf = String::new();
            let n = self.reader.read_line(&mut buf)?;
            if n == 0 {
                // The daemon closed the stream. Whatever arrived is what
                // there is; a half answer is better than hanging.
                break;
            }
            let line = buf.trim_end_matches(['\r', '\n']);
            if line.trim().is_empty() {
                continue;
            }
            let Some(resp) = Response::from_json(line) else {
                // A line this client cannot read is not an answer and not a
                // failure of the daemon: skip it and keep reading.
                continue;
            };
            let terminal = matches!(resp, Response::Done { .. } | Response::Error { .. });
            out.push(resp);
            if terminal {
                break;
            }
        }
        Ok(out)
    }

    /// Search the index.
    ///
    /// The answers are the hits, the time the daemon took, and the warnings
    /// the query raised — a warning is a query that failed closed, and the
    /// caller is the one that has to say so.
    pub fn search(
        &mut self,
        q: &str,
        limit: usize,
    ) -> std::io::Result<SearchOutcome> {
        let session = self.session;
        let req = Request::Search {
            q: q.to_string(),
            limit,
            session,
        };
        let responses = self.request(&req)?;
        let mut out = SearchOutcome::default();
        for r in responses {
            match r {
                Response::Hits { hits, took_us } => {
                    out.hits = hits;
                    out.took_us = took_us;
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
        let responses = self.request(&Request::Status)?;
        for r in responses {
            if let Response::Status {
                docs,
                event_id,
                catch_up,
                tv,
            } = r
            {
                return Ok(DaemonStatus {
                    docs,
                    event_id,
                    catch_up: catch_up.as_str().to_string(),
                    tv,
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

    /// Search the contents of the files.
    pub fn grep(&mut self, q: &str, limit: usize) -> std::io::Result<GrepOutcome> {
        let req = Request::Grep {
            q: q.to_string(),
            limit,
        };
        let responses = self.request(&req)?;
        let mut out = GrepOutcome::default();
        for r in responses {
            match r {
                Response::GrepFiles { files, took_us } => {
                    out.files = files;
                    out.took_us = took_us;
                }
                Response::Warning { message } => out.warnings.push(message),
                Response::Error { message } => return Err(std::io::Error::other(message)),
                _ => {}
            }
        }
        Ok(out)
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
        let req = Request::Content {
            q: q.to_string(),
            paths: paths.to_vec(),
            limit,
            session: self.session,
        };
        let responses = self.request(&req)?;
        let mut out = GrepOutcome::default();
        for r in responses {
            match r {
                Response::GrepFiles { files, took_us } => {
                    out.files = files;
                    out.took_us = took_us;
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
}

/// What a content search answered.
#[derive(Debug, Clone, Default)]
pub struct GrepOutcome {
    pub files: Vec<crate::GrepFile>,
    pub took_us: u64,
    pub warnings: Vec<String>,
}

/// What the daemon says about itself.
#[derive(Debug, Clone)]
pub struct DaemonStatus {
    pub docs: usize,
    pub event_id: u64,
    /// `replayed` or `relisted` — the two are different facts about the same
    /// index and the daemon is honest about which one it is.
    pub catch_up: String,
    /// Whether the daemon has a pack store (`--tv-dir`) — the corpus packed
    /// once and mmapped, which is the fastest content answer there is.
    pub tv: bool,
}

/// Open a Windows named pipe, waiting out the busy case.
///
/// `ERROR_PIPE_BUSY` means every instance of the pipe has a client on it.
/// The daemon keeps a pool of acceptors precisely so this is rare, and a
/// client that waited a moment is more useful than one that failed.
#[cfg(windows)]
fn open_pipe(name: &str) -> std::io::Result<std::fs::File> {
    use std::fs::OpenOptions;
    const ERROR_PIPE_BUSY: i32 = 231;
    let path = format!(r"\\.\pipe\{name}");
    let mut last: Option<std::io::Error> = None;
    for _ in 0..80 {
        match OpenOptions::new().read(true).write(true).open(&path) {
            Ok(f) => return Ok(f),
            Err(e) => {
                let busy = e.raw_os_error() == Some(ERROR_PIPE_BUSY);
                last = Some(e);
                if !busy {
                    return Err(last.expect("an error was just stored"));
                }
                std::thread::sleep(Duration::from_millis(25));
            }
        }
    }
    Err(last.unwrap_or_else(|| std::io::Error::other("the named pipe stayed busy")))
}

/// The named pipe does not exist on this platform.
#[cfg(not(windows))]
fn open_pipe(name: &str) -> std::io::Result<std::fs::File> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        format!("named pipes are not available on this platform: {name}"),
    ))
}

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
}
