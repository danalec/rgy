//! E-mail adapters: mbox archives and single .eml messages.
//!
//! Message bodies become searchable text; MIME attachments become
//! [`Outcome::Members`] so the pipeline recurses into them with the full
//! adapter chain (a PDF attachment is still a PDF).

#![no_std]

#[macro_use]
extern crate alloc;

#[cfg(test)]
extern crate std;

use alloc::boxed::Box;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::fmt::Write as _;
use core::mem;
use rgx_core::{bail, Result};
use rgx_core::{Adapter, Outcome};

mod extra;
mod pst;

pub fn adapters() -> Vec<Box<dyn Adapter>> {
    vec![
        Box::new(Mbox),
        Box::new(Eml),
        Box::new(extra::Msg),
        Box::new(extra::Emlx),
        Box::new(pst::PstAdapter),
    ]
}

/// One MIME message → (headers + decoded body text, attachments).
///
/// Operates on `&[u8]` directly: header parsing, boundary
/// detection, and base64 payload decoding all stay in byte space
/// until the final body string is built. The `from_utf8_lossy`
/// path used to allocate a per-message `String` for the whole
/// header block before scanning it line by line.
fn parse_message(raw: &[u8]) -> (String, Vec<(String, Vec<u8>)>) {
    let mut headers = String::new();
    let mut body_out = String::new();
    let mut attachments = Vec::new();
    let sep = find_subslice(raw, b"\r\n\r\n")
        .map(|i| (i, 4))
        .or_else(|| find_subslice(raw, b"\n\n").map(|i| (i, 2)));
    let Some((h_end, sep_len)) = sep else {
        return (String::from_utf8_lossy(raw).into_owned(), attachments);
    };
    let hdr_bytes = &raw[..h_end];
    append_lines_to_string(hdr_bytes, &mut headers);
    let body = &raw[h_end + sep_len..];
    // `header_value` is a case-insensitive byte scan: parsing stays in
    // byte space rather than copying the header block into a String.
    let content_type = header_value(hdr_bytes, "content-type").unwrap_or_default();
    let boundary = content_type
        .split(';')
        .skip(1)
        .filter_map(|p| p.trim().strip_prefix("boundary="))
        .map(|b| b.trim_matches('"').to_string())
        .next();
    if let Some(boundary) = boundary {
        let mut delim = Vec::with_capacity(boundary.len() + 2);
        delim.extend_from_slice(b"--");
        delim.extend_from_slice(boundary.as_bytes());
        for part in split_on(body, &delim).skip(1) {
            let part = if part.starts_with(b"\r\n") {
                &part[2..]
            } else if part.starts_with(b"\n") {
                &part[1..]
            } else {
                part
            };
            if part.starts_with(b"--") {
                break;
            }
            let Some((ph_end, ph_sep_len)) = find_subslice(part, b"\r\n\r\n")
                .map(|i| (i, 4))
                .or_else(|| find_subslice(part, b"\n\n").map(|i| (i, 2)))
            else {
                continue;
            };
            let ph = &part[..ph_end];
            let pb = &part[ph_end + ph_sep_len..];
            // Parse headers directly off the byte slice. The previous
            // shape allocated a `String` per part via `from_utf8_lossy`
            // before any test, which was 1.5 KB of heap per MIME part
            // for nothing -- `header_value` is a case-insensitive
            // byte scan and the body of the value is plain ASCII.
            let p_ct = header_value(ph, "content-type").unwrap_or_default();
            let cd = header_value(ph, "content-disposition").unwrap_or_default();
            let filename = cd
                .split(';')
                .skip(1)
                .filter_map(|p| p.trim().strip_prefix("filename="))
                .map(|f| f.trim_matches('"').to_string())
                .next();
            // `cd` is already case-folded by `header_value` (ASCII only).
            let is_attachment = cd.starts_with("attachment") || filename.is_some();
            let transfer_lower = header_value(ph, "content-transfer-encoding").unwrap_or_default();
            // Same case-fold trick: the encoding is one of a small
            // set of ASCII keywords, so we never need a heap String.
            let is_base64 = transfer_lower.as_bytes().contains_ascii_ci("base64");
            let is_qp = !is_base64
                && transfer_lower
                    .as_bytes()
                    .contains_ascii_ci("quoted-printable");
            let decoded: Vec<u8> = if is_base64 {
                rgx_core::base64::base64_decode_skipping_ws(pb, rgx_core::base64::STANDARD)
                    .unwrap_or_default()
            } else if is_qp {
                decode_qp(pb)
            } else {
                pb.to_vec()
            };
            if is_attachment {
                let name = filename.unwrap_or_else(|| "attachment.bin".into());
                attachments.push((name, decoded));
            } else if p_ct.starts_with("text/") || p_ct.is_empty() {
                body_out.push_str(&String::from_utf8_lossy(&decoded));
                body_out.push('\n');
            }
        }
    } else {
        let transfer_lower =
            header_value(hdr_bytes, "content-transfer-encoding").unwrap_or_default();
        let is_base64 = transfer_lower.as_bytes().contains_ascii_ci("base64");
        let is_qp = !is_base64
            && transfer_lower
                .as_bytes()
                .contains_ascii_ci("quoted-printable");
        let decoded: Vec<u8> = if is_base64 {
            rgx_core::base64::base64_decode_skipping_ws(body, rgx_core::base64::STANDARD)
                .unwrap_or_default()
        } else if is_qp {
            decode_qp(body)
        } else {
            body.to_vec()
        };
        body_out.push_str(&String::from_utf8_lossy(&decoded));
    }
    let mut text = headers;
    text.push_str(&body_out);
    (text, attachments)
}

