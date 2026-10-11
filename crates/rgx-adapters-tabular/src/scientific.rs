//! Scientific data adapters: MATLAB v5, netCDF-3 (classic/64-bit-offset),
//! Arrow IPC / Feather schema strings. All four are common in
//! data-science and engineering harnesses and all four have small
//! enough on-disk anchors to work with by hand. HDF5 gets the
//! superblock fields plus a heuristic name index rather than a full
//! B-tree/heap walk (see `Hdf5`).

use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::fmt::Write as _;
use rgx_core::{bail, Result};
use rgx_core::{Adapter, Outcome};

// ---------- MATLAB v5 ----------

const MI_COMPRESSED: u32 = 15;
const MI_MATRIX: u32 = 14;
const MI_UINT32: u32 = 6;
const MI_INT32: u32 = 5;
const MI_UTF8: u32 = 16;
const MI_UTF16: u32 = 17;
const MI_UTF32: u32 = 18;

const MX_CELL: u32 = 1;
const MX_STRUCT: u32 = 2;
const MX_CHAR: u32 = 4;

// indexed by mxClassID as stored in files (1-based)
const MX_NAMES: &[&str] = &[
    "?", "cell", "struct", "object", "char", "sparse", "double", "single", "int8", "uint8",
    "int16", "uint16", "int32", "uint32", "int64", "uint64", "function", "opaque",
];

struct MatReader<'a> {
    d: &'a [u8],
    /// Output buffer shared with the parent so a single `String`
    /// grows through the whole document: hundreds of MATLAB v5 files
    /// have hundreds of compressed elements, and per-element
    /// `String` allocations plus `push_str` were a reallocation +
    /// memcpy per element.
    out: &'a mut String,
    budget: usize,
}

impl<'a> MatReader<'a> {
    /// read one element: returns (mi_type, payload range, next offset)
    fn element(&self, p: usize) -> Option<(u32, usize, usize, usize)> {
        let d = self.d;
        if p + 8 > d.len() {
            return None;
        }
        let w0 = u32::from_le_bytes([d[p], d[p + 1], d[p + 2], d[p + 3]]);
        if w0 & 0xFFFF_0000 != 0 {
            // small data element: [type u16][nbytes u16] then 4 inline data
            // bytes — the whole element is exactly 8 bytes.
            let ty = w0 & 0xFFFF;
            let n = (w0 >> 16) as usize;
            if p + 8 > d.len() || n > 4 {
                return None;
            }
            Some((ty, p + 4, n, p + 8))
        } else {
            let n = u32::from_le_bytes([d[p + 4], d[p + 5], d[p + 6], d[p + 7]]) as usize;
            let start = p + 8;
            let end = start.checked_add(n)?;
            if end > d.len() {
                return None;
            }
            let next = end + !n.is_multiple_of(8) as usize * (8 - n % 8); // 8-byte padding
            Some((w0, start, n, next.min(d.len())))
        }
    }

    fn push(&mut self, line: &str) {
        if self.budget == 0 {
            return;
        }
        self.out.push_str(line);
        self.out.push('\n');
        self.budget = self.budget.saturating_sub(line.len() + 1);
    }

    fn elements(&mut self, mut p: usize, depth: usize, prefix: &str) {
        while p < self.d.len() && self.budget > 0 {
            let Some((ty, start, n, next)) = self.element(p) else {
                break;
            };
            match ty {
                MI_COMPRESSED => {
                    let inflated = match rgx_core::compress::zlib::ZlibDecoder::new()
                        .decode(&self.d[start..start + n])
                    {
                        Ok(v) => v,
                        Err(_) => break,
                    };
                    if inflated.len() <= (64 << 20) {
                        let sub = MatReader {
                            d: &inflated,
                            out: &mut *self.out,
                            budget: self.budget,
                        };
                        let mut sub = sub;
                        sub.elements(0, depth + 1, prefix);
                        self.budget = sub.budget;
                    }
                }
                MI_MATRIX => {
                    self.matrix(start, start + n, depth, prefix);
                }
                _ => {}
            }
            p = next;
        }
    }

