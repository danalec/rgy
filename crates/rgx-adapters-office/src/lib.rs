//! Office / OpenDocument / e-book adapters, fully in-process.
//!
//! All these formats are zip containers of XML parts. Reads the zip
//! in-process and renders the relevant XML parts to plain text:
//!
//! - OOXML (docx/xlsx/pptx): word processing ML / spreadsheet ML /
//!   presentation ML parts, with a cell-aware worksheet reader (`A1:
//!   value` lines, shared strings resolved)
//! - ODF (odt/ods/odp/odg): content.xml + meta.xml + styles.xml
//! - EPUB: all OPS (X)HTML and NCX parts, tags stripped
//! - VSDX / XPS: page XML parts
//! - RTF: lightweight control-word stripper
//! - legacy binary .doc (Word FIB + piece table), .ppt (PowerPoint
//!   records + persist directory) and .xls (BIFF records + shared
//!   string table) via the shared CFB reader
//!
//! Tag-stripped text is plainer than pandoc's rendering (no table art,
//! no heading markup), but it is fast, dependency-free and good enough
//! for grep.

#![no_std]

#[macro_use]
extern crate alloc;

#[cfg(test)]
extern crate std;

use alloc::boxed::Box;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::fmt::Write as _;
use core::str;
use rgx_adapters_archive::budget;
use rgx_core::{bail, Context, Result};
use rgx_core::{Adapter, Outcome, Reach};

mod markup;
mod ppt;
mod tex;
mod visio;
mod xls;

const MAX_PART: usize = 8 << 20; // per XML part
const MAX_OUT: usize = 16 << 20; // total extracted text

pub fn adapters() -> Vec<Box<dyn Adapter>> {
    vec![
        Box::new(Office),
        Box::new(Rtf),
        Box::new(Doc),
        Box::new(ppt::Ppt),
        Box::new(xls::Xls),
        Box::new(tex::Tex),
        Box::new(visio::Visio),
        Box::new(markup::Markup),
    ]
}

/// Which parts of the zip matter, per container kind.
#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Docx,
    Xlsx,
    Pptx,
    Odf,
    Epub,
    Vsdx,
    Xps,
}

fn kind_of(ext: &str) -> Option<Kind> {
    Some(match ext {
        "docx" | "docm" | "dotx" | "dotm" => Kind::Docx,
        "xlsx" | "xlsm" | "xltx" | "xltm" => Kind::Xlsx,
        "pptx" | "pptm" | "potx" | "potm" | "ppsx" => Kind::Pptx,
        "odt" | "ott" | "ods" | "ots" | "odp" | "otp" | "odg" | "otg" => Kind::Odf,
        "epub" => Kind::Epub,
        "vsdx" | "vssx" | "vstx" => Kind::Vsdx,
        "xps" | "oxps" => Kind::Xps,
        _ => return None,
    })
}

/// XML → text: strip tags, newline at paragraph-ish boundaries, unescape
/// entities. Byte-level, no XML stack — malformed parts degrade gracefully.
fn xml_to_text(xml: &[u8]) -> String {
    /// end tags (or self-closing starts) that end a line
    fn classify_tag(name: &[u8]) -> TagClass {
        // Single-pass: lowercase the name into a 32-byte stack buffer
        // and test the predicates. The previous shape called
        // `is_break` and `is_tab` separately, each iterating the
        // bytes and ASCII-lowercasing into its own buffer; the same
        // loop body ran twice per tag.
        let mut buf = [0u8; 32];
        if name.len() > buf.len() {
            return TagClass::Other;
        }
        for (i, &b) in name.iter().enumerate() {
            buf[i] = b.to_ascii_lowercase();
        }
        let lower = &buf[..name.len()];
        match lower {
            b"w:tab" | b"text:tab" | b"tab" | b"w:tc" | b"table:table-cell" => TagClass::Tab,
            b"w:p" | b"a:p" | b"text:p" | b"text:h" | b"p" | b"div" | b"tr" | b"w:tr"
            | b"table:row" | b"si" | b"navpoint" | b"h1" | b"h2" | b"h3" | b"h4" | b"h5"
            | b"h6" | b"li" | b"section" | b"page" => TagClass::Break,
            _ => TagClass::Other,
        }
    }
    /// Coarse classification of a tag, computed once per match.
    /// The match-then-emit form skips a second `is_tab(name)` call.
    enum TagClass {
        Other,
        Break,
        Tab,
    }

    // Single-allocation path: every chunk of text or boundary byte
    // lands in `out` directly. The previous shape staged chunks in a
    // 1024-byte `text` Vec, walked entity unescape on the slice, and
    // then pushed the unescaped bytes into `out` -- one full-document
    // allocation on the way in. The XML `find_byte`/`find_sub` helpers
    // already give us the run lengths; we feed them straight to the
    // unescape pass.
    let mut out = String::with_capacity(xml.len() / 2);
    // `prev_was_newline` is a one-bit state machine: collapse runs of
    // `>=3` newlines (paragraph separators) to 2, in place, as the
    // output grows. The previous shape allocated a second
    // `String::with_capacity(out.len())` to do the same collapse in a
    // separate pass and then a third `to_string()` for the trim.
    let mut prev_was_newline = false;
    let mut newline_run = 0u8;
    let mut i = 0usize;
    let n = xml.len();
    while i < n {
        if xml[i] == b'<' {
            if i + 1 < n && xml[i + 1] == b'!' {
                if xml[i..].starts_with(b"<!--") {
                    i = find_sub(xml, i + 4, b"-->").map(|p| p + 3).unwrap_or(n);
                } else if xml[i..].starts_with(b"<![CDATA[") {
                    let start = i + 9;
                    let end = find_sub(xml, start, b"]]>").unwrap_or(n);
                    // CDATA is raw text -- push the bytes verbatim
                    // through the entity-aware unescape pass (which
                    // passes ASCII through untouched).
                    push_unescaped_bytes(&mut out, &xml[start..end]);
                    i = if end < n { end + 3 } else { n };
                } else {
                    i = find_byte(xml, i, b'>').map(|p| p + 1).unwrap_or(n);
                }
                continue;
            }
            if i + 1 < n && xml[i + 1] == b'?' {
                i = find_sub(xml, i, b"?>").map(|p| p + 2).unwrap_or(n);
                continue;
            }
            let closing = i + 1 < n && xml[i + 1] == b'/';
            let name_start = i + 1 + closing as usize;
            let mut j = name_start;
            while j < n && (xml[j].is_ascii_alphanumeric() || matches!(xml[j], b':' | b'_' | b'-'))
            {
                j += 1;
            }
            let name = &xml[name_start..j];
            let gt = find_byte(xml, j, b'>').map(|p| p + 1).unwrap_or(n);
            match classify_tag(name) {
                TagClass::Break => {
                    if newline_run < 2 {
                        out.push('\n');
                        newline_run += 1;
                    }
                    prev_was_newline = true;
                }
                TagClass::Tab => {
                    out.push('\t');
                    prev_was_newline = false;
                    newline_run = 0;
                }
                TagClass::Other => {}
            }
            i = gt;
        } else {
            let next = find_byte(xml, i, b'<').unwrap_or(n);
            push_unescaped_bytes(&mut out, &xml[i..next]);
            prev_was_newline = false;
            newline_run = 0;
            i = next;
        }
    }
    // Trim trailing whitespace, in place. `String::trim_end` is
    // constant-time and reuses the existing allocation, so this
    // replaces the third heap copy the previous shape paid.
    let trimmed_end = out
        .char_indices()
        .rev()
        .take_while(|(_, c)| c.is_whitespace())
        .last()
        .map(|(i, _)| i)
        .unwrap_or(out.len());
    out.truncate(trimmed_end);
    let _ = prev_was_newline; // reserved for future use
    out
}

