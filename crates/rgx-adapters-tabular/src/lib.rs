//! Tabular & geospatial adapters.
//!
//! Databases are dumped as grep-friendly TSV (schema + rows) rather than
//! queried — the whole point is making them greppable, not serving SQL.

#![no_std]

#[macro_use]
extern crate alloc;

#[cfg(any(test, feature = "std"))]
extern crate std;

use core::fmt::Write as _;
#[cfg(feature = "std")]
use std::io::Write as _;

use alloc::boxed::Box;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use rgx_core::{bail, Context, Result};
use rgx_core::{Adapter, Outcome};

mod scientific;

pub fn adapters() -> Vec<Box<dyn Adapter>> {
    vec![
        #[cfg(feature = "std")]
        Box::new(Sqlite),
        #[cfg(feature = "std")]
        Box::new(DuckDb),
        Box::new(Dbf),
        Box::new(Shp),
        Box::new(Parquet),
        Box::new(scientific::MatV5),
        Box::new(scientific::NetCdf3),
        Box::new(scientific::ArrowIpc),
        Box::new(scientific::Hdf5),
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
fn rd_f64le(b: &[u8], o: usize) -> f64 {
    f64::from_le_bytes(b[o..o + 8].try_into().unwrap())
}

/// SQLite: deserialize in-memory via `sqlite3_deserialize` (zero temp file I/O),
/// dump `sqlite_master` then every table as TSV.
#[cfg(feature = "std")]
pub struct Sqlite;
#[cfg(feature = "std")]
impl Adapter for Sqlite {
    fn name(&self) -> &'static str {
        "sqlite"
    }
    fn extensions(&self) -> &'static [&'static str] {
        &["sqlite", "sqlite3", "db", "db3", "sdb", "rdb"]
    }
    fn extract(&self, data: &[u8], _path: &str) -> Result<Outcome> {
        if data.len() < 16 || &data[0..15] != b"SQLite format 3" {
            bail!("not a SQLite database (bad header)");
        }
        dump_sqlite_in_memory(data)
    }
}

/// DuckDB database: columnar database with magic "DUCK" at offset 8.
#[cfg(feature = "std")]
pub struct DuckDb;
#[cfg(feature = "std")]
impl Adapter for DuckDb {
    fn name(&self) -> &'static str {
        "duckdb"
    }
    fn extensions(&self) -> &'static [&'static str] {
        &["duckdb"]
    }
    fn extract(&self, data: &[u8], _path: &str) -> Result<Outcome> {
        if data.len() < 16 || &data[8..12] != b"DUCK" {
            bail!("not a DuckDB database (bad magic)");
        }
        let mut out = Vec::new();
        writeln!(out, "-- DuckDB database --")?;
        let mut cur = Vec::new();
        for &b in data {
            if (0x20..=0x7e).contains(&b) || b == b'\t' {
                cur.push(b);
            } else {
                if cur.len() >= 4 {
                    out.extend_from_slice(&cur);
                    out.push(b'\n');
                }
                cur.clear();
            }
        }
        if cur.len() >= 4 {
            out.extend_from_slice(&cur);
            out.push(b'\n');
        }
        Ok(Outcome::Text(out))
    }
}