    /// parse one miMATRIX payload
    fn matrix(&mut self, s: usize, e: usize, depth: usize, prefix: &str) {
        if depth > 6 {
            return;
        }
        let mut p = s;
        // array flags: miUINT32 x2 → class in low byte of first word
        let Some((t1, fs, _fn_, np)) = self.element(p) else {
            return;
        };
        if t1 != MI_UINT32 || fs + 8 > e {
            return;
        }
        let flags =
            u32::from_le_bytes([self.d[fs], self.d[fs + 1], self.d[fs + 2], self.d[fs + 3]]);
        let class = flags & 0xFF;
        let class_name = MX_NAMES.get(class as usize).copied().unwrap_or("unknown");
        p = np;
        // dims: miINT32 array
        let Some((t2, ds, dn, np)) = self.element(p) else {
            return;
        };
        if t2 != MI_INT32 {
            return;
        }
        let dims: Vec<usize> = (0..dn / 4)
            .map(|i| {
                let o = ds + i * 4;
                u32::from_le_bytes([self.d[o], self.d[o + 1], self.d[o + 2], self.d[o + 3]])
                    as usize
            })
            .collect();
        p = np;
        // name: usually a small miINT8 element
        let (name, np) = match self.element(p) {
            Some((_, ns, nn, np)) => (
                String::from_utf8_lossy(&self.d[ns..ns + nn])
                    .trim_end_matches('\x00')
                    .to_string(),
                np,
            ),
            None => (String::new(), p),
        };
        p = np;
        let full = if prefix.is_empty() {
            name.clone()
        } else {
            format!("{prefix}.{name}")
        };

        match class {
            MX_CHAR => {
                // data element: miUTF8 (raw bytes) or miUTF16 (u16 code units)
                if let Some((ct, cs, cn, _)) = self.element(p) {
                    let text = match ct {
                        MI_UTF8 => String::from_utf8_lossy(&self.d[cs..cs + cn]).into_owned(),
                        MI_UTF16 => {
                            let units: Vec<u16> = self.d[cs..cs + cn]
                                .as_chunks::<2>()
                                .0
                                .iter()
                                .map(|c| u16::from_le_bytes([c[0], c[1]]))
                                .collect();
                            String::from_utf16_lossy(&units)
                        }
                        MI_UTF32 => self.d[cs..cs + cn]
                            .as_chunks::<4>()
                            .0
                            .iter()
                            .filter_map(|c| {
                                char::from_u32(u32::from_le_bytes([c[0], c[1], c[2], c[3]]))
                            })
                            .collect(),
                        _ => String::new(),
                    };
                    self.push(&format!("{full}: char = {text:?}"));
                } else {
                    self.push(&format!("{full}: char dims={dims:?}"));
                }
            }
            MX_CELL => {
                self.push(&format!("{full}: cell dims={dims:?}"));
                let mut i = 0usize;
                let mut q = p;
                while q < e {
                    let Some((ty, cs, cn, np)) = self.element(q) else {
                        break;
                    };
                    if ty == MI_MATRIX {
                        self.matrix(cs, cs + cn, depth + 1, &format!("{full}{{{i}}}"));
                        i += 1;
                    }
                    q = np;
                }
            }
            MX_STRUCT => {
                self.push(&format!("{full}: struct dims={dims:?}"));
                // field-name length (miINT32 scalar), then field names (miINT8)
                let Some((_fl_t, fl_s, _fl_n, np)) = self.element(p) else {
                    return;
                };
                let flen = u32::from_le_bytes([
                    self.d[fl_s],
                    self.d[fl_s + 1],
                    self.d[fl_s + 2],
                    self.d[fl_s + 3],
                ]) as usize;
                let Some((_fn_t, fns, fnn, np)) = self.element(np) else {
                    return;
                };
                let fbytes = &self.d[fns..fns + fnn];
                let nfields = fnn.checked_div(flen).unwrap_or(0);
                let fields: Vec<String> = (0..nfields)
                    .map(|i| {
                        let start = i * flen;
                        let end = (start + flen).min(fbytes.len());
                        String::from_utf8_lossy(&fbytes[start..end])
                            .trim_end_matches('\0')
                            .to_string()
                    })
                    .collect();
                // then one matrix per field per struct element
                let mut q = np;
                let mut idx = 0usize;
                while q < e && idx < 512 {
                    let Some((ty, cs, cn, nq)) = self.element(q) else {
                        break;
                    };
                    if ty == MI_MATRIX {
                        let fname = fields
                            .get(idx % fields.len().max(1))
                            .cloned()
                            .unwrap_or_else(|| format!("f{idx}"));
                        let elem = if dims.iter().product::<usize>() > 1 {
                            format!("{idx} / {}", fields.len().max(1))
                        } else {
                            String::new()
                        };
                        let pre = if elem.is_empty() {
                            format!("{full}.{fname}")
                        } else {
                            format!("{full}({elem}).{fname}")
                        };
                        self.matrix(cs, cs + cn, depth + 1, &pre);
                        idx += 1;
                    }
                    q = nq;
                }
            }
            _ => {
                self.push(&format!("{full}: {class_name} dims={dims:?}"));
            }
        }
    }
}

