//! Media-metadata adapters: EXIF/TIFF, audio tags, FITS, DICOM.
//!
//! Heavy binary payloads (pixels, waveforms) are never extracted — only
//! metadata. Audio tags are parsed manually (ID3v1, RIFF INFO, FLAC Vorbis
//! comment, Ogg Vorbis) so the crate stays light and dependency-free beyond
//! kamadak-exif.

#![no_std]

#[macro_use]
extern crate alloc;

#[cfg(test)]
extern crate std;

use alloc::boxed::Box;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::fmt::Write as _;
use rgx_core::{bail, Context, Result};
use rgx_core::{Adapter, Outcome};

mod asf;
mod avi;
mod exif;
mod extra;
mod flv;
mod imgmeta;
mod mp4mkv;
mod mpeg;

pub fn adapters() -> Vec<Box<dyn Adapter>> {
    vec![
        Box::new(ExifJpeg),
        Box::new(Tiff),
        Box::new(imgmeta::Gif),
        Box::new(imgmeta::Bmp),
        Box::new(avi::AviInfo),
        Box::new(asf::AsfInfo),
        Box::new(flv::FlvInfo),
        Box::new(mpeg::MpegInfo),
        Box::new(AudioTags),
        Box::new(Fits),
        Box::new(Dicom),
        Box::new(extra::FontNames),
        Box::new(extra::Woff),
        Box::new(extra::Mobi),
        Box::new(mp4mkv::VideoTags),
    ]
}

fn rd_u16le(b: &[u8], o: usize) -> u16 {
    u16::from_le_bytes([b[o], b[o + 1]])
}
fn rd_u32le(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}
fn rd_u32be(b: &[u8], o: usize) -> u32 {
    u32::from_be_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}

/// Whole-file EXIF listing: every IFD (primary, thumbnail, Exif, GPS,
/// Interop) walked tag by tag. The in-house reader does this for JPEG,
/// WebP, PNG and TIFF — kamadak-exif needed its container-aware API for
/// the same, which is what made it a dependency.
fn fmt_exif(r: &exif::ExifReader) -> String {
    let mut out = String::new();
    let ifds: [(&str, &[exif::IfdEntry]); 5] = [
        ("ifd0", &r.ifd0),
        ("ifd1", &r.ifd1),
        ("exif", &r.exif_ifd),
        ("gps", &r.gps_ifd),
        ("interop", &r.interop_ifd),
    ];
    for (name, entries) in ifds {
        for e in entries {
            // `tag_name` returns a `&'static str` for the well-known tags;
            // the `format!` fallback only fires for the long tail of
            // vendor-specific tags that aren't in the kamadak table. The
            // value formatter is `Display` already, so route everything
            // through `writeln!` to keep the loop alloc-free.
            if let Some(tag) = exif::tag_name(e.tag) {
                let _ = writeln!(out, "{name} {tag}: {}", r.entry_value(e));
            } else {
                let _ = writeln!(out, "{name} 0x{:04x}: {}", e.tag, r.entry_value(e));
            }
        }
    }
    out
}

/// JPEG EXIF via kamadak-exif.
pub struct ExifJpeg;
impl Adapter for ExifJpeg {
    fn name(&self) -> &'static str {
        "exif"
    }
    fn extensions(&self) -> &'static [&'static str] {
        &["jpg", "jpeg", "webp", "png"]
    }
    fn extract(&self, data: &[u8], _path: &str) -> Result<Outcome> {
        // The in-house reader unwraps the container itself (JPEG marker
        // walk, WebP RIFF chunk, PNG eXIf chunk). HEIC/HEIF/AVIF are not
        // claimed until the ISO-BMFF `Exif` item parser lands — there the
        // payload is an item, not a top-level chunk.
        let r = exif::ExifReader::parse_container(data)
            .map_err(rgx_core::Error::msg)
            .context("no EXIF payload")?;
        let text = fmt_exif(&r);
        if text.trim().is_empty() {
            bail!("EXIF container present but no fields");
        }
        Ok(Outcome::Text(text.into_bytes()))
    }
}