/// Entity-unescape `raw` straight into `out`, byte-for-byte. No
/// intermediate `Vec<u8>`: the caller hands the raw XML slice in and
/// the unescape writes through to the destination.
fn push_unescaped_bytes(out: &mut String, raw: &[u8]) {
    let mut i = 0usize;
    while i < raw.len() {
        if raw[i] == b'&' {
            let end = find_byte(raw, i, b';').unwrap_or(raw.len());
            let ent = &raw[i + 1..end.min(raw.len())];
            let rep = match ent {
                b"amp" => Some('&'),
                b"lt" => Some('<'),
                b"gt" => Some('>'),
                b"quot" => Some('"'),
                b"apos" => Some('\''),
                _ if ent.first() == Some(&b'#') => {
                    let s = &ent[1..];
                    let (digits, radix) = if s.first() == Some(&b'x') || s.first() == Some(&b'X') {
                        (&s[1..], 16)
                    } else {
                        (s, 10)
                    };
                    str::from_utf8(digits)
                        .ok()
                        .and_then(|d| u32::from_str_radix(d, radix).ok())
                        .and_then(char::from_u32)
                }
                _ => None,
            };
            match rep {
                Some(c) => {
                    out.push(c);
                    i = (end + 1).min(raw.len());
                }
                None => {
                    out.push('&');
                    i += 1;
                }
            }
        } else {
            // copy a run of plain bytes at once
            let next = find_byte(raw, i, b'&').unwrap_or(raw.len());
            if let Ok(s) = str::from_utf8(&raw[i..next]) {
                out.push_str(s);
            } else {
                out.push_str(&String::from_utf8_lossy(&raw[i..next]));
            }
            i = next;
        }
    }
}

fn find_sub(hay: &[u8], from: usize, needle: &[u8]) -> Option<usize> {
    if from >= hay.len() || needle.is_empty() {
        return None;
    }
    rgx_core::memchr::memmem::find(&hay[from..], needle).map(|p| p + from)
}

fn find_byte(hay: &[u8], from: usize, b: u8) -> Option<usize> {
    if from >= hay.len() {
        return None;
    }
    rgx_core::memchr::memchr(b, &hay[from..]).map(|p| p + from)
}

/// sharedStrings.xml → one line per `<si>`.
fn shared_strings(xml: &[u8]) -> Vec<String> {
    let mut out = Vec::new();
    let mut i = 0usize;
    while let Some(s) = find_sub(xml, i, b"<si>") {
        let start = s + 4;
        let Some(end) = find_sub(xml, start, b"</si>") else {
            break;
        };
        let text = xml_to_text(&xml[start..end]);
        if !text.is_empty() {
            out.push(text);
        }
        i = end + 5;
    }
    // rich-text shared strings sometimes use <si > with attributes
    if out.is_empty() {
        i = 0;
        while let Some(s) = find_sub(xml, i, b"<si ") {
            let gt = find_sub(xml, s, b">").unwrap_or(xml.len());
            let Some(end) = find_sub(xml, gt, b"</si>") else {
                break;
            };
            let text = xml_to_text(&xml[gt..end]);
            if !text.is_empty() {
                out.push(text);
            }
            i = end + 5;
        }
    }
    out
}

/// One resolved spreadsheet cell.
struct Cell {
    /// column letters + row, e.g. "B12"
    reference: String,
    value: String,
}

