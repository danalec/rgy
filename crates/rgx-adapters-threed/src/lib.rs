//! 3D-model adapters: extract structure and names, not geometry payloads.
//! Text-based formats (obj, mtl, off, usda) are plain text and need no
//! adapter — they fall through to raw search.

#![no_std]

#[macro_use]
extern crate alloc;

#[cfg(test)]
extern crate std;

use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::cmp::Reverse;
use core::fmt::Write as _;
use core::str;
use rgx_core::{bail, Context, Result};
use rgx_core::{Adapter, Outcome};

pub fn adapters() -> Vec<Box<dyn Adapter>> {
    vec![
        Box::new(Glb),
        Box::new(Stl),
        Box::new(Ply),
        Box::new(Fbx),
        Box::new(Pcd),
        Box::new(Las),
        Box::new(Vtk),
        Box::new(Blend),
        Box::new(Threeds),
        Box::new(Nifti),
        Box::new(Mrc),
    ]
}

fn rd_u32(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}
fn rd_f64(b: &[u8], o: usize) -> f64 {
    f64::from_le_bytes(b[o..o + 8].try_into().unwrap())
}
fn rd_i32(b: &[u8], o: usize) -> i32 {
    i32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}
fn rd_f32(b: &[u8], o: usize) -> f32 {
    f32::from_le_bytes(b[o..o + 4].try_into().unwrap())
}

/// JSON chunk of a .glb, summarized: counts + every name field.
pub struct Glb;
impl Adapter for Glb {
    fn name(&self) -> &'static str {
        "glb"
    }
    fn extensions(&self) -> &'static [&'static str] {
        &["glb"]
    }
    fn extract(&self, data: &[u8], _path: &str) -> Result<Outcome> {
        if data.len() < 20 || &data[0..4] != b"glTF" {
            bail!("not a GLB file (bad magic)");
        }
        let total = rd_u32(data, 8) as usize;
        if total > data.len() {
            bail!("GLB length field overruns file");
        }
        // first chunk: u32 len + u32 type ("JSON")
        let clen = rd_u32(data, 12) as usize;
        if rd_u32(data, 16) != u32::from_le_bytes(*b"JSON") {
            bail!("GLB has no JSON chunk");
        }
        // the declared chunk length must fit inside the declared total
        // length *and* the actual file — fuzzed headers lie about both
        let json_end = 20usize.saturating_add(clen);
        if json_end > total || json_end > data.len() {
            bail!("GLB JSON chunk length overruns file");
        }
        let text = str::from_utf8(&data[20..json_end]).context("GLB JSON chunk is not UTF-8")?;
        // The lazy walk, not the Arena parser: the summary needs seven
        // fixed keys, each item's `name`, and the `asset` fragment --
        // the accessor/bufferView tails that dominate a glTF chunk are
        // validated and never materialised. Every walk error takes the
        // same context the Arena parser's errors took, so a malformed
        // chunk fails exactly as it failed before.
        let mut doc = rgx_core::djson::ondemand::Ondemand::new(text)
            .map_err(rgx_core::Error::msg)
            .context("GLB JSON chunk is not valid JSON")?;
        let jerr = |e: rgx_core::djson::parser::JsonParseError| {
            rgx_core::Error::msg(e).context("GLB JSON chunk is not valid JSON")
        };

        // One forward pass collects everything; output order is the
        // fixed key list below, not source order. Per key, the LAST
        // duplicate entry wins (the Arena's `object_get` semantics),
        // which is why each entry overwrites rather than accumulates.
        const KEYS: [&str; 7] = [
            "scenes",
            "nodes",
            "meshes",
            "materials",
            "animations",
            "skins",
            "images",
        ];
        let mut found: [Option<(usize, Vec<String>)>; 7] = Default::default();
        let mut asset: Option<String> = None;

        let mut root = doc.root().map_err(jerr)?;
        if root.is_object().map_err(jerr)? {
            root.object_entries(|k, mut v| {
                if k.as_ref() == "asset" {
                    asset = Some(v.to_compact_string()?);
                    return Ok(true);
                }
                if let Some(slot) = KEYS.iter().position(|&key| key == k.as_ref()) {
                    let mut got = None;
                    if v.is_array()? {
                        let mut len = 0usize;
                        let mut names = Vec::new();
                        v.array_items(|_, item| {
                            len += 1;
                            let mut name = None;
                            item.find_key("name", |n| {
                                name = n.as_str()?;
                                Ok(())
                            })?;
                            if let Some(n) = name {
                                names.push(n.into_owned());
                            }
                            Ok(true)
                        })?;
                        got = Some((len, names));
                    }
                    found[slot] = got;
                }
                Ok(true)
            })
            .map_err(jerr)?;
        } else {
            root.skip().map_err(jerr)?;
        }
        doc.finish().map_err(jerr)?;

        let mut out = String::new();
        for (i, key) in KEYS.iter().enumerate() {
            if let Some((len, names)) = &found[i] {
                let _ = writeln!(out, "{key}: {len}");
                for name in names {
                    let _ = writeln!(out, "  {key}[..]: {name}");
                }
            }
        }
        if let Some(a) = &asset {
            let _ = writeln!(out, "asset: {a}");
        }
        Ok(Outcome::Text(out.into_bytes()))
    }
}

pub struct Stl;
impl Adapter for Stl {
    fn name(&self) -> &'static str {
        "stl"
    }
    fn extensions(&self) -> &'static [&'static str] {
        &["stl"]
    }
    fn extract(&self, data: &[u8], _path: &str) -> Result<Outcome> {
        if data.starts_with(b"solid") {
            // ASCII STL is plain text — search it raw (bail semantics)
            return Ok(Outcome::Bail("ascii stl is plain text".into()));
        }
        if data.len() < 84 {
            bail!("unrecognized stl file");
        }
        let header = String::from_utf8_lossy(&data[..80]).trim().to_string();
        let count = rd_u32(data, 80);
        Ok(Outcome::Text(
            format!("stl: {count} triangles\nheader: {header}\n").into_bytes(),
        ))
    }
}