/// Open the database directly in memory and walk it. The previous shape
/// wrote the whole buffer to a `temp_dir()` path so rusqlite could
/// `open` it as a regular file -- hundreds of database files in a
/// corpus caused write amplification, lock contention across workers
/// and SSD wear; a multi-hundred-megabyte .db could spend more time
/// in `std::fs::write` than in the query.
///
/// The connection is loaded via `sqlite3_deserialize` so SQLite hands
/// the bytes straight to its in-memory pager. rusqlite's
/// `deserialize_bytes` requires `&'static [u8]` because the FFI call
/// retains the pointer; the transmute is sound because `data` lives
/// for the duration of the `conn` (the walker is single-threaded
/// per-file and the read buffer is borrowed until the adapter
/// returns).
fn dump_sqlite_in_memory(data: &[u8]) -> Result<Outcome> {
    let mut conn = rusqlite::Connection::open_in_memory()
        .map_err(|e| rgx_core::anyhow!("open sqlite (in-memory): {e}"))?;
    // SAFETY: see the function comment. The data slice is borrowed
    // from the walker's read buffer and is alive for the lifetime
    // of `conn` (we never release `conn` before this function
    // returns, and the connection owns the deserialized pages).
    let static_data: &'static [u8] = unsafe { std::mem::transmute::<&[u8], &'static [u8]>(data) };
    conn.deserialize_bytes("main", static_data)
        .map_err(|e| rgx_core::anyhow!("sqlite deserialize: {e}"))?;
    let mut out: Vec<u8> = Vec::new();
    writeln!(out, "-- sqlite_master --")?;
    {
        let mut stmt = conn.prepare("SELECT type, name, tbl_name, sql FROM sqlite_master")?;
        let mut rows = stmt.query([])?;
        while let Some(r) = rows.next()? {
            let t: String = r.get(0)?;
            let n: String = r.get(1)?;
            let tn: Option<String> = r.get(2)?;
            let s: Option<String> = r.get(3)?;
            writeln!(
                out,
                "{}\t{}\t{}\t{}",
                t,
                n,
                tn.as_deref().unwrap_or(""),
                s.as_deref().unwrap_or("")
            )?;
        }
    }
    writeln!(out, "-- tables --")?;
    let tables: Vec<String> = {
        let mut stmt = conn.prepare(
            "SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%'",
        )?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        let mut v = Vec::new();
        for r in rows {
            v.push(r?);
        }
        v
    };
    for table in tables {
        writeln!(out, "-- table: {table} --")?;
        let sql = format!("SELECT * FROM \"{}\"", table.replace('"', "\"\""));
        let mut stmt = conn
            .prepare(&sql)
            .with_context(|| format!("select from {table}"))?;
        let ncol = stmt.column_count();
        let names: Vec<String> = (0..ncol)
            .map(|i| stmt.column_name(i).unwrap_or("").to_string())
            .collect();
        out.extend_from_slice(names.join("\t").as_bytes());
        out.push(b'\n');
        let mut rows = stmt.query([])?;
        while let Some(r) = rows.next()? {
            let mut buf = String::with_capacity(64);
            for i in 0..ncol {
                if i > 0 {
                    buf.push('\t');
                }
                let v: rusqlite::types::ValueRef<'_> = r.get_ref(i)?;
                match v {
                    rusqlite::types::ValueRef::Null => {}
                    rusqlite::types::ValueRef::Integer(n) => {
                        let _ = write!(&mut buf, "{n}");
                    }
                    rusqlite::types::ValueRef::Real(f) => {
                        let _ = write!(&mut buf, "{f}");
                    }
                    rusqlite::types::ValueRef::Text(t) => {
                        let s = std::str::from_utf8(t).unwrap_or("");
                        let _ = write!(&mut buf, "{s}");
                    }
                    rusqlite::types::ValueRef::Blob(b) => {
                        let _ = write!(&mut buf, "<{} bytes>", b.len());
                    }
                }
            }
            buf.push('\n');
            out.extend_from_slice(buf.as_bytes());
        }
    }
    Ok(Outcome::Text(out))
}

/// dBase/DBF (still ubiquitous in GIS + legacy enterprise): header, field
/// descriptors, fixed-length records -> TSV.
pub struct Dbf;
impl Adapter for Dbf {
    fn name(&self) -> &'static str {
        "dbf"
    }
    fn extensions(&self) -> &'static [&'static str] {
        &["dbf"]
    }
    fn extract(&self, data: &[u8], _path: &str) -> Result<Outcome> {
        if data.len() < 32 {
            bail!("too small for DBF");
        }
        let version = data[0];
        // dBase/FoxPro/Visual FoxPro versions span 0x02..0x8b plus 0xf5..0xfb
        // (FoxPro 2.x, VFP with DBC, etc.). An allowlist rejects real
        // FoxPro-era files, so accept any non-zero version byte and let the
        // structural validation below decide.
        if version == 0x00 {
            bail!("not a DBF file (version byte is zero)");
        }
        let n_records = rd_u32le(data, 4) as usize;
        let header_size = rd_u16le(data, 8) as usize;
        let record_size = rd_u16le(data, 10) as usize;
        if header_size < 33 || header_size > data.len() || record_size < 1 {
            bail!("bad DBF header sizes");
        }
        // field descriptors: 32-byte entries until 0x0D
        let mut fields: Vec<(String, usize, usize)> = Vec::new(); // (name, offset, len)
        let mut p = 32usize;
        while p + 32 <= header_size && data[p] != 0x0d {
            let name = String::from_utf8_lossy(&data[p..p + 11])
                .trim_matches(char::from(0))
                .to_string();
            let len = data[p + 16] as usize;
            fields.push((name, 0, len));
            p += 32;
        }
        // compute offsets
        let mut off = 1usize; // skip delete flag
        for f in fields.iter_mut() {
            f.1 = off;
            off += f.2;
        }
        let mut out = String::new();
        out.push_str(
            &fields
                .iter()
                .map(|f| f.0.as_str())
                .collect::<Vec<_>>()
                .join("\t"),
        );
        out.push('\n');
        let mut rec = header_size;
        let mut emitted = 0usize;
        let mut line = String::new();
        while emitted < n_records.min(1_000_000) && rec + record_size <= data.len() {
            if data[rec] == 0x1a {
                break;
            } // EOF marker
            if data[rec] != b'*' {
                // not deleted
                line.clear();
                for (i, (name, off, len)) in fields.iter().enumerate() {
                    if i > 0 {
                        line.push('\t');
                    }
                    let _ = name;
                    let (off, len) = (*off, *len);
                    let end = (rec + off + len).min(rec + record_size);
                    let field = String::from_utf8_lossy(&data[rec + off..end]);
                    line.push_str(field.trim_matches(char::from(0)).trim_end());
                }
                out.push_str(&line);
                out.push('\n');
                emitted += 1;
            }
            rec += record_size;
        }
        if emitted == 0 {
            bail!("DBF contains no records");
        }
        Ok(Outcome::Text(out.into_bytes()))
    }
}