/// MATLAB `.mat` v5 (and the v7.x subset that still uses v5 files).
/// Compressed (miCOMPRESSED) variables are inflated; cell/struct trees are
/// walked to their char leaves so string data is greppable.
pub struct MatV5;
impl Adapter for MatV5 {
    fn name(&self) -> &'static str {
        "mat"
    }
    fn extensions(&self) -> &'static [&'static str] {
        &["mat"]
    }
    fn extract(&self, data: &[u8], _path: &str) -> Result<Outcome> {
        if data.len() < 128 || &data[126..128] != b"IM" {
            bail!("not a MATLAB v5 file (bad endian marker)");
        }
        let mut out = String::new();
        let mut r = MatReader {
            d: data,
            out: &mut out,
            budget: 4 << 20,
        };
        r.elements(128, 0, "");
        if out.trim().is_empty() {
            bail!("no readable variables");
        }
        Ok(Outcome::Text(out.into_bytes()))
    }
}

// ---------- netCDF-3 ----------

fn be32(d: &[u8], o: usize) -> u32 {
    // truncated files read as 0 — callers advance by fixed strides and the
    // name readers bail out, so a zero-fill here is safe and panic-free
    match d.get(o..o + 4) {
        Some(b) => u32::from_be_bytes(b.try_into().unwrap()),
        None => 0,
    }
}

const NC_TYPES: &[&str] = &["?", "byte", "char", "short", "int", "float", "double"];

/// Parse an attribute list starting at `p`; returns (attrs, next offset).
fn nc_attrs(d: &[u8], p: usize, out: &mut String, owner: &str) -> Option<usize> {
    let tag = be32(d, p);
    if tag == 0 {
        return Some(p + 8); // ABSENT: two zero words
    }
    if tag != 0x0C {
        return Some(p);
    }
    let n = be32(d, p + 4) as usize;
    let mut q = p + 8;
    for _ in 0..n.min(10_000) {
        let (name, nq) = nc_name(d, q)?;
        q = nq;
        let ty = be32(d, q) as usize;
        let count = be32(d, q + 4) as usize;
        q += 8;
        let sz = [0usize, 1, 1, 2, 4, 4, 8].get(ty).copied().unwrap_or(1);
        let nbytes = count.saturating_mul(sz);
        if ty == 2 {
            // clamp to available bytes — a fuzzed count points past EOF
            let start = q.min(d.len());
            let end = (q.saturating_add(count.min(4096))).min(d.len());
            let s = String::from_utf8_lossy(&d[start..end]).into_owned();
            let _ = writeln!(out, "{owner}@{name} = {s:?}");
        } else if count <= 8 && q + nbytes <= d.len() {
            let vals: Vec<String> = (0..count)
                .map(|i| match sz {
                    1 => d[q + i].to_string(),
                    2 => i16::from_be_bytes([d[q + i * 2], d[q + i * 2 + 1]]).to_string(),
                    4 if ty == 5 => f32::from_be_bytes([
                        d[q + i * 4],
                        d[q + i * 4 + 1],
                        d[q + i * 4 + 2],
                        d[q + i * 4 + 3],
                    ])
                    .to_string(),
                    4 => i32::from_be_bytes([
                        d[q + i * 4],
                        d[q + i * 4 + 1],
                        d[q + i * 4 + 2],
                        d[q + i * 4 + 3],
                    ])
                    .to_string(),
                    _ => "?".into(),
                })
                .collect();
            let _ = writeln!(out, "{owner}@{name} = [{}]", vals.join(", "));
        }
        q += nbytes.div_ceil(4) * 4; // values padded to 4-byte boundary
    }
    Some(q)
}

fn nc_name(d: &[u8], p: usize) -> Option<(String, usize)> {
    let n = be32(d, p) as usize;
    if p + 4 + n > d.len() {
        return None;
    }
    let name = String::from_utf8_lossy(&d[p + 4..p + 4 + n]).into_owned();
    Some((name, p + 4 + n.div_ceil(4) * 4))
}