pub struct Ply;
impl Adapter for Ply {
    fn name(&self) -> &'static str {
        "ply"
    }
    fn extensions(&self) -> &'static [&'static str] {
        &["ply"]
    }
    fn extract(&self, data: &[u8], _path: &str) -> Result<Outcome> {
        if !data.starts_with(b"ply") {
            bail!("not a ply file");
        }
        let header_end = find_subslice(data, b"end_header").context("ply header not terminated")?;
        let header = String::from_utf8_lossy(&data[..header_end]).to_string();
        if header.contains("format ascii") {
            return Ok(Outcome::Bail("ascii ply is plain text".into()));
        }
        // keep only the header lines (element/property structure)
        Ok(Outcome::Text(header.into_bytes()))
    }
}

fn find_subslice(hay: &[u8], needle: &[u8]) -> Option<usize> {
    rgx_core::memchr::memmem::find(hay, needle)
}

/// Binary FBX: walk the node record tree, print node names + string props.
/// ASCII FBX bails to plain-text search.
pub struct Fbx;
impl Adapter for Fbx {
    fn name(&self) -> &'static str {
        "fbx"
    }
    fn extensions(&self) -> &'static [&'static str] {
        &["fbx"]
    }
    fn extract(&self, data: &[u8], _path: &str) -> Result<Outcome> {
        const MAGIC: &[u8; 23] = b"Kaydara FBX Binary  \x00\x1a\x00";
        if data.len() < 27 || &data[..23] != MAGIC {
            if data.starts_with(b"; FBX")
                || find_subslice(&data[..64.min(data.len())], b"FBXHeaderExtension").is_some()
            {
                return Ok(Outcome::Bail("ascii fbx is plain text".into()));
            }
            bail!("not a binary fbx file");
        }
        let version = rd_u32(data, 23);
        let ver740 = version >= 7400;
        let mut out = format!("fbx version: {version}\n");
        let mut pos = 27usize;
        let end = data.len();
        walk_fbx_nodes(data, &mut pos, end, ver740, 0, &mut out)?;
        Ok(Outcome::Text(out.into_bytes()))
    }
}

fn walk_fbx_nodes(
    d: &[u8],
    pos: &mut usize,
    limit: usize,
    ver740: bool,
    depth: usize,
    out: &mut String,
) -> Result<()> {
    if depth > 64 {
        return Ok(());
    }
    loop {
        let (end_off, _nprops, prop_len) = if ver740 {
            if *pos + 25 > d.len() {
                return Ok(());
            }
            let end = u64::from_le_bytes(d[*pos..*pos + 8].try_into()?) as usize;
            if end == 0 {
                return Ok(()); // null record = end of sibling list
            }
            (
                end,
                rd_u32(d, *pos + 8),
                d[*pos + 12..*pos + 20].try_into().map(u64::from_le_bytes)? as usize,
            )
        } else {
            if *pos + 13 > d.len() {
                return Ok(());
            }
            let end = rd_u32(d, *pos) as usize;
            if end == 0 {
                return Ok(());
            }
            (end, rd_u32(d, *pos + 4), rd_u32(d, *pos + 8) as usize)
        };
        let nlen_off = if ver740 { 20 } else { 12 };
        let nlen = d[*pos + nlen_off] as usize;
        let name_start = *pos + nlen_off + 1;
        let name_end = name_start.saturating_add(nlen);
        if name_end > d.len() {
            return Ok(()); // truncated record name
        }
        let name = String::from_utf8_lossy(&d[name_start..name_end]).to_string();
        out.push_str(&"  ".repeat(depth));
        let _ = writeln!(out, "node: {name}");
        // parse string properties (type 'S')
        let mut p = name_start + nlen;
        let prop_end = p.saturating_add(prop_len);
        while p + 1 < prop_end.min(d.len()) {
            match d[p] {
                b'S' | b'R' => {
                    let Some(lb) = d.get(p + 1..p + 5) else { break };
                    let len = u32::from_le_bytes(lb.try_into().unwrap()) as usize;
                    if p + 5 + len <= d.len() {
                        let s = String::from_utf8_lossy(&d[p + 5..p + 5 + len]).to_string();
                        out.push_str(&"  ".repeat(depth));
                        let _ = writeln!(out, "  S: {s}");
                        p += 5 + len;
                    } else {
                        break;
                    }
                }
                b'C' | b'B' | b'Y' | b'I' | b'F' | b'D' | b'L' => {
                    p += match d[p] {
                        b'C' | b'B' | b'Y' => 2,
                        b'I' | b'F' => 5,
                        _ => 9,
                    };
                }
                b'i' | b'f' | b'l' | b'd' | b'b' => {
                    // array: [type][n u32][encoding u32][complen u32][data]
                    let Some(lb) = d.get(p + 9..p + 13) else {
                        break;
                    };
                    let data_len = u32::from_le_bytes(lb.try_into().unwrap()) as usize;
                    p = p.saturating_add(13).saturating_add(data_len);
                }
                _ => break,
            }
        }
        let record_start = *pos;
        // prop_len comes from the file and can be u64::MAX on fuzzed input
        *pos = name_start.saturating_add(nlen).saturating_add(prop_len);
        // nested children live before end_off
        if *pos < end_off.saturating_sub(if ver740 { 25 } else { 13 }) && end_off <= d.len() {
            walk_fbx_nodes(d, pos, end_off, ver740, depth + 1, out)?;
        }
        *pos = end_off;
        // a record whose end does not move past its own start would loop
        // forever — fuzzed headers craft exactly that
        if end_off <= record_start || *pos >= limit {
            return Ok(());
        }
    }
}

pub struct Pcd;
impl Adapter for Pcd {
    fn name(&self) -> &'static str {
        "pcd"
    }
    fn extensions(&self) -> &'static [&'static str] {
        &["pcd"]
    }
    fn extract(&self, data: &[u8], _path: &str) -> Result<Outcome> {
        let header_end = find_subslice(data, b"DATA").context("pcd header has no DATA line")?;
        let header = String::from_utf8_lossy(&data[..header_end]).to_string();
        if header.contains("DATA ascii") {
            return Ok(Outcome::Bail("ascii pcd is plain text".into()));
        }
        Ok(Outcome::Text(header.into_bytes()))
    }
}

