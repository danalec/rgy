// SPDX-License-Identifier: Apache-2.0

//! Real icons: the desktop theme's own PNGs, decoded here.
//!
//! The rows used to carry a shape drawn out of a few fills — honest at
//! sixteen pixels, and not the icon the desktop shows for that file. So the
//! launcher asks the icon theme instead: it looks for the MIME type's icon
//! under the freedesktop theme roots, decodes the PNG, and scales it to the
//! row. A machine with no theme keeps the drawn shape, which is the same
//! bargain [`crate::font`] makes with typography.
//!
//! # Why a decoder and not libpng
//!
//! `libpng` reports every error by `longjmp`ing to a buffer the caller owns,
//! and a `longjmp` through Rust frames is undefined behaviour. The tree
//! already refuses to link anything it does not declare itself, so the
//! choice was a decoder or no icons. A decoder it is: `inflate`, the five
//! PNG row filters, and the four colour types a theme icon uses. It is
//! `no_std`, it has no dependency, and the shapes it reads are the ones in
//! `/usr/share/icons`.
//!
//! # The limits, said plainly
//!
//! Bit depth 8 only, no interlacing, and the four colour types (grey, RGB,
//! palette, RGBA). That is every icon in `hicolor`; a 16-bit or interlaced
//! PNG answers `None`, the same as a missing one, and the row falls back to
//! the drawn shape. A decoder that guesses is worse than one that says no.

use alloc::vec;
use alloc::vec::Vec;

/// One decoded icon: 16x16 straight RGBA, row-major.
pub const ICON_SIZE: u32 = 16;

/// A decoded, scaled icon.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Icon {
    /// `ICON_SIZE * ICON_SIZE` pixels, `0xRRGGBBAA` each.
    pub pixels: Vec<u32>,
}

/// Where the theme's icons live, in order. Both hosts' roots, and the two
/// sizes a 16-pixel row reads best from.
pub fn candidate_dirs() -> &'static [&'static str] {
    &[
        // Exact size first, so the row never depends on a scale-down: a
        // sixteen-pixel icon is a sixteen-pixel icon.
        "/usr/share/icons/hicolor/16x16/mimetypes",
        "/usr/share/icons/AdwaitaLegacy/16x16/mimetypes",
        "/usr/share/icons/Adwaita/16x16/mimetypes",
        "/usr/local/share/icons/hicolor/16x16/mimetypes",
        "/usr/local/share/icons/AdwaitaLegacy/16x16/mimetypes",
        "/usr/local/share/icons/Adwaita/16x16/mimetypes",
        // Then the next sizes up, which the scaler averages down.
        "/usr/share/icons/hicolor/24x24/mimetypes",
        "/usr/share/icons/AdwaitaLegacy/24x24/mimetypes",
        "/usr/share/icons/Adwaita/24x24/mimetypes",
        "/usr/local/share/icons/hicolor/24x24/mimetypes",
        "/usr/local/share/icons/AdwaitaLegacy/24x24/mimetypes",
        "/usr/local/share/icons/Adwaita/24x24/mimetypes",
        "/usr/share/icons/hicolor/32x32/mimetypes",
        "/usr/local/share/icons/hicolor/32x32/mimetypes",
        "/usr/share/icons/hicolor/48x48/mimetypes",
        "/usr/local/share/icons/hicolor/48x48/mimetypes",
    ]
}

/// The icon file names to try for a kind, best first.
///
/// The names are the freedesktop MIME ones a `hicolor` theme ships: a theme
/// that has `text-x-generic` for a document and no `application-pdf` still
/// answers with the generic one, which is exactly what the desktop would
/// show for an unknown document.
pub fn candidates_for(kind: crate::render::IconFamily) -> &'static [&'static str] {
    use crate::render::IconFamily::*;
    match kind {
        Folder => &[
            "inode-directory.png",
            "folder.png",
            "folder-blue.png",
            "gnome-fs-directory.png",
        ],
        Document => &[
            "text-x-generic.png",
            "x-office-document.png",
            "application-pdf.png",
            "text-plain.png",
        ],
        Image => &[
            "image-x-generic.png",
            "image-png.png",
            "image-jpeg.png",
            "media-photo.png",
        ],
        Video => &["video-x-generic.png", "video-mp4.png", "media-video.png"],
        Audio => &["audio-x-generic.png", "audio-mpeg.png", "media-audio.png"],
        Application => &[
            "application-x-executable.png",
            "application-x-executable-script.png",
            "exec.png",
        ],
        File => &[
            "text-x-generic.png",
            "application-octet-stream.png",
            "empty.png",
        ],
    }
}