/// Shapefile .shp: header + shape type + bounding box. Geometry counts come
/// from records; for search purposes the header metadata is the payload.
pub struct Shp;
impl Adapter for Shp {
    fn name(&self) -> &'static str {
        "shp"
    }
    fn extensions(&self) -> &'static [&'static str] {
        &["shp"]
    }
    fn extract(&self, data: &[u8], _path: &str) -> Result<Outcome> {
        if data.len() < 100 || rd_u32be(data, 0) != 9994 {
            bail!("not a shapefile (bad magic)");
        }
        let shape_type = rd_u32le(data, 32);
        let shape_name = match shape_type {
            0 => "NullShape",
            1 => "Point",
            3 => "PolyLine",
            5 => "Polygon",
            8 => "MultiPoint",
            11 => "PointZ",
            13 => "PolyLineZ",
            15 => "PolygonZ",
            18 => "MultiPointZ",
            21 => "PointM",
            23 => "PolyLineM",
            25 => "PolygonM",
            28 => "MultiPointM",
            31 => "MultiPatch",
            other => bail!("unknown shape type {other}"),
        };
        let mut out = format!("shapefile\nshape type: {shape_name} ({shape_type})\n");
        let _ = writeln!(
            out,
            "bbox x: {} .. {}",
            rd_f64le(data, 36),
            rd_f64le(data, 44)
        );
        let _ = writeln!(
            out,
            "bbox y: {} .. {}",
            rd_f64le(data, 52),
            rd_f64le(data, 60)
        );
        let _ = writeln!(
            out,
            "bbox z: {} .. {}",
            rd_f64le(data, 68),
            rd_f64le(data, 76)
        );
        let _ = writeln!(
            out,
            "bbox m: {} .. {}",
            rd_f64le(data, 84),
            rd_f64le(data, 92)
        );
        // count records
        let mut p = 100usize;
        let mut n = 0usize;
        while p + 8 <= data.len() {
            let content_len = rd_u32be(data, p + 4) as usize * 2;
            p += 8 + content_len;
            n += 1;
        }
        let _ = writeln!(out, "records: {n}");
        Ok(Outcome::Text(out.into_bytes()))
    }
}

// ---------------------------------------------------------------------------
// Parquet: hand-rolled Thrift-compact reader over the file footer.
// No arrow/parquet dependency — the footer is a small metadata struct and
// we only need the schema tree + row count (ported from the rga fork).
// ---------------------------------------------------------------------------

const MAX_SCHEMA_ELEMENTS: usize = 100_000;

const T_STOP: u8 = 0;
const T_TRUE: u8 = 1;
const T_FALSE: u8 = 2;
const T_BYTE: u8 = 3;
const T_I16: u8 = 4;
const T_I32: u8 = 5;
const T_I64: u8 = 6;
const T_DOUBLE: u8 = 7;
const T_BINARY: u8 = 8;
const T_LIST: u8 = 9;
const T_SET: u8 = 10;
const T_MAP: u8 = 11;
const T_STRUCT: u8 = 12;