pub struct Las;
impl Adapter for Las {
    fn name(&self) -> &'static str {
        "las"
    }
    fn extensions(&self) -> &'static [&'static str] {
        &["las", "laz"]
    }
    fn extract(&self, data: &[u8], _path: &str) -> Result<Outcome> {
        if data.len() < 227 || &data[0..4] != b"LASF" {
            bail!("not a LAS file (bad signature)");
        }
        let version = format!("{}.{}", data[24], data[25]);
        let point_format = data[104] & 0x3f;
        let legacy_count = rd_u32(data, 107);
        let (x_max, x_min) = (rd_f64(data, 179), rd_f64(data, 187));
        let (y_max, y_min) = (rd_f64(data, 195), rd_f64(data, 203));
        let (z_max, z_min) = (rd_f64(data, 211), rd_f64(data, 219));
        Ok(Outcome::Text(
            format!(
                "las version: {version}\npoint format: {point_format}\npoints: {legacy_count}\n\
             bbox: ({x_min},{y_min},{z_min}) .. ({x_max},{y_max},{z_max})\n"
            )
            .into_bytes(),
        ))
    }
}

/// Legacy VTK: title + dataset type (+ dims for structured sets). Binary
/// files contribute only the header; ASCII bails to plain-text search.
pub struct Vtk;
impl Adapter for Vtk {
    fn name(&self) -> &'static str {
        "vtk"
    }
    fn extensions(&self) -> &'static [&'static str] {
        &["vtk"]
    }
    fn extract(&self, data: &[u8], _path: &str) -> Result<Outcome> {
        let first = data
            .get(..70)
            .map(|h| String::from_utf8_lossy(h).to_string())
            .context("vtk file too small")?;
        if !first.starts_with("# vtk DataFile Version") {
            bail!("not a legacy vtk file");
        }
        if find_subslice(&data[..data.len().min(4096)], b"BINARY").is_some() {
            let header: String = first.lines().take(6).map(|l| format!("{l}\n")).collect();
            return Ok(Outcome::Text(header.into_bytes()));
        }
        Ok(Outcome::Bail("ascii vtk is plain text".into()))
    }
}

// ---------- Blender .blend ----------

/// Blender scene file: walk the block sequence (code, size, sdna, count)
/// and lift the names out of ID blocks — object, mesh, material, camera,
/// light, scene names are what a code search wants from a .blend. The name
/// sits at offset 4×pointer_size inside every two-uppercase-letter block
/// (ID struct header: 4 pointers then `name[66]`).
pub struct Blend;
impl Adapter for Blend {
    fn name(&self) -> &'static str {
        "blend"
    }
    fn extensions(&self) -> &'static [&'static str] {
        &["blend"]
    }
    fn extract(&self, data: &[u8], _path: &str) -> Result<Outcome> {
        if data.len() < 12 || &data[0..7] != b"BLENDER" {
            bail!("not a Blender file (bad magic)");
        }
        let ptr = match data[7] {
            b'_' => 4usize,
            b'-' => 8,
            c => bail!("unknown Blender pointer-size marker {c:#x}"),
        };
        let le = match data[8] {
            b'v' => true,
            b'V' => false,
            c => bail!("unknown Blender endianness marker {c:#x}"),
        };
        let version = String::from_utf8_lossy(&data[9..12]).into_owned();
        let rd32 = |b: &[u8], o: usize| -> u32 {
            let s = &b[o..o + 4];
            if le {
                u32::from_le_bytes([s[0], s[1], s[2], s[3]])
            } else {
                u32::from_be_bytes([s[0], s[1], s[2], s[3]])
            }
        };
        let mut out = format!(
            "blender v{version} ptr={ptr}B {}\n",
            if le { "little-endian" } else { "big-endian" }
        );
        let mut p = 12usize;
        // block codes are 4 raw bytes, NUL-padded ("OB\0\0"). Counting by
        // the raw `[u8; 4]` keeps the inner loop alloc-free: a file with
        // 100 000 blocks used to allocate one `String` per block just to
        // bump the histogram counter. The owned `String` is built only
        // for the 24 lines the summary actually prints.
        let mut counts: BTreeMap<[u8; 4], usize> = BTreeMap::new();
        let mut names = 0usize;
        let mut blocks = 0usize;
        while p + 4 + 4 + ptr + 8 <= data.len() && blocks < 100_000 {
            let code = &data[p..p + 4];
            if code == b"ENDB" {
                break;
            }
            let size = rd32(data, p + 4) as usize;
            let body = p + 4 + 4 + ptr + 8;
            if size > (1 << 30) || body.checked_add(size).is_none_or(|e| e > data.len()) {
                bail!("blend block overruns file");
            }
            blocks += 1;
            // Copy the 4-byte code into an owned `[u8; 4]` so the
            // histogram key is hashable; `entry` requires `K: Borrow<[u8; 4]>`,
            // which the borrowed slice `&[u8]` is not.
            let code_arr: [u8; 4] = [code[0], code[1], code[2], code[3]];
            *counts.entry(code_arr).or_insert(0) += 1;
            // ID blocks: two uppercase letters carry a name at 4*ptr
            let is_id = code[0].is_ascii_uppercase() && code[1].is_ascii_uppercase();
            if is_id && names < 4096 {
                let noff = body + 4 * ptr;
                // name window must fit inside both the file and the block
                let end = (noff + 66).min(data.len()).min(body + size);
                if noff < end {
                    let avail = &data[noff..end];
                    let end = avail.iter().position(|&b| b == 0).unwrap_or(avail.len());
                    let name = String::from_utf8_lossy(&avail[..end]).trim().to_string();
                    if !name.is_empty() && out.len() < (1 << 20) {
                        // ASCII prefix is always valid UTF-8; the
                        // 2-letter code is rendered as a str slice to
                        // avoid a transient String.
                        let code_prefix = core::str::from_utf8(&code_arr[..2]).unwrap_or("??");
                        let _ = writeln!(out, "{code_prefix}: {name}");
                        names += 1;
                    }
                }
            }
            p = body + size;
        }
        if blocks == 0 {
            bail!("no blend blocks");
        }
        let mut hist: Vec<([u8; 4], usize)> = counts.into_iter().collect();
        hist.sort_by_key(|a| Reverse(a.1));
        let summary: Vec<String> = hist
            .iter()
            .take(24)
            .map(|(c, n)| {
                // Trim NULs once, here, for the 24 lines that actually ship.
                let trimmed_end = c.iter().position(|&b| b == 0).unwrap_or(c.len());
                let code_str = core::str::from_utf8(&c[..trimmed_end]).unwrap_or("?");
                format!("{code_str}×{n}")
            })
            .collect();
        let _ = writeln!(out, "blocks: {blocks} [{}]", summary.join(" "));
        Ok(Outcome::Text(out.into_bytes()))
    }
}