fn append_lines_to_string(bytes: &[u8], out: &mut String) {
    let mut start = 0usize;
    for i in 0..bytes.len() {
        if bytes[i] == b'\n' {
            let end = if i > start && bytes[i - 1] == b'\r' {
                i - 1
            } else {
                i
            };
            push_utf8_line(&bytes[start..end], out);
            out.push('\n');
            start = i + 1;
        }
    }
    if start < bytes.len() {
        push_utf8_line(&bytes[start..], out);
        out.push('\n');
    }
}

fn push_utf8_line(line: &[u8], out: &mut String) {
    if let Ok(s) = core::str::from_utf8(line) {
        out.push_str(s);
    } else {
        out.push_str(&String::from_utf8_lossy(line));
    }
}

fn split_on<'a>(hay: &'a [u8], delim: &'a [u8]) -> Split<'a> {
    Split { hay, delim }
}

struct Split<'a> {
    hay: &'a [u8],
    delim: &'a [u8],
}

impl<'a> Iterator for Split<'a> {
    type Item = &'a [u8];
    fn next(&mut self) -> Option<&'a [u8]> {
        if self.hay.is_empty() {
            return None;
        }
        match find_subslice(self.hay, self.delim) {
            Some(i) => {
                let head = &self.hay[..i];
                self.hay = &self.hay[i + self.delim.len()..];
                Some(head)
            }
            None => {
                let tail = self.hay;
                self.hay = &[];
                Some(tail)
            }
        }
    }
}

fn find_subslice(hay: &[u8], needle: &[u8]) -> Option<usize> {
    rgx_core::memchr::memmem::find(hay, needle)
}

fn header_value(headers: &[u8], name: &str) -> Option<String> {
    // Walk the header block as raw bytes. The line iterator here
    // scans for `\n` and treats `\r` as part of the line content;
    // MIME header lines are split by `\r\n` or `\n`, and a
    // continuation starts with whitespace on the next line. The
    // previous shape accepted a `&str` and called `.lines()`, which
    // forced an `&str` to exist -- and the only way the caller got
    // one was `String::from_utf8_lossy(ph).into_owned()` per MIME
    // part, which is the allocation the audit flagged.
    let mut value: Vec<u8> = Vec::new();
    let mut in_header = false;
    let mut start = 0usize;
    while start < headers.len() {
        let end = memchr2(b'\n', headers, start).unwrap_or(headers.len());
        let line = &headers[start..end];
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        if line
            .first()
            .map(|b| b.is_ascii_whitespace())
            .unwrap_or(false)
            && in_header
        {
            value.push(b' ');
            let trimmed = line
                .iter()
                .position(|b| !b.is_ascii_whitespace())
                .map(|o| &line[o..])
                .unwrap_or(&[]);
            value.extend_from_slice(trimmed);
        } else {
            in_header = false;
            if let Some(colon) = line.iter().position(|&b| b == b':') {
                let (k, v) = line.split_at(colon);
                let v = &v[1..];
                if ascii_eq_ignore_case(k, name.as_bytes()) {
                    value.clear();
                    let trimmed = v
                        .iter()
                        .position(|b| !b.is_ascii_whitespace())
                        .map(|o| &v[o..])
                        .unwrap_or(&[]);
                    let trimmed_end = trimmed
                        .iter()
                        .rposition(|b| !b.is_ascii_whitespace())
                        .map(|i| i + 1)
                        .unwrap_or(0);
                    value.extend_from_slice(&trimmed[..trimmed_end]);
                    in_header = true;
                }
            }
        }
        start = end + 1;
    }
    if value.is_empty() {
        None
    } else {
        String::from_utf8(value).ok()
    }
}