/// A decoded PNG, straight RGBA8.
pub struct Rgba {
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// `width * height` pixels, RGBA bytes.
    pub bytes: Vec<u8>,
}

/// Decode a PNG file's bytes.
///
/// `None` for every reason a decoder can refuse: not a PNG, a depth or
/// colour type this build does not read, a broken stream, an interlaced
/// file. The caller treats a refusal like a missing icon.
pub fn decode_png(data: &[u8]) -> Option<Rgba> {
    const SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
    if data.len() < 8 || data[..8] != SIGNATURE {
        return None;
    }
    let mut pos = 8;
    let mut width = 0u32;
    let mut height = 0u32;
    let mut colour = 0u8;
    let mut palette: Vec<[u8; 3]> = Vec::new();
    let mut alpha_table: Vec<u8> = Vec::new();
    let mut idat: Vec<u8> = Vec::new();
    let mut saw_header = false;
    while pos + 8 <= data.len() {
        let length = be32(&data[pos..]) as usize;
        let kind = &data[pos + 4..pos + 8];
        let body_start = pos + 8;
        let body_end = body_start.checked_add(length)?;
        if body_end + 4 > data.len() {
            return None;
        }
        let body = &data[body_start..body_end];
        match kind {
            b"IHDR" => {
                if body.len() < 13 {
                    return None;
                }
                width = be32(body);
                height = be32(&body[4..]);
                colour = body[9];
                let interlace = body[12];
                if width == 0 || height == 0 || interlace != 0 || body[8] != 8 {
                    return None;
                }
                // 0 grey, 2 RGB, 3 palette, 6 RGBA. 4 (grey+alpha) is not an
                // icon a theme ships, and reading it would need a fifth arm
                // for nothing.
                if !matches!(colour, 0 | 2 | 3 | 6) {
                    return None;
                }
                saw_header = true;
            }
            b"PLTE" => {
                palette = body.as_chunks::<3>().0.to_vec();
            }
            b"tRNS" => {
                alpha_table = body.to_vec();
            }
            b"IDAT" => idat.extend_from_slice(body),
            b"IEND" => break,
            _ => {}
        }
        pos = body_end + 4;
    }
    if !saw_header || idat.is_empty() {
        return None;
    }
    let channels: usize = match colour {
        0 => 1,
        2 => 3,
        3 => 1,
        6 => 4,
        _ => return None,
    };
    let stride = (width as usize).checked_mul(channels)?;
    let expected = (stride + 1).checked_mul(height as usize)?;
    let raw = inflate(&idat)?;
    if raw.len() < expected {
        return None;
    }
    let rows = unfilter(&raw, stride, height as usize)?;
    let mut bytes = Vec::with_capacity((width as usize) * (height as usize) * 4);
    for row in 0..height as usize {
        let line = &rows[row * stride..row * stride + stride];
        for col in 0..width as usize {
            let px = &line[col * channels..col * channels + channels];
            let (r, g, b, a) = match colour {
                0 => (px[0], px[0], px[0], 255),
                2 => (px[0], px[1], px[2], 255),
                3 => {
                    let index = px[0] as usize;
                    let [r, g, b] = *palette.get(index)?;
                    (r, g, b, alpha_table.get(index).copied().unwrap_or(255))
                }
                _ => (px[0], px[1], px[2], px[3]),
            };
            bytes.extend_from_slice(&[r, g, b, a]);
        }
    }
    Some(Rgba {
        width,
        height,
        bytes,
    })
}

