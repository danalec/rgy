//! Video-container metadata, hand-rolled: MP4/MOV (iTunes-style `ilst`
//! atoms under moov/udta/meta) and MKV/WebM (EBML Tags under Segment).
//! Only tags/titles are extracted — never the media streams themselves.
//!
//! Canonical full implementation: `upstream::b01::io::media` (typed container
//! kind + ordered tags with raw fourcc/element IDs).

use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::fmt::Write as _;
use rgx_core::{bail, Result};
use rgx_core::{Adapter, Outcome};

const EBML_MAGIC: [u8; 4] = [0x1A, 0x45, 0xDF, 0xA3];

pub fn extract_text(data: &[u8]) -> Result<String> {
    if data.starts_with(&EBML_MAGIC) {
        mkv_tags(data)
    } else {
        mp4_tags(data)
    }
}

// ---------------------------------------------------------------- MP4

fn fourcc(b: &[u8]) -> [u8; 4] {
    [b[0], b[1], b[2], b[3]]
}

/// iTunes metadata keys we render, mapped to friendly names.
fn mp4_key_name(k: &[u8; 4]) -> Option<&'static str> {
    Some(match k {
        [0xA9, b'n', b'a', b'm'] => "title",
        [0xA9, b'A', b'R', b'T'] | [b'a', b'A', b'R', b'T'] => "artist",
        [0xA9, b'a', b'l', b'b'] => "album",
        [0xA9, b'd', b'a', b'y'] => "year",
        [0xA9, b'c', b'm', b't'] => "comment",
        [0xA9, b'g', b'e', b'n'] => "genre",
        [0xA9, b't', b'o', b'o'] => "encoder",
        [0xA9, b'w', b'r', b't'] => "composer",
        [b's', b't', b'i', b'l'] => "subtitle",
        _ => return None,
    })
}

fn is_mp4_container(ty: &[u8; 4]) -> bool {
    matches!(
        ty,
        b"moov" | b"udta" | b"trak" | b"mdia" | b"minf" | b"stbl" | b"edts" | b"dinf" | b"meta"
    )
}

fn mp4_box(data: &[u8], p: usize, end: usize) -> Option<(usize, [u8; 4], usize, usize)> {
    if p + 8 > end {
        return None;
    }
    let size = super::rd_u32be(data, p) as u64;
    let ty = fourcc(&data[p + 4..p + 8]);
    let (body, bend) = match size {
        0 => (p + 8, end),
        1 => {
            if p + 16 > end {
                return None;
            }
            let large = u64::from_be_bytes(data[p + 8..p + 16].try_into().ok()?);
            (p + 16, (p as u64 + large).min(end as u64) as usize)
        }
        s if s >= 8 => (p + 8, (p as u64 + s).min(end as u64) as usize),
        _ => return None,
    };
    if bend < body || bend > end {
        return None;
    }
    Some((body, ty, bend, if size == 1 { 16 } else { 8 }))
}