struct CReader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> CReader<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }
    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        if self.pos + n > self.data.len() {
            bail!("thrift: truncated");
        }
        let s = &self.data[self.pos..self.pos + n];
        self.pos += n;
        Ok(s)
    }
    fn u8(&mut self) -> Result<u8> {
        Ok(*self.take(1)?.first().unwrap())
    }
    fn varint(&mut self) -> Result<u64> {
        let mut result: u64 = 0;
        for shift in (0..70).step_by(7) {
            let byte = self.u8()?;
            result |= ((byte & 0x7f) as u64) << shift;
            if byte & 0x80 == 0 {
                return Ok(result);
            }
        }
        bail!("thrift: varint too long")
    }
    fn zigzag(&mut self) -> Result<i64> {
        let v = self.varint()?;
        Ok(((v >> 1) as i64) ^ -((v & 1) as i64))
    }
    fn binary(&mut self) -> Result<&'a [u8]> {
        let len = self.varint()? as usize;
        if len > 1 << 24 {
            bail!("thrift: binary too long ({len})");
        }
        self.take(len)
    }
    fn list_header(&mut self) -> Result<(u8, usize)> {
        let b = self.u8()?;
        let elem_type = b & 0x0f;
        let size_nibble = (b >> 4) & 0x0f;
        let size = if size_nibble == 15 {
            self.varint()? as usize
        } else {
            size_nibble as usize
        };
        Ok((elem_type, size))
    }
    /// skip a value of the given type (unknown fields)
    fn skip(&mut self, type_id: u8) -> Result<()> {
        match type_id {
            T_TRUE | T_FALSE => Ok(()),
            T_BYTE => {
                self.u8()?;
                Ok(())
            }
            T_I16 | T_I32 | T_I64 => {
                self.zigzag()?;
                Ok(())
            }
            T_DOUBLE => {
                self.take(8)?;
                Ok(())
            }
            T_BINARY => {
                self.binary()?;
                Ok(())
            }
            T_LIST | T_SET => {
                let (elem_type, n) = self.list_header()?;
                if n > 1 << 20 {
                    bail!("thrift: list too long ({n})");
                }
                for _ in 0..n {
                    self.skip(elem_type)?;
                }
                Ok(())
            }
            T_MAP => {
                // compact protocol: single byte packs key (high nibble) and
                // value (low nibble) types; the size is ALWAYS a varint
                let b = self.u8()?;
                let key_type = (b >> 4) & 0x0f;
                let val_type = b & 0x0f;
                let n = self.varint()? as usize;
                if n > 1 << 20 {
                    bail!("thrift: map too long ({n})");
                }
                for _ in 0..n {
                    self.skip(key_type)?;
                    self.skip(val_type)?;
                }
                Ok(())
            }
            T_STRUCT => self.skip_struct(),
            other => bail!("thrift: unsupported type {other}"),
        }
    }
    /// read a struct skipping all fields (nested unknown structs)
    fn skip_struct(&mut self) -> Result<()> {
        loop {
            let b = self.u8()?;
            if b == T_STOP {
                return Ok(());
            }
            let type_id = b & 0x0f;
            if b >> 4 == 0 {
                // explicit field id follows
                self.zigzag()?;
            }
            self.skip(type_id)?;
        }
    }
}

/// read one field header; returns None on STOP; field ids accumulate deltas
fn field_begin(r: &mut CReader, last_id: &mut i16) -> Result<Option<u8>> {
    let b = r.u8()?;
    if b == T_STOP {
        return Ok(None);
    }
    let type_id = b & 0x0f;
    let delta = (b >> 4) & 0x0f;
    if delta == 0 {
        *last_id = r.zigzag()? as i16;
    } else {
        *last_id += delta as i16;
    }
    Ok(Some(type_id))
}

struct SchemaElement {
    name: String,
    primitive_type: Option<i32>,
    repetition: Option<i32>,
    num_children: i32,
}

fn parquet_type_name(t: i32) -> &'static str {
    match t {
        0 => "BOOLEAN",
        1 => "INT32",
        2 => "INT64",
        3 => "INT96",
        4 => "FLOAT",
        5 => "DOUBLE",
        6 => "BYTE_ARRAY",
        7 => "FIXED_LEN_BYTE_ARRAY",
        _ => "UNKNOWN",
    }
}

fn parquet_repetition_name(r: i32) -> &'static str {
    match r {
        0 => "REQUIRED",
        1 => "OPTIONAL",
        2 => "REPEATED",
        _ => "UNKNOWN",
    }
}

fn read_schema_element(r: &mut CReader) -> Result<SchemaElement> {
    let mut el = SchemaElement {
        name: String::new(),
        primitive_type: None,
        repetition: None,
        num_children: 0,
    };
    let mut last_id: i16 = 0;
    while let Some(type_id) = field_begin(r, &mut last_id)? {
        match (last_id, type_id) {
            (1, T_I32) => el.primitive_type = Some(r.zigzag()? as i32),
            (3, T_I32) => el.repetition = Some(r.zigzag()? as i32),
            (4, T_BINARY) => el.name = String::from_utf8_lossy(r.binary()?).into_owned(),
            (5, T_I32) => el.num_children = r.zigzag()? as i32,
            _ => r.skip(type_id)?,
        }
    }
    Ok(el)
}

fn parse_footer(metadata: &[u8]) -> Result<(i64, Vec<SchemaElement>)> {
    let mut r = CReader::new(metadata);
    // parquet writers usually serialize FileMetaData directly as a compact
    // struct; some include the full thrift message envelope (0x82 + version)
    if r.u8()? == 0x82 {
        r.u8()?; // protocol version
    } else {
        r.pos = 0;
    }
    let mut num_rows: Option<i64> = None;
    let mut schema: Option<Vec<SchemaElement>> = None;
    let mut last_id: i16 = 0;
    while let Some(type_id) = field_begin(&mut r, &mut last_id)? {
        match (last_id, type_id) {
            (2, T_LIST) => {
                let (elem_type, n) = r.list_header()?;
                if elem_type != T_STRUCT || n > MAX_SCHEMA_ELEMENTS {
                    bail!("parquet schema list malformed");
                }
                let mut elems = Vec::with_capacity(n);
                for _ in 0..n {
                    elems.push(read_schema_element(&mut r)?);
                }
                schema = Some(elems);
            }
            (3, T_I64) => num_rows = Some(r.zigzag()?),
            _ => r.skip(type_id)?,
        }
    }
    let schema = schema.ok_or_else(|| rgx_core::anyhow!("parquet footer has no schema"))?;
    Ok((num_rows.unwrap_or(-1), schema))
}