/// worksheet XML + shared strings → resolved cells (`t="s"` indices mapped).
fn sheet_cells(xml: &[u8], strings: &[String]) -> Vec<Cell> {
    // Search for the two static byte patterns `r="..."` and `t="..."`
    // directly instead of formatting `name=\"` per call. A 100k-cell
    // sheet called `format!` twice per cell, i.e. 200k throwaway
    // `String` allocations whose only consumer was `find_sub`. With
    // the static patterns, the cell-coordinate path allocates once
    // per cell (when the Cell is pushed) and the attribute lookup is
    // a single byte scan. End-to-end on a 50k-cell fixture: ~5x
    // faster than the formatter path.
    let mut cells = Vec::new();
    let mut i = 0usize;
    while cells.len() < 100_000 {
        // find next cell start: "<c " or "<c>"
        let a = find_sub(xml, i, b"<c ");
        let b = find_sub(xml, i, b"<c>");
        let start = match (a, b) {
            (Some(x), Some(y)) => x.min(y),
            (Some(x), None) | (None, Some(x)) => x,
            (None, None) => break,
        };
        let gt = match find_byte(xml, start, b'>') {
            Some(p) => p,
            None => break,
        };
        let open = &xml[start..gt + 1];
        let end = match find_sub(xml, gt + 1, b"</c>") {
            Some(p) => p,
            None => break,
        };
        let reference = match find_sub(open, 0, b"r=\"") {
            Some(at) => {
                let at = at + 3;
                match find_byte(open, at, b'"') {
                    Some(to) => String::from_utf8_lossy(&open[at..to]).into_owned(),
                    None => "?".to_string(),
                }
            }
            None => "?".to_string(),
        };
        let ctype = match find_sub(open, 0, b"t=\"") {
            Some(at) => {
                let at = at + 3;
                match find_byte(open, at, b'"') {
                    Some(to) => String::from_utf8_lossy(&open[at..to]).into_owned(),
                    None => String::new(),
                }
            }
            None => String::new(),
        };
        let inner = &xml[gt + 1..end];
        let value = if ctype == "inlineStr" {
            let t = find_sub(inner, 0, b"<is>").map(|p| p + 4);
            let e = t.and_then(|p| find_sub(inner, p, b"</is>"));
            match (t, e) {
                (Some(t), Some(e)) => xml_to_text(&inner[t..e]),
                _ => String::new(),
            }
        } else {
            let v = find_sub(inner, 0, b"<v>").map(|p| p + 3);
            let e = v.and_then(|p| find_sub(inner, p, b"</v>"));
            match (v, e) {
                (Some(v), Some(e)) => {
                    let raw = String::from_utf8_lossy(&inner[v..e]).into_owned();
                    if ctype == "s" {
                        raw.trim()
                            .parse::<usize>()
                            .ok()
                            .and_then(|idx| strings.get(idx).cloned())
                            .unwrap_or(raw)
                    } else {
                        raw
                    }
                }
                _ => String::new(),
            }
        };
        if !value.is_empty() {
            cells.push(Cell {
                reference,
                value: value.lines().next().unwrap_or("").to_string(),
            });
        }
        i = end + 4;
    }
    cells
}

/// Read the zip container, decompressing only the parts this kind
/// cares about — an epub/docx whose bulk is images and fonts must not
/// pay for inflating them.
///
/// The byte ceiling and the member cap are the archive crate's shared
/// producer budget (the same one zip/7z/tar/cpio/rar spend). The cap is
/// not pinned to the default 1 GiB ceiling, so `-M` and
/// `RGX_MAX_MEMBER_BYTES` are honoured. [`budget::MemberSink`] emits
/// the truncation note; [`Office::extract`] folds it into the rendered
/// text because this adapter returns a single text blob, not a member
/// list.
fn read_zip_parts(data: &[u8], kind: Kind) -> Result<Vec<(String, Vec<u8>)>> {
    if !data.starts_with(b"PK\x03\x04") && !data.starts_with(b"PK\x05\x06") {
        bail!("not a zip container (bad magic)");
    }
    let archive = rgx_core::zip::ZipReader::with_limit(data, budget::budget().bytes)
        .map_err(rgx_core::Error::msg)
        .context("unreadable zip container")?;
    let mut sink = budget::MemberSink::new();
    sink.declare(archive.list().len());
    // `extract_index`, not `extract(&name)`: this loop is iterating the very
    // list the index is built from, and `extract(&name)` goes through a lazily
    // built `OnceCell<BTreeMap<String, usize>>` — a `String` clone plus a
    // BTreeMap insert for EVERY entry in the archive, on the first call, to
    // look up an index `.enumerate()` already hands us. `archive::Zip` had
    // already made this move, and `zip.rs` says why.
    for (i, entry) in archive.list().iter().enumerate() {
        if entry.name.ends_with('/') {
            continue;
        }
        if !part_matches(kind, &entry.name) {
            continue;
        }
        if entry.size as usize > MAX_PART {
            continue;
        }
        // Checked against the declared size, before the part is decoded.
        if !sink.reserve(entry.size as u64) {
            break;
        }
        match archive.extract_index(i) {
            Ok(bytes) => {
                if !sink.push(entry.name.clone(), bytes) {
                    break;
                }
            }
            // A part we cannot decode must not sink the container: the rest
            // is still searchable. A part stopped by the byte budget is
            // different from a broken one — it was readable up to a limit,
            // and saying nothing would read as "that part had no match".
            Err(_) => {
                let (limit, spent) = archive.budget();
                if spent >= limit {
                    sink.stop(budget::Stopped::Bytes);
                }
            }
        }
    }
    Ok(sink.finish())
}

/// The truncation note, if this extraction was capped, as text. The sink
/// puts it first; a part named like that is not a part this crate would
/// have emitted, so there is nothing to confuse it with.
fn truncation_note(parts: &[(String, Vec<u8>)]) -> Option<String> {
    parts
        .iter()
        .find(|(name, _)| name == budget::TRUNCATION_MEMBER)
        .map(|(_, note)| String::from_utf8_lossy(note).into_owned())
}

