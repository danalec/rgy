//! Machine-learning model file adapters.
//!
//! These never extract weights (multi-GB payloads of floats are useless for
//! text search) — they extract *metadata*: tensors, shapes, hyperparameters,
//! architecture strings. That is exactly what an LLM agent wants when grepping
//! a model zoo.

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
use rgx_core::{bail, Context, Result};
use rgx_core::{Adapter, Outcome};

fn rd_u16(b: &[u8], o: usize) -> u16 {
    u16::from_le_bytes([b[o], b[o + 1]])
}
fn rd_u32(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}
fn rd_u64(b: &[u8], o: usize) -> u64 {
    u64::from_le_bytes(b[o..o + 8].try_into().unwrap())
}
fn rd_f32(b: &[u8], o: usize) -> f32 {
    f32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}
fn rd_f64(b: &[u8], o: usize) -> f64 {
    f64::from_le_bytes(b[o..o + 8].try_into().unwrap())
}

mod extra;

pub fn adapters() -> Vec<Box<dyn Adapter>> {
    vec![
        Box::new(Gguf),
        Box::new(Safetensors),
        Box::new(Npy),
        Box::new(Protobuf),
        Box::new(Tiktoken),
        Box::new(extra::KerasZip),
        Box::new(extra::Torch),
        Box::new(extra::PickleStrings),
    ]
}

/// GGUF (llama.cpp model containers): header + key/value metadata.
///
/// Layout: magic "GGUF", u32 version, u64 n_tensors, u64 n_kv, then n_kv
/// entries of: string key, u32 value-type, value. Strings are u64-len
/// prefixed; array values are u32-type + u64-len + elements.
pub struct Gguf;
impl Adapter for Gguf {
    fn name(&self) -> &'static str {
        "gguf"
    }
    fn extensions(&self) -> &'static [&'static str] {
        &["gguf"]
    }
    fn extract(&self, data: &[u8], _path: &str) -> Result<Outcome> {
        if data.len() < 24 || &data[0..4] != b"GGUF" {
            bail!("not a GGUF file (bad magic)");
        }
        let version = rd_u32(data, 4);
        if version > 3 {
            bail!("unsupported GGUF version {version}");
        }
        let n_kv = rd_u64(data, 16) as usize;
        let mut p = 24usize;
        let mut out = format!("gguf version: {version}\n");
        for _ in 0..n_kv.min(4096) {
            if p + 8 > data.len() {
                break;
            }
            let klen = rd_u64(data, p) as usize;
            p += 8;
            // checked_add: klen comes from the file and may be usize::MAX —
            // `p + klen + 4` would overflow before the bound check ran
            let Some(kend) = p.checked_add(klen) else {
                break;
            };
            if kend + 4 > data.len() {
                break;
            }
            // Borrow the key as `&str`; the previous shape allocated
            // an owned String for every header key. The `writeln!`
            // below is happy to take a borrowed slice, and the
            // vtype/match arms below push the value directly into
            // `out` so the only String allocation in this loop
            // is gone.
            let key = core::str::from_utf8(&data[p..kend]).unwrap_or("");
            p = kend;
            let vtype = rd_u32(data, p);
            p += 4;
            // fixed-size values: ensure the payload fits before reading —
            // the +4 guard above only covered the type tag itself
            let vsz = match vtype {
                0 | 1 | 7 => 1,
                2 | 3 => 2,
                4..=6 => 4,
                8..=10 => 8,
                _ => 0,
            };
            if vsz > 0 && p + vsz > data.len() {
                break;
            }
            use core::fmt::Write as _;
            let _ = write!(out, "{key}: ");
            match vtype {
                0 | 1 => {
                    let v = data[p];
                    p += 1;
                    let _ = write!(out, "{v}");
                }
                2 => {
                    let v = rd_u16(data, p);
                    p += 2;
                    let _ = write!(out, "{v}");
                }
                3 => {
                    let v = rd_u16(data, p) as i16;
                    p += 2;
                    let _ = write!(out, "{v}");
                }
                4 => {
                    let v = rd_u32(data, p);
                    p += 4;
                    let _ = write!(out, "{v}");
                }
                5 => {
                    let v = rd_u32(data, p) as i32;
                    p += 4;
                    let _ = write!(out, "{v}");
                }
                6 => {
                    let v = rd_f32(data, p);
                    p += 4;
                    let _ = write!(out, "{v}");
                }
                7 => {
                    bail!("bool GGUF value unsupported")
                }
                8 => {
                    let v = rd_u64(data, p);
                    p += 8;
                    let _ = write!(out, "{v}");
                }
                9 => {
                    let v = rd_u64(data, p) as i64;
                    p += 8;
                    let _ = write!(out, "{v}");
                }
                10 => {
                    let v = rd_f64(data, p);
                    p += 8;
                    let _ = write!(out, "{v}");
                }
                11 => {
                    // string
                    if p + 8 > data.len() {
                        break;
                    }
                    let vlen = rd_u64(data, p) as usize;
                    p += 8;
                    if vlen > data.len().saturating_sub(p) {
                        break;
                    }
                    // Borrow the string slice; do not allocate.
                    let s = core::str::from_utf8(&data[p..p + vlen]).unwrap_or("");
                    let _ = write!(out, "{s}");
                    p += vlen;
                }
                12..=15 => {
                    // array of u8/i8/u16/i16/u32/i32/f32/u64/i64/f64/bool/str
                    if p + 4 + 8 > data.len() {
                        break;
                    }
                    let etype = rd_u32(data, p);
                    p += 4;
                    let elen = rd_u64(data, p) as usize;
                    p += 8;
                    let sz = match etype {
                        0 | 1 => 1,
                        2 | 3 => 2,
                        4 | 5 | 6 | 10 => 4,
                        7 => 1,
                        8 | 9 | 11 => 8,
                        _ => break,
                    };
                    p = p.saturating_add(elen.saturating_mul(sz));
                    let _ = writeln!(out, "<array len={elen}>");
                    // The value was already terminated by the array
                    // length line; skip the per-key writeln below.
                    continue;
                }
                _ => break,
            };
            // Terminate the key:value line.
            let _ = writeln!(out);
        }
        // tensor names: u64 n * (string name, u32 n_dims, u64 dims..., u32 type, u64 offset)
        // already reported counts in header; tensor names are nice-to-have, skip safely
        Ok(Outcome::Text(out.into_bytes()))
    }
}