// ---------- 3D Studio .3ds ----------

/// 3ds Max chunk hierarchy: u16 id + u32 length, recursive. Lift object
/// names (0x4000), material names (0xA000) and texture filenames (0xA300)
/// — everything else gets counted, not decoded.
pub struct Threeds;
impl Adapter for Threeds {
    fn name(&self) -> &'static str {
        "3ds"
    }
    fn extensions(&self) -> &'static [&'static str] {
        &["3ds", "max3ds"]
    }
    fn extract(&self, data: &[u8], _path: &str) -> Result<Outcome> {
        if data.len() < 6 {
            bail!("3ds file too small");
        }
        let mut out = String::new();
        let mut counts: BTreeMap<u16, usize> = BTreeMap::new();
        let mut entries = 0usize;
        walk3ds(data, 0, data.len(), 0, &mut |id, payload, named| {
            *counts.entry(id).or_default() += 1;
            if named && entries < 2048 && out.len() < (1 << 20) {
                let label = match id {
                    0x4000 => "object",
                    0xA000 => "material",
                    0xA300 => "texture",
                    _ => "name",
                };
                let end = payload
                    .iter()
                    .position(|&b| b == 0)
                    .unwrap_or(payload.len());
                let name = String::from_utf8_lossy(&payload[..end]).trim().to_string();
                if !name.is_empty() {
                    let _ = writeln!(out, "{label}: {name}");
                    entries += 1;
                }
            }
        });
        if counts.is_empty() {
            bail!("no 3ds chunks");
        }
        let main = counts.get(&0x4D4D).copied().unwrap_or(0);
        if main == 0 {
            bail!("no 0x4D4D main chunk (not a 3ds file)");
        }
        let mut summary = format!("3ds main chunks: {main}\n");
        summary.push_str(&out);
        Ok(Outcome::Text(summary.into_bytes()))
    }
}

/// recursive chunk walk; `f` sees (id, payload-prefix, carries-a-name)
fn walk3ds(
    data: &[u8],
    start: usize,
    end: usize,
    depth: usize,
    f: &mut impl FnMut(u16, &[u8], bool),
) {
    if depth > 12 {
        return;
    }
    let mut p = start;
    while p + 6 <= end.min(data.len()) {
        let id = u16::from_le_bytes([data[p], data[p + 1]]);
        let len = u32::from_le_bytes([data[p + 2], data[p + 3], data[p + 4], data[p + 5]]) as usize;
        if len < 6 {
            return; // corrupt length: stop this level
        }
        let cend = match p.checked_add(len) {
            Some(e) if e <= end.min(data.len()) => e,
            _ => return,
        };
        let payload = &data[p + 6..cend];
        let named = matches!(id, 0x4000 | 0xA000 | 0xA300);
        f(id, payload, named);
        // container chunks worth descending into
        if matches!(id, 0x4D4D | 0x3D3D | 0x4100 | 0xA200 | 0xB000) {
            walk3ds(data, p + 6, cend, depth + 1, f);
        }
        p = cend;
    }
}

// ---------- NIfTI neuroimaging volume ----------

fn datatype_name(code: i16) -> &'static str {
    match code {
        2 => "uint8",
        4 => "int16",
        8 => "int32",
        16 => "float32",
        32 => "float64",
        64 => "float128",
        128 => "rgb",
        256 => "int8",
        512 => "uint16",
        768 => "uint32",
        1024 => "int64",
        1280 => "uint64",
        _ => "unknown",
    }
}

/// NIfTI-1/2 medical volume header: dims, voxel datatype/size, spacing,
/// description, intent name and qform/sform orientation codes. The voxel
/// payload is never decoded — a code search wants the metadata.
pub struct Nifti;
impl Adapter for Nifti {
    fn name(&self) -> &'static str {
        "nifti"
    }
    fn extensions(&self) -> &'static [&'static str] {
        &["nii"]
    }
    fn extract(&self, data: &[u8], _path: &str) -> Result<Outcome> {
        if data.len() < 348 {
            bail!("shorter than a nifti-1 header");
        }
        let sizeof_hdr = rd_u32(data, 0) as usize;
        let (version, magic) = match sizeof_hdr {
            348 => {
                let magic = &data[344..348];
                (1, magic)
            }
            540 if data.len() >= 540 => {
                let magic = &data[488..492];
                (2, magic)
            }
            _ => bail!("not a nifti file (sizeof_hdr = {sizeof_hdr})"),
        };
        let mut out = String::from("nifti volume\n");
        match version {
            1 => {
                let _ = writeln!(
                    out,
                    "version: 1 ({})",
                    String::from_utf8_lossy(magic).trim_end_matches('\0')
                );
                let ndim = data[40] as usize;
                let dims: Vec<i16> = (0..8)
                    .map(|i| i16::from_le_bytes([data[40 + i * 2], data[41 + i * 2]]))
                    .collect();
                let datatype = i16::from_le_bytes([data[70], data[71]]);
                let bitpix = i16::from_le_bytes([data[72], data[73]]);
                let pixdim: Vec<f32> = (0..8)
                    .map(|i| f32::from_le_bytes(data[76 + i * 4..80 + i * 4].try_into().unwrap()))
                    .collect();
                let vox_offset = f32::from_le_bytes(data[108..112].try_into().unwrap());
                let descrip = String::from_utf8_lossy(&data[148..228])
                    .trim_end_matches('\0')
                    .to_string();
                let intent_name = String::from_utf8_lossy(&data[328..344])
                    .trim_end_matches('\0')
                    .to_string();
                let qform = i16::from_le_bytes([data[252], data[253]]);
                let sform = i16::from_le_bytes([data[254], data[255]]);
                let _ = writeln!(
                    out,
                    "datatype: {} ({} bit)\ndim: {:?}\npixdim: {:?}\nvoxel data offset: {vox_offset}\n",
                    datatype_name(datatype),
                    bitpix,
                    &dims[..ndim.min(7) + 1],
                    &pixdim[..ndim.min(7) + 1]
                );
                let _ = writeln!(out, "qform: {qform} sform: {sform}");
                if !descrip.is_empty() {
                    let _ = writeln!(out, "description: {descrip}");
                }
                if !intent_name.is_empty() {
                    let _ = writeln!(out, "intent: {intent_name}");
                }
            }
            _ => {
                let _ = writeln!(
                    out,
                    "version: 2 ({})",
                    String::from_utf8_lossy(magic).trim_end_matches('\0')
                );
                let datatype = i16::from_le_bytes([data[4], data[5]]);
                let bitpix = i16::from_le_bytes([data[6], data[7]]);
                let dims: Vec<i16> = (0..8)
                    .map(|i| i16::from_le_bytes([data[8 + i * 2], data[9 + i * 2]]))
                    .collect();
                let pixdim: Vec<f64> = (0..8)
                    .map(|i| f64::from_le_bytes(data[48 + i * 8..56 + i * 8].try_into().unwrap()))
                    .collect();
                let vox_offset = i64::from_le_bytes(data[112..120].try_into().unwrap());
                let descrip = String::from_utf8_lossy(&data[184..264])
                    .trim_end_matches('\0')
                    .to_string();
                let intent_name = String::from_utf8_lossy(&data[456..472])
                    .trim_end_matches('\0')
                    .to_string();
                let qform = rd_u32(data, 288);
                let sform = rd_u32(data, 292);
                let _ = writeln!(
                    out,
                    "datatype: {} ({} bit)\ndim: {:?}\npixdim: {:?}\nvoxel data offset: {vox_offset}\n",
                    datatype_name(datatype),
                    bitpix,
                    &dims[..8],
                    &pixdim[..8]
                );
                let _ = writeln!(out, "qform: {qform} sform: {sform}");
                if !descrip.is_empty() {
                    let _ = writeln!(out, "description: {descrip}");
                }
                if !intent_name.is_empty() {
                    let _ = writeln!(out, "intent: {intent_name}");
                }
            }
        }
        Ok(Outcome::Text(out.into_bytes()))
    }
}