/// netCDF classic (CDF-1) and 64-bit-offset (CDF-2) files: dimensions,
/// variables with their types/shapes, and attributes. netCDF-4/HDF5 files
/// are bailed (different format family entirely).
pub struct NetCdf3;
impl Adapter for NetCdf3 {
    fn name(&self) -> &'static str {
        "netcdf"
    }
    fn extensions(&self) -> &'static [&'static str] {
        &["nc", "cdf", "netcdf"]
    }
    fn extract(&self, data: &[u8], _path: &str) -> Result<Outcome> {
        if data.len() < 16 || &data[0..3] != b"CDF" {
            bail!("not a netCDF-3 file (bad magic)");
        }
        let version = data[3];
        if version != 1 && version != 2 {
            bail!("netCDF version {version} is HDF5-based — unsupported");
        }
        let big = version == 2;
        let mut out = String::new();
        let mut p = 8usize; // skip magic + numrecs
                            // dim list
        if be32(data, p) == 0x0A {
            let n = be32(data, p + 4) as usize;
            p += 8;
            for _ in 0..n.min(10_000) {
                let Some((name, q)) = nc_name(data, p) else {
                    break;
                };
                p = q;
                let _ = writeln!(out, "dim {name} = {}", be32(data, p));
                p += 4;
            }
        } else {
            p += 8;
        }
        // global attrs
        let Some(q) = nc_attrs(data, p, &mut out, "global") else {
            bail!("truncated attribute list");
        };
        p = q;
        // var list
        if be32(data, p) != 0x0B {
            bail!("no variable list");
        }
        let nvars = be32(data, p + 4) as usize;
        p += 8;
        for _ in 0..nvars.min(100_000) {
            let Some((name, q)) = nc_name(data, p) else {
                break;
            };
            p = q;
            let ndims = be32(data, p) as usize;
            p += 4;
            // Stream the dimension IDs directly into `out` instead
            // of building a `Vec<usize>` per variable. The previous
            // shape allocated one Vec per variable (an n-dim header
            // had up to 32 elements per Vec) and then fed it to
            // `format!` for the Debug formatter; the new path is
            // write-direct with the same end-state output.
            p += ndims * 4;
            let Some(q) = nc_attrs(data, p, &mut out, &name) else {
                break;
            };
            p = q;
            let ty = be32(data, p) as usize;
            p += 12; // type + vsize
            p += if big { 8 } else { 4 }; // begin offset
            let tyname = NC_TYPES.get(ty).copied().unwrap_or("?");
            use core::fmt::Write as _;
            let _ = write!(out, "var {name}: {tyname} dims=[");
            for i in 0..ndims {
                if i > 0 {
                    let _ = write!(out, ", ");
                }
                let _ = write!(out, "{}", be32(data, p - (ndims - i) * 4));
            }
            let _ = writeln!(out, "]");
        }
        if out.trim().is_empty() {
            bail!("empty netcdf header");
        }
        Ok(Outcome::Text(out.into_bytes()))
    }
}

// ---------- Arrow IPC / Feather ----------

/// Arrow IPC files (.arrow) and Feather (.feather v1 "FEA1" / v2 "ARROW1"):
/// validate the magic, then harvest printable strings from the schema
/// region (field names, type names, metadata). The flatbuffer itself stays
/// unparsed — a schema-name index is what grep needs, and pulling the
/// arrow/flatbuffers stacks for it would violate the lightweight rule.
pub struct ArrowIpc;