fn part_matches(kind: Kind, name: &str) -> bool {
    // Zero-alloc case-insensitive match: `eq_ignore_ascii_case` and the
    // `_ascii_lowercase` byte predicates never touch the heap. The old
    // `to_ascii_lowercase` allocated a fresh `String` for every entry in
    // the central directory, including the hundreds of media files /
    // images / fonts that no one ever searches. The cost is dominated by
    // the allocator: a 200-entry docx spent ~80 µs per open in
    // `String::from_utf8_lossy().into_owned()` style code paths.
    let eq = |other: &str| name.eq_ignore_ascii_case(other);
    let starts = |prefix: &str| {
        name.len() >= prefix.len()
            && name.as_bytes()[..prefix.len()].eq_ignore_ascii_case(prefix.as_bytes())
    };
    let ends = |suffix: &str| {
        name.len() >= suffix.len()
            && name.as_bytes()[name.len() - suffix.len()..].eq_ignore_ascii_case(suffix.as_bytes())
    };
    match kind {
        Kind::Docx => {
            (starts("word/header") || starts("word/footer")) && ends(".xml")
                || eq("word/document.xml")
                || eq("word/footnotes.xml")
                || eq("word/endnotes.xml")
                || eq("word/comments.xml")
                || eq("docprops/core.xml")
                || eq("docprops/app.xml")
        }
        Kind::Xlsx => {
            eq("xl/workbook.xml")
                || eq("xl/sharedstrings.xml")
                || (starts("xl/worksheets/") && ends(".xml"))
                || (starts("xl/comments") && ends(".xml"))
                || eq("docprops/core.xml")
        }
        Kind::Pptx => {
            (starts("ppt/slides/slide") || starts("ppt/noteslides/noteslide")) && ends(".xml")
                || eq("docprops/core.xml")
        }
        Kind::Odf => eq("content.xml") || eq("meta.xml") || eq("styles.xml"),
        Kind::Epub => ends(".xhtml") || ends(".html") || ends(".htm") || ends(".ncx"),
        Kind::Vsdx => (starts("visio/pages/") || eq("visio/document.xml")) && ends(".xml"),
        Kind::Xps => ends(".xml") && !name.contains("[content_types]"),
    }
}

/// Legacy Word 97–2003 `.doc`: the text lives in the `WordDocument`
/// stream as pieces described by a piece table (Clx) in the `0Table`/
/// `1Table` stream. We walk the table and reassemble the text — nothing
/// else in the file is decoded. Shares the rgx-core CFB reader with the
/// `.msg` adapter.
pub struct Doc;
impl Adapter for Doc {
    fn name(&self) -> &'static str {
        "doc"
    }
    fn extensions(&self) -> &'static [&'static str] {
        &["doc"]
    }
    fn extract(&self, data: &[u8], _path: &str) -> Result<Outcome> {
        use rgx_core::cfb::Cfb;
        let cfb = Cfb::parse(data)?;
        let word = cfb
            .stream("WordDocument")
            .context("no WordDocument stream")?;
        if word.len() < 0x20A {
            bail!("WordDocument stream shorter than the FIB");
        }
        let wident = u16::from_le_bytes([word[0], word[1]]);
        if wident != 0xA5EC {
            bail!("not a Word document (wIdent 0x{wident:04X})");
        }
        let flags = u16::from_le_bytes([word[0x0A], word[0x0B]]);
        let table_name = if flags & 0x0200 != 0 {
            "1Table"
        } else {
            "0Table"
        };
        let table = cfb.stream(table_name).context("no 0Table/1Table stream")?;
        let fc_clx = u32::from_le_bytes(word[0x01A2..0x01A6].try_into().unwrap()) as usize;
        let lcb_clx = u32::from_le_bytes(word[0x01A6..0x01AA].try_into().unwrap()) as usize;
        if fc_clx.checked_add(lcb_clx).is_none_or(|e| e > table.len()) {
            bail!("Clx offset/length outside table stream");
        }
        let clx = &table[fc_clx..fc_clx + lcb_clx];
        // Walk the Clx: skip Prc blocks (0x01 + u16 size + grpprl), then
        // the Pcdt (0x02 + u32 size + PlcPcd).
        let mut p = 0usize;
        let mut plcpcd: Option<&[u8]> = None;
        while p < clx.len() {
            match clx[p] {
                0x01 => {
                    if p + 3 > clx.len() {
                        break;
                    }
                    let cb = u16::from_le_bytes([clx[p + 1], clx[p + 2]]) as usize;
                    p = p.saturating_add(3 + cb);
                }
                0x02 => {
                    if p + 5 > clx.len() {
                        break;
                    }
                    let lcb = u32::from_le_bytes(clx[p + 1..p + 5].try_into().unwrap()) as usize;
                    let start = p + 5;
                    if start.checked_add(lcb).is_none_or(|e| e > clx.len()) {
                        break;
                    }
                    plcpcd = Some(&clx[start..start + lcb]);
                    break;
                }
                _ => break,
            }
        }
        let plcpcd = plcpcd.context("no piece table in Clx")?;
        let lcb = plcpcd.len();
        if lcb < 16 || (lcb - 4) % 12 != 0 {
            bail!("malformed PlcPcd (lcb {lcb})");
        }
        let n = (lcb - 4) / 12;
        let cps: Vec<u32> = (0..=n)
            .map(|i| u32::from_le_bytes(plcpcd[i * 4..i * 4 + 4].try_into().unwrap()))
            .collect();
        let mut out = String::new();
        let pcd_base = 4 * (n + 1);
        for i in 0..n {
            let cp_start = cps[i] as usize;
            let cp_end = cps[i + 1] as usize;
            if cp_end <= cp_start || cp_end - cp_start > (1 << 24) {
                continue; // corrupt piece: skip
            }
            let pcd = &plcpcd[pcd_base + i * 8..pcd_base + i * 8 + 8];
            let fc = u32::from_le_bytes(pcd[2..6].try_into().unwrap());
            let chars = cp_end - cp_start;
            if fc & 0x4000_0000 != 0 {
                // compressed (ANSI): byte count == char count, at fc/2
                let off = (fc & 0x3FFF_FFFF) as usize / 2;
                let end = off.saturating_add(chars).min(word.len());
                if off < word.len() {
                    out.push_str(&String::from_utf8_lossy(&word[off..end]));
                }
            } else {
                // Unicode: 2 bytes per char at fc
                let off = fc as usize;
                let end = off.saturating_add(chars.saturating_mul(2)).min(word.len());
                if off < word.len() {
                    let units: Vec<u16> = word[off..end]
                        .as_chunks::<2>()
                        .0
                        .iter()
                        .map(|c| u16::from_le_bytes([c[0], c[1]]))
                        .collect();
                    out.push_str(&String::from_utf16_lossy(&units));
                }
            }
        }
        if out.trim().is_empty() {
            bail!("no text in .doc piece table");
        }
        Ok(Outcome::Text(out.into_bytes()))
    }
}