/// Hugging Face safetensors: 8-byte LE header length + JSON header.
pub struct Safetensors;
impl Adapter for Safetensors {
    fn name(&self) -> &'static str {
        "safetensors"
    }
    fn extensions(&self) -> &'static [&'static str] {
        &["safetensors", "sft", "st"]
    }
    fn extract(&self, data: &[u8], _path: &str) -> Result<Outcome> {
        if data.len() < 8 {
            bail!("too small for safetensors");
        }
        let hlen = rd_u64(data, 0) as usize;
        // saturating_sub: an absurd hlen must fail the bound, not the
        // addition (8 + hlen overflowed with hlen = usize::MAX)
        if hlen == 0 || hlen > (1 << 26) || hlen > data.len().saturating_sub(8) {
            bail!("bad safetensors header length");
        }
        let header_arena = rgx_core::djson::parse(
            str::from_utf8(&data[8..8 + hlen]).context("safetensors header is not UTF-8")?,
        )
        .map_err(rgx_core::Error::msg)
        .context("safetensors header is not JSON")?;
        let header = header_arena.root();
        let mut out = String::new();
        if let Some(meta_v) = header.get(&header_arena, "__metadata__") {
            if let Some(meta) = meta_v.as_object(&header_arena) {
                for (k_idx, v) in meta.iter() {
                    let k = header_arena.get_string(*k_idx);
                    let val_text = v.as_str(&header_arena).unwrap_or("?");
                    use core::fmt::Write as _;
                    let _ = writeln!(out, "{k}: {val_text}");
                }
            }
        }
        if let Some(obj) = header.as_object(&header_arena) {
            for (k_idx, t) in obj.iter() {
                let name = header_arena.get_string(*k_idx);
                if name == "__metadata__" {
                    continue;
                }
                let dtype = t
                    .get(&header_arena, "dtype")
                    .and_then(|d| d.as_str(&header_arena))
                    .unwrap_or("?");
                let shape = t
                    .get(&header_arena, "shape")
                    .and_then(|s| s.as_array(&header_arena));
                use core::fmt::Write as _;
                let _ = write!(out, "tensor {name}: dtype={dtype} shape=[");
                if let Some(a) = shape {
                    for (i, v) in a.iter().enumerate() {
                        if i > 0 {
                            let _ = write!(out, "x");
                        }
                        match v.as_u64() {
                            Some(u) => {
                                let _ = write!(out, "{u}");
                            }
                            None => {
                                let _ = write!(out, "?");
                            }
                        }
                    }
                }
                let _ = writeln!(out, "]");
            }
        }
        Ok(Outcome::Text(out.into_bytes()))
    }
}