/// Undo the five PNG row filters.
fn unfilter(raw: &[u8], stride: usize, height: usize) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(stride * height);
    let mut previous = vec_prefix(stride);
    for row in 0..height {
        let at = row * (stride + 1);
        let filter = raw[at];
        let line = &raw[at + 1..at + 1 + stride];
        let mut current = Vec::with_capacity(stride);
        for i in 0..stride {
            let left = if i >= 1 { current[i - 1] } else { 0 };
            let up = previous[i];
            let up_left = if i >= 1 { previous[i - 1] } else { 0 };
            let value = match filter {
                0 => line[i],
                1 => line[i].wrapping_add(left),
                2 => line[i].wrapping_add(up),
                3 => line[i].wrapping_add(((left as u16 + up as u16) / 2) as u8),
                4 => line[i].wrapping_add(paeth(left, up, up_left)),
                _ => return None,
            };
            current.push(value);
        }
        out.extend_from_slice(&current);
        previous = current;
    }
    Some(out)
}

/// A zeroed row of the given length.
fn vec_prefix(len: usize) -> Vec<u8> {
    vec![0u8; len]
}

/// The Paeth predictor, from the PNG specification.
fn paeth(a: u8, b: u8, c: u8) -> u8 {
    let p = a as i32 + b as i32 - c as i32;
    let pa = (p - a as i32).abs();
    let pb = (p - b as i32).abs();
    let pc = (p - c as i32).abs();
    if pa <= pb && pa <= pc {
        a
    } else if pb <= pc {
        b
    } else {
        c
    }
}

/// Inflate a zlib stream (RFC 1950 wrapping RFC 1951).
pub fn inflate(data: &[u8]) -> Option<Vec<u8>> {
    if data.len() < 2 {
        return None;
    }
    let cmf = data[0];
    let flg = data[1];
    if cmf & 0x0f != 8 {
        return None;
    }
    if !(cmf as u16 * 256 + flg as u16).is_multiple_of(31) {
        return None;
    }
    let mut bits = BitReader::new(&data[2..]);
    inflate_blocks(&mut bits)
}

/// The deflate blocks, one after another until the last.
fn inflate_blocks(bits: &mut BitReader<'_>) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    loop {
        let last = bits.bits(1)?;
        let kind = bits.bits(2)?;
        match kind {
            0 => {
                // Stored: byte-aligned, its own length twice in a row.
                bits.align();
                let len = bits.bits(16)? as usize;
                let check = bits.bits(16)? as usize;
                if len != !check & 0xffff {
                    return None;
                }
                for _ in 0..len {
                    out.push(bits.byte()?);
                }
            }
            1 => {
                let (lit, dist) = fixed_tables();
                inflate_codes(bits, &lit, &dist, &mut out)?;
            }
            2 => {
                let (lit, dist) = dynamic_tables(bits)?;
                inflate_codes(bits, &lit, &dist, &mut out)?;
            }
            _ => return None,
        }
        if last == 1 {
            return Some(out);
        }
    }
}

/// A canonical Huffman table: `(length, code)` per symbol.
type Table = Vec<(u8, u16)>;

/// Build a decoding table from the code lengths.
fn table_from_lengths(lengths: &[u8]) -> Option<Table> {
    let mut count = [0u16; 16];
    for len in lengths {
        if *len > 15 {
            return None;
        }
        if *len > 0 {
            count[*len as usize] += 1;
        }
    }
    let mut next = [0u16; 16];
    let mut code = 0u16;
    for len in 1..16 {
        code = (code + count[len - 1]) << 1;
        next[len] = code;
    }
    let mut table = Vec::with_capacity(lengths.len());
    for (symbol, len) in lengths.iter().enumerate() {
        if *len == 0 {
            table.push((0, 0));
            continue;
        }
        let assigned = next[*len as usize];
        next[*len as usize] += 1;
        table.push((*len, assigned));
        let _ = symbol;
    }
    Some(table)
}