/// TIFF/GeoTIFF: the in-house reader parses a bare TIFF directly.
pub struct Tiff;
impl Adapter for Tiff {
    fn name(&self) -> &'static str {
        "tiff"
    }
    fn extensions(&self) -> &'static [&'static str] {
        &["tif", "tiff"]
    }
    fn extract(&self, data: &[u8], _path: &str) -> Result<Outcome> {
        let mut r = exif::ExifReader::new(data);
        r.parse_tiff()
            .map_err(rgx_core::Error::msg)
            .context("not a TIFF IFD structure")?;
        let text = fmt_exif(&r);
        if text.trim().is_empty() {
            bail!("TIFF contains no readable tags");
        }
        Ok(Outcome::Text(text.into_bytes()))
    }
}

/// Audio tags, hand-rolled: ID3v1 (MP3), RIFF INFO (WAV), Vorbis comment
/// (FLAC), Vorbis/Ogg. No external crates — a few dozen bytes each.
pub struct AudioTags;
impl Adapter for AudioTags {
    fn name(&self) -> &'static str {
        "audiotags"
    }
    fn extensions(&self) -> &'static [&'static str] {
        &["mp3", "wav", "flac", "ogg", "oga", "opus"]
    }
    fn extract(&self, data: &[u8], path: &str) -> Result<Outcome> {
        let ext = rgx_core::extension_of(path);
        let text = match &*ext {
            "mp3" => {
                let v2 = id3v2(data);
                if v2.trim().is_empty() {
                    id3v1(data)
                } else {
                    v2
                }
            }
            "wav" => riff_info(data),
            "flac" => flac_vorbis(data),
            "ogg" | "oga" | "opus" => ogg_vorbis(data),
            _ => bail!("no lightweight tag parser for .{ext}"),
        };
        if text.trim().is_empty() {
            bail!("no tags found");
        }
        Ok(Outcome::Text(text.into_bytes()))
    }
}

fn trim_cstr(b: &[u8]) -> String {
    let s = String::from_utf8_lossy(b);
    s.trim_matches(char::from(0)).trim().to_string()
}

/// ID3v1: last 128 bytes, "TAG" + title/artist/album/year/comment 30/30/30/4/30.
fn id3v1(data: &[u8]) -> String {
    if data.len() < 128 {
        return String::new();
    }
    let tag = &data[data.len() - 128..];
    if &tag[0..3] != b"TAG" {
        return String::new();
    }
    let f = |o: usize, n: usize| trim_cstr(&tag[o..o + n]);
    let mut out = String::new();
    let _ = writeln!(out, "title: {}", f(3, 30));
    let _ = writeln!(out, "artist: {}", f(33, 30));
    let _ = writeln!(out, "album: {}", f(63, 30));
    let _ = writeln!(out, "year: {}", f(93, 4));
    let _ = writeln!(out, "comment: {}", f(97, 30));
    if tag[125] == 0 && tag[126] != 0 {
        let _ = writeln!(out, "track: {}", tag[126]);
    }
    out
}

/// ID3v2 (2.3/2.4): header "ID3" + 3 bytes + syncsafe size, then 10-byte
/// frames (id, syncsafe size, flags). Text frames carry a 1-byte encoding
/// prefix. We map the common T* frames to friendly names.
fn id3v2(data: &[u8]) -> String {
    if data.len() < 10 || &data[0..3] != b"ID3" {
        return String::new();
    }
    let major = data[3];
    if !(2..=4).contains(&major) {
        return String::new();
    }
    // tag size: 4 syncsafe bytes (7 bits each)
    let size = ((data[6] & 0x7f) as usize) << 21
        | ((data[7] & 0x7f) as usize) << 14
        | ((data[8] & 0x7f) as usize) << 7
        | (data[9] & 0x7f) as usize;
    let end = (10 + size).min(data.len());
    let mut out = String::new();
    let mut p = 10usize;
    while p + 10 <= end {
        let id = &data[p..p + 4];
        if id.iter().all(|&b| b == 0) {
            break;
        }
        let fsize = if major == 4 {
            ((data[p + 4] & 0x7f) as usize) << 21
                | ((data[p + 5] & 0x7f) as usize) << 14
                | ((data[p + 6] & 0x7f) as usize) << 7
                | (data[p + 7] & 0x7f) as usize
        } else {
            rd_u32be_sz(data, p + 4)
        };
        if fsize == 0 || p + 10 + fsize > end {
            break;
        }
        let payload = &data[p + 10..p + 10 + fsize];
        let name = match id {
            b"TIT2" => Some("title"),
            b"TPE1" => Some("artist"),
            b"TALB" => Some("album"),
            b"TYER" | b"TDRC" => Some("year"),
            b"TRCK" => Some("track"),
            b"TCOM" => Some("composer"),
            b"TCON" => Some("genre"),
            b"TXXX" => Some("comment"),
            _ => None,
        };
        if let Some(name) = name {
            let text = decode_id3_text(payload);
            let text = if id == b"TXXX" {
                // "description\0text" — keep both halves
                text.replace('\u{0}', ": ")
            } else {
                text
            };
            if !text.trim().is_empty() {
                let _ = writeln!(out, "{name}: {}", text.trim());
            }
        }
        p += 10 + fsize;
    }
    out
}