/// NumPy .npy v1/v2: magic \x93NUMPY, version, header-length, header dict.
pub struct Npy;
impl Adapter for Npy {
    fn name(&self) -> &'static str {
        "npy"
    }
    fn extensions(&self) -> &'static [&'static str] {
        &["npy"]
    }
    fn extract(&self, data: &[u8], _path: &str) -> Result<Outcome> {
        if data.len() < 10 || &data[0..6] != b"\x93NUMPY" {
            bail!("not an npy file (bad magic)");
        }
        let major = data[6];
        let (hlen, hoff) = match major {
            1 => (rd_u16(data, 8) as usize, 10usize),
            2 | 3 => (rd_u32(data, 8) as usize, 12usize),
            _ => bail!("unsupported npy version {major}"),
        };
        if hoff + hlen > data.len() {
            bail!("npy header overruns file");
        }
        // The header is a Python literal dict; parse the three keys we need
        // without evaluating anything.
        let header = String::from_utf8_lossy(&data[hoff..hoff + hlen]);
        let get = |key: &str| -> String {
            let pat = format!("'{key}':").to_lowercase();
            let h = header.to_lowercase();
            let Some(mut i) = h.find(&pat) else {
                return "?".into();
            };
            i += pat.len();
            let rest = header[i..].trim_start();
            // scan the value: tuples like (2, 3) run to their ')', scalars
            // and strings stop at ',' or '}'
            let bytes = rest.as_bytes();
            let mut depth = 0usize;
            let mut end = bytes.len();
            for (j, &c) in bytes.iter().enumerate() {
                match c {
                    b'(' | b'[' => depth += 1,
                    b')' | b']' => {
                        if depth == 0 {
                            end = j + 1;
                            break;
                        }
                        depth -= 1;
                        if depth == 0 {
                            end = j + 1;
                            break;
                        }
                    }
                    b',' | b'}' if depth == 0 => {
                        end = j;
                        break;
                    }
                    _ => {}
                }
            }
            rest[..end].trim().trim_matches('\'').to_string()
        };
        let descr = get("descr");
        let fortran = get("fortran_order");
        let shape = get("shape").replace(['(', ')'], "");
        let itemsize: usize = descr
            .chars()
            .skip(1)
            .collect::<String>()
            .parse()
            .unwrap_or(0);
        let count: usize = shape
            .split(',')
            .filter_map(|s| s.trim().parse::<usize>().ok())
            .product();
        let payload = data.len() - (hoff + hlen);
        let text = format!(
            "npy: descr={descr} fortran_order={fortran} shape=({shape}) elements={count} payload_bytes={payload} itemsize={itemsize}\n"
        );
        Ok(Outcome::Text(text.into_bytes()))
    }
}