/// Decode literals and back-references until the end-of-block symbol.
fn inflate_codes(
    bits: &mut BitReader<'_>,
    lit: &Table,
    dist: &Table,
    out: &mut Vec<u8>,
) -> Option<()> {
    const LENGTH_BASE: [u16; 29] = [
        3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115,
        131, 163, 195, 227, 258,
    ];
    const LENGTH_EXTRA: [u8; 29] = [
        0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0,
    ];
    const DIST_BASE: [u16; 30] = [
        1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537,
        2049, 3073, 4097, 6145, 8193, 12289, 16385, 24577,
    ];
    const DIST_EXTRA: [u8; 30] = [
        0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12,
        13, 13,
    ];
    loop {
        let symbol = decode_symbol(bits, lit)?;
        if symbol < 256 {
            out.push(symbol as u8);
            continue;
        }
        if symbol == 256 {
            return Some(());
        }
        let index = (symbol - 257) as usize;
        if index >= LENGTH_BASE.len() {
            return None;
        }
        let length = LENGTH_BASE[index] as usize
            + bits.bits(LENGTH_EXTRA[index] as u32).unwrap_or(0) as usize;
        let dsym = decode_symbol(bits, dist)? as usize;
        if dsym >= DIST_BASE.len() {
            return None;
        }
        let back =
            DIST_BASE[dsym] as usize + bits.bits(DIST_EXTRA[dsym] as u32).unwrap_or(0) as usize;
        if back == 0 || back > out.len() {
            return None;
        }
        // A back-reference may overlap what it is writing: that is how a run
        // is encoded, so the copy is byte by byte.
        let start = out.len() - back;
        for i in 0..length {
            let byte = out[start + i];
            out.push(byte);
        }
    }
}

/// Read one symbol with a canonical table, bit by bit.
fn decode_symbol(bits: &mut BitReader<'_>, table: &Table) -> Option<u16> {
    let mut code = 0u16;
    let mut first = 0u16;
    let mut index = 0u16;
    for len in 1..=15u32 {
        code |= bits.bits(1)? as u16;
        let count = table.iter().filter(|(l, _)| *l as u32 == len).count() as u16;
        if code >= first && code - first < count {
            let mut seen = 0u16;
            for (symbol, (l, c)) in table.iter().enumerate() {
                if *l as u32 != len {
                    continue;
                }
                if *c == code {
                    let _ = (index, seen);
                    return Some(symbol as u16);
                }
                seen += 1;
            }
        }
        index += count;
        first = (first + count) << 1;
        code <<= 1;
    }
    None
}

/// The fixed literal and distance tables of RFC 1951.
fn fixed_tables() -> (Table, Table) {
    let mut lit_lengths = [0u8; 288];
    for (i, slot) in lit_lengths.iter_mut().enumerate() {
        *slot = match i {
            0..=143 => 8,
            144..=255 => 9,
            256..=279 => 7,
            _ => 8,
        };
    }
    let dist_lengths = [5u8; 30];
    (
        table_from_lengths(&lit_lengths).unwrap_or_default(),
        table_from_lengths(&dist_lengths).unwrap_or_default(),
    )
}

/// The per-block tables of a dynamic block.
fn dynamic_tables(bits: &mut BitReader<'_>) -> Option<(Table, Table)> {
    const ORDER: [usize; 19] = [
        16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15,
    ];
    let hlit = bits.bits(5)? as usize + 257;
    let hdist = bits.bits(5)? as usize + 1;
    let hclen = bits.bits(4)? as usize + 4;
    let mut code_lengths = [0u8; 19];
    for slot in ORDER.iter().take(hclen) {
        code_lengths[*slot] = bits.bits(3)? as u8;
    }
    let code_table = table_from_lengths(&code_lengths)?;
    let mut lengths = Vec::with_capacity(hlit + hdist);
    while lengths.len() < hlit + hdist {
        let symbol = decode_symbol(bits, &code_table)?;
        match symbol {
            0..=15 => lengths.push(symbol as u8),
            16 => {
                let previous = *lengths.last()?;
                let repeat = 3 + bits.bits(2)? as usize;
                for _ in 0..repeat {
                    lengths.push(previous);
                }
            }
            17 => {
                let repeat = 3 + bits.bits(3)? as usize;
                lengths.resize(lengths.len() + repeat, 0);
            }
            18 => {
                let repeat = 11 + bits.bits(7)? as usize;
                lengths.resize(lengths.len() + repeat, 0);
            }
            _ => return None,
        }
    }
    if lengths.len() != hlit + hdist {
        return None;
    }
    let lit = table_from_lengths(&lengths[..hlit])?;
    let dist = table_from_lengths(&lengths[hlit..])?;
    Some((lit, dist))
}