/// MRC/CCP4 volume-data header reader.
///
/// MRC is the medical-/structural-biology volume format: a 1024-byte
/// little-endian header followed by a voxel slab. The searchable
/// shape is the header — voxel counts, cell dimensions, mode, the
/// 800-byte label block — and the format itself encodes no text past
/// the labels, so a search that wants to find a volume named
/// "nucleosome" needs the labels and the comment that some tools
/// stash in byte 800. CCP4 extends MRC: at offset 208 the file carries
/// the magic "MAP " (little-endian 0x4d415020), which the adapter
/// reads to print `format: ccp4` instead of `format: mrc`.
pub struct Mrc;
impl Adapter for Mrc {
    fn name(&self) -> &'static str {
        "mrc"
    }
    fn extensions(&self) -> &'static [&'static str] {
        &["mrc", "ccp4", "map"]
    }
    fn extract(&self, data: &[u8], _path: &str) -> Result<Outcome> {
        // The header is 1024 bytes; the file is required to be at
        // least that long. A truncated header is not a fatal error --
        // the volume is unreadable either way, but emitting a
        // searchable note lets the user find that fact with the
        // matcher rather than discovering it from a silent skip.
        if data.len() < 1024 {
            bail!("mrc/CCP4 header is 1024 bytes; file is {}", data.len());
        }
        let nx = rd_i32(data, 0);
        let ny = rd_i32(data, 4);
        let nz = rd_i32(data, 8);
        let mode = rd_i32(data, 12);
        let cell_a = rd_f32(data, 40);
        let cell_b = rd_f32(data, 44);
        let cell_c = rd_f32(data, 48);
        let alpha = rd_f32(data, 52);
        let beta = rd_f32(data, 56);
        let gamma = rd_f32(data, 60);
        let dmin = rd_f32(data, 76);
        let dmax = rd_f32(data, 80);
        let dmean = rd_f32(data, 84);
        let ispg = rd_i32(data, 88);
        let nlabl = rd_i32(data, 92);
        // CCP4 magic: at offset 208, the bytes "MAP " (little-endian
        // 0x20 0x50 0x41 0x4d reads as " MAP" forward, but the file
        // stores them as 0x4d 0x41 0x50 0x20 which is "MAP ").
        let format = if data.len() >= 212 && &data[208..212] == b"MAP " {
            "ccp4"
        } else {
            "mrc"
        };
        let mode_name = match mode {
            0 => "int8",
            1 => "int16",
            2 => "float32",
            3 => "complex-int32",
            4 => "complex-float32",
            5 => "int16-unsigned",
            6 => "float16",
            _ => "unknown",
        };
        let mut out = String::with_capacity(1024);
        let _ = writeln!(out, "format: {format}");
        let _ = writeln!(out, "dimensions: {nx} {ny} {nz}");
        let _ = writeln!(out, "mode: {mode} ({mode_name})");
        let _ = writeln!(out, "cell: {cell_a:.3} {cell_b:.3} {cell_c:.3} A");
        let _ = writeln!(out, "angles: {alpha:.2} {beta:.2} {gamma:.2} deg");
        let _ = writeln!(out, "density: min={dmin:.4} max={dmax:.4} mean={dmean:.4}");
        let _ = writeln!(out, "spacegroup: {ispg}");
        // 10 labels of 80 bytes each, starting at offset 104. NLABL is
        // informational; the fixed 10-slot layout is the contract.
        let labels_end = 104 + 10 * 80;
        let labels_end = labels_end.min(data.len());
        if labels_end > 104 {
            for (i, label) in data[104..labels_end].chunks(80).take(10).enumerate() {
                let text = String::from_utf8_lossy(label)
                    .trim_end_matches('\0')
                    .trim()
                    .to_string();
                if !text.is_empty() {
                    let _ = writeln!(out, "label{i}: {text}");
                }
                if nlabl >= 0 && i + 1 >= nlabl as usize {
                    break;
                }
            }
        }
        Ok(Outcome::Text(out.into_bytes()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn binary_stl_triangle_count() {
        let mut stl = vec![0u8; 80];
        stl[0] = b'x'; // not "solid" -> binary
        stl.extend_from_slice(&2u32.to_le_bytes());
        stl.extend_from_slice(&[0u8; 100]);
        let out = Stl.extract(&stl, "p.stl").unwrap();
        match out {
            Outcome::Text(t) => assert!(String::from_utf8_lossy(&t).contains("2 triangles")),
            _ => panic!("expected text"),
        }
    }

    #[test]
    fn ascii_stl_bails() {
        let out = Stl.extract(b"solid foo\nendsolid foo\n", "p.stl").unwrap();
        assert!(matches!(out, Outcome::Bail(_)));
    }

    fn nifti1_fixture() -> Vec<u8> {
        let mut d = vec![0u8; 360];
        d[0..4].copy_from_slice(&348u32.to_le_bytes());
        d[40] = 3; // ndim
        for (i, v) in [64i16, 64, 32].iter().enumerate() {
            d[40 + i * 2..42 + i * 2].copy_from_slice(&v.to_le_bytes());
        }
        d[70..72].copy_from_slice(&16i16.to_le_bytes()); // float32
        d[72..74].copy_from_slice(&32i16.to_le_bytes()); // bitpix
        for i in 0..4 {
            let px = 2.0f32.to_le_bytes();
            d[76 + i * 4..80 + i * 4].copy_from_slice(&px);
        }
        d[108..112].copy_from_slice(&352.0f32.to_le_bytes()); // vox_offset
        d[148..148 + 12].copy_from_slice(b"ZIRCALOY MRI");
        d[344..348].copy_from_slice(b"n+1\0");
        d
    }

    #[test]
    fn nifti1_summarizes_header() {
        let out = Nifti.extract(&nifti1_fixture(), "scan.nii").unwrap();
        let Outcome::Text(t) = out else {
            panic!("text")
        };
        let s = String::from_utf8_lossy(&t);
        assert!(s.contains("version: 1 (n+1)"), "{s}");
        assert!(s.contains("float32 (32 bit)"), "{s}");
        assert!(s.contains("description: ZIRCALOY MRI"), "{s}");
    }

    #[test]
    fn nifti2_summarizes_header() {
        let mut d = vec![0u8; 560];
        d[0..4].copy_from_slice(&540u32.to_le_bytes());
        d[4..6].copy_from_slice(&16i16.to_le_bytes()); // float32
        d[6..8].copy_from_slice(&32i16.to_le_bytes()); // bitpix
        d[8..10].copy_from_slice(&2i16.to_le_bytes()); // ndim
        d[488..492].copy_from_slice(b"n+2\0");
        let out = Nifti.extract(&d, "scan.nii").unwrap();
        let Outcome::Text(t) = out else {
            panic!("text")
        };
        let s = String::from_utf8_lossy(&t);
        assert!(s.contains("version: 2 (n+2)"), "{s}");
    }

    #[test]
    fn nifti_never_panics_on_mutation() {
        rgx_core::fuzzkit::assert_never_panics(
            vec![
                ("nifti1", nifti1_fixture()),
                ("sizeof-only", 348u32.to_le_bytes().to_vec()),
                ("empty", vec![]),
            ],
            1200,
            "nii",
            |data| {
                let _ = Nifti.extract(data, "fuzz.nii");
            },
        );
    }

    #[test]
    fn glb_summarizes_names() {
        let json = br#"{"asset":{"version":"2.0"},"scenes":[{"name":"main"}],"nodes":[{"name":"root"},{"name":"wheel"}],"meshes":[{"name":"body"}]}"#;
        let mut glb = Vec::new();
        glb.extend_from_slice(b"glTF");
        glb.extend_from_slice(&2u32.to_le_bytes());
        glb.extend_from_slice(&((12 + 8 + json.len()) as u32).to_le_bytes());
        glb.extend_from_slice(&(json.len() as u32).to_le_bytes());
        glb.extend_from_slice(b"JSON");
        glb.extend_from_slice(json);
        let out = Glb.extract(&glb, "m.glb").unwrap();
        match out {
            Outcome::Text(t) => {
                let t = String::from_utf8_lossy(&t);
                assert!(t.contains("nodes: 2") && t.contains("wheel") && t.contains("meshes: 1"));
            }
            _ => panic!("expected text"),
        }
    }

    /// Wrap a JSON chunk in the minimal GLB container the adapter
    /// accepts: 12-byte header + 8-byte chunk header + chunk.
    fn glb_wrap(json: &[u8]) -> Vec<u8> {
        let mut glb = Vec::new();
        glb.extend_from_slice(b"glTF");
        glb.extend_from_slice(&2u32.to_le_bytes());
        glb.extend_from_slice(&((12 + 8 + json.len()) as u32).to_le_bytes());
        glb.extend_from_slice(&(json.len() as u32).to_le_bytes());
        glb.extend_from_slice(b"JSON");
        glb.extend_from_slice(json);
        glb
    }

    /// Golden: the full GLB summary, byte for byte.
    ///
    /// The `asset` fragment pins the one order quirk of the Arena
    /// parser: object keys serialise in first-appearance rank order,
    /// not source order. `"version"` is interned under `"pre"` before
    /// `"generator"` appears, so `version` leads the `asset` line even
    /// though `generator` comes first in the source. The lazy walk
    /// must reproduce this exactly.
    #[test]
    fn glb_output_is_pinned_byte_for_byte() {
        let json = br#"{"pre":{"version":null},"asset":{"generator":"g","version":"2.0"},"scenes":[{"name":"Scene"}],"nodes":[{"name":"Wheel","mesh":0},{"name":"Door"}],"meshes":[{}],"images":[{},{"name":"tex"}]}"#;
        let out = Glb.extract(&glb_wrap(json), "m.glb").unwrap();
        let Outcome::Text(t) = out else {
            panic!("expected text");
        };
        let s = String::from_utf8_lossy(&t);
        assert_eq!(
            s,
            "scenes: 1\n  scenes[..]: Scene\nnodes: 2\n  nodes[..]: Wheel\n  \
nodes[..]: Door\nmeshes: 1\nimages: 2\n  images[..]: tex\n\
asset: {\"version\":\"2.0\",\"generator\":\"g\"}\n"
        );
    }

    /// Duplicate keys take the last value, matching the Arena parser's
    /// `BTreeMap` lookup.
    #[test]
    fn glb_duplicate_keys_take_the_last_value() {
        let json = br#"{"images":[{"name":"first"}],"images":[{"name":"second"}],"nodes":[{"name":"a"},{"name":"a"}]}"#;
        let out = Glb.extract(&glb_wrap(json), "m.glb").unwrap();
        let Outcome::Text(t) = out else {
            panic!("expected text");
        };
        let s = String::from_utf8_lossy(&t);
        assert_eq!(
            s,
            "nodes: 2\n  nodes[..]: a\n  nodes[..]: a\nimages: 1\n  images[..]: second\n"
        );
    }

    /// A valid JSON chunk that is not an object summarises to nothing,
    /// just as `Value::get` finds nothing on a non-object.
    #[test]
    fn glb_non_object_json_chunk_yields_empty_text() {
        let out = Glb.extract(&glb_wrap(b"[1, 2, 3]"), "m.glb").unwrap();
        let Outcome::Text(t) = out else {
            panic!("expected text");
        };
        assert!(t.is_empty(), "{t:?}");
    }

    /// Invalid JSON in the chunk is an adapter failure with its
    /// context message, not a silent empty summary.
    #[test]
    fn glb_invalid_json_reports_the_context_error() {
        let err = match Glb.extract(&glb_wrap(b"{\"a\": 01}"), "m.glb") {
            Ok(_) => panic!("bad number must fail"),
            Err(e) => e,
        };
        let msg = format!("{err}");
        assert!(msg.contains("GLB JSON chunk is not valid JSON"), "{msg}");
    }

    mod fuzz {
        use super::*;

        /// header + one OB block whose ID name sits at 4×ptr + ENDB
        fn blend_fixture() -> Vec<u8> {
            let mut d = b"BLENDER-v293".to_vec();
            // OB block: code + size + oldaddr(8) + sdna(4) + count(4) + body
            let body = {
                let mut b = vec![0u8; 32]; // 4 pointers
                b.extend_from_slice(b"Cube_ZIRCALOY_probe\0");
                b
            };
            d.extend_from_slice(b"OB\0\0");
            d.extend_from_slice(&(body.len() as u32).to_le_bytes());
            d.extend_from_slice(&[0u8; 8]); // old address
            d.extend_from_slice(&0u32.to_le_bytes()); // sdna
            d.extend_from_slice(&1u32.to_le_bytes()); // count
            d.extend_from_slice(&body);
            d.extend_from_slice(b"ENDB");
            d.extend_from_slice(&[0u8; 16]);
            d
        }

        /// 4D4D main chunk containing a 4000 object chunk
        fn threeds_fixture() -> Vec<u8> {
            let mut obj = Vec::new();
            obj.extend_from_slice(&0x4000u16.to_le_bytes());
            let name = b"turbine_blade";
            let obj_len = 6 + name.len() + 1 + 6; // header + name + empty subchunk header
            obj.extend_from_slice(&(obj_len as u32).to_le_bytes());
            obj.extend_from_slice(name);
            obj.push(0);
            obj.extend_from_slice(&0x4100u16.to_le_bytes());
            obj.extend_from_slice(&6u32.to_le_bytes()); // empty mesh chunk
            let mut d = Vec::new();
            d.extend_from_slice(&0x4D4Du16.to_le_bytes());
            d.extend_from_slice(&((6 + obj.len()) as u32).to_le_bytes());
            d.extend_from_slice(&obj);
            d
        }

        #[test]
        fn blend_lifts_id_names() {
            let out = Blend.extract(&blend_fixture(), "s.blend").unwrap();
            let Outcome::Text(t) = out else {
                panic!("text")
            };
            let s = String::from_utf8_lossy(&t);
            assert!(s.contains("blender v293"), "{s}");
            assert!(s.contains("OB: Cube_ZIRCALOY_probe"), "{s}");
            assert!(s.contains("blocks: 1"), "{s}");
        }

        #[test]
        fn threeds_lifts_object_names() {
            let out = Threeds.extract(&threeds_fixture(), "m.3ds").unwrap();
            let Outcome::Text(t) = out else {
                panic!("text")
            };
            let s = String::from_utf8_lossy(&t);
            assert!(s.contains("object: turbine_blade"), "{s}");
            assert!(s.contains("3ds main chunks: 1"), "{s}");
        }

        #[test]
        fn blend_never_panics_on_mutation() {
            rgx_core::fuzzkit::assert_never_panics(
                vec![
                    ("v293", blend_fixture()),
                    ("magic-only", b"BLENDER-v".to_vec()),
                    ("be", b"BLENDER_V400".to_vec()),
                    ("empty", vec![]),
                ],
                1200,
                "blend",
                |data| {
                    let _ = Blend.extract(data, "fuzz.blend");
                },
            );
        }

        #[test]
        fn threeds_never_panics_on_mutation() {
            rgx_core::fuzzkit::assert_never_panics(
                vec![
                    ("object", threeds_fixture()),
                    ("main-only", {
                        let mut d = 0x4D4Du16.to_le_bytes().to_vec();
                        d.extend_from_slice(&6u32.to_le_bytes());
                        d
                    }),
                    ("empty", vec![]),
                ],
                1200,
                "3ds",
                |data| {
                    let _ = Threeds.extract(data, "fuzz.3ds");
                },
            );
        }

        fn binary_stl() -> Vec<u8> {
            let mut stl = vec![0u8; 80];
            stl[0] = b'x';
            stl.extend_from_slice(&2u32.to_le_bytes());
            stl.extend_from_slice(&[0u8; 100]);
            stl
        }

        fn glb_fixture() -> Vec<u8> {
            let json = br#"{"asset":{"version":"2.0"},"nodes":[{"name":"root"}]}"#;
            let mut glb = Vec::new();
            glb.extend_from_slice(b"glTF");
            glb.extend_from_slice(&2u32.to_le_bytes());
            glb.extend_from_slice(&((12 + 8 + json.len()) as u32).to_le_bytes());
            glb.extend_from_slice(&(json.len() as u32).to_le_bytes());
            glb.extend_from_slice(b"JSON");
            glb.extend_from_slice(json);
            glb
        }

        fn binary_fbx() -> Vec<u8> {
            let mut d = b"Kaydara FBX Binary  \x00\x1a\x00".to_vec();
            d.extend_from_slice(&7400u32.to_le_bytes());
            // one node record + null record
            d.extend_from_slice(&[0u8; 25]);
            d
        }

        fn las_header() -> Vec<u8> {
            let mut d = vec![0u8; 227];
            d[0..4].copy_from_slice(b"LASF");
            d[24] = 1;
            d[25] = 2;
            d
        }

        #[test]
        fn stl_never_panics_on_mutation() {
            rgx_core::fuzzkit::assert_never_panics(
                vec![
                    ("binary", binary_stl()),
                    ("magic-only", b"solid ".to_vec()),
                    ("empty", vec![]),
                ],
                1200,
                "stl",
                |data| {
                    let _ = Stl.extract(data, "fuzz.stl");
                },
            );
        }

        #[test]
        fn glb_never_panics_on_mutation() {
            rgx_core::fuzzkit::assert_never_panics(
                vec![
                    ("glb", glb_fixture()),
                    ("magic-only", b"glTF".to_vec()),
                    ("empty", vec![]),
                ],
                1200,
                "glb",
                |data| {
                    let _ = Glb.extract(data, "fuzz.glb");
                },
            );
        }

        #[test]
        fn fbx_never_panics_on_mutation() {
            rgx_core::fuzzkit::assert_never_panics(
                vec![
                    ("binary", binary_fbx()),
                    ("magic-only", b"Kaydara FBX Binary  \x00\x1a\x00".to_vec()),
                    ("empty", vec![]),
                ],
                1500,
                "fbx",
                |data| {
                    let _ = Fbx.extract(data, "fuzz.fbx");
                },
            );
        }

        #[test]
        fn ply_never_panics_on_mutation() {
            let ply =
                b"ply\nformat binary_little_endian 1.0\nelement vertex 3\nend_header\n".to_vec();
            rgx_core::fuzzkit::assert_never_panics(
                vec![
                    ("binary-ply", ply),
                    ("magic-only", b"ply\n".to_vec()),
                    ("empty", vec![]),
                ],
                1000,
                "ply",
                |data| {
                    let _ = Ply.extract(data, "fuzz.ply");
                },
            );
        }

        #[test]
        fn pcd_never_panics_on_mutation() {
            let pcd = b"# .PCD v0.7\nVERSION 0.7\nFIELDS x y z\nSIZE 4 4 4\nTYPE F F F\nCOUNT 1 1 1\nWIDTH 1\nHEIGHT 1\nPOINTS 1\nDATA binary\n".to_vec();
            rgx_core::fuzzkit::assert_never_panics(
                vec![("binary-pcd", pcd), ("empty", vec![])],
                1000,
                "pcd",
                |data| {
                    let _ = Pcd.extract(data, "fuzz.pcd");
                },
            );
        }

        #[test]
        fn las_never_panics_on_mutation() {
            rgx_core::fuzzkit::assert_never_panics(
                vec![
                    ("las", las_header()),
                    ("magic-only", b"LASF".to_vec()),
                    ("empty", vec![]),
                ],
                1200,
                "las",
                |data| {
                    let _ = Las.extract(data, "fuzz.las");
                },
            );
        }

        #[test]
        fn vtk_never_panics_on_mutation() {
            let vtk = b"# vtk DataFile Version 3.0\nZIRCALOY sim\nBINARY\nDATASET STRUCTURED_POINTS\nDIMENSIONS 2 2 2\n".to_vec();
            rgx_core::fuzzkit::assert_never_panics(
                vec![
                    ("binary-vtk", vtk),
                    ("magic-only", b"# vtk DataFile Version".to_vec()),
                    ("empty", vec![]),
                ],
                1000,
                "vtk",
                |data| {
                    let _ = Vtk.extract(data, "fuzz.vtk");
                },
            );
        }

        /// Build a 1024-byte MRC header with the given dims, mode and a
        /// label in slot 0. The header is the entire extractable text
        /// for an MRC; the voxel slab that follows is just bytes.
        fn mrc_header(nx: i32, ny: i32, nz: i32, mode: i32, label: &[u8]) -> Vec<u8> {
            let mut h = vec![0u8; 1024];
            h[0..4].copy_from_slice(&nx.to_le_bytes());
            h[4..8].copy_from_slice(&ny.to_le_bytes());
            h[8..12].copy_from_slice(&nz.to_le_bytes());
            h[12..16].copy_from_slice(&mode.to_le_bytes());
            h[40..44].copy_from_slice(&1.0f32.to_le_bytes()); // cell_a
            h[44..48].copy_from_slice(&1.0f32.to_le_bytes()); // cell_b
            h[48..52].copy_from_slice(&1.0f32.to_le_bytes()); // cell_c
            h[52..56].copy_from_slice(&90.0f32.to_le_bytes()); // alpha
            h[56..60].copy_from_slice(&90.0f32.to_le_bytes()); // beta
            h[60..64].copy_from_slice(&90.0f32.to_le_bytes()); // gamma
            h[76..80].copy_from_slice(&0.0f32.to_le_bytes()); // dmin
            h[80..84].copy_from_slice(&1.0f32.to_le_bytes()); // dmax
            h[84..88].copy_from_slice(&0.5f32.to_le_bytes()); // dmean
            h[88..92].copy_from_slice(&1i32.to_le_bytes()); // ispg
            h[92..96].copy_from_slice(&1i32.to_le_bytes()); // nlabl
                                                            // label slot 0 starts at offset 104 and is 80 bytes
            let len = label.len().min(80);
            h[104..104 + len].copy_from_slice(&label[..len]);
            h
        }

        #[test]
        fn mrc_extracts_a_label() {
            let h = mrc_header(64, 64, 32, 1, b"nucleosome map EMDB-1234            ");
            let out = Mrc.extract(&h, "test.mrc").unwrap();
            let txt = String::from_utf8_lossy(out_text(&out));
            assert!(txt.contains("format: mrc"));
            assert!(txt.contains("dimensions: 64 64 32"));
            assert!(txt.contains("mode: 1 (int16)"));
            assert!(txt.contains("nucleosome"));
        }

        #[test]
        fn mrc_recognises_ccp4_via_map_magic() {
            let mut h = mrc_header(8, 8, 8, 2, b"");
            // CCP4 magic at offset 208.
            h[208..212].copy_from_slice(b"MAP ");
            let out = Mrc.extract(&h, "test.ccp4").unwrap();
            let txt = String::from_utf8_lossy(out_text(&out));
            assert!(txt.contains("format: ccp4"), "got {txt}");
            assert!(txt.contains("mode: 2 (float32)"));
        }

        #[test]
        fn mrc_rejects_a_truncated_header() {
            let too_short = vec![0u8; 512];
            assert!(Mrc.extract(&too_short, "x.mrc").is_err());
        }

        /// `out_text` pulls the bytes out of an `Outcome::Text`. The
        /// `Outcome` enum is non-exhaustive, so the `as` cast keeps the
        /// match exhaustive at the call site.
        fn out_text(o: &Outcome) -> &[u8] {
            match o {
                Outcome::Text(v) => v,
                _ => &[],
            }
        }
    }
}