fn rd_u32be_sz(b: &[u8], o: usize) -> usize {
    rd_u32be(b, o) as usize
}

/// Decode an ID3 text frame payload: 1 encoding byte + text.
fn decode_id3_text(payload: &[u8]) -> String {
    if payload.is_empty() {
        return String::new();
    }
    let body = &payload[1..];
    match payload[0] {
        0 => body
            .iter()
            .take_while(|&&b| b != 0)
            .map(|&b| b as char)
            .collect::<String>(),
        3 => String::from_utf8_lossy(body)
            .trim_matches(char::from(0))
            .to_string(),
        // UTF-16 variants: decode the even-length prefix lossily
        1 | 2 => {
            let mut s = String::new();
            for ch in body.as_chunks::<2>().0.iter().take(512) {
                let cp = if payload[0] == 1 {
                    u16::from_le_bytes([ch[0], ch[1]])
                } else {
                    u16::from_be_bytes([ch[0], ch[1]])
                };
                if cp == 0 {
                    break;
                }
                if let Some(c) = char::from_u32(cp as u32) {
                    s.push(c);
                }
            }
            s
        }
        _ => String::from_utf8_lossy(body)
            .trim_matches(char::from(0))
            .to_string(),
    }
}

/// RIFF INFO list: "LIST" chunk with INFO type, subchunks INAM/IART/IPRD...
fn riff_info(data: &[u8]) -> String {
    if data.len() < 12 || &data[0..4] != b"RIFF" || &data[8..12] != b"WAVE" {
        return String::new();
    }
    let mut out = String::new();
    let mut p = 12usize;
    while p + 8 <= data.len() {
        let id = &data[p..p + 4];
        let size = rd_u32le(data, p + 4) as usize;
        if id == b"LIST" && p + 8 + size <= data.len() && &data[p + 8..p + 12] == b"INFO" {
            let mut q = p + 12;
            let end = p + 8 + size;
            while q + 8 <= end {
                let key = String::from_utf8_lossy(&data[q..q + 4]).into_owned();
                let sz = rd_u32le(data, q + 4) as usize;
                if q + 8 + sz > end {
                    break;
                }
                let val = trim_cstr(&data[q + 8..q + 8 + sz]);
                if !val.is_empty() {
                    let _ = writeln!(out, "{key}: {val}");
                }
                q += 8 + sz + (sz & 1);
            }
        }
        p += 8 + size + (size & 1);
    }
    out
}

/// FLAC metadata blocks: block type 4 = Vorbis comment (vendor + key=value).
fn flac_vorbis(data: &[u8]) -> String {
    if data.len() < 42 || &data[0..4] != b"fLaC" {
        return String::new();
    }
    let mut p = 4usize;
    while p + 4 <= data.len() {
        let last = data[p] & 0x80 != 0;
        let btype = data[p] & 0x7f;
        let len =
            ((data[p + 1] as usize) << 16) | ((data[p + 2] as usize) << 8) | data[p + 3] as usize;
        p += 4;
        if btype == 4 && p + len <= data.len() {
            return vorbis_comment(&data[p..p + len]);
        }
        p += len;
        if last {
            break;
        }
    }
    String::new()
}