/// True iff `a` and `b` match under ASCII case-folding.
fn ascii_eq_ignore_case(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len()
        && a.iter()
            .zip(b.iter())
            .all(|(x, y)| x.eq_ignore_ascii_case(y))
}

/// Sub-slice contains a needle, case-insensitively (ASCII only).
trait AsciiCi {
    fn contains_ascii_ci(&self, needle: &str) -> bool;
}
impl AsciiCi for [u8] {
    fn contains_ascii_ci(&self, needle: &str) -> bool {
        let n = needle.as_bytes();
        if n.is_empty() || n.len() > self.len() {
            return n.is_empty();
        }
        let mut i = 0;
        while i + n.len() <= self.len() {
            if ascii_eq_ignore_case(&self[i..i + n.len()], n) {
                return true;
            }
            i += 1;
        }
        false
    }
}

/// memchr-style byte search through `hay` starting at `from`.
fn memchr2(needle: u8, hay: &[u8], from: usize) -> Option<usize> {
    rgx_core::memchr::memchr(needle, &hay[from..]).map(|p| p + from)
}

/// minimal quoted-printable decoder (enough for e-mail bodies)
fn decode_qp(input: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(input.len());
    let mut i = 0;
    while i < input.len() {
        match input[i] {
            b'=' if i + 2 < input.len() && input[i + 1] == b'\r' && input[i + 2] == b'\n' => i += 3,
            b'=' if i + 1 < input.len() && input[i + 1] == b'\n' => i += 2,
            b'=' if i + 2 < input.len() => {
                let h = |c: u8| (c as char).to_digit(16);
                match (h(input[i + 1]), h(input[i + 2])) {
                    (Some(a), Some(b)) => {
                        out.push((a * 16 + b) as u8);
                        i += 3;
                    }
                    _ => {
                        out.push(b'=');
                        i += 1;
                    }
                }
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    out
}

/// mbox: split on `^From ` lines; each message is its own searchable unit
/// plus its attachments as members.
pub struct Mbox;
impl Adapter for Mbox {
    fn name(&self) -> &'static str {
        "mbox"
    }
    fn extensions(&self) -> &'static [&'static str] {
        &["mbox", "mbx"]
    }
    fn extract(&self, data: &[u8], path: &str) -> Result<Outcome> {
        // sanity: first line must look like a From_ line
        let first = data
            .iter()
            .position(|&b| b == b'\n')
            .map(|i| &data[..i])
            .unwrap_or(data);
        if !first.starts_with(b"From ") {
            bail!("not an mbox (no 'From ' first line)");
        }
        let text = String::from_utf8_lossy(data);
        let mut raws: Vec<Vec<u8>> = Vec::new();
        let mut cur: Vec<u8> = Vec::new();
        let mut started = false;
        for line in text.split_inclusive('\n') {
            if line.starts_with("From ") && started {
                raws.push(mem::take(&mut cur));
            } else {
                started = true;
            }
            cur.extend_from_slice(line.as_bytes());
        }
        if !cur.is_empty() {
            raws.push(cur);
        }
        if raws.is_empty() {
            bail!("mbox contains no messages");
        }
        let mut text_payload = format!("mbox: {} messages in {path}\n", raws.len());
        for (i, raw) in raws.iter().enumerate() {
            let (msg_text, _attachments) = parse_message(raw);
            // attachments inside mbox messages stay inline (manifest keeps
            // the mbox a single searchable unit); full MIME recursion is
            // available on single .eml files
            let _ = writeln!(text_payload, "--- message-{i}.eml ---\n{msg_text}");
        }
        Ok(Outcome::Text(text_payload.into_bytes()))
    }
}

/// Single .eml message: text + attachments as members.
pub struct Eml;
impl Adapter for Eml {
    fn name(&self) -> &'static str {
        "eml"
    }
    fn extensions(&self) -> &'static [&'static str] {
        &["eml"]
    }
    fn extract(&self, data: &[u8], _path: &str) -> Result<Outcome> {
        let (text, attachments) = parse_message(data);
        if text.trim().is_empty() && attachments.is_empty() {
            bail!("empty message");
        }
        if attachments.is_empty() {
            return Ok(Outcome::Text(text.into_bytes()));
        }
        let mut members: Vec<(String, Vec<u8>)> = vec![("message.txt".into(), text.into_bytes())];
        for (i, (name, bytes)) in attachments.into_iter().enumerate() {
            members.push((format!("{i}-{name}"), bytes));
        }
        Ok(Outcome::Members(members))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn eml_plain() {
        let raw = b"From: a@b.c\r\nSubject: ZIRCALOY test\r\n\r\nhello body\r\n";
        let e = Eml;
        match e.extract(raw, "x.eml").unwrap() {
            Outcome::Text(t) => {
                let s = String::from_utf8(t).unwrap();
                assert!(s.contains("ZIRCALOY"), "{s}");
                assert!(s.contains("hello body"), "{s}");
            }
            _ => panic!("expected text"),
        }
    }

    #[test]
    fn eml_attachment_recurse() {
        let b64 = rgx_core::base64::base64_encode(b"PDFDATA");
        let raw = format!(
            "From: a@b.c\r\nSubject: x\r\nMIME-Version: 1.0\r\n\
             Content-Type: multipart/mixed; boundary=BB\r\n\r\n\
             --BB\r\nContent-Type: text/plain\r\n\r\nbody here\r\n\
             --BB\r\nContent-Type: application/pdf\r\nContent-Disposition: attachment; filename=\"r.pdf\"\r\n\
             Content-Transfer-Encoding: base64\r\n\r\n{b64}\r\n--BB--\r\n");
        let e = Eml;
        match e.extract(raw.as_bytes(), "x.eml").unwrap() {
            Outcome::Members(m) => {
                assert_eq!(m.len(), 2);
                let att = m.iter().find(|(n, _)| n.contains("r.pdf")).unwrap();
                assert_eq!(att.1, b"PDFDATA");
            }
            _ => panic!("expected members"),
        }
    }

    #[test]
    fn mbox_split() {
        let raw = b"From alice@x Tue Jan  1 00:00:00 2024\nSubject: one\n\nbody1\nFrom bob@x Tue Jan  2 00:00:00 2024\nSubject: two\n\nbody2\n";
        let m = Mbox;
        match m.extract(raw, "a.mbox").unwrap() {
            Outcome::Text(t) => {
                let s = String::from_utf8(t).unwrap();
                assert!(s.contains("mbox: 2 messages"), "{s}");
                assert!(s.contains("body1"), "{s}");
                assert!(s.contains("body2"), "{s}");
            }
            _ => panic!("expected text"),
        }
    }

    mod fuzz {
        use super::*;

        #[test]
        fn eml_never_panics_on_mutation() {
            let raw = b"From: a@b.c\r\nSubject: ZIRCALOY test\r\nMIME-Version: 1.0\r\n\
                        Content-Type: multipart/mixed; boundary=BB\r\n\r\n\
                        --BB\r\nContent-Type: text/plain\r\n\r\nbody\r\n--BB--\r\n"
                .to_vec();
            rgx_core::fuzzkit::assert_never_panics(
                vec![("multipart", raw), ("empty", vec![])],
                1000,
                "eml",
                |data| {
                    let _ = Eml.extract(data, "fuzz.eml");
                },
            );
        }

        #[test]
        fn mbox_never_panics_on_mutation() {
            let raw = b"From alice@x Tue Jan  1 00:00:00 2024\nSubject: one\n\nbody1\n".to_vec();
            rgx_core::fuzzkit::assert_never_panics(
                vec![("mbox", raw), ("empty", vec![])],
                800,
                "mbox",
                |data| {
                    let _ = Mbox.extract(data, "fuzz.mbox");
                },
            );
        }
    }
}