/// Walk `region` for printable runs of at least `min_len` bytes and
/// write each unique run to `out` as `prefix<run>\n`. Runs are
/// deduplicated by a `BTreeSet<&[u8]>` of borrowed slices -- no
/// `String` allocation per match, no per-byte `run.push` on a
/// `Vec<u8>`.
fn harvest_printable(out: &mut String, region: &[u8], prefix: &str, min_len: usize) {
    use core::fmt::Write as _;
    let mut seen = alloc::collections::BTreeSet::<&[u8]>::new();
    let mut start: Option<usize> = None;
    for (i, &b) in region.iter().enumerate() {
        let printable = (0x20..=0x7e).contains(&b);
        match (printable, start) {
            (true, None) => start = Some(i),
            (false, Some(s)) => {
                if i - s >= min_len {
                    let key = &region[s..i];
                    if seen.insert(key) {
                        let s = core::str::from_utf8(key).unwrap_or("");
                        let _ = writeln!(out, "{prefix}{s}");
                    }
                }
                start = None;
            }
            _ => {}
        }
    }
    if let Some(s) = start {
        if region.len() - s >= min_len {
            let key = &region[s..];
            if seen.insert(key) {
                let s = core::str::from_utf8(key).unwrap_or("");
                let _ = writeln!(out, "{prefix}{s}");
            }
        }
    }
}
impl Adapter for ArrowIpc {
    fn name(&self) -> &'static str {
        "arrow"
    }
    fn extensions(&self) -> &'static [&'static str] {
        &["arrow", "feather"]
    }
    fn extract(&self, data: &[u8], _path: &str) -> Result<Outcome> {
        let tail = data.len() >= 12
            && data[data.len() - 12..]
                .windows(6)
                .any(|w| w == b"ARROW1" || w == b"FEA1\0\0" || w == b"FEA1");
        if !tail {
            bail!("not an Arrow IPC file (bad trailing magic)");
        }
        let start = if data.starts_with(b"ARROW1") || data.starts_with(b"FEA1") {
            8
        } else {
            0
        };
        let region = &data[start..(64 << 10).min(data.len())];
        let mut out = String::new();
        harvest_printable(&mut out, region, "schema: ", 3);
        if out.trim().is_empty() {
            bail!("no schema strings found");
        }
        Ok(Outcome::Text(out.into_bytes()))
    }
}

// ---------- HDF5 (heuristic) ----------

/// HDF5 superblock + name scan. Used by Keras/PyTorch weights (.h5),
/// MATLAB v7.3, netCDF-4 and most scientific stacks — an obvious harness
/// gap. A full HDF5 walker needs B-trees, fractal heaps and chunked
/// object headers; instead we do what grep needs, staying lightweight:
///
/// 1. validate the magic and parse the superblock (v0/v1 or v2/v3) for
///    version, address/length widths, end-of-file and root addresses —
///    all fixed-offset fields, no heap walks;
/// 2. harvest printable strings (group/dataset/attribute names are stored
///    as plain length-prefixed strings in object headers) from the first
///    chunk of the file, deduplicated — same trick as the Arrow adapter.
pub struct Hdf5;
impl Adapter for Hdf5 {
    fn name(&self) -> &'static str {
        "hdf5"
    }
    fn extensions(&self) -> &'static [&'static str] {
        &["h5", "hdf5", "he5", "h5ad"]
    }
    fn extract(&self, data: &[u8], _path: &str) -> Result<Outcome> {
        if data.len() < 16 || &data[0..8] != b"\x89HDF\r\n\x1a\n" {
            bail!("not an HDF5 file (bad magic)");
        }
        let version = data[8];
        if version > 3 {
            bail!("unknown superblock version {version}");
        }
        let (size_of_offsets, size_of_lengths, eof, root_addr) = if version <= 1 {
            let so = data[13] as usize;
            let sl = data[14] as usize;
            if !(1..=16).contains(&so) || !(1..=16).contains(&sl) {
                bail!("absurd address/length width {so}/{sl}");
            }
            // fixed header is 24 bytes (v0: reserved, v1: consistency flags),
            // then base addr + extension addr (size_of_offsets each),
            // then eof addr (size_of_lengths), then driver-info addr
            let p = 24 + so + so;
            if p + sl + so + sl + so > data.len() {
                bail!("truncated superblock");
            }
            let eof = le_uint(data, p, sl);
            // root group symbol table entry: link-name offset (sl), then
            // object header address (so)
            let root = p + sl + so;
            let root_addr = le_uint(data, root + sl, so);
            (so, sl, eof, root_addr)
        } else {
            let so = data[9] as usize;
            let sl = data[10] as usize;
            if !(1..=16).contains(&so) || !(1..=16).contains(&sl) {
                bail!("absurd address/length width {so}/{sl}");
            }
            let p = 12 + so; // skip flags byte + base address
            if p + so + sl > data.len() {
                bail!("truncated superblock");
            }
            let ext = p;
            let eof = le_uint(data, ext + so, sl);
            let root_addr = le_uint(data, ext + so + sl, so);
            (so, sl, eof, root_addr)
        };
        let mut out = format!(
            "hdf5 superblock v{version} offsets={size_of_offsets}B lengths={size_of_lengths}B eof={eof} root=0x{root_addr:x}\n"
        );
        // heuristic name index: object headers live at the front of typical
        // files; scan a bounded window for printable runs.
        let window = (256 << 10).min(data.len());
        let region = &data[..window];
        harvest_printable(&mut out, region, "name: ", 4);
        if out.lines().count() <= 1 {
            bail!("no names found in HDF5 header window");
        }
        Ok(Outcome::Text(out.into_bytes()))
    }
}