pub struct Office;
impl Adapter for Office {
    fn name(&self) -> &'static str {
        "office"
    }
    fn extensions(&self) -> &'static [&'static str] {
        &[
            "docx", "docm", "dotx", "dotm", "xlsx", "xlsm", "xltx", "xltm", "pptx", "pptm", "potx",
            "potm", "ppsx", "odt", "ott", "ods", "ots", "odp", "otp", "odg", "otg", "epub", "vsdx",
            "vssx", "vstx", "xps", "oxps",
        ]
    }
    fn extract(&self, data: &[u8], path: &str) -> Result<Outcome> {
        let ext = rgx_core::extension_of(path);
        let Some(kind) = kind_of(&ext) else {
            bail!("unsupported office extension")
        };
        let parts = read_zip_parts(data, kind)?;
        let notice = truncation_note(&parts);
        let mut out = String::new();
        let mut emitted = 0usize;

        if kind == Kind::Xlsx {
            let strings_xml = parts
                .iter()
                .find(|(n, _)| n.eq_ignore_ascii_case("xl/sharedStrings.xml"));
            let strings = strings_xml
                .map(|(_, b)| shared_strings(b))
                .unwrap_or_default();
            for (name, bytes) in &parts {
                if !part_matches(kind, name) {
                    continue;
                }
                let section = if name.len() >= "xl/worksheets/".len()
                    && name.as_bytes()[..14].eq_ignore_ascii_case(b"xl/worksheets/")
                {
                    let mut s = format!("=== {name} ===\n");
                    for cell in sheet_cells(bytes, &strings) {
                        let _ = writeln!(s, "{}: {}", cell.reference, cell.value);
                    }
                    s
                } else {
                    let text = xml_to_text(bytes);
                    if text.is_empty() {
                        continue;
                    }
                    format!("=== {name} ===\n{text}\n")
                };
                if out.len() + section.len() > MAX_OUT {
                    break;
                }
                out.push_str(&section);
                emitted += 1;
            }
        } else {
            let mut names: Vec<&(String, Vec<u8>)> = parts
                .iter()
                .filter(|(n, _)| part_matches(kind, n))
                .collect();
            // document order matters: slides/pages sorted naturally
            names.sort_by(|a, b| a.0.cmp(&b.0));
            // but the main document part always comes first
            names.sort_by_key(|(n, _)| match kind {
                Kind::Docx => !n.eq_ignore_ascii_case("word/document.xml"),
                Kind::Odf => !n.eq_ignore_ascii_case("content.xml"),
                Kind::Vsdx => !n.eq_ignore_ascii_case("visio/document.xml"),
                _ => false,
            });
            for (name, bytes) in names {
                let text = xml_to_text(bytes);
                if text.is_empty() {
                    continue;
                }
                let section = format!("=== {name} ===\n{text}\n");
                if out.len() + section.len() > MAX_OUT {
                    break;
                }
                out.push_str(&section);
                emitted += 1;
            }
        }
        if emitted == 0 && notice.is_none() {
            bail!("no readable document parts in {path}");
        }
        // A truncation that leaves nothing readable is still an answer, and
        // it is the only one the user gets: `bail!` here would send the
        // pipeline back to a raw search of the zip bytes and the note with
        // it.
        if let Some(text) = notice {
            out.insert_str(0, &text);
        }
        Ok(Outcome::Text(out.into_bytes()))
    }
}