/// Protobuf wire format: walk fields, print printable length-delimited
/// payloads (architecture strings, vocab entries, names). Covers
/// .onnx/.pb/.model without pulling a full protobuf stack.
pub struct Protobuf;
impl Adapter for Protobuf {
    fn name(&self) -> &'static str {
        "protobuf"
    }
    fn extensions(&self) -> &'static [&'static str] {
        &["pb", "onnx", "model", "tflite", "pt2", "mlmodel"]
    }
    fn extract(&self, data: &[u8], _path: &str) -> Result<Outcome> {
        // cheap plausibility check: a protobuf usually starts with a
        // field tag (varint key != 0) and has no NULs in the first 16 bytes
        if data.len() < 8 || data[0] == 0 {
            bail!("not plausibly protobuf");
        }
        if data[..16.min(data.len())].contains(&0) {
            bail!("NUL in protobuf header region");
        }
        let mut out = String::new();
        let mut p = 0usize;
        let mut depth = 0usize;
        while p < data.len() && out.len() < 1 << 20 {
            // read varint key
            let mut key: u64 = 0;
            let mut shift = 0;
            let key_start = p;
            while p < data.len() && shift < 64 {
                let b = data[p];
                p += 1;
                key |= ((b & 0x7f) as u64) << shift;
                if b & 0x80 == 0 {
                    break;
                }
                shift += 7;
            }
            if p == key_start || p - key_start > 10 {
                break;
            }
            let field = key >> 3;
            let wire = key & 7;
            match wire {
                0 => {
                    // varint
                    let mut v: u64 = 0;
                    let mut shift = 0;
                    while p < data.len() && shift < 64 {
                        let b = data[p];
                        p += 1;
                        v |= ((b & 0x7f) as u64) << shift;
                        if b & 0x80 == 0 {
                            break;
                        }
                        shift += 7;
                    }
                    if field <= 100_000 && out.len() < 1 << 18 {
                        let _ = writeln!(out, "field {field}: {v}");
                    }
                }
                1 => p = p.saturating_add(8), // 64-bit
                2 => {
                    // length-delimited
                    if p >= data.len() {
                        break;
                    }
                    let mut len: u64 = 0;
                    let mut shift = 0;
                    while p < data.len() && shift < 64 {
                        let b = data[p];
                        p += 1;
                        len |= ((b & 0x7f) as u64) << shift;
                        if b & 0x80 == 0 {
                            break;
                        }
                        shift += 7;
                    }
                    if len > (1 << 24) {
                        break;
                    }
                    let len = len as usize;
                    if p + len > data.len() {
                        break;
                    }
                    let payload = &data[p..p + len];
                    // printable text? emit it; else recurse one level
                    let printable = !payload.is_empty()
                        && payload.len() >= 3
                        && payload
                            .iter()
                            .filter(|&&b| (0x20..0x7f).contains(&b) || b == b'\n')
                            .count()
                            * 10
                            >= payload.len() * 9;
                    if printable {
                        let s = String::from_utf8_lossy(payload);
                        for line in s.lines().take(50) {
                            if !line.trim().is_empty() {
                                out.push_str(line);
                                out.push('\n');
                            }
                        }
                    } else if depth < 3 && len >= 2 {
                        depth += 1;
                        // nested message: walk it inline
                        let mut q = 0usize;
                        let mut local = String::new();
                        while q + 2 < payload.len() && local.len() < 1 << 16 {
                            let nested = &payload[q..];
                            if nested[0] == 0 {
                                break;
                            }
                            let mut nk: u64 = 0;
                            let mut sh = 0;
                            let ns = q;
                            while q < payload.len() && sh < 64 {
                                let b = payload[q];
                                q += 1;
                                nk |= ((b & 0x7f) as u64) << sh;
                                if b & 0x80 == 0 {
                                    break;
                                }
                                sh += 7;
                            }
                            if q == ns || q - ns > 10 {
                                break;
                            }
                            let w = nk & 7;
                            if w == 2 {
                                if q >= payload.len() {
                                    break;
                                }
                                let mut nl: u64 = 0;
                                let mut sh = 0;
                                while q < payload.len() && sh < 64 {
                                    let b = payload[q];
                                    q += 1;
                                    nl |= ((b & 0x7f) as u64) << sh;
                                    if b & 0x80 == 0 {
                                        break;
                                    }
                                    sh += 7;
                                }
                                if nl > (1 << 20) || q + nl as usize > payload.len() {
                                    break;
                                }
                                let sub = &payload[q..q + nl as usize];
                                if !sub.is_empty()
                                    && sub.iter().filter(|&&b| (0x20..=0x7e).contains(&b)).count()
                                        * 10
                                        >= sub.len() * 9
                                {
                                    let s = String::from_utf8_lossy(sub);
                                    for line in s.lines().take(20) {
                                        if !line.trim().is_empty() {
                                            local.push_str(line);
                                            local.push('\n');
                                        }
                                    }
                                }
                                q += nl as usize;
                            } else if w == 0 {
                                while q < payload.len() && payload[q] & 0x80 != 0 {
                                    q += 1;
                                }
                                q = (q + 1).min(payload.len());
                            } else {
                                break;
                            }
                        }
                        out.push_str(&local);
                        depth -= 1;
                    }
                    p += len;
                }
                5 => p = p.saturating_add(4), // 32-bit
                _ => break,                   // groups (3/4) deprecated — stop
            }
            // give up early on files that don't look like protobuf at all
            if out.is_empty() && p > 4096 {
                break;
            }
        }
        if out.trim().is_empty() {
            bail!("no printable protobuf content found");
        }
        Ok(Outcome::Text(out.into_bytes()))
    }
}