/// render the flat schema element list as an indented tree; recursion depth
/// is capped so a crafted footer cannot overflow the stack
fn render_schema(elems: &[SchemaElement]) -> String {
    const MAX_DEPTH: usize = 64;
    let mut out = String::new();
    let mut idx = 0;
    fn render(elems: &[SchemaElement], idx: &mut usize, depth: usize, out: &mut String) {
        if *idx >= elems.len() {
            return;
        }
        let el = &elems[*idx];
        *idx += 1;
        let indent = "  ".repeat(depth.min(MAX_DEPTH));
        if el.num_children > 0 {
            let _ = writeln!(out, "{indent}{} (group)", el.name);
            if depth < MAX_DEPTH {
                for _ in 0..el.num_children {
                    render(elems, idx, depth + 1, out);
                }
            }
        } else {
            let t = el
                .primitive_type
                .map(parquet_type_name)
                .unwrap_or("UNKNOWN");
            let rep = el.repetition.map(parquet_repetition_name).unwrap_or("");
            let _ = writeln!(out, "{indent}{}: {t} {rep}", el.name);
        }
    }
    render(elems, &mut idx, 0, &mut out);
    out
}

fn parse_parquet(data: &[u8]) -> Result<String> {
    if data.len() < 12 {
        bail!("parquet file too small");
    }
    if &data[0..4] != b"PAR1" {
        bail!("not a parquet file (bad magic)");
    }
    let footer_len_pos = data.len() - 8;
    if &data[footer_len_pos + 4..data.len()] != b"PAR1" {
        bail!("not a parquet file (bad footer magic)");
    }
    let metadata_len =
        u32::from_le_bytes(data[footer_len_pos..footer_len_pos + 4].try_into().unwrap()) as usize;
    if metadata_len + 8 > data.len() {
        bail!("parquet footer length overruns file");
    }
    let metadata = &data[data.len() - 8 - metadata_len..footer_len_pos];
    let (num_rows, schema) = parse_footer(metadata)?;
    Ok(format!("num_rows: {num_rows}\n{}", render_schema(&schema)))
}

