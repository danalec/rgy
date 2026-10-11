//! JSON-lines protocol types shared by `searchd`, stdio, and `searchctl`
//! (`IMPLEMENTATION_PLAN.md` §3/§4.4).
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
    Search { q: String, limit: usize },
    /// Content grep: `{"op":"grep","q":…,"limit":…}` (Phase 4 engine).
    Grep { q: String, limit: usize },
    /// `{"op":"status"}` — index size, event id, uptime.
    Status,
}

/// A response line.
#[derive(Debug, Clone, PartialEq)]
pub enum Response {
    /// `{"op":"hits","hits":[…],"took_us":…}` — one per request with matches.
    Hits { hits: Vec<Hit>, took_us: u64 },
    /// `{"op":"done","count":…}` — terminator for a hit stream.
    Done { count: usize },
    /// `{"op":"status","docs":…,"event_id":…}`.
    Status { docs: usize, event_id: u64 },
    /// `{"op":"grep-files","files":[…],"took_us":…}` — content matches.
    GrepFiles { files: Vec<GrepFile>, took_us: u64 },
    /// `{"op":"error","message":…}` — the request failed; the stream lives.
    Error { message: String },
    /// `{"op":"warning","message":…}` — fail-closed query warning (unknown
    /// `type:`, bad `re:`), matching everything-rs's `warning:` lines.
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
            Request::Search { q, limit } => {
                format!(r#"{{"op":"search","q":"{}","limit":{}}}"#, esc(q), limit)
            }
            Request::Grep { q, limit } => {
                format!(r#"{{"op":"grep","q":"{}","limit":{}}}"#, esc(q), limit)
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
                    .map(|h| format!(r#"{{"path":"{}","score":{}}}"#, esc(&h.path), h.score))
                    .collect();
                format!(
                    r#"{{"op":"hits","hits":[{}],"took_us":{}}}"#,
                    items.join(","),
                    took_us
                )
            }
            Response::Done { count } => format!(r#"{{"op":"done","count":{count}}}"#),
            Response::Status { docs, event_id } => {
                format!(r#"{{"op":"status","docs":{docs},"event_id":{event_id}}}"#)
            }
            Response::GrepFiles { files, took_us } => {
                let items: Vec<String> = files
                    .iter()
                    .map(|f| {
                        let ls: Vec<String> = f
                            .lines
                            .iter()
                            .map(|(n, t)| format!("[{n},\"{}\"]", esc(t)))
                            .collect();
                        format!(
                            r#"{{"path":"{}","lines":[{}]}}"#,
                            esc(&f.path),
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
                format!(r#"{{"op":"error","message":"{}"}}"#, esc(message))
            }
            Response::Warning { message } => {
                format!(r#"{{"op":"warning","message":"{}"}}"#, esc(message))
            }
        }
    }
}

fn esc(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
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
                s.push(match e {
                    'n' => '\n',
                    't' => '\t',
                    'r' => '\r',
                    other => other,
                });
                i += 2;
            }
            other => {
                s.push(other);
                i += 1;
            }
        }
    }
    None
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

    #[test]
    fn request_round_trips() {
        let r = Request::Search {
            q: "report type:pdf".into(),
            limit: 25,
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
}