fn mp4_walk(
    data: &[u8],
    start: usize,
    end: usize,
    depth: usize,
    in_ilst_path: bool,
    tags: &mut Vec<(&'static str, String)>,
) {
    if depth > 12 {
        return;
    }
    let mut p = start;
    while let Some((body, ty, bend, _hdrlen)) = mp4_box(data, p, end) {
        if in_ilst_path {
            // children of ilst are metadata keys like ©nam
            if let Some(name) = mp4_key_name(&ty) {
                if let Some(v) = mp4_ilst_value(data, body, bend) {
                    if !v.trim().is_empty() {
                        // The name comes from a small fixed table of
                        // friendly labels, so a borrow into .rodata
                        // is enough -- the previous `name.to_string()`
                        // was a heap allocation per tag for a string
                        // the program already owns.
                        tags.push((name, v.trim().to_string()));
                    }
                }
            }
        } else if ty == *b"ilst" {
            mp4_walk(data, body, bend, depth + 1, true, tags);
        } else if ty == *b"meta" {
            // full box: 4 bytes version/flags before children
            if body + 4 <= bend {
                mp4_walk(data, body + 4, bend, depth + 1, false, tags);
            }
        } else if is_mp4_container(&ty) {
            mp4_walk(data, body, bend, depth + 1, false, tags);
        }
        if bend <= p {
            break;
        }
        p = bend;
    }
}

/// Inside a metadata key atom: first child is a `data` box whose payload
/// starts with type u32 + locale u32; type 1 = UTF-8, 2 = UTF-16BE.
fn mp4_ilst_value(data: &[u8], start: usize, end: usize) -> Option<String> {
    let (body, ty, bend, _) = mp4_box(data, start, end)?;
    if &ty != b"data" || body + 8 > bend {
        return None;
    }
    let kind = super::rd_u32be(data, body);
    let value = &data[body + 8..bend];
    Some(match kind {
        2 => {
            let units: Vec<u16> = value
                .as_chunks::<2>()
                .0
                .iter()
                .map(|c| u16::from_be_bytes([c[0], c[1]]))
                .collect();
            String::from_utf16_lossy(&units)
        }
        _ => String::from_utf8_lossy(value).into_owned(),
    })
}

fn mp4_tags(data: &[u8]) -> Result<String> {
    if data.len() < 8 {
        bail!("not a video container (too small)");
    }
    // sanity: offset-4 fourcc should be a plausible box type
    let first = fourcc(&data[4..8]);
    if !is_mp4_container(&first)
        && first != *b"ftyp"
        && first != *b"mdat"
        && first != *b"free"
        && first != *b"wide"
    {
        bail!(
            "not an MP4/MOV container (bad first box {})",
            String::from_utf8_lossy(&first)
        );
    }
    let mut tags: Vec<(&'static str, String)> = Vec::new();
    mp4_walk(data, 0, data.len(), 0, false, &mut tags);
    if tags.is_empty() {
        bail!("no ilst metadata atoms found");
    }
    let mut out = String::new();
    // Write directly via `write!` instead of `format!` + `push_str`:
    // the keys are `&'static str` so the macro expands to a single
    // write per key without the throwaway String the previous path
    // allocated per tag.
    use core::fmt::Write as _;
    for (k, v) in tags {
        let _ = writeln!(out, "{k}: {v}");
    }
    Ok(out)
}

// ---------------------------------------------------------------- MKV / EBML

/// EBML variable-length integer: leading zero bits give the width; the
/// marker bit is part of the value for IDs, masked off for sizes.
fn vint(data: &[u8], p: usize, strip_marker: bool) -> Option<(u64, usize)> {
    let b = *data.get(p)?;
    let len = if b & 0x80 != 0 {
        1
    } else if b & 0x40 != 0 {
        2
    } else if b & 0x20 != 0 {
        3
    } else if b & 0x10 != 0 {
        4
    } else if b & 0x08 != 0 {
        5
    } else if b & 0x04 != 0 {
        6
    } else if b & 0x02 != 0 {
        7
    } else if b & 0x01 != 0 {
        8
    } else {
        return None;
    };
    let mut v = b as u64;
    for k in 1..len {
        v = (v << 8) | *data.get(p + k)? as u64;
    }
    if strip_marker {
        v &= (1u64 << (7 * len)) - 1;
        if v == (1u64 << (7 * len)) - 1 {
            return Some((u64::MAX, len)); // unknown size
        }
    }
    Some((v, len))
}

const ID_SEGMENT: u64 = 0x1853_8067;
const ID_INFO: u64 = 0x1549_A966;
const ID_TITLE: u64 = 0x7BA9;
const ID_MUXINGAPP: u64 = 0x4D80;
const ID_WRITINGAPP: u64 = 0x5741;
const ID_DOCTYPE: u64 = 0x4282;
const ID_TAGS: u64 = 0x1254_A3A6;
const ID_TAG: u64 = 0x7373;
const ID_SIMPLETAG: u64 = 0x67C8;
const ID_TAGNAME: u64 = 0x45A3;
const ID_TAGSTRING: u64 = 0x4487;

fn ebml_elements(
    data: &[u8],
    start: usize,
    end: usize,
    depth: usize,
    out: &mut Vec<(String, String)>,
) {
    if depth > 16 {
        return;
    }
    let end = end.min(data.len());
    let mut p = start;
    while p + 2 <= end {
        let Some((id, ilen)) = vint(data, p, false) else {
            break;
        };
        let Some((mut size, slen)) = vint(data, p + ilen, true) else {
            break;
        };
        let body = p + ilen + slen;
        if body > end {
            break;
        }
        if size == u64::MAX {
            size = (end - body) as u64;
        }
        let bend = body.saturating_add(size as usize).min(end);
        match id {
            // EBML header (0x1A45DFA3) + the containers we care about
            0x1A45_DFA3 | ID_SEGMENT | ID_TAGS | ID_INFO => {
                ebml_elements(data, body, bend, depth + 1, out);
            }
            ID_TAG => {
                let mut name = String::new();
                let mut value = String::new();
                simple_tag(data, body, bend, depth + 1, &mut name, &mut value);
                if !name.is_empty() && !value.is_empty() {
                    out.push((name, value));
                }
            }
            ID_TITLE => out.push((
                "title".into(),
                String::from_utf8_lossy(&data[body..bend]).into_owned(),
            )),
            ID_MUXINGAPP => out.push((
                "muxingapp".into(),
                String::from_utf8_lossy(&data[body..bend]).into_owned(),
            )),
            ID_WRITINGAPP => out.push((
                "writingapp".into(),
                String::from_utf8_lossy(&data[body..bend]).into_owned(),
            )),
            ID_DOCTYPE => out.push((
                "doctype".into(),
                String::from_utf8_lossy(&data[body..bend]).into_owned(),
            )),
            _ => {}
        }
        if bend <= p {
            break;
        }
        p = bend;
    }
}

/// SimpleTag children: TagName + TagString (nested SimpleTags ignored for
/// grep purposes; first level is what people tag).
fn simple_tag(
    data: &[u8],
    start: usize,
    end: usize,
    depth: usize,
    name: &mut String,
    value: &mut String,
) {
    if depth > 8 {
        return;
    }
    let mut p = start;
    while p + 2 <= end {
        let Some((id, ilen)) = vint(data, p, false) else {
            break;
        };
        let Some((mut size, slen)) = vint(data, p + ilen, true) else {
            break;
        };
        let body = p + ilen + slen;
        if body > end {
            break;
        }
        if size == u64::MAX {
            size = (end - body) as u64;
        }
        let bend = body.saturating_add(size as usize).min(end);
        match id {
            ID_TAGNAME if name.is_empty() => {
                *name = String::from_utf8_lossy(&data[body..bend]).into_owned()
            }
            ID_TAGSTRING if value.is_empty() => {
                *value = String::from_utf8_lossy(&data[body..bend]).into_owned()
            }
            // SimpleTags nest (e.g. targets); take the first level that has data
            ID_SIMPLETAG if name.is_empty() && value.is_empty() => {
                let (mut n2, mut v2) = (String::new(), String::new());
                simple_tag(data, body, bend, depth + 1, &mut n2, &mut v2);
                if !n2.is_empty() {
                    *name = n2;
                }
                if !v2.is_empty() {
                    *value = v2;
                }
            }
            _ => {}
        }
        if bend <= p {
            break;
        }
        p = bend;
    }
}

fn mkv_tags(data: &[u8]) -> Result<String> {
    let mut tags = Vec::new();
    ebml_elements(data, 0, data.len(), 0, &mut tags);
    if tags.is_empty() {
        bail!("no EBML tags found");
    }
    let mut out = String::new();
    for (k, v) in tags {
        if !v.trim().is_empty() {
            let _ = writeln!(out, "{k}: {v}");
        }
    }
    if out.trim().is_empty() {
        bail!("no EBML tags found");
    }
    Ok(out)
}

pub struct VideoTags;
impl Adapter for VideoTags {
    fn name(&self) -> &'static str {
        "videotags"
    }
    fn extensions(&self) -> &'static [&'static str] {
        &["mp4", "m4v", "mov", "mkv", "webm", "m4a", "aac"]
    }
    fn extract(&self, data: &[u8], _path: &str) -> Result<Outcome> {
        Ok(Outcome::Text(extract_text(data)?.into_bytes()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mp4_box_b(ty: &[u8; 4], payload: &[u8]) -> Vec<u8> {
        let mut v = Vec::with_capacity(8 + payload.len());
        v.extend_from_slice(&((payload.len() + 8) as u32).to_be_bytes());
        v.extend_from_slice(ty);
        v.extend_from_slice(payload);
        v
    }

    fn ilst_key(key: [u8; 4], value: &str) -> Vec<u8> {
        let mut data = Vec::new();
        data.extend_from_slice(&1u32.to_be_bytes()); // type: UTF-8
        data.extend_from_slice(&0u32.to_be_bytes()); // locale
        data.extend_from_slice(value.as_bytes());
        let data_box = mp4_box_b(b"data", &data);
        let mut k = Vec::new();
        k.extend_from_slice(&((data_box.len() + 8) as u32).to_be_bytes());
        k.extend_from_slice(&key);
        k.extend_from_slice(&data_box);
        k
    }

    fn sample_mp4() -> Vec<u8> {
        let ilst = mp4_box_b(
            b"ilst",
            &[
                ilst_key([0xA9, b'n', b'a', b'm'], "ZIRCALOY documentary"),
                ilst_key([0xA9, b'A', b'R', b'T'], "Lab Band"),
                ilst_key([0xA9, b'd', b'a', b'y'], "2026"),
            ]
            .concat(),
        );
        let mut meta_payload = vec![0u8; 4]; // fullbox version/flags
        meta_payload.extend_from_slice(&ilst);
        let meta = mp4_box_b(b"meta", &meta_payload);
        let udta = mp4_box_b(b"udta", &meta);
        let moov = mp4_box_b(b"moov", &udta);
        let ftyp = mp4_box_b(b"ftyp", b"isom\0\0\0\x01isom");
        [ftyp, moov].concat()
    }

    // ---- EBML test builders ----

    /// ID vint: width comes from the ID's bit length (the marker bit is
    /// part of the ID value itself).
    fn id_vint(id: u64) -> Vec<u8> {
        let w = (64 - id.leading_zeros()).div_ceil(8) as usize;
        (id | (1u64 << (7 * w))).to_be_bytes()[8 - w..].to_vec()
    }

    /// size vint: smallest width whose 7*w payload bits fit the value.
    fn size_vint(n: usize) -> Vec<u8> {
        let mut w = 1usize;
        while n >= (1usize << (7 * w)) - 1 {
            w += 1;
        }
        ((n as u64) | (1u64 << (7 * w))).to_be_bytes()[8 - w..].to_vec()
    }

    fn ebml_el(id: u64, payload: &[u8]) -> Vec<u8> {
        let mut v = id_vint(id);
        v.extend_from_slice(&size_vint(payload.len()));
        v.extend_from_slice(payload);
        v
    }

    fn sample_mkv() -> Vec<u8> {
        let mut header = Vec::new();
        header.extend_from_slice(&ebml_el(0x4286, &[1])); // EBMLVersion
        header.extend_from_slice(&ebml_el(ID_DOCTYPE, b"matroska"));
        let mut doc = ebml_el(0x1A45DFA3, &header); // EBML header element
        let mut info = Vec::new();
        info.extend_from_slice(&ebml_el(ID_TITLE, b"ZIRCALOY capture"));
        info.extend_from_slice(&ebml_el(ID_MUXINGAPP, b"rgx-test"));
        let mut simple = Vec::new();
        simple.extend_from_slice(&ebml_el(ID_TAGNAME, b"SUBJECT"));
        simple.extend_from_slice(&ebml_el(ID_TAGSTRING, b"cladding analysis"));
        let mut tag = Vec::new();
        tag.extend_from_slice(&ebml_el(ID_SIMPLETAG, &simple));
        let mut tags = Vec::new();
        tags.extend_from_slice(&ebml_el(ID_TAG, &tag));
        let mut seg = Vec::new();
        seg.extend_from_slice(&ebml_el(ID_INFO, &info));
        seg.extend_from_slice(&ebml_el(ID_TAGS, &tags));
        doc.extend_from_slice(&ebml_el(ID_SEGMENT, &seg));
        doc
    }

    #[test]
    fn mp4_ilst_tags() {
        let text = extract_text(&sample_mp4()).unwrap();
        assert!(text.contains("title: ZIRCALOY documentary"), "{text}");
        assert!(text.contains("artist: Lab Band"), "{text}");
        assert!(text.contains("year: 2026"), "{text}");
    }

    #[test]
    fn mkv_ebml_tags() {
        let text = extract_text(&sample_mkv()).unwrap();
        assert!(text.contains("doctype: matroska"), "{text}");
        assert!(text.contains("title: ZIRCALOY capture"), "{text}");
        assert!(text.contains("muxingapp: rgx-test"), "{text}");
        assert!(text.contains("SUBJECT: cladding analysis"), "{text}");
    }

    #[test]
    fn rejects_garbage() {
        assert!(extract_text(b"not a video container at all").is_err());
        assert!(extract_text(&[]).is_err());
    }

    #[test]
    fn never_panics_on_mutation() {
        rgx_core::fuzzkit::assert_never_panics(
            vec![
                ("mp4", sample_mp4()),
                ("mkv", sample_mkv()),
                ("empty", vec![]),
            ],
            2000,
            "videotags",
            |data| {
                let _ = extract_text(data);
            },
        );
    }
}