/// Parquet: row count + column schema (names, types, repetition, nesting)
/// from the file footer — no arrow/parquet stack required.
pub struct Parquet;
impl Adapter for Parquet {
    fn name(&self) -> &'static str {
        "parquet"
    }
    fn extensions(&self) -> &'static [&'static str] {
        &["parquet"]
    }
    fn extract(&self, data: &[u8], _path: &str) -> Result<Outcome> {
        Ok(Outcome::Text(parse_parquet(data)?.into_bytes()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // -- minimal thrift-compact encoder for fixtures --

    fn zz(v: i64) -> u64 {
        ((v << 1) ^ (v >> 63)) as u64
    }
    fn varint(mut v: u64, out: &mut Vec<u8>) {
        loop {
            let mut b = (v & 0x7f) as u8;
            v >>= 7;
            if v != 0 {
                b |= 0x80;
            }
            out.push(b);
            if v == 0 {
                break;
            }
        }
    }
    fn field(delta: i16, type_id: u8, out: &mut Vec<u8>) {
        out.push(((delta as u8) << 4) | type_id);
    }
    fn i32_field(delta: i16, v: i32, out: &mut Vec<u8>) {
        field(delta, T_I32, out);
        varint(zz(v as i64), out);
    }
    fn i64_field(delta: i16, v: i64, out: &mut Vec<u8>) {
        field(delta, T_I64, out);
        varint(zz(v), out);
    }
    fn bin_field(delta: i16, s: &str, out: &mut Vec<u8>) {
        field(delta, T_BINARY, out);
        varint(s.len() as u64, out);
        out.extend_from_slice(s.as_bytes());
    }
    fn stop(out: &mut Vec<u8>) {
        out.push(T_STOP);
    }

    fn make_group(name: &str, num_children: i32) -> Vec<u8> {
        let mut v = Vec::new();
        bin_field(4, name, &mut v); // field id 4 (delta 4): name
        i32_field(1, num_children, &mut v); // field id 5: num_children
        stop(&mut v);
        v
    }

    fn make_leaf(name: &str, ptype: i32, rep: i32) -> Vec<u8> {
        let mut v = Vec::new();
        i32_field(1, ptype, &mut v); // field id 1: primitive type
        i32_field(2, rep, &mut v); // field id 3 (delta 2): repetition
        bin_field(1, name, &mut v); // field id 4 (delta 1): name
        stop(&mut v);
        v
    }

    fn make_parquet(metadata_body: &[u8]) -> Vec<u8> {
        // real parquet files serialize FileMetaData without the thrift
        // message envelope, so the fixture does the same
        let footer = metadata_body.to_vec();
        let mut file = vec![];
        file.extend_from_slice(b"PAR1");
        file.extend_from_slice(&footer);
        file.extend_from_slice(&(footer.len() as u32).to_le_bytes());
        file.extend_from_slice(b"PAR1");
        file
    }

    fn extract_text(data: &[u8]) -> String {
        let p = Parquet;
        match p.extract(data, "data.parquet").unwrap() {
            Outcome::Text(t) => String::from_utf8(t).unwrap().trim().to_string(),
            _ => panic!("expected text"),
        }
    }

    #[test]
    fn parquet_flat_schema() {
        // FileMetaData: version(1), schema(2), num_rows(3)
        let mut md = Vec::new();
        i32_field(1, 1, &mut md);
        field(1, T_LIST, &mut md);
        // list<struct> in the extended-length form, 3 elements:
        // root group + 2 leaf columns
        md.push(0xf0 | T_STRUCT);
        varint(3, &mut md);
        md.extend_from_slice(&make_group("message", 2));
        md.extend_from_slice(&make_leaf("name", 6, 1)); // BYTE_ARRAY OPTIONAL
        md.extend_from_slice(&make_leaf("score", 5, 1)); // DOUBLE OPTIONAL
        i64_field(1, 1_000_000, &mut md); // num_rows (delta 1 from field 2)
        stop(&mut md);
        let file = make_parquet(&md);

        assert_eq!(
            extract_text(&file),
            "num_rows: 1000000\nmessage (group)\n  name: BYTE_ARRAY OPTIONAL\n  score: DOUBLE OPTIONAL"
        );
    }

    #[test]
    fn parquet_deeply_nested_schema_does_not_stack_overflow() {
        // a crafted footer with a linear chain of 10k nested groups must
        // not overflow the stack; output flattens at the depth cap
        const N: usize = 10_000;
        let mut md = Vec::new();
        i32_field(1, 1, &mut md);
        field(1, T_LIST, &mut md);
        md.push(0xf0 | T_STRUCT);
        varint(N as u64, &mut md);
        for i in 0..N {
            md.extend_from_slice(&make_group(&format!("g{i}"), 1));
        }
        // final element: a leaf so the chain ends
        md.extend_from_slice(&make_leaf("leaf", 1, 0));
        i64_field(1, 1, &mut md);
        stop(&mut md);
        let file = make_parquet(&md);

        let text = extract_text(&file);
        // cap at MAX_DEPTH=64: 65 groups render (g0..g64), then output stops
        assert!(
            text.contains("g0 (group)"),
            "got {}",
            text.lines().next().unwrap_or("")
        );
        assert!(text.contains("g64 (group)"), "missing capped tail");
        assert!(!text.contains("g65"), "should flatten at the depth cap");
    }

    #[test]
    fn parquet_map_field_is_skipped_via_varint_size() {
        // FileMetaData with an unknown field holding a compact map with a
        // varint size > 14 (old code read the size nibble and bailed).
        // field ids ascend: version(1), schema(2), num_rows(3), map(4)
        let mut md = Vec::new();
        i32_field(1, 1, &mut md); // version
        field(1, T_LIST, &mut md); // field 2: schema
        md.push(0xf0 | T_STRUCT);
        varint(2, &mut md);
        md.extend_from_slice(&make_group("message", 1));
        md.extend_from_slice(&make_leaf("leaf", 1, 0)); // INT32 REQUIRED
        i64_field(1, 0, &mut md); // field 3: num_rows
                                  // unknown field 4: map<binary, binary> with 20 entries (varint size)
        field(1, T_MAP, &mut md);
        md.push((T_BINARY << 4) | T_BINARY);
        varint(20, &mut md);
        for i in 0..20 {
            let k = format!("k{i}");
            varint(k.len() as u64, &mut md);
            md.extend_from_slice(k.as_bytes());
            let v = format!("v{i}");
            varint(v.len() as u64, &mut md);
            md.extend_from_slice(v.as_bytes());
        }
        stop(&mut md);
        let file = make_parquet(&md);

        let text = extract_text(&file);
        assert!(text.contains("message (group)"), "got {text}");
        assert!(text.contains("leaf: INT32 REQUIRED"), "got {text}");
        assert!(text.contains("num_rows: 0"), "got {text}");
    }

    /// One-field DBF holding a single record whose NAME is "abc".
    /// Callers overwrite byte 0 to exercise other version bytes.
    fn dbf_with_record() -> Vec<u8> {
        let mut data = vec![0u8; 32];
        data[0] = 0x30; // dBase IV
        data[4..8].copy_from_slice(&1u32.to_le_bytes()); // 1 record
        data[8..10].copy_from_slice(&65u16.to_le_bytes()); // header: 32 + 1 field*32 + 0x0D
        data[10..12].copy_from_slice(&12u16.to_le_bytes()); // record size
        let mut f = vec![0u8; 32];
        f[0..4].copy_from_slice(b"NAME");
        f[11] = b'C';
        f[16] = 10;
        data.extend_from_slice(&f);
        data.push(0x0d);
        let mut rec = vec![b' '; 12];
        rec[1..5].copy_from_slice(b"abc ");
        data.extend_from_slice(&rec);
        data.push(0x1a);
        data
    }

    #[test]
    fn dbf_roundtrip() {
        let mut data = vec![0u8; 32];
        data[0] = 0x30; // dBase IV
        data[4..8].copy_from_slice(&1u32.to_le_bytes()); // 1 record
        data[8..10].copy_from_slice(&65u16.to_le_bytes()); // header: 32 + 1 field*32 + 0x0D
        data[10..12].copy_from_slice(&12u16.to_le_bytes()); // record size
                                                            // one field: NAME, char, len 10
        let mut f = vec![0u8; 32];
        f[0..4].copy_from_slice(b"NAME");
        f[11] = b'C';
        f[16] = 10;
        data.extend_from_slice(&f);
        data.push(0x0d);
        let mut rec = vec![b' '; 12];
        rec[1..5].copy_from_slice(b"abc ");
        data.extend_from_slice(&rec);
        data.push(0x1a);
        let d = Dbf;
        match d.extract(&data, "a.dbf").unwrap() {
            Outcome::Text(t) => {
                let s = String::from_utf8(t).unwrap();
                assert!(s.contains("NAME"), "{s}");
                assert!(s.contains("abc"), "{s}");
            }
            _ => panic!("expected text"),
        }
    }

    #[test]
    fn dbf_accepts_foxpro_version() {
        // FoxPro 2.x uses version byte 0xf5, outside the dBase 0x02..=0x8b
        // range. A version allowlist would bail and fall through to raw
        // binary search, hiding the records entirely.
        let mut data = dbf_with_record();
        data[0] = 0xf5;
        match Dbf.extract(&data, "a.dbf").unwrap() {
            Outcome::Text(t) => {
                let s = String::from_utf8(t).unwrap();
                assert!(s.contains("NAME"), "{s}");
                assert!(s.contains("abc"), "records must still be read: {s}");
            }
            _ => panic!("expected text"),
        }
    }

    #[test]
    fn dbf_rejects_zero_version() {
        // The structural validation only runs for a plausible version byte.
        let mut data = dbf_with_record();
        data[0] = 0x00;
        assert!(Dbf.extract(&data, "a.dbf").is_err());
    }

    #[test]
    fn shp_header() {
        let mut data = vec![0u8; 100];
        data[0..4].copy_from_slice(&9994u32.to_be_bytes());
        data[32..36].copy_from_slice(&1u32.to_le_bytes()); // Point
        data[36..44].copy_from_slice(&1.0f64.to_le_bytes());
        data[44..52].copy_from_slice(&2.0f64.to_le_bytes());
        let s = Shp;
        match s.extract(&data, "a.shp").unwrap() {
            Outcome::Text(t) => {
                let t = String::from_utf8(t).unwrap();
                assert!(t.contains("Point"), "{t}");
                assert!(t.contains("bbox x: 1"), "{t}");
            }
            _ => panic!("expected text"),
        }
    }

    #[test]
    #[cfg(feature = "std")]
    fn sqlite_dump() {
        let tmp = std::env::temp_dir().join(format!("rgx-test-{}.sqlite", std::process::id()));
        {
            let conn = rusqlite::Connection::open(&tmp).unwrap();
            conn.execute_batch(
                "CREATE TABLE t(id INTEGER, name TEXT); INSERT INTO t VALUES (1, 'ZIRCALOY');",
            )
            .unwrap();
        }
        let bytes = std::fs::read(&tmp).unwrap();
        let _ = std::fs::remove_file(&tmp);
        let sq = Sqlite;
        match sq.extract(&bytes, "t.sqlite").unwrap() {
            Outcome::Text(t) => {
                let s = String::from_utf8(t).unwrap();
                assert!(s.contains("CREATE TABLE t"), "{s}");
                assert!(s.contains("ZIRCALOY"), "{s}");
            }
            _ => panic!("expected text"),
        }
    }

    #[test]
    #[cfg(feature = "std")]
    fn sqlite_wal_database_leaves_no_side_files() {
        // A WAL-mode database opened with plain SQLITE_OPEN_READ_ONLY makes
        // SQLite create empty -wal and -shm files next to it. The previous
        // shape copied the database to %TEMP% and ran immutable=1, so the
        // cleanup pass had to delete any pre-existing side files after
        // every search. The new shape uses `sqlite3_deserialize` on an
        // in-memory database, so temp dir is never touched.
        //
        // Note: a fresh WAL-mode SQLite main file is empty (the data lives
        // in the -wal sidecar), so we read both files into a single byte
        // vector. The deserialize is the standard main-file path; the
        // assertion that matters here is that the adapter did not touch
        // the system temp dir.
        let dir = std::env::temp_dir();
        let stem = format!("rgx-wal-test-{}", std::process::id());
        let src = dir.join(format!("{stem}.sqlite"));
        {
            let conn = rusqlite::Connection::open(&src).unwrap();
            conn.execute_batch(
                "PRAGMA journal_mode=WAL; CREATE TABLE t(id INTEGER); INSERT INTO t VALUES (7);",
            )
            .unwrap();
        }
        let bytes = std::fs::read(&src).unwrap();
        let _ = std::fs::remove_file(&src);
        let _ = std::fs::remove_file(dir.join(format!("{stem}.sqlite-wal")));
        let _ = std::fs::remove_file(dir.join(format!("{stem}.sqlite-shm")));

        // Even if the deserializer refuses the WAL main file (the data
        // lives in the -wal sidecar in this mode), the adapter must NOT
        // have written any rgx-* files to the temp dir. That is the
        // invariant the test pins.
        let _ = Sqlite.extract(&bytes, "t.sqlite");
        let leftovers: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.starts_with("rgx-") && n.contains(".sqlite"))
            .collect();
        assert!(
            leftovers.is_empty(),
            "adapter leaked temp files: {leftovers:?}"
        );
    }

    mod fuzz {
        use super::*;

        fn parquet_flat() -> Vec<u8> {
            let mut md = Vec::new();
            i32_field(1, 1, &mut md);
            field(1, T_LIST, &mut md);
            md.push(0xf0 | T_STRUCT);
            varint(3, &mut md);
            md.extend_from_slice(&make_group("message", 2));
            md.extend_from_slice(&make_leaf("name", 6, 1));
            md.extend_from_slice(&make_leaf("score", 5, 1));
            i64_field(1, 1_000_000, &mut md);
            stop(&mut md);
            make_parquet(&md)
        }

        fn dbf_one_record() -> Vec<u8> {
            let mut data = vec![0u8; 32];
            data[0] = 0x30;
            data[4..8].copy_from_slice(&1u32.to_le_bytes());
            data[8..10].copy_from_slice(&65u16.to_le_bytes());
            data[10..12].copy_from_slice(&12u16.to_le_bytes());
            let mut f = vec![0u8; 32];
            f[0..4].copy_from_slice(b"NAME");
            f[11] = b'C';
            f[16] = 10;
            data.extend_from_slice(&f);
            data.push(0x0d);
            data.extend_from_slice(&[b' '; 12]);
            data.push(0x1a);
            data
        }

        fn shp_point() -> Vec<u8> {
            let mut data = vec![0u8; 100];
            data[0..4].copy_from_slice(&9994u32.to_be_bytes());
            data[32..36].copy_from_slice(&1u32.to_le_bytes());
            data[36..44].copy_from_slice(&1.0f64.to_le_bytes());
            data[44..52].copy_from_slice(&2.0f64.to_le_bytes());
            data
        }

        #[test]
        fn parquet_never_panics_on_mutation() {
            rgx_core::fuzzkit::assert_never_panics(
                vec![
                    ("flat", parquet_flat()),
                    ("magic-only", b"PAR1....PAR1".to_vec()),
                    ("empty", vec![]),
                ],
                1500,
                "parquet",
                |data| {
                    let _ = Parquet.extract(data, "fuzz.parquet");
                },
            );
        }

        #[test]
        fn dbf_never_panics_on_mutation() {
            rgx_core::fuzzkit::assert_never_panics(
                vec![("one-rec", dbf_one_record()), ("empty", vec![])],
                1200,
                "dbf",
                |data| {
                    let _ = Dbf.extract(data, "fuzz.dbf");
                },
            );
        }

        #[test]
        fn duckdb_extracts_strings() {
            let mut data = vec![0u8; 16];
            data[8..12].copy_from_slice(b"DUCK");
            data.extend_from_slice(b"some_duckdb_table_data");
            let out = match DuckDb.extract(&data, "test.duckdb").unwrap() {
                Outcome::Text(t) => String::from_utf8(t).unwrap(),
                _ => panic!("expected text"),
            };
            assert!(out.contains("some_duckdb_table_data"));
        }

        #[test]
        fn duckdb_rejects_bad_magic() {
            assert!(DuckDb.extract(b"not duckdb at all", "test.duckdb").is_err());
        }

        #[test]
        fn shp_never_panics_on_mutation() {
            rgx_core::fuzzkit::assert_never_panics(
                vec![("point", shp_point()), ("empty", vec![])],
                1200,
                "shp",
                |data| {
                    let _ = Shp.extract(data, "fuzz.shp");
                },
            );
        }
    }
}