/// OpenAI tiktoken `.tiktoken` vocabularies: lines of `base64 rank`.
/// Decoding each token makes the vocabulary greppable as plain text.
pub struct Tiktoken;
impl Adapter for Tiktoken {
    fn name(&self) -> &'static str {
        "tiktoken"
    }
    fn extensions(&self) -> &'static [&'static str] {
        &["tiktoken"]
    }
    fn extract(&self, data: &[u8], _path: &str) -> Result<Outcome> {
        // fast shape check: first line must be "<base64> <digits>"
        let first_nl = data.iter().position(|&b| b == b'\n').unwrap_or(data.len());
        let first = String::from_utf8_lossy(&data[..first_nl]);
        let mut parts = first.split_whitespace();
        let (Some(tok), Some(rank), None) = (parts.next(), parts.next(), parts.next()) else {
            bail!("not a tiktoken file (expected '<base64> <rank>')");
        };
        if rank.parse::<u64>().is_err() || rgx_core::base64::base64_decode_strict(tok).is_err() {
            bail!("not a tiktoken file (bad line shape)");
        }
        let mut out = String::new();
        for line in String::from_utf8_lossy(data).lines().take(1_000_000) {
            let mut it = line.split_whitespace();
            let (Some(tok), Some(rank)) = (it.next(), it.next()) else {
                continue;
            };
            if let Ok(bytes) = rgx_core::base64::base64_decode_strict(tok) {
                let text = String::from_utf8_lossy(&bytes);
                let text = text.escape_default().to_string();
                let _ = writeln!(out, "token {rank}: {text}");
            }
        }
        Ok(Outcome::Text(out.into_bytes()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// libFuzzer-found: hlen = u64::MAX as usize made `8 + hlen` overflow
    /// in the bound check itself. Must bail, never panic.
    #[test]
    fn safetensors_absurd_header_length() {
        let mut data = u64::MAX.to_le_bytes().to_vec();
        data.extend_from_slice(&[0xff; 27]);
        assert!(Safetensors.extract(&data, "m.safetensors").is_err());
    }

    /// libFuzzer-found: klen/vlen of usize::MAX in the GGUF kv walk
    /// overflowed `p + len + 4`, and u64 values could dangle past EOF.
    #[test]
    fn gguf_absurd_lengths_and_tail_values() {
        let mut data = b"GGUF".to_vec();
        data.extend_from_slice(&3u32.to_le_bytes()); // version
        data.extend_from_slice(&[0u8; 8]); // tensor count
        data.extend_from_slice(&1u64.to_le_bytes()); // n_kv
        data.extend_from_slice(&u64::MAX.to_le_bytes()); // klen
        data.extend_from_slice(&[0u8; 64]);
        assert!(Gguf.extract(&data, "m.gguf").is_ok());
        // u64 value as the very last 4+8 bytes: tag fits, value doesn't
        let mut data = b"GGUF".to_vec();
        data.extend_from_slice(&3u32.to_le_bytes());
        data.extend_from_slice(&[0u8; 8]);
        data.extend_from_slice(&1u64.to_le_bytes()); // n_kv
        data.extend_from_slice(&0u64.to_le_bytes()); // klen
        data.extend_from_slice(&8u32.to_le_bytes()); // vtype = u64
        data.extend_from_slice(&[0u8; 4]); // only 4 bytes of the value
        assert!(Gguf.extract(&data, "m.gguf").is_ok());
    }

    #[test]
    fn safetensors_header() {
        let mut header = br#"{"__metadata__":{"format":"pt"},"w":{"dtype":"F32","shape":[2,3],"data_offsets":[0,24]}}"#.to_vec();
        let mut data = (header.len() as u64).to_le_bytes().to_vec();
        data.append(&mut header);
        let st = Safetensors;
        match st.extract(&data, "m.safetensors").unwrap() {
            Outcome::Text(t) => {
                let s = String::from_utf8(t).unwrap();
                assert!(s.contains("format: pt"), "{s}");
                assert!(s.contains("tensor w: dtype=F32 shape=[2x3]"), "{s}");
            }
            _ => panic!("expected text"),
        }
    }

    #[test]
    fn npy_v1_header() {
        let mut data = b"\x93NUMPY\x01\x00".to_vec();
        let hdr = b"{'descr': '<f4', 'fortran_order': False, 'shape': (2, 3), }";
        let pad = 64 - ((10 + hdr.len() + 1) % 64);
        data.extend_from_slice(&(hdr.len() as u16).to_le_bytes());
        data.extend_from_slice(hdr);
        data.extend(std::iter::repeat_n(b' ', pad));
        data.push(b'\n');
        data.extend(std::iter::repeat_n(0u8, 24));
        let npy = Npy;
        match npy.extract(&data, "a.npy").unwrap() {
            Outcome::Text(t) => {
                let s = String::from_utf8(t).unwrap();
                assert!(s.contains("descr=<f4"), "{s}");
                assert!(s.contains("shape=(2, 3)"), "{s}");
                assert!(s.contains("elements=6"), "{s}");
            }
            _ => panic!("expected text"),
        }
    }

    #[test]
    fn tiktoken_decodes() {
        // base64 of "hello"
        let data = b"aGVsbG8= 42\n";
        let tt = Tiktoken;
        match tt.extract(data, "v.tiktoken").unwrap() {
            Outcome::Text(t) => {
                let s = String::from_utf8(t).unwrap();
                assert!(s.contains("token 42: hello"), "{s}");
            }
            _ => panic!("expected text"),
        }
    }

    mod fuzz {
        use super::*;

        fn safetensors_fixture() -> Vec<u8> {
            let header = br#"{"__metadata__":{"format":"pt"},"w":{"dtype":"F32","shape":[2,3],"data_offsets":[0,24]}}"#.to_vec();
            let mut data = (header.len() as u64).to_le_bytes().to_vec();
            data.extend_from_slice(&header);
            data.extend_from_slice(&[0u8; 24]);
            data
        }

        fn npy_fixture() -> Vec<u8> {
            let mut data = b"\x93NUMPY\x01\x00".to_vec();
            let hdr = b"{'descr': '<f4', 'fortran_order': False, 'shape': (2, 3), }";
            let pad = 64 - ((10 + hdr.len() + 1) % 64);
            data.extend_from_slice(&(hdr.len() as u16).to_le_bytes());
            data.extend_from_slice(hdr);
            data.extend(std::iter::repeat_n(b' ', pad));
            data.push(b'\n');
            data.extend(std::iter::repeat_n(0u8, 24));
            data
        }

        #[test]
        fn safetensors_never_panics_on_mutation() {
            rgx_core::fuzzkit::assert_never_panics(
                vec![("st", safetensors_fixture()), ("empty", vec![])],
                1200,
                "safetensors",
                |data| {
                    let _ = Safetensors.extract(data, "fuzz.safetensors");
                },
            );
        }

        #[test]
        fn npy_never_panics_on_mutation() {
            rgx_core::fuzzkit::assert_never_panics(
                vec![
                    ("npy", npy_fixture()),
                    ("magic-only", b"\x93NUMPY\x01\x00".to_vec()),
                    ("empty", vec![]),
                ],
                1200,
                "npy",
                |data| {
                    let _ = Npy.extract(data, "fuzz.npy");
                },
            );
        }

        #[test]
        fn gguf_never_panics_on_mutation() {
            // minimal GGUF v3 header: magic, version, tensor_count, metadata_kv_count
            let mut d = b"GGUF".to_vec();
            d.extend_from_slice(&3u32.to_le_bytes());
            d.extend_from_slice(&0u64.to_le_bytes());
            d.extend_from_slice(&0u64.to_le_bytes());
            rgx_core::fuzzkit::assert_never_panics(
                vec![
                    ("gguf", d),
                    ("magic-only", b"GGUF".to_vec()),
                    ("empty", vec![]),
                ],
                1500,
                "gguf",
                |data| {
                    let _ = Gguf.extract(data, "fuzz.gguf");
                },
            );
        }

        #[test]
        fn protobuf_never_panics_on_mutation() {
            // a couple of protobuf key/value varints
            let mut d = vec![0x08, 0x96, 0x01]; // field 1 varint 150
            d.extend_from_slice(&[0x12, 0x07]); // field 2 len 7
            d.extend_from_slice(b"ZIRCALO");
            rgx_core::fuzzkit::assert_never_panics(
                vec![
                    ("proto", d),
                    ("varint-only", vec![0x08, 0x96, 0x01]),
                    ("empty", vec![]),
                ],
                1500,
                "protobuf",
                |data| {
                    let _ = Protobuf.extract(data, "fuzz.pb");
                },
            );
        }
    }
}