fn vorbis_comment(b: &[u8]) -> String {
    if b.len() < 8 {
        return String::new();
    }
    let vlen = rd_u32le(b, 0) as usize;
    if 4 + vlen + 4 > b.len() {
        return String::new();
    }
    let vendor = trim_cstr(&b[4..4 + vlen]);
    let n = rd_u32le(b, 4 + vlen) as usize;
    let mut out = format!("vendor: {vendor}\n");
    let mut p = 8 + vlen;
    for _ in 0..n.min(100_000) {
        if p + 4 > b.len() {
            break;
        }
        let l = rd_u32le(b, p) as usize;
        p += 4;
        if p + l > b.len() {
            break;
        }
        let kv = String::from_utf8_lossy(&b[p..p + l]);
        if let Some((k, v)) = kv.split_once('=') {
            let _ = writeln!(out, "{k}: {v}");
        }
        p += l;
    }
    out
}

/// Ogg: find first "\x03vorbis" packet and parse its comment header.
fn ogg_vorbis(data: &[u8]) -> String {
    if data.len() < 64 || &data[0..4] != b"OggS" {
        return String::new();
    }
    // search the first ~64 KiB for the vorbis id header packet marker
    let window = &data[..data.len().min(1 << 16)];
    let Some(pos) = rgx_core::memchr::memmem::find(window, b"\x03vorbis") else {
        return String::new();
    };
    vorbis_comment(&window[pos + 7..])
}

/// FITS astronomy images: 80-byte keyword cards until END, `KEY = value`.
pub struct Fits;
impl Adapter for Fits {
    fn name(&self) -> &'static str {
        "fits"
    }
    fn extensions(&self) -> &'static [&'static str] {
        &["fits", "fit", "fts"]
    }
    fn extract(&self, data: &[u8], _path: &str) -> Result<Outcome> {
        // FITS keyword = 8 chars, uppercase / digits / space / '-' / '_'
        if data.len() < 80
            || !data[..8].iter().all(|&b| {
                b.is_ascii_uppercase() || b.is_ascii_digit() || matches!(b, b' ' | b'-' | b'_')
            })
        {
            bail!("not a FITS file (bad keyword card)");
        }
        let mut out = String::new();
        let mut p = 0usize;
        while p + 80 <= data.len() && out.len() < 1 << 20 {
            let card = &data[p..p + 80];
            p += 80;
            let key = trim_cstr(&card[0..8]);
            if key == "END" {
                break;
            }
            if card.len() >= 10 && card[8] == b'=' {
                let value = String::from_utf8_lossy(&card[10..])
                    .split('/')
                    .next()
                    .unwrap_or("")
                    .trim()
                    .trim_matches('\'')
                    .to_string();
                let _ = writeln!(out, "{key}: {value}");
            } else if !key.is_empty() {
                out.push_str(&key);
                out.push('\n');
            }
        }
        if out.trim().is_empty() {
            bail!("FITS header has no keywords");
        }
        Ok(Outcome::Text(out.into_bytes()))
    }
}

