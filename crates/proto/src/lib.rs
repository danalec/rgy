// SPDX-License-Identifier: Apache-2.0

//! JSON-lines protocol types shared by `searchd`, stdio, and `searchctl`
//!.
//!
//! One request per line in, one response per line out — identical over
//! named pipe (Windows), UDS (Unix), and stdio. Hand-rolled encode/decode:
//! the protocol is small enough that a dependency would be the bigger
//! surface. Field names are the fsearch op model; rgx `--serve` translates
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
pub use client::{Client, DaemonStatus, GrepOutcome, SearchOutcome};
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
    /// Content grep: `{"op":"grep","q":…,"limit":…}` (Phase 4 engine).
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
    Content {
        q: String,
        paths: Vec<String>,
        limit: usize,
        /// The session id, for the same reason the name search has one: a
        /// pattern that narrows the previous one is answered from the
        /// previous answer instead of from the scope.
        session: Option<u64>,
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

/// A response line.
#[derive(Debug, Clone, PartialEq)]
pub enum Response {
    /// `{"op":"hits","hits":[…],"took_us":…}` — one per request with matches.
    Hits { hits: Vec<Hit>, took_us: u64 },
    /// `{"op":"done","count":…}` — terminator for a hit stream.
    Done { count: usize },
    /// `{"op":"status","docs":…,"event_id":…,"catch_up":…,"tv":…}`.
    ///
    /// `catch_up` is additive: a reader that does not know the field keeps
    /// working, and the field is what tells a replayed catch-up from a
    /// relisted one.
    ///
    /// `tv` says whether the daemon was started with a **pack store** —
    /// the corpus packed once and mmapped — and it is what tells a client
    /// that the content layer is answered by the store rather than by a
    /// walk. It is additive in the same way: a reader that does not know
    /// it reads "no store", which is the conservative answer.
    Status {
        docs: usize,
        event_id: u64,
        catch_up: CatchUp,
        /// Whether the daemon has a pack store (`--tv-dir`).
        tv: bool,
    },
    /// `{"op":"grep-files","files":[…],"took_us":…}` — content matches.
    GrepFiles { files: Vec<GrepFile>, took_us: u64 },
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

/// One file's content matches: path and the matching lines as
/// `(line number, text)` — the `grep:` result shape (rg's `path:line:text`).
#[derive(Debug, Clone, PartialEq)]
pub struct GrepFile {
    pub path: String,
    pub lines: Vec<(usize, String)>,
}

impl Request {
    /// Parse one JSON line. Returns `None` on anything malformed.
    pub fn from_json(line: &str) -> Option<Request> {
        let v = parse_json(line).ok()?;
        let obj = v.as_object()?;
        let op = obj.get("op")?.as_str()?;
        let limit = obj
            .get("limit")
            .and_then(|l| l.as_num())
            .map(|n| n as usize)
            .unwrap_or(50);
        match op {
            "search" => Some(Request::Search {
                q: obj.get("q")?.as_str()?.to_string(),
                limit,
                session: obj
                    .get("session")
                    .and_then(|s| s.as_num())
                    .map(|n| n as u64),
            }),
            "grep" | "content" => Some(Request::Grep {
                q: obj.get("q")?.as_str()?.to_string(),
                limit,
            }),
            "grep-scoped" => {
                let paths = obj
                    .get("paths")?
                    .as_array()?
                    .iter()
                    .filter_map(|p| p.as_str().map(|s| s.to_string()))
                    .collect();
                Some(Request::Content {
                    q: obj.get("q")?.as_str()?.to_string(),
                    paths,
                    limit,
                    session: obj
                        .get("session")
                        .and_then(|s| s.as_num())
                        .map(|n| n as u64),
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
            Request::Content {
                q,
                paths,
                limit,
                session,
            } => {
                let items: Vec<String> = paths
                    .iter()
                    .map(|p| format!("\"{}\"", escape_json_string(p)))
                    .collect();
                match session {
                    Some(s) => format!(
                        r#"{{"op":"grep-scoped","q":"{}","paths":[{}],"limit":{},"session":{}}}"#,
                        escape_json_string(q),
                        items.join(","),
                        limit,
                        s
                    ),
                    None => format!(
                        r#"{{"op":"grep-scoped","q":"{}","paths":[{}],"limit":{}}}"#,
                        escape_json_string(q),
                        items.join(","),
                        limit
                    ),
                }
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
    /// `op` this reader does not know.
    NotAnAnswer,
    /// The line nests deeper than the reader allows.
    TooDeep,
}

impl Response {
    pub fn to_json(&self) -> String {
        match self {
            Response::Hits { hits, took_us } => {
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
                format!(
                    r#"{{"op":"hits","hits":[{}],"took_us":{}}}"#,
                    items.join(","),
                    took_us
                )
            }
            Response::Done { count } => format!(r#"{{"op":"done","count":{count}}}"#),
            Response::Status {
                docs,
                event_id,
                catch_up,
                tv,
            } => {
                format!(
                    r#"{{"op":"status","docs":{docs},"event_id":{event_id},"catch_up":"{}","tv":{tv}}}"#,
                    catch_up.as_str()
                )
            }
            Response::GrepFiles { files, took_us } => {
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
                format!(
                    r#"{{"op":"grep-files","files":[{}],"took_us":{}}}"#,
                    items.join(","),
                    took_us
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
                let hits = obj
                    .get("hits")
                    .and_then(|h| h.as_array())
                    .ok_or(LineError::NotAnAnswer)?
                    .iter()
                    .filter_map(|it| {
                        let o = it.as_object()?;
                        let path = o.get("path")?.as_str()?.to_string();
                        let score = o.get("score").and_then(|n| n.as_num()).unwrap_or(0.0) as i32;
                        let span = match o.get("span").and_then(|s| s.as_array()) {
                            Some(items) => items
                                .iter()
                                .filter_map(|n| n.as_num())
                                .map(|n| n as u32)
                                .collect(),
                            None => Vec::new(),
                        };
                        // The folder flag is additive: an older daemon's
                        // hit has no field, and a hit with no field is a
                        // file.
                        let folder = o.get("folder").and_then(|f| f.as_bool()).unwrap_or(false);
                        Some(Hit {
                            path,
                            score,
                            span,
                            folder,
                        })
                    })
                    .collect();
                let took_us = obj.get("took_us").and_then(|n| n.as_num()).unwrap_or(0.0) as u64;
                Response::Hits { hits, took_us }
            }
            "done" => {
                let count = obj.get("count").and_then(|n| n.as_num()).unwrap_or(0.0) as usize;
                Response::Done { count }
            }
            "status" => {
                let docs = obj.get("docs").and_then(|n| n.as_num()).unwrap_or(0.0) as usize;
                let event_id = obj.get("event_id").and_then(|n| n.as_num()).unwrap_or(0.0) as u64;
                let catch_up = match obj.get("catch_up").and_then(|s| s.as_str()) {
                    Some("replayed") => CatchUp::Replayed,
                    _ => CatchUp::Relisted,
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
                }
            }
            "grep-files" => {
                let files = obj
                    .get("files")
                    .and_then(|f| f.as_array())
                    .ok_or(LineError::NotAnAnswer)?
                    .iter()
                    .filter_map(|it| {
                        let o = it.as_object()?;
                        let path = o.get("path")?.as_str()?.to_string();
                        let lines = match o.get("lines").and_then(|l| l.as_array()) {
                            Some(ls) => ls
                                .iter()
                                .filter_map(|l| {
                                    let pair = l.as_array()?;
                                    let n = pair.first()?.as_num()? as usize;
                                    let t = pair.get(1)?.as_str()?.to_string();
                                    Some((n, t))
                                })
                                .collect(),
                            None => Vec::new(),
                        };
                        Some(GrepFile { path, lines })
                    })
                    .collect();
                let took_us = obj.get("took_us").and_then(|n| n.as_num()).unwrap_or(0.0) as u64;
                Response::GrepFiles { files, took_us }
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
            }
            .to_json(),
            r#"{"op":"hits","hits":[{"path":"p","score":5}],"took_us":6}"#
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
            }
            .to_json(),
            r#"{"op":"status","docs":3,"event_id":9,"catch_up":"relisted","tv":false}"#
        );
        // The other value, spelled the way the rules spells it.
        assert_eq!(
            Response::Status {
                docs: 3,
                event_id: 9,
                catch_up: CatchUp::Replayed,
                tv: false,
            }
            .to_json(),
            r#"{"op":"status","docs":3,"event_id":9,"catch_up":"replayed","tv":false}"#
        );
        assert_eq!(
            Response::GrepFiles {
                files: vec![GrepFile {
                    path: "f".into(),
                    lines: vec![(1, "t".into())]
                }],
                took_us: 8,
            }
            .to_json(),
            r#"{"op":"grep-files","files":[{"path":"f","lines":[[1,"t"]]}],"took_us":8}"#
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
        }
        .to_json();
        assert!(!line.contains('\n'), "the line carries a newline: {line:?}");
        assert!(!line.contains('\t'), "the line carries a tab: {line:?}");
        assert_eq!(
            line,
            r#"{"op":"grep-files","files":[{"path":"C:\\dir\there","lines":[[1,"a\tb\nc\u0001d"]]}],"took_us":3}"#
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
                took_us: 3
            }
        );
    }

    #[test]
    fn a_hit_without_a_span_still_reads() {
        // The span is additive: an older daemon's line has no span, and a
        // client that meets one must not fall over on it.
        let back =
            Response::from_json(r#"{"op":"hits","hits":[{"path":"p","score":5}],"took_us":6}"#)
                .expect("the line reads back");
        assert_eq!(
            back,
            Response::Hits {
                hits: vec![Hit::new("p".into(), 5)],
                took_us: 6
            }
        );
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
                took_us: 1
            }
        );

        // A file does not carry the field at all: the common hit is the
        // short one.
        let file = Hit::new("C:/a/notes.txt".into(), 2);
        let line = Response::Hits {
            hits: vec![file.clone()],
            took_us: 1,
        }
        .to_json();
        assert!(!line.contains("folder"), "a file says nothing: {line}");
        let back = Response::from_json(&line).expect("the line reads back");
        assert_eq!(
            back,
            Response::Hits {
                hits: vec![file],
                took_us: 1
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
            },
            Response::Done { count: 3 },
            Response::Status {
                docs: 4,
                event_id: 5,
                catch_up: CatchUp::Replayed,
                tv: true,
            },
            Response::GrepFiles {
                files: vec![GrepFile {
                    path: "f".into(),
                    lines: vec![(7, "line".into())],
                }],
                took_us: 8,
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
            }
        );
    }

    #[test]
    fn a_scoped_content_request_round_trips_with_its_scope() {
        let req = Request::Content {
            q: "tesla".to_string(),
            paths: vec!["C:/a.txt".to_string(), "C:/b\".txt".to_string()],
            limit: 10,
            session: Some(7),
        };
        let line = req.to_json();
        assert!(line.contains(r#""op":"grep-scoped""#), "the op is: {line}");
        assert!(line.contains(r#""session":7"#), "the session is: {line}");
        assert_eq!(Request::from_json(&line), Some(req));
    }

    #[test]
    fn a_scoped_content_request_without_a_session_is_the_request_as_it_was() {
        let req = Request::Content {
            q: "tesla".to_string(),
            paths: vec!["C:/a.txt".to_string()],
            limit: 10,
            session: None,
        };
        let line = req.to_json();
        assert!(!line.contains("session"), "the session is absent: {line}");
        assert_eq!(Request::from_json(&line), Some(req));
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
            Request::from_json(r#"{"op":"content","q":"x","limit":1}"#),
            Some(Request::Grep {
                q: "x".to_string(),
                limit: 1
            })
        );
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