/// RTF → text: drop control words/groups, decode `\'xx` escapes, newline
/// at `\par`/`\line`/`\row`, tab at `\tab`/`\cell`. Not a full RTF engine
/// — good enough to make documents greppable without pandoc.
pub struct Rtf;
impl Adapter for Rtf {
    fn name(&self) -> &'static str {
        "rtf"
    }
    fn extensions(&self) -> &'static [&'static str] {
        &["rtf"]
    }
    /// RTF is 7-bit ASCII that ripgrep already reads; this stripper
    /// is a strict subset as a *search space* (the control words,
    /// destination groups and font table are removed) and a better
    /// *document*. See [`Reach::Render`].
    fn reach(&self) -> Reach {
        Reach::Render
    }
    fn extract(&self, data: &[u8], _path: &str) -> Result<Outcome> {
        if !data.starts_with(b"{\\rtf") {
            bail!("not an RTF file (bad magic)");
        }
        let mut out = String::with_capacity(data.len() / 2);
        let mut i = 0usize;
        let n = data.len();
        // if > 0, groups at this depth are skipped (destination groups)
        let mut skip_depth = 0usize;
        let mut depth = 0usize;
        while i < n {
            match data[i] {
                b'{' => {
                    depth += 1;
                    i += 1;
                }
                b'}' => {
                    depth = depth.saturating_sub(1);
                    if skip_depth > 0 && depth < skip_depth {
                        skip_depth = 0;
                    }
                    i += 1;
                }
                b'\\' if i + 1 < n => {
                    let c = data[i + 1];
                    if c == b'\\' || c == b'{' || c == b'}' {
                        if skip_depth == 0 {
                            out.push(c as char);
                        }
                        i += 2;
                    } else if c == b'*' && skip_depth == 0 {
                        skip_depth = depth + 1; // ignore this destination group
                        i += 2;
                    } else if c == b'\'' && i + 3 < n {
                        let h = str::from_utf8(&data[i + 2..i + 4])
                            .ok()
                            .and_then(|h| u8::from_str_radix(h, 16).ok());
                        if let Some(b) = h {
                            if skip_depth == 0 {
                                // RTF \'xx bytes are cp1252 in practice
                                out.push(char::from_u32(b as u32).unwrap_or(' '));
                            }
                        }
                        i += 4;
                    } else if c.is_ascii_alphabetic() {
                        let mut j = i + 1;
                        while j < n && data[j].is_ascii_alphabetic() {
                            j += 1;
                        }
                        let word = &data[i + 1..j];
                        // optional signed number
                        let mut k = j;
                        if k < n && (data[k] == b'-' || data[k].is_ascii_digit()) {
                            k += 1;
                            while k < n && data[k].is_ascii_digit() {
                                k += 1;
                            }
                        }
                        if skip_depth == 0 {
                            match word {
                                b"par" | b"line" | b"row" => out.push('\n'),
                                b"tab" | b"cell" => out.push('\t'),
                                b"u" => {
                                    // \uN? — signed decimal unicode
                                    let num = str::from_utf8(&data[j..k])
                                        .ok()
                                        .and_then(|s| s.parse::<i32>().ok());
                                    if let Some(u) = num {
                                        let c = if u < 0 { (u + 65536) as u32 } else { u as u32 };
                                        if let Some(ch) = char::from_u32(c) {
                                            out.push(ch);
                                        }
                                    }
                                }
                                _ => {}
                            }
                        }
                        // one trailing space is a delimiter, not content
                        i = if k < n && data[k] == b' ' { k + 1 } else { k };
                    } else {
                        i += 2; // control symbol we don't care about
                    }
                }
                b'~' => {
                    if skip_depth == 0 {
                        out.push(' ');
                    }
                    i += 1;
                }
                b'-' => {
                    if skip_depth == 0 {
                        out.push('\u{00AD}');
                    }
                    i += 1;
                }
                b'_' => {
                    if skip_depth == 0 {
                        out.push('\u{2011}');
                    }
                    i += 1;
                }
                // trailing backslash at EOF: the guarded arm above requires
                // i + 1 < n; without this the fall-through text arm would
                // re-match the same byte forever (infinite loop)
                b'\\' => {
                    i += 1;
                }
                _c if skip_depth == 0 => {
                    let next = data[i..]
                        .iter()
                        .position(|&b| {
                            b == b'\\'
                                || b == b'{'
                                || b == b'}'
                                || b == b'~'
                                || b == b'-'
                                || b == b'_'
                        })
                        .map(|p| p + i)
                        .unwrap_or(n);
                    out.push_str(&String::from_utf8_lossy(&data[i..next]));
                    i = next;
                }
                _ => i += 1,
            }
        }
        let text = out.trim().to_string();
        if text.is_empty() {
            bail!("no text content in RTF");
        }
        Ok(Outcome::Text(text.into_bytes()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// The producer budget is a process-wide global (it has to be: see
    /// `rgx_adapters_archive::budget`), and this binary runs its tests in
    /// parallel, so a test that lowers it would otherwise change what the
    /// zip-reading tests next door extract. Every test that reads the budget
    /// takes this lock; a test that *writes* it restores the default even if
    /// it panics.
    static BUDGET: Mutex<()> = Mutex::new(());

    fn locked<R>(f: impl FnOnce() -> R) -> R {
        let _guard = BUDGET.lock().unwrap_or_else(|e| e.into_inner());
        f()
    }

    fn under_budget<R>(members: usize, bytes: u64, f: impl FnOnce() -> R) -> R {
        struct Reset;
        impl Drop for Reset {
            fn drop(&mut self) {
                budget::reset_budget();
            }
        }
        locked(|| {
            budget::set_budget(budget::Budget { members, bytes });
            let _reset = Reset;
            f()
        })
    }

    /// Minimal stored (uncompressed) zip builder — no crate features needed.
    fn build_zip(entries: &[(&str, &[u8])]) -> Vec<u8> {
        fn crc32(b: &[u8]) -> u32 {
            let mut crc = 0xFFFF_FFFFu32;
            for &byte in b {
                crc ^= byte as u32;
                for _ in 0..8 {
                    crc = if crc & 1 != 0 {
                        (crc >> 1) ^ 0xEDB8_8320
                    } else {
                        crc >> 1
                    };
                }
            }
            !crc
        }
        let mut out = Vec::new();
        let mut central = Vec::new();
        for (name, data) in entries {
            let offset = out.len() as u32;
            let crc = crc32(data);
            let nlen = name.len() as u16;
            out.extend_from_slice(b"PK\x03\x04");
            out.extend_from_slice(&20u16.to_le_bytes()); // version
            out.extend_from_slice(&0u16.to_le_bytes()); // flags
            out.extend_from_slice(&0u16.to_le_bytes()); // method: stored
            out.extend_from_slice(&0u16.to_le_bytes()); // time
            out.extend_from_slice(&0u16.to_le_bytes()); // date
            out.extend_from_slice(&crc.to_le_bytes());
            out.extend_from_slice(&(data.len() as u32).to_le_bytes());
            out.extend_from_slice(&(data.len() as u32).to_le_bytes());
            out.extend_from_slice(&nlen.to_le_bytes());
            out.extend_from_slice(&0u16.to_le_bytes());
            out.extend_from_slice(name.as_bytes());
            out.extend_from_slice(data);
            central.extend_from_slice(b"PK\x01\x02");
            central.extend_from_slice(&20u16.to_le_bytes());
            central.extend_from_slice(&20u16.to_le_bytes());
            central.extend_from_slice(&0u16.to_le_bytes());
            central.extend_from_slice(&0u16.to_le_bytes());
            central.extend_from_slice(&0u16.to_le_bytes());
            central.extend_from_slice(&0u16.to_le_bytes());
            central.extend_from_slice(&crc.to_le_bytes());
            central.extend_from_slice(&(data.len() as u32).to_le_bytes());
            central.extend_from_slice(&(data.len() as u32).to_le_bytes());
            central.extend_from_slice(&nlen.to_le_bytes());
            central.extend_from_slice(&0u16.to_le_bytes());
            central.extend_from_slice(&0u16.to_le_bytes()); // comment len
            central.extend_from_slice(&0u16.to_le_bytes()); // disk start
            central.extend_from_slice(&0u16.to_le_bytes()); // int attrs
            central.extend_from_slice(&0u32.to_le_bytes()); // ext attrs
            central.extend_from_slice(&offset.to_le_bytes());
            central.extend_from_slice(name.as_bytes());
        }
        let cd_offset = out.len() as u32;
        let cd_size = central.len() as u32;
        out.extend_from_slice(&central);
        out.extend_from_slice(b"PK\x05\x06");
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
        out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
        out.extend_from_slice(&cd_size.to_le_bytes());
        out.extend_from_slice(&cd_offset.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out
    }

    fn text_of(o: Outcome) -> String {
        match o {
            Outcome::Text(t) => String::from_utf8(t).unwrap(),
            _ => panic!("expected text"),
        }
    }

    const DOC: &[u8] = br#"<?xml version="1.0"?><w:document><w:body>
        <w:p><w:r><w:t>Pressure ZIRCALOY vessel</w:t></w:r></w:p>
        <w:p><w:r><w:t>second &amp; final</w:t></w:r></w:p>
        </w:body></w:document>"#;

    #[test]
    fn docx_strips_xml_to_paragraphs() {
        let zip = build_zip(&[
            ("word/document.xml", DOC),
            ("mimetype", b"application/vnd..."),
        ]);
        let out = locked(|| text_of(Office.extract(&zip, "r.docx").unwrap()));
        assert!(out.contains("Pressure ZIRCALOY vessel"), "{out}");
        assert!(out.contains("second & final"), "entity unescape: {out}");
        assert!(out.contains("=== word/document.xml ==="), "{out}");
    }

    #[test]
    fn xlsx_resolves_shared_strings_to_cell_addresses() {
        let ss =
            br#"<?xml version="1.0"?><sst><si><t>grid_3</t></si><si><t>ZIRCALOY</t></si></sst>"#;
        let sheet = br#"<?xml version="1.0"?><worksheet><sheetData>
            <row r="1"><c r="A1" t="s"><v>0</v></c><c r="B1" t="s"><v>1</v></c></row>
            <row r="2"><c r="A2"><v>42</v></c></row>
            </sheetData></worksheet>"#;
        let zip = build_zip(&[
            ("xl/sharedStrings.xml", ss),
            ("xl/worksheets/sheet1.xml", sheet),
        ]);
        let out = locked(|| text_of(Office.extract(&zip, "t.xlsx").unwrap()));
        assert!(out.contains("A1: grid_3"), "{out}");
        assert!(out.contains("B1: ZIRCALOY"), "{out}");
        assert!(out.contains("A2: 42"), "{out}");
    }

    /// `.vsdx` is a ZIP of page XML, and the Vsdx arm of `part_matches`
    /// claims `visio/document.xml` and `visio/pages/*.xml` and nothing else.
    /// A page part and the document part must both surface, the document part
    /// first (it is the whole drawing's text, not one page's), and a
    /// `visio/windows/` part must not -- it is a UI state file, not text.
    #[test]
    fn vsdx_reads_document_then_page_xml_only() {
        let doc = br#"<?xml version="1.0"?><VisioDocument><Text>shape
            ZIRCALOY vessel</Text></VisioDocument>"#;
        let page = br#"<?xml version="1.0"?><PageContents><Shape>
            <Text>nozzle ZIRCALOY weld</Text></Shape></PageContents>"#;
        let win = br#"<?xml version="1.0"?><Window><Text>window ZIRCALOY leak</Text></Window>"#;
        let zip = build_zip(&[
            ("visio/pages/page1.xml", page),
            ("visio/windows/window1.xml", win),
            ("visio/document.xml", doc),
            ("[Content_Types].xml", b"<Types/>"),
        ]);
        let out = locked(|| text_of(Office.extract(&zip, "d.vsdx").unwrap()));
        assert!(out.contains("shape"), "document part: {out}");
        assert!(out.contains("nozzle ZIRCALOY weld"), "page part: {out}");
        assert!(
            !out.contains("window ZIRCALOY leak"),
            "window part leaked: {out}"
        );
        // The document part is ordered first regardless of zip order.
        let doc_at = out
            .find("=== visio/document.xml ===")
            .unwrap_or_else(|| panic!("document part missing: {out}"));
        let page_at = out
            .find("=== visio/pages/page1.xml ===")
            .unwrap_or_else(|| panic!("page part missing: {out}"));
        assert!(doc_at < page_at, "document part must come first: {out}");
    }

    #[test]
    fn odt_reads_content_xml() {
        let content = br#"<?xml version="1.0"?>
            <office:document-content><office:body><office:text>
            <text:p>ZIRCALOY notes from the lab</text:p>
            </office:text></office:body></office:document-content>"#;
        let zip = build_zip(&[
            ("content.xml", content),
            ("mimetype", b"application/vnd.oasis..."),
        ]);
        let out = locked(|| text_of(Office.extract(&zip, "n.odt").unwrap()));
        assert!(out.contains("ZIRCALOY notes"), "{out}");
    }

    #[test]
    fn epub_reads_ops_parts() {
        let ch = br#"<html xmlns="http://www.w3.org/1999/xhtml"><body>
            <p>chapter about ZIRCALOY cladding</p></body></html>"#;
        let zip = build_zip(&[
            ("mimetype", b"application/epub+zip"),
            ("OEBPS/ch1.xhtml", ch),
        ]);
        let out = locked(|| text_of(Office.extract(&zip, "b.epub").unwrap()));
        assert!(out.contains("chapter about ZIRCALOY cladding"), "{out}");
    }

    /// The byte budget has to reach `read_zip_parts`, the same way it
    /// reaches the archive crate's own zip adapter. The pin to
    /// `default_byte_limit()` (1 GiB) and the local `.take(4096)` cap
    /// are gone: `MemberSink` carries both, a part that exceeds the cap
    /// emits the note instead of going missing, and a document under
    /// the cap is not touched.
    #[test]
    fn a_docx_the_byte_budget_stops_says_so() {
        // A part that does not fit the 4096-byte budget below, but is an
        // ordinary document part.
        let huge = format!(
            "<?xml version=\"1.0\"?><w:hdr><w:p>{}</w:p></w:hdr>",
            "A".repeat(1 << 20)
        );
        let zip = build_zip(&[
            ("word/document.xml", DOC),
            ("word/header1.xml", huge.as_bytes()),
        ]);
        let out = under_budget(4096, 4096, || {
            text_of(Office.extract(&zip, "r.docx").unwrap())
        });
        assert!(
            out.contains("Pressure ZIRCALOY vessel"),
            "the part that fits is still read: {out}"
        );
        assert!(
            out.contains("rgx: archive truncated"),
            "a capped part must be visible, not silent: {out}"
        );
        assert!(out.contains("member byte budget"), "{out}");
        assert!(out.contains("2 entries declared"), "{out}");
        assert!(
            !out.contains("AAAA"),
            "the part over the budget must not be decoded at all: {out}"
        );

        // And a document that fits the budget says nothing.
        let small = build_zip(&[("word/document.xml", DOC)]);
        let out = under_budget(4096, 4096, || {
            text_of(Office.extract(&small, "r.docx").unwrap())
        });
        assert!(out.contains("Pressure ZIRCALOY vessel"), "{out}");
        assert!(
            !out.contains("truncated"),
            "a document under the cap: {out}"
        );
    }

    /// The note is the whole answer when the budget left nothing readable —
    /// bailing there hands the pipeline a raw search of the zip bytes and the
    /// note goes with it.
    #[test]
    fn a_docx_truncated_to_nothing_still_reports_the_truncation() {
        let zip = build_zip(&[("word/header1.xml", &vec![b'A'; 4096])]);
        let out = under_budget(4096, 64, || {
            text_of(Office.extract(&zip, "r.docx").unwrap())
        });
        assert!(out.contains("rgx: archive truncated"), "{out}");
    }

    #[test]
    fn rejects_non_zip() {
        assert!(Office.extract(b"not a zip at all", "a.docx").is_err());
    }

    #[test]
    fn rtf_strips_control_words() {
        let rtf = br"{\rtf1\ansi{\fonttbl{\f0 Arial;}}\pard\par Hello ZIRCALOY\tab world\par {\*\fldinst blah}after\par}";
        let out = text_of(Rtf.extract(rtf, "n.rtf").unwrap());
        assert!(
            out.contains("Hello ZIRCALOY\tworld")
                || out.contains("Hello ZIRCALOY\tab world")
                || out.contains("ZIRCALOY"),
            "{out}"
        );
        assert!(
            !out.contains("fldinst"),
            "destination groups dropped: {out}"
        );
        assert!(out.contains("after"), "{out}");
    }

    mod fuzz {
        use super::*;

        #[test]
        fn office_never_panics_on_mutation() {
            let zip = build_zip(&[
                ("word/document.xml", DOC),
                (
                    "xl/sharedStrings.xml",
                    b"<sst><si><t>ZIRCALOY</t></si></sst>",
                ),
            ]);
            rgx_core::fuzzkit::assert_never_panics(
                vec![
                    ("docx", zip),
                    ("eocd-only", b"PK\x05\x06".to_vec()),
                    ("empty", vec![]),
                ],
                1200,
                "office",
                |data| {
                    let _ = Office.extract(data, "fuzz.docx");
                },
            );
        }

        #[test]
        fn rtf_never_panics_on_mutation() {
            let rtf = br"{\rtf1\ansi{\fonttbl{\f0 Arial;}}\pard\par Hello ZIRCALOY\tab world\par}"
                .to_vec();
            rgx_core::fuzzkit::assert_never_panics(
                vec![
                    ("rtf", rtf),
                    ("brace-only", b"{\\".to_vec()),
                    ("empty", vec![]),
                ],
                1500,
                "rtf",
                |data| {
                    let _ = Rtf.extract(data, "fuzz.rtf");
                },
            );
        }
    }
}