/// DICOM (medical imaging): 128-byte preamble + "DICM", then data elements.
/// Stops before pixel data (7FE0,0010); strings and numeric VRs only.
pub struct Dicom;
impl Adapter for Dicom {
    fn name(&self) -> &'static str {
        "dicom"
    }
    fn extensions(&self) -> &'static [&'static str] {
        &["dcm", "dicom", "ima"]
    }
    fn extract(&self, data: &[u8], _path: &str) -> Result<Outcome> {
        if data.len() < 132 || &data[128..132] != b"DICM" {
            bail!("not a DICOM file (missing DICM preamble)");
        }
        // long VRs carry 32-bit lengths (OB, OD, OF, OL, OW, SQ, UC, UR, UT, UN)
        const LONG_VR: &[&[u8; 2]] = &[
            b"OB", b"OD", b"OF", b"OL", b"OW", b"SQ", b"UC", b"UR", b"UT", b"UN",
        ];
        let mut out = String::new();
        let mut p = 132usize;
        while p + 8 <= data.len() && out.len() < 1 << 20 {
            let group = rd_u16le(data, p);
            let element = rd_u16le(data, p + 2);
            if group == 0x7fe0 && element == 0x0010 {
                break;
            } // pixel data
            if group == 0xfffe {
                // item delimiter / sequence item: skip length
                let len = rd_u32le(data, p + 4) as usize;
                p += 8;
                if len == 0xFFFF_FFFF {
                    break;
                }
                p += len;
                continue;
            }
            let vr = [data[p + 4], data[p + 5]];
            if vr[0] == 0 || !vr[0].is_ascii_alphabetic() {
                break; // implicit VR or garbage — stop cleanly
            }
            let (vlen, header_len) = if LONG_VR.contains(&&vr) {
                (rd_u32le(data, p + 8) as usize, 12usize)
            } else {
                (rd_u16le(data, p + 6) as usize, 8usize)
            };
            if vlen == 0xFFFF_FFFF {
                break;
            } // undefined length
            let voff = p + header_len;
            if voff + vlen > data.len() {
                break;
            }
            let tag = format!("({:04X},{:04X})", group, element);
            let value = match &vr {
                b"PN" | b"LO" | b"SH" | b"ST" | b"LT" | b"UT" | b"DA" | b"TM" | b"UI" | b"CS"
                | b"AS" | b"AE" | b"DS" | b"IS" | b"DT" => {
                    let s = String::from_utf8_lossy(&data[voff..voff + vlen]);
                    Some(s.trim_matches(char::from(0)).trim().to_string())
                }
                b"US" if vlen == 2 => Some(rd_u16le(data, voff).to_string()),
                b"UL" if vlen == 4 => Some(rd_u32le(data, voff).to_string()),
                _ => None,
            };
            if let Some(v) = value {
                if !v.is_empty() {
                    let vr_s = String::from_utf8_lossy(&vr);
                    let _ = writeln!(out, "{tag} {vr_s}: {v}");
                }
            }
            p = voff + vlen;
        }
        if out.trim().is_empty() {
            bail!("no DICOM elements decoded");
        }
        Ok(Outcome::Text(out.into_bytes()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn id3v1_basic() {
        let mut mp3 = vec![0xff; 500];
        let mut tag = vec![0u8; 128];
        tag[0..3].copy_from_slice(b"TAG");
        tag[3..9].copy_from_slice(b"Song\0\0");
        tag[33..39].copy_from_slice(b"Band\0\0");
        mp3.extend_from_slice(&tag);
        let t = id3v1(&mp3);
        assert!(t.contains("title: Song"), "{t}");
        assert!(t.contains("artist: Band"), "{t}");
    }

    #[test]
    fn fits_cards() {
        let card = |k: &str, v: &str| {
            let mut c = format!("{k:<8}= {v}");
            c.truncate(80);
            format!("{c:<80}")
        };
        let mut data = String::new();
        data.push_str(&card("SIMPLE", "T / conforms"));
        data.push_str(&card("BITPIX", "8"));
        data.push_str(&card("END", ""));
        let f = Fits;
        match f.extract(data.as_bytes(), "a.fits").unwrap() {
            Outcome::Text(t) => {
                let s = String::from_utf8(t).unwrap();
                assert!(s.contains("SIMPLE: T"), "{s}");
                assert!(s.contains("BITPIX: 8"), "{s}");
            }
            _ => panic!("expected text"),
        }
    }

    #[test]
    fn dicom_preamble() {
        let mut data = vec![0u8; 128];
        data.extend_from_slice(b"DICM");
        // (0008,0060) Modality CS len 2 "MR"
        data.extend_from_slice(&[0x08, 0x00, 0x60, 0x00]);
        data.extend_from_slice(b"CS");
        data.extend_from_slice(&2u16.to_le_bytes());
        data.extend_from_slice(b"MR");
        let d = Dicom;
        match d.extract(&data, "x.dcm").unwrap() {
            Outcome::Text(t) => {
                let s = String::from_utf8(t).unwrap();
                assert!(s.contains("(0008,0060) CS: MR"), "{s}");
            }
            _ => panic!("expected text"),
        }
    }

    mod fuzz {
        use super::*;

        fn fits_cards() -> Vec<u8> {
            let card = |k: &str, v: &str| {
                let mut c = format!("{k:<8}= {v}");
                c.truncate(80);
                format!("{c:<80}")
            };
            let mut data = String::new();
            data.push_str(&card("SIMPLE", "T / conforms"));
            data.push_str(&card("BITPIX", "8"));
            data.push_str(&card("END", ""));
            data.into_bytes()
        }

        fn dicom_mr() -> Vec<u8> {
            let mut data = vec![0u8; 128];
            data.extend_from_slice(b"DICM");
            data.extend_from_slice(&[0x08, 0x00, 0x60, 0x00]);
            data.extend_from_slice(b"CS");
            data.extend_from_slice(&2u16.to_le_bytes());
            data.extend_from_slice(b"MR");
            data
        }

        fn mp3_id3v1() -> Vec<u8> {
            let mut mp3 = vec![0xffu8; 500];
            let mut tag = vec![0u8; 128];
            tag[0..3].copy_from_slice(b"TAG");
            tag[3..9].copy_from_slice(b"Song\0\0");
            mp3.extend_from_slice(&tag);
            mp3
        }

        fn mini_jpeg() -> Vec<u8> {
            let mut j = Vec::new();
            j.extend_from_slice(&[0xFF, 0xD8]); // SOI
            j.extend_from_slice(&[0xFF, 0xE0]); // APP0
            j.extend_from_slice(&16u16.to_be_bytes());
            j.extend_from_slice(b"JFIF\0\x01\x01\0\0\x01\0\x01\0\0");
            j.extend_from_slice(&[0xFF, 0xD9]); // EOI
            j
        }

        fn mini_tiff() -> Vec<u8> {
            let mut t = Vec::new();
            t.extend_from_slice(b"II*\x00"); // little-endian, magic 42
            t.extend_from_slice(&8u32.to_le_bytes()); // IFD offset
            t.extend_from_slice(&0u16.to_le_bytes()); // 0 entries
            t.extend_from_slice(&0u32.to_le_bytes()); // next IFD
            t
        }

        #[test]
        fn fits_never_panics_on_mutation() {
            rgx_core::fuzzkit::assert_never_panics(
                vec![("cards", fits_cards()), ("empty", vec![])],
                1000,
                "fits",
                |data| {
                    let _ = Fits.extract(data, "fuzz.fits");
                },
            );
        }

        #[test]
        fn dicom_never_panics_on_mutation() {
            rgx_core::fuzzkit::assert_never_panics(
                vec![
                    ("mr", dicom_mr()),
                    ("dicm-only", b"DICM".to_vec()),
                    ("empty", vec![]),
                ],
                1500,
                "dicom",
                |data| {
                    let _ = Dicom.extract(data, "fuzz.dcm");
                },
            );
        }

        #[test]
        fn audio_tags_never_panics_on_mutation() {
            rgx_core::fuzzkit::assert_never_panics(
                vec![("id3v1", mp3_id3v1()), ("empty", vec![])],
                1000,
                "audio-tags",
                |data| {
                    let _ = AudioTags.extract(data, "fuzz.mp3");
                },
            );
        }

        #[test]
        fn exif_jpeg_never_panics_on_mutation() {
            rgx_core::fuzzkit::assert_never_panics(
                vec![
                    ("jpeg", mini_jpeg()),
                    ("soi-only", vec![0xFF, 0xD8]),
                    ("empty", vec![]),
                ],
                1500,
                "exif-jpeg",
                |data| {
                    let _ = ExifJpeg.extract(data, "fuzz.jpg");
                },
            );
        }

        #[test]
        fn tiff_never_panics_on_mutation() {
            rgx_core::fuzzkit::assert_never_panics(
                vec![
                    ("tiff", mini_tiff()),
                    ("magic-only", b"II*\x00".to_vec()),
                    ("empty", vec![]),
                ],
                1500,
                "tiff",
                |data| {
                    let _ = Tiff.extract(data, "fuzz.tiff");
                },
            );
        }
    }
}