/// Bits, least significant first, as deflate writes them.
struct BitReader<'a> {
    data: &'a [u8],
    at: usize,
    bit: u32,
}

impl<'a> BitReader<'a> {
    fn new(data: &'a [u8]) -> BitReader<'a> {
        BitReader {
            data,
            at: 0,
            bit: 0,
        }
    }

    /// The next `count` bits, low bit first.
    fn bits(&mut self, count: u32) -> Option<u32> {
        let mut value = 0u32;
        for i in 0..count {
            let byte = *self.data.get(self.at)?;
            let bit = (byte >> (self.bit % 8)) & 1;
            value |= (bit as u32) << i;
            self.bit += 1;
            if self.bit.is_multiple_of(8) {
                self.at += 1;
            }
        }
        Some(value)
    }

    /// Skip to the next byte boundary.
    fn align(&mut self) {
        if !self.bit.is_multiple_of(8) {
            self.at += 1;
        }
        self.bit = 0;
    }

    /// One whole byte, aligned.
    fn byte(&mut self) -> Option<u8> {
        let byte = *self.data.get(self.at)?;
        self.at += 1;
        Some(byte)
    }
}

/// A big-endian 32-bit number, as PNG chunks carry them.
fn be32(bytes: &[u8]) -> u32 {
    ((bytes[0] as u32) << 24)
        | ((bytes[1] as u32) << 16)
        | ((bytes[2] as u32) << 8)
        | bytes[3] as u32
}

impl Icon {
    /// Scale a decoded image down to [`ICON_SIZE`] with box averaging.
    ///
    /// Theme icons are 24, 32 or 48 pixels and the row is 16: averaging is
    /// the cheap filter that keeps a thin stroke from disappearing, which
    /// nearest-neighbour would do to a 48-pixel icon at a third of its size.
    pub fn from_rgba(rgba: &Rgba) -> Icon {
        let mut pixels = Vec::with_capacity((ICON_SIZE * ICON_SIZE) as usize);
        for out_y in 0..ICON_SIZE {
            for out_x in 0..ICON_SIZE {
                let x0 = out_x * rgba.width / ICON_SIZE;
                let x1 = ((out_x + 1) * rgba.width).div_ceil(ICON_SIZE).max(x0 + 1);
                let y0 = out_y * rgba.height / ICON_SIZE;
                let y1 = ((out_y + 1) * rgba.height).div_ceil(ICON_SIZE).max(y0 + 1);
                let (mut r, mut g, mut b, mut a, mut n) = (0u32, 0u32, 0u32, 0u32, 0u32);
                for y in y0..y1.min(rgba.height) {
                    for x in x0..x1.min(rgba.width) {
                        let at = ((y * rgba.width + x) * 4) as usize;
                        let px = &rgba.bytes[at..at + 4];
                        let alpha = px[3] as u32;
                        // A transparent pixel must not darken the average:
                        // the colours are weighted by their own coverage.
                        r += px[0] as u32 * alpha;
                        g += px[1] as u32 * alpha;
                        b += px[2] as u32 * alpha;
                        a += alpha;
                        n += 1;
                    }
                }
                if n == 0 || a == 0 {
                    pixels.push(0);
                    continue;
                }
                let _ = n;
                pixels.push(
                    ((a / n) << 24)
                        | (((r / a) & 0xff) << 16)
                        | (((g / a) & 0xff) << 8)
                        | ((b / a) & 0xff),
                );
            }
        }
        Icon { pixels }
    }
}

/// Read a whole file, up to a cap.
///
/// An icon is a few kilobytes; the cap is there so a path that is not an
/// icon at all (a FIFO, a device, a huge file) cannot swallow memory. The
/// answer is `None` on any refusal, which is the same answer as a missing
/// file.
fn read_file(path: &str) -> Option<Vec<u8>> {
    const CAP: usize = 512 * 1024;
    let mut c_path: Vec<u8> = Vec::with_capacity(path.len() + 1);
    c_path.extend_from_slice(path.as_bytes());
    c_path.push(0);
    // SAFETY: the path is NUL-terminated and the descriptor is closed on
    // every path out of here.
    unsafe {
        let fd = crate::x::open(c_path.as_ptr().cast(), 0);
        if fd < 0 {
            return None;
        }
        let mut out: Vec<u8> = Vec::new();
        let mut chunk = [0u8; 8192];
        loop {
            let got = crate::x::read(fd, chunk.as_mut_ptr().cast(), chunk.len());
            if got <= 0 {
                break;
            }
            let n = got as usize;
            out.extend_from_slice(&chunk[..n]);
            if out.len() >= CAP {
                break;
            }
        }
        crate::x::close(fd);
        if out.is_empty() {
            return None;
        }
        Some(out)
    }
}

/// Load the theme's icon for a family, scaled to the row's size.
///
/// The search is directory by directory and name by name, and the first
/// file that decodes wins: the order of [`candidate_dirs`] is the order of
/// preference, and the order of [`candidates_for`] is the order of
/// specificity.
pub fn load(family: crate::render::IconFamily) -> Option<Icon> {
    let names = candidates_for(family);
    for dir in candidate_dirs() {
        for name in names {
            let mut path = alloc::string::String::with_capacity(dir.len() + name.len() + 2);
            path.push_str(dir);
            path.push('/');
            path.push_str(name);
            let Some(bytes) = read_file(&path) else {
                continue;
            };
            let Some(rgba) = decode_png(&bytes) else {
                continue;
            };
            return Some(Icon::from_rgba(&rgba));
        }
    }
    None
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_signature_is_checked_before_anything_else() {
        assert!(decode_png(b"not a png").is_none());
        assert!(decode_png(&[]).is_none());
    }

    #[test]
    fn an_empty_stream_is_refused_not_guessed() {
        assert!(inflate(&[]).is_none());
        assert!(inflate(&[0x78]).is_none());
    }

    /// A zlib-wrapped stored block decodes to its own bytes.
    #[test]
    fn a_stored_block_round_trips() {
        // 0x78 0x01 is the zlib header for a stored, unset dictionary
        // stream; the block is "last, stored", then len and ~len.
        let data = [0x78u8, 0x01, 0x01, 0x03, 0x00, 0xfc, 0xff, b'a', b'b', b'c'];
        assert_eq!(inflate(&data).as_deref(), Some(&b"abc"[..]));
    }

    #[test]
    fn every_icon_family_has_candidates() {
        use crate::render::IconFamily::*;
        for kind in [Folder, Document, Image, Video, Audio, Application, File] {
            let names = candidates_for(kind);
            assert!(!names.is_empty(), "{kind:?} has no candidate");
            for name in names {
                assert!(name.ends_with(".png"), "{name} is not a png");
            }
        }
    }

    #[test]
    fn both_hosts_icon_roots_are_searched() {
        let dirs = candidate_dirs();
        assert!(dirs.iter().any(|d| d.starts_with("/usr/share/icons")));
        assert!(dirs.iter().any(|d| d.starts_with("/usr/local/share/icons")));
    }

    #[test]
    fn the_paeth_predictor_matches_the_specification() {
        // The three cases of the specification's own table.
        assert_eq!(paeth(10, 20, 15), 15);
        assert_eq!(paeth(0, 0, 0), 0);
        assert_eq!(paeth(200, 100, 150), 150);
    }

    /// A 2x2 RGBA image scales to sixteen squares of its own colour,
    /// weighted by coverage.
    #[test]
    fn scaling_average_keeps_a_solid_colour_solid() {
        let rgba = Rgba {
            width: 2,
            height: 2,
            bytes: [0x10u8, 0x20, 0x30, 0xff].repeat(4),
        };
        let icon = Icon::from_rgba(&rgba);
        assert_eq!(icon.pixels.len(), (ICON_SIZE * ICON_SIZE) as usize);
        for px in &icon.pixels {
            assert_eq!(*px, 0xff10_2030, "a solid icon stays one colour");
        }
    }

    /// A fully transparent icon stays transparent rather than turning black.
    #[test]
    fn scaling_a_transparent_image_keeps_the_alpha_down() {
        let rgba = Rgba {
            width: 4,
            height: 4,
            bytes: [0u8, 0, 0, 0].repeat(16),
        };
        let icon = Icon::from_rgba(&rgba);
        assert!(icon.pixels.iter().all(|p| *p == 0));
    }
}