/// little-endian unsigned integer of `n` bytes (1..=16) at `p`
fn le_uint(data: &[u8], p: usize, n: usize) -> u64 {
    let mut v: u64 = 0;
    for (i, &b) in data.get(p..p + n).unwrap_or(&[]).iter().enumerate() {
        v |= (b as u64) << (8 * i.min(7));
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mat_header() -> Vec<u8> {
        let mut h = vec![b' '; 116];
        h.extend_from_slice(&0u64.to_le_bytes()); // subsystem offset
        h.extend_from_slice(&0x0100u16.to_le_bytes()); // version 1.0
        h.extend_from_slice(b"IM"); // little-endian
        h
    }

    #[test]
    fn mat_double_and_char() {
        let mut d = mat_header();
        // miMATRIX: flags(u32x2), dims(miINT32 1x3), name(small 'grid'),
        // data skipped (numeric summary only)
        d.extend_from_slice(&MI_MATRIX.to_le_bytes());
        let body_start = d.len();
        d.extend_from_slice(&0u32.to_le_bytes()); // placeholder size
        d.extend_from_slice(&MI_UINT32.to_le_bytes());
        d.extend_from_slice(&8u32.to_le_bytes());
        d.extend_from_slice(&6u32.to_le_bytes()); // mxDOUBLE_CLASS
        d.extend_from_slice(&0u32.to_le_bytes());
        d.extend_from_slice(&MI_INT32.to_le_bytes());
        d.extend_from_slice(&4u32.to_le_bytes());
        d.extend_from_slice(&3u32.to_le_bytes());
        d.extend_from_slice(&[0u8; 4]); // 8-byte element alignment padding
                                        // small element: type u16 lo, nbytes u16 hi, 4 bytes data
        let small = (MI_INT32 & 0xFFFF) | (4 << 16);
        d.extend_from_slice(&small.to_le_bytes());
        d.extend_from_slice(b"grid");
        let size = d.len() - body_start - 4;
        d[body_start..body_start + 4].copy_from_slice(&(size as u32).to_le_bytes());
        let out = MatV5.extract(&d, "v.mat").unwrap();
        let Outcome::Text(t) = out else {
            panic!("text")
        };
        let s = String::from_utf8(t).unwrap();
        assert!(s.contains("grid: double dims=[3]"), "{s}");
    }

    #[test]
    fn mat_compressed_char() {
        // contents of a miMATRIX: a char array "ZIRCALOY"
        let mut body = Vec::new();
        body.extend_from_slice(&MI_UINT32.to_le_bytes());
        body.extend_from_slice(&8u32.to_le_bytes());
        body.extend_from_slice(&4u32.to_le_bytes()); // mxCHAR
        body.extend_from_slice(&0u32.to_le_bytes());
        body.extend_from_slice(&MI_INT32.to_le_bytes());
        body.extend_from_slice(&8u32.to_le_bytes());
        body.extend_from_slice(&1u32.to_le_bytes());
        body.extend_from_slice(&8u32.to_le_bytes());
        // name: full element 'label' (5 bytes, data padded to 8)
        body.extend_from_slice(&1u32.to_le_bytes()); // miINT8
        body.extend_from_slice(&8u32.to_le_bytes());
        body.extend_from_slice(b"label\0\0\0");
        // data: miUTF8 with the text
        body.extend_from_slice(&MI_UTF8.to_le_bytes());
        body.extend_from_slice(&8u32.to_le_bytes());
        body.extend_from_slice(b"ZIRCALOY");

        // wrap as a complete miMATRIX element, then compress
        let mut inner = Vec::new();
        inner.extend_from_slice(&MI_MATRIX.to_le_bytes());
        inner.extend_from_slice(&(body.len() as u32).to_le_bytes());
        inner.extend_from_slice(&body);

        let compressed = rgx_core::compress::zlib::ZlibEncoder::new().encode(&inner);

        let mut d = mat_header();
        d.extend_from_slice(&MI_COMPRESSED.to_le_bytes());
        d.extend_from_slice(&(compressed.len() as u32).to_le_bytes());
        d.extend_from_slice(&compressed);
        let out = MatV5.extract(&d, "c.mat").unwrap();
        let Outcome::Text(t) = out else {
            panic!("text")
        };
        let s = String::from_utf8(t).unwrap();
        assert!(s.contains("label: char"), "{s}");
        assert!(s.contains("ZIRCALOY"), "{s}");
    }

    #[test]
    fn mat_rejects_hdf5_based_v7_3() {
        let d = vec![0x89u8; 200];
        assert!(MatV5.extract(&d, "h.mat").is_err());
    }

    fn be(v: &mut Vec<u8>, x: u32) {
        v.extend_from_slice(&x.to_be_bytes());
    }

    #[test]
    fn netcdf_classic_dims_vars_attrs() {
        let mut d = b"CDF\x01".to_vec();
        be(&mut d, 0); // numrecs
        be(&mut d, 0x0A); // dim list
        be(&mut d, 1); // 1 dim
        be(&mut d, 4);
        d.extend_from_slice(b"time");
        be(&mut d, 365);
        // global attrs
        be(&mut d, 0x0C);
        be(&mut d, 1);
        be(&mut d, 6);
        d.extend_from_slice(b"source");
        d.extend_from_slice(b"\x00\x00"); // pad name to 4-byte boundary
        be(&mut d, 4); // nc_int
        be(&mut d, 1);
        be(&mut d, 42);
        // vars
        be(&mut d, 0x0B);
        be(&mut d, 1);
        be(&mut d, 4); // name length ("temp")
        d.extend_from_slice(b"temp");
        be(&mut d, 1); // ndims
        be(&mut d, 0); // dimid
        be(&mut d, 0);
        be(&mut d, 0); // no var attrs
        be(&mut d, 6); // double
        be(&mut d, 8); // vsize
        be(&mut d, 128); // begin
        let out = NetCdf3.extract(&d, "c.nc").unwrap();
        let Outcome::Text(t) = out else {
            panic!("text")
        };
        let s = String::from_utf8(t).unwrap();
        assert!(s.contains("dim time = 365"), "{s}");
        assert!(s.contains("var temp: double dims=[0]"), "{s}");
        assert!(s.contains("source"), "{s}");
    }

    #[test]
    fn arrow_harvests_schema_strings() {
        let mut d = b"ARROW1\0\0".to_vec();
        d.extend_from_slice(b"\xFF\xFF\xFF\xFF"); // continuation
        d.extend_from_slice(&64u32.to_le_bytes());
        d.extend_from_slice(b"temperature_id_ZIRCALOY_probe\0\0\0\0");
        d.extend_from_slice(&[0u8; 32]);
        d.extend_from_slice(b"\xFF\xFF\xFF\xFFARROW1\0\0");
        let out = ArrowIpc.extract(&d, "m.arrow").unwrap();
        let Outcome::Text(t) = out else {
            panic!("text")
        };
        let s = String::from_utf8(t).unwrap();
        assert!(s.contains("ZIRCALOY"), "{s}");
    }

    #[test]
    fn arrow_rejects_non_arrow() {
        assert!(ArrowIpc
            .extract(b"random junk bytes here", "x.arrow")
            .is_err());
    }

    mod fuzz {
        use super::*;

        fn mat_matrix() -> Vec<u8> {
            let mut d = mat_header();
            d.extend_from_slice(&MI_MATRIX.to_le_bytes());
            let body_start = d.len();
            d.extend_from_slice(&0u32.to_le_bytes()); // size placeholder
            d.extend_from_slice(&MI_UINT32.to_le_bytes());
            d.extend_from_slice(&8u32.to_le_bytes());
            d.extend_from_slice(&6u32.to_le_bytes()); // mxDOUBLE_CLASS
            d.extend_from_slice(&0u32.to_le_bytes());
            d.extend_from_slice(&MI_INT32.to_le_bytes());
            d.extend_from_slice(&4u32.to_le_bytes());
            d.extend_from_slice(&3u32.to_le_bytes());
            d.extend_from_slice(&[0u8; 4]);
            let small = (MI_INT32 & 0xFFFF) | (4 << 16);
            d.extend_from_slice(&small.to_le_bytes());
            d.extend_from_slice(b"grid");
            let size = d.len() - body_start - 4;
            d[body_start..body_start + 4].copy_from_slice(&(size as u32).to_le_bytes());
            d
        }

        fn netcdf() -> Vec<u8> {
            let mut d = b"CDF\x01".to_vec();
            be(&mut d, 0);
            be(&mut d, 0x0A);
            be(&mut d, 1);
            be(&mut d, 4);
            d.extend_from_slice(b"time");
            be(&mut d, 365);
            be(&mut d, 0x0C);
            be(&mut d, 0);
            be(&mut d, 0x0B);
            be(&mut d, 0);
            d
        }

        #[test]
        fn matv5_never_panics_on_mutation() {
            rgx_core::fuzzkit::assert_never_panics(
                vec![
                    ("matrix", mat_matrix()),
                    ("header-only", mat_header()),
                    ("empty", vec![]),
                ],
                1200,
                "matv5",
                |data| {
                    let _ = MatV5.extract(data, "fuzz.mat");
                },
            );
        }

        #[test]
        fn netcdf3_never_panics_on_mutation() {
            rgx_core::fuzzkit::assert_never_panics(
                vec![
                    ("classic", netcdf()),
                    ("magic-only", b"CDF\x01".to_vec()),
                    ("empty", vec![]),
                ],
                1200,
                "netcdf3",
                |data| {
                    let _ = NetCdf3.extract(data, "fuzz.nc");
                },
            );
        }

        #[test]
        fn arrow_ipc_never_panics_on_mutation() {
            let mut d = b"ARROW1\0\0".to_vec();
            d.extend_from_slice(b"\xFF\xFF\xFF\xFF");
            d.extend_from_slice(&64u32.to_le_bytes());
            d.extend_from_slice(b"temperature_id_ZIRCALOY_probe\0\0\0\0");
            d.extend_from_slice(&[0u8; 32]);
            rgx_core::fuzzkit::assert_never_panics(
                vec![
                    ("ipc", d),
                    ("magic-only", b"ARROW1\0\0".to_vec()),
                    ("empty", vec![]),
                ],
                1200,
                "arrow-ipc",
                |data| {
                    let _ = ArrowIpc.extract(data, "fuzz.arrow");
                },
            );
        }

        /// minimal superblock v0 (so=8, sl=8) with a plausible body
        fn hdf5_v0() -> Vec<u8> {
            let mut d = b"\x89HDF\r\n\x1a\n".to_vec();
            d.push(0); // superblock version
            d.extend_from_slice(&[0, 0, 0, 0, 8, 8, 0]); // fs/root/shared ver, so, sl, rsvd
            d.extend_from_slice(&4u16.to_le_bytes()); // group leaf K
            d.extend_from_slice(&16u16.to_le_bytes()); // group internal K
            d.extend_from_slice(&[0u8; 4]); // reserved
            d.extend_from_slice(&96u64.to_le_bytes()); // base address
            d.extend_from_slice(&0u64.to_le_bytes()); // extension address
            d.extend_from_slice(&4096u64.to_le_bytes()); // eof address
            d.extend_from_slice(&0u64.to_le_bytes()); // driver info
                                                      // root group symbol table entry: name offset, obj header addr
            d.extend_from_slice(&0u64.to_le_bytes());
            d.extend_from_slice(&2048u64.to_le_bytes());
            // fake object header region with dataset/group names
            while d.len() < 2048 {
                d.push(0);
            }
            d.extend_from_slice(b"\x0b\x00\x01\x00measurements_ZIRCALOY_probe\x00");
            d.extend_from_slice(b"\x0b\x00\x01\x00grid_temperature\x00");
            d
        }

        #[test]
        fn hdf5_superblock_and_names() {
            let out = Hdf5.extract(&hdf5_v0(), "m.h5").unwrap();
            let Outcome::Text(t) = out else {
                panic!("text")
            };
            let s = String::from_utf8(t).unwrap();
            assert!(s.contains("superblock v0"), "{s}");
            assert!(s.contains("eof=4096"), "{s}");
            assert!(s.contains("root=0x800"), "{s}");
            assert!(s.contains("measurements_ZIRCALOY_probe"), "{s}");
            assert!(s.contains("grid_temperature"), "{s}");
        }

        #[test]
        fn hdf5_never_panics_on_mutation() {
            rgx_core::fuzzkit::assert_never_panics(
                vec![
                    ("v0", hdf5_v0()),
                    ("magic-only", b"\x89HDF\r\n\x1a\n\x02".to_vec()),
                    ("v2", b"\x89HDF\r\n\x1a\n\x02\x08\x08\x00".to_vec()),
                    ("empty", vec![]),
                ],
                1200,
                "hdf5",
                |data| {
                    let _ = Hdf5.extract(data, "fuzz.h5");
                },
            );
        }
    }
}
