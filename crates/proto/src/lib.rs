// SPDX-License-Identifier: Apache-2.0

//! JSON-lines protocol types shared by `searchd`, stdio, and `searchctl`
//! (`docs/design.md` §2.4).
//!
//! One request per line in, one response per line out — identical over
//! named pipe (Windows), UDS (Unix), and stdio. Hand-rolled encode/decode:
//! the protocol is small enough that a dependency would be the bigger
//! surface. Field names are the fsearch op model; rgx `--serve` translates
//! to the same engine calls behind a compatibility front.

/// A request line: an op plus its arguments.
#[derive(Debug, Clone, PartialEq)]
pub enum Request {
    /// Name search: `{"op":"search","q":…,"limit":…}`.
    ///
    /// The `session` is the incremental-search id (parity row S.10): the
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
    /// `{"op":"status"}` — index size, event id, uptime.
    Status,
}

/// How the index caught up after the last restart.
///
/// Two values, and the difference is the point (`docs/design.md` §3, rule 5
/// — "live-feed honesty"). The field carries what the daemon *did*, not what
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
    /// `{"op":"status","docs":…,"event_id":…,"catch_up":…}`.
    ///
    /// `catch_up` is additive: a reader that does not know the field keeps
    /// working, and the field is what tells a replayed catch-up from a
    /// relisted one (`docs/design.md` §3, rule 5).
    Status {
        docs: usize,
        event_id: u64,
        catch_up: CatchUp,
    },
    /// `{"op":"grep-files","files":[…],"took_us":…}` — content matches.
    GrepFiles { files: Vec<GrepFile>, took_us: u64 },
    /// `{"op":"error","message":…}` — the request failed; the stream lives.
    Error { message: String },
    /// `{"op":"warning","message":…}` — fail-closed query warning (unknown
    /// `type:`, bad `re:`), matching zen-rs's `warning:` lines.
    Warning { message: String },
}

/// One hit: a path and its score.
#[derive(Debug, Clone, PartialEq)]
pub struct Hit {
    pub path: String,
    pub score: i32,
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
        let v = parse_json(line)?;
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
            Request::Status => r#"{"op":"status"}"#.to_string(),
        }
    }
}

impl Response {
    pub fn to_json(&self) -> String {
        match self {
            Response::Hits { hits, took_us } => {
                let items: Vec<String> = hits
                    .iter()
                    .map(|h| {
                        format!(
                            r#"{{"path":"{}","score":{}}}"#,
                            escape_json_string(&h.path),
                            h.score
                        )
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
            } => {
                format!(
                    r#"{{"op":"status","docs":{docs},"event_id":{event_id},"catch_up":"{}"}}"#,
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
    fn as_str(&self) -> Option<&str> {
        match self {
            Json::Str(s) => Some(s),
            _ => None,
        }
    }
    fn as_num(&self) -> Option<f64> {
        match self {
            Json::Num(n) => Some(*n),
            _ => None,
        }
    }
    fn as_object(&self) -> Option<&Vec<(String, Json)>> {
        match self {
            Json::Obj(o) => Some(o),
            _ => None,
        }
    }
}

trait ObjGet {
    fn get(&self, k: &str) -> Option<&Json>;
}
impl ObjGet for Vec<(String, Json)> {
    fn get(&self, k: &str) -> Option<&Json> {
        self.iter().find(|(key, _)| key == k).map(|(_, v)| v)
    }
}

fn parse_json(s: &str) -> Option<Json> {
    let b: Vec<char> = s.chars().collect();
    let (v, i) = value(&b, 0)?;
    // Trailing whitespace only.
    if b[i..].iter().any(|c| !c.is_whitespace()) {
        return None;
    }
    Some(v)
}

fn value(b: &[char], i: usize) -> Option<(Json, usize)> {
    let mut i = skip_ws(b, i);
    match b.get(i)? {
        '"' => {
            let (s, j) = string(b, i)?;
            Some((Json::Str(s), j))
        }
        '{' => {
            i += 1;
            let mut out = Vec::new();
            i = skip_ws(b, i);
            if b.get(i) == Some(&'}') {
                return Some((Json::Obj(out), i + 1));
            }
            loop {
                i = skip_ws(b, i);
                let (k, j) = string(b, i)?;
                i = skip_ws(b, j);
                if b.get(i) != Some(&':') {
                    return None;
                }
                let (v, j) = value(b, i + 1)?;
                out.push((k, v));
                i = skip_ws(b, j);
                match b.get(i)? {
                    ',' => i += 1,
                    '}' => return Some((Json::Obj(out), i + 1)),
                    _ => return None,
                }
            }
        }
        '[' => {
            i += 1;
            let mut out = Vec::new();
            i = skip_ws(b, i);
            if b.get(i) == Some(&']') {
                return Some((Json::Arr(out), i + 1));
            }
            loop {
                let (v, j) = value(b, i)?;
                out.push(v);
                i = skip_ws(b, j);
                match b.get(i)? {
                    ',' => i += 1,
                    ']' => return Some((Json::Arr(out), i + 1)),
                    _ => return None,
                }
            }
        }
        't' if b[i..].starts_with(&['t', 'r', 'u', 'e']) => Some((Json::Bool(true), i + 4)),
        'f' if b[i..].starts_with(&['f', 'a', 'l', 's', 'e']) => Some((Json::Bool(false), i + 5)),
        'n' if b[i..].starts_with(&['n', 'u', 'l', 'l']) => Some((Json::Null, i + 4)),
        _ => num(b, i),
    }
}

fn string(b: &[char], i: usize) -> Option<(String, usize)> {
    if b.get(i) != Some(&'"') {
        return None;
    }
    let mut s = String::new();
    let mut i = i + 1;
    while let Some(&c) = b.get(i) {
        match c {
            '"' => return Some((s, i + 1)),
            '\\' => {
                let e = *b.get(i + 1)?;
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
                        let (cp, j) = unicode_escape(b, i)?;
                        s.push(char::from_u32(cp)?);
                        i = j;
                    }
                    // An escape RFC 8259 does not define is not JSON. The
                    // reader does not guess what was meant.
                    _ => return None,
                }
            }
            // A control character that arrived raw is not JSON either
            // (RFC 8259 §7): the writer never emits one, and a line that
            // carries one did not come from a JSON writer.
            c if (c as u32) < 0x20 => return None,
            other => {
                s.push(other);
                i += 1;
            }
        }
    }
    None
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

fn num(b: &[char], i: usize) -> Option<(Json, usize)> {
    let start = i;
    let mut i = i;
    while matches!(b.get(i), Some(c) if c.is_ascii_digit() || matches!(c, '-' | '+' | '.' | 'e' | 'E'))
    {
        i += 1;
    }
    if i == start {
        return None;
    }
    let s: String = b[start..i].iter().collect();
    Some((Json::Num(s.parse().ok()?), i))
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
                    score: 5
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
            }
            .to_json(),
            r#"{"op":"status","docs":3,"event_id":9,"catch_up":"relisted"}"#
        );
        // The other value, spelled the way `docs/design.md` §3 spells it.
        assert_eq!(
            Response::Status {
                docs: 3,
                event_id: 9,
                catch_up: CatchUp::Replayed,
            }
            .to_json(),
            r#"{"op":"status","docs":3,"event_id":9,"catch_up":"replayed"}"#
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
    /// are the ones `docs/design.md` §3 uses: a journal-backed platform
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
}
