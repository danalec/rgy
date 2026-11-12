// SPDX-License-Identifier: Apache-2.0

//! A reader for the SQLite file format.
//!
//! The adapter needs two things out of a database: the schema (the tables
//! and the columns of each one) and the rows. SQLite keeps both in the
//! file, so a reader is a matter of following the format -- the header,
//! the pages, the b-trees, the records, the varints, the serial types and
//! the overflow pages.
//!
//! What the format does *not* keep is the column names. A record holds
//! the values; the names live in the `CREATE TABLE` statement in the
//! schema table. So this module carries a small parser for that statement
//! as well: enough of one to read a column list, the primary key and the
//! generated columns.
//!
//! What is not here: the write-ahead log, the rollback journal, the
//! pointer map, the expression evaluator and the virtual table modules.
//! A database whose log has not been checkpointed is a database whose
//! file is not the database, and this reader says so instead of guessing.
//!
//! The format is the one the SQLite project documents:
//! <https://www.sqlite.org/fileformat.html>.

use alloc::string::{String, ToString};
use alloc::vec::Vec;

/// The text encoding of a database.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Encoding {
    /// UTF-8, the usual one.
    Utf8,
    /// UTF-16, little endian.
    Utf16Le,
    /// UTF-16, big endian.
    Utf16Be,
}

/// Why a database could not be read.
#[derive(Debug)]
pub enum Error {
    /// The bytes are not a database at all.
    NotADatabase,
    /// The database is a shape this reader does not read.
    Unsupported(&'static str),
    /// The database is broken.
    Malformed(String),
}

impl core::fmt::Display for Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Error::NotADatabase => f.write_str("not a SQLite database"),
            Error::Unsupported(why) => f.write_str(why),
            Error::Malformed(why) => write!(f, "malformed database: {why}"),
        }
    }
}

/// A value of a record.
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    /// No value at all.
    Null,
    /// A whole number.
    Integer(i64),
    /// A floating point number.
    Real(f64),
    /// Text, as UTF-8 bytes. A database in UTF-16 is converted on the way
    /// in, so the bytes here are always UTF-8.
    Text(Vec<u8>),
    /// A blob.
    Blob(Vec<u8>),
}

/// One row of a table, as the record holds it.
#[derive(Clone, Debug)]
pub struct Row {
    /// The rowid of the row, for a table that has one.
    pub rowid: i64,
    /// The values, in the order the record stores them.
    pub values: Vec<Value>,
}

/// One row of the schema table.
#[derive(Clone, Debug)]
pub struct SchemaRow {
    /// `table`, `index`, `view` or `trigger`.
    pub kind: String,
    /// The name of the object.
    pub name: String,
    /// The table the object belongs to.
    pub table_name: String,
    /// The root page of the object's b-tree. Zero for a view or a trigger,
    /// which have no b-tree of their own.
    pub root_page: u32,
    /// The statement that created the object, when the schema records one.
    /// An index that SQLite created for itself has none.
    pub sql: Option<String>,
}

/// A column of a table, as the schema describes it.
#[derive(Clone, Debug)]
pub struct Column {
    /// The name the table gives the column.
    pub name: String,
    /// The value a row that predates the column carries: the default the
    /// column was declared with, when the default is a literal this reader
    /// understands.
    pub default: Option<Value>,
    /// The value of the column is computed when the row is read: the
    /// expression is in the schema, not in the file, and this reader does
    /// not evaluate expressions. Such a column has no value here.
    pub computed: bool,
}

/// A table of the database.
#[derive(Clone, Debug)]
pub struct Table {
    /// The name of the table.
    pub name: String,
    /// The root page of the table's b-tree.
    pub root_page: u32,
    /// The columns of the table, in the order the table declares them.
    pub columns: Vec<Column>,
    /// For each declared column, the field of the record that holds it --
    /// or `None` when the column is not in the record.
    pub record_fields: Vec<Option<usize>>,
    /// The column that stands for the rowid, when the table has one.
    pub rowid_alias: Option<usize>,
    /// The table stores its rows in an index b-tree: the primary key
    /// columns come first in the record, and the rowid does not exist.
    pub without_rowid: bool,
    /// The table is a virtual table: its rows live in a module, not in
    /// the file.
    pub virtual_table: bool,
}

impl Table {
    /// The values of one row, in the order the columns are declared.
    pub fn row_values(&self, row: &Row) -> Vec<Value> {
        let mut out = Vec::with_capacity(self.columns.len());
        for (index, column) in self.columns.iter().enumerate() {
            if Some(index) == self.rowid_alias {
                // The column is the rowid: the record stores NULL for it
                // and the rowid is the value.
                out.push(Value::Integer(row.rowid));
                continue;
            }
            if column.computed {
                // The value of a computed column is computed when the row
                // is read: the expression is in the schema, not in the
                // file. This reader reads the file, so it has no answer.
                out.push(Value::Null);
                continue;
            }
            match self.record_fields[index] {
                Some(field) if field < row.values.len() => out.push(row.values[field].clone()),
                // The row predates the column: the value is the default
                // the column was declared with.
                Some(_) => out.push(column.default.clone().unwrap_or(Value::Null)),
                // The column is computed when it is read, and this reader
                // does not evaluate expressions.
                None => out.push(Value::Null),
            }
        }
        out
    }
}

/// A database read from its bytes.
pub struct Db<'a> {
    data: &'a [u8],
    page_size: usize,
    usable: usize,
    encoding: Encoding,
    pages: u32,
}

impl<'a> Db<'a> {
    /// Open the bytes of a database file.
    ///
    /// The bytes are the whole file: a database is a file, and the parts
    /// of a database that live in other files -- the write-ahead log, the
    /// rollback journal -- are not read here, so a database that carries
    /// its data in one of them is refused rather than half-read.
    pub fn open(data: &'a [u8]) -> Result<Self, Error> {
        if data.len() < 100 || &data[..16] != b"SQLite format 3\0" {
            return Err(Error::NotADatabase);
        }
        let page_size = match u16::from_be_bytes([data[16], data[17]]) {
            1 => 65536,
            n => n as usize,
        };
        if page_size < 512 || !page_size.is_power_of_two() {
            return Err(Error::Malformed(format!("page size {page_size}")));
        }
        let usable = page_size - data[20] as usize;
        if usable < 480 {
            return Err(Error::Malformed(format!("usable page size {usable}")));
        }
        let write_version = data[18];
        let read_version = data[19];
        if read_version > 2 || write_version > 2 {
            return Err(Error::Unsupported(
                "the file format version is newer than this reader knows",
            ));
        }
        if read_version == 2 || write_version == 2 {
            return Err(Error::Unsupported(
                "the database is in write-ahead log mode: the log is a separate file, and this reader does not read it",
            ));
        }
        let encoding = match u32::from_be_bytes([data[56], data[57], data[58], data[59]]) {
            1 => Encoding::Utf8,
            2 => Encoding::Utf16Le,
            3 => Encoding::Utf16Be,
            _ => {
                return Err(Error::Unsupported(
                    "the text encoding is not one this reader knows",
                ))
            }
        };
        // The header carries the size of the database in pages, and it is
        // only right when the change counter and the version-valid-for
        // number agree. When they do not, the length of the file is the
        // answer.
        let change = u32::from_be_bytes([data[24], data[25], data[26], data[27]]);
        let valid = u32::from_be_bytes([data[92], data[93], data[94], data[95]]);
        let header_pages = u32::from_be_bytes([data[28], data[29], data[30], data[31]]);
        let file_pages = (data.len() / page_size) as u32;
        let pages = if change == valid && header_pages != 0 {
            header_pages.min(file_pages)
        } else {
            file_pages
        };
        Ok(Db {
            data,
            page_size,
            usable,
            encoding,
            pages,
        })
    }

    /// The bytes of one page.
    fn page(&self, number: u32) -> Result<&'a [u8], Error> {
        if number == 0 || number > self.pages {
            return Err(Error::Malformed(format!(
                "page {number} is not in the file"
            )));
        }
        let start = (number as u64 - 1) * self.page_size as u64;
        let end = start + self.page_size as u64;
        if end > self.data.len() as u64 {
            return Err(Error::Malformed(format!(
                "page {number} is past the end of the file"
            )));
        }
        Ok(&self.data[start as usize..end as usize])
    }

    /// The rows of the schema table, in the order the schema holds them.
    pub fn schema(&self) -> Result<Vec<SchemaRow>, Error> {
        let mut rows = Vec::new();
        self.walk(1, false, &mut rows, 0)?;
        let mut out = Vec::with_capacity(rows.len());
        for row in rows {
            // The schema table has five columns: the type of the object,
            // its name, the table it belongs to, the root page of its
            // b-tree and the statement that created it.
            let kind = text_of(&row.values, 0)?;
            let name = text_of(&row.values, 1)?;
            let table_name = text_of(&row.values, 2)?;
            let root_page = match row.values.get(3) {
                Some(Value::Integer(n)) => *n as u32,
                _ => 0,
            };
            let sql = match row.values.get(4) {
                Some(Value::Text(bytes)) => {
                    Some(String::from_utf8(bytes.clone()).map_err(|_| {
                        Error::Malformed("the schema holds text that is not UTF-8".to_string())
                    })?)
                }
                _ => None,
            };
            out.push(SchemaRow {
                kind,
                name,
                table_name,
                root_page,
                sql,
            });
        }
        Ok(out)
    }

    /// The tables of the database, with the columns of each one.
    ///
    /// The tables SQLite keeps for itself -- `sqlite_sequence` and the
    /// rest -- are not among them: they are the database's own machinery,
    /// and a reader that dumped them would answer questions nobody asked.
    pub fn tables(&self) -> Result<Vec<Table>, Error> {
        let mut out = Vec::new();
        for row in self.schema()? {
            if row.kind != "table" || is_internal_name(&row.name) {
                continue;
            }
            let sql = row.sql.as_deref().unwrap_or("");
            let def = parse_create_table(sql);
            out.push(build_table(&row.name, row.root_page, def, sql));
        }
        Ok(out)
    }

    /// The rows of a table, in the order the table holds them.
    pub fn rows(&self, table: &Table) -> Result<Vec<Row>, Error> {
        let mut out = Vec::new();
        self.walk(table.root_page, table.without_rowid, &mut out, 0)?;
        Ok(out)
    }

    /// Walk a b-tree and collect the records it holds.
    fn walk(&self, root: u32, index: bool, out: &mut Vec<Row>, depth: u32) -> Result<(), Error> {
        if depth > 64 {
            return Err(Error::Malformed(
                "the b-tree is deeper than a database can be".into(),
            ));
        }
        let page = self.page(root)?;
        // Page 1 carries the 100-byte header of the database before its
        // b-tree header; every other page starts with the b-tree header.
        let base = if root == 1 { 100 } else { 0 };
        let header = PageHeader::read(page, base)?;
        let interior = header.kind == 0x02 || header.kind == 0x05;
        let pointers = base + if interior { 12 } else { 8 };
        for cell in 0..header.cells {
            let at = pointers + cell * 2;
            let offset = match page.get(at..at + 2) {
                Some(bytes) => u16::from_be_bytes([bytes[0], bytes[1]]) as usize,
                None => return Err(Error::Malformed("a cell pointer is past the page".into())),
            };
            let bytes = match page.get(offset..) {
                Some(bytes) => bytes,
                None => return Err(Error::Malformed("a cell points past the page".into())),
            };
            match header.kind {
                // A table b-tree leaf: the payload, then the rowid.
                0x0d => {
                    let (size, rest) = varint(bytes)?;
                    let (rowid, rest) = varint(rest)?;
                    let payload = self.read_payload(rest, size as usize, index)?;
                    out.push(Row {
                        rowid: rowid as i64,
                        values: self.record(&payload)?,
                    });
                }
                // An index b-tree leaf: the payload alone.
                0x0a => {
                    let (size, rest) = varint(bytes)?;
                    let payload = self.read_payload(rest, size as usize, index)?;
                    out.push(Row {
                        rowid: 0,
                        values: self.record(&payload)?,
                    });
                }
                // An interior page: the left child of each cell, and the
                // right-most child of the page. The keys themselves are
                // not needed to walk the tree.
                0x05 | 0x02 => {
                    let child = match bytes.get(0..4) {
                        Some(bytes) => u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]),
                        None => {
                            return Err(Error::Malformed("an interior cell is too short".into()))
                        }
                    };
                    self.walk(child, index, out, depth + 1)?;
                }
                _ => return Err(Error::Malformed("a page is not a b-tree page".into())),
            }
        }
        if interior {
            self.walk(header.right, index, out, depth + 1)?;
        }
        Ok(())
    }

    /// The payload of a cell: the bytes on the page, and the bytes the
    /// overflow pages hold.
    fn read_payload(&self, cell: &[u8], total: usize, index: bool) -> Result<Vec<u8>, Error> {
        let local = self.local_size(total, index);
        if cell.len() < local {
            return Err(Error::Malformed(
                "a cell is shorter than its payload".into(),
            ));
        }
        let mut payload = Vec::with_capacity(total);
        payload.extend_from_slice(&cell[..local]);
        if local >= total {
            return Ok(payload);
        }
        let next = match cell.get(local..local + 4) {
            Some(bytes) => u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]),
            None => return Err(Error::Malformed("a cell has no overflow pointer".into())),
        };
        let mut number = next;
        let mut left = total - local;
        let mut hops = 0usize;
        while left > 0 {
            hops += 1;
            if hops > 1 << 20 {
                return Err(Error::Malformed("the overflow chain does not end".into()));
            }
            let page = self.page(number)?;
            let take = left.min(self.usable - 4);
            payload.extend_from_slice(&page[4..4 + take]);
            left -= take;
            number = u32::from_be_bytes([page[0], page[1], page[2], page[3]]);
            if left > 0 && number == 0 {
                return Err(Error::Malformed(
                    "the overflow chain ends before the payload does".into(),
                ));
            }
        }
        Ok(payload)
    }

    /// The number of payload bytes a cell keeps on its own page.
    ///
    /// The thresholds are the ones the format fixes: a table b-tree leaf
    /// keeps up to `U - 35` bytes, an index page keeps up to
    /// `((U - 12) * 64 / 255) - 23`, and the spill is arranged so that a
    /// cell never keeps less than `((U - 12) * 32 / 255) - 23` bytes.
    fn local_size(&self, total: usize, index: bool) -> usize {
        let usable = self.usable;
        let min = ((usable - 12) * 32 / 255) - 23;
        let max = if index {
            ((usable - 12) * 64 / 255) - 23
        } else {
            usable - 35
        };
        if total <= max {
            return total;
        }
        let k = min + (total - min) % (usable - 4);
        if k <= max {
            k
        } else {
            min
        }
    }

    /// The values of a record.
    fn record(&self, payload: &[u8]) -> Result<Vec<Value>, Error> {
        let (header, mut rest) = varint(payload)?;
        let header = header as usize;
        let mut used = payload.len() - rest.len();
        if header < used || header > payload.len() {
            return Err(Error::Malformed(
                "a record header is longer than the record".into(),
            ));
        }
        let mut types = Vec::new();
        while used < header {
            let (serial, tail) = varint(rest)?;
            types.push(serial);
            rest = tail;
            used = payload.len() - rest.len();
        }
        let mut body = header;
        let mut values = Vec::with_capacity(types.len());
        for serial in types {
            let (value, size) = self.value(serial, payload.get(body..).unwrap_or(&[]))?;
            values.push(value);
            body += size;
        }
        Ok(values)
    }

    /// One value of a record, and the number of bytes it took.
    fn value(&self, serial: u64, bytes: &[u8]) -> Result<(Value, usize), Error> {
        let short = || Error::Malformed("a record is shorter than its header says".into());
        Ok(match serial {
            0 => (Value::Null, 0),
            1 => (Value::Integer(be_i8(bytes)? as i64), 1),
            2 => (Value::Integer(be_i16(bytes)? as i64), 2),
            3 => (Value::Integer(be_i24(bytes)? as i64), 3),
            4 => (Value::Integer(be_i32(bytes)? as i64), 4),
            5 => (Value::Integer(be_i48(bytes)?), 6),
            6 => (Value::Integer(be_i64(bytes)?), 8),
            7 => (Value::Real(be_f64(bytes)?), 8),
            8 => (Value::Integer(0), 0),
            9 => (Value::Integer(1), 0),
            10 | 11 => {
                return Err(Error::Malformed(
                    "a record uses a reserved serial type".into(),
                ))
            }
            n if n % 2 == 0 => {
                let size = ((n - 12) / 2) as usize;
                let bytes = bytes.get(..size).ok_or_else(short)?;
                (Value::Blob(bytes.to_vec()), size)
            }
            n => {
                let size = ((n - 13) / 2) as usize;
                let bytes = bytes.get(..size).ok_or_else(short)?;
                (Value::Text(self.text(bytes)), size)
            }
        })
    }

    /// The text of a value, as UTF-8.
    ///
    /// A database in UTF-16 stores its text in UTF-16, and SQLite hands it
    /// out as UTF-8; so does this reader.
    fn text(&self, bytes: &[u8]) -> Vec<u8> {
        match self.encoding {
            Encoding::Utf8 => bytes.to_vec(),
            Encoding::Utf16Le => utf16_to_utf8(bytes, false),
            Encoding::Utf16Be => utf16_to_utf8(bytes, true),
        }
    }
}

/// The header of a b-tree page.
struct PageHeader {
    /// The kind of the page: 0x02 and 0x05 are interior pages, 0x0a and
    /// 0x0d are leaves.
    kind: u8,
    /// The number of cells on the page.
    cells: usize,
    /// The right-most child of an interior page.
    right: u32,
}

impl PageHeader {
    fn read(page: &[u8], base: usize) -> Result<Self, Error> {
        let short = || Error::Malformed("a page is shorter than its header".into());
        let kind = *page.get(base).ok_or_else(short)?;
        let cells = match page.get(base + 3..base + 5) {
            Some(bytes) => u16::from_be_bytes([bytes[0], bytes[1]]) as usize,
            None => return Err(short()),
        };
        let interior = kind == 0x02 || kind == 0x05;
        let right = if interior {
            match page.get(base + 8..base + 12) {
                Some(bytes) => u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]),
                None => return Err(short()),
            }
        } else {
            0
        };
        Ok(PageHeader { kind, cells, right })
    }
}

/// A varint: a big-endian integer of one to nine bytes, and the bytes
/// that follow it.
fn varint(bytes: &[u8]) -> Result<(u64, &[u8]), Error> {
    let mut value: u64 = 0;
    for index in 0..8 {
        let byte = match bytes.get(index) {
            Some(byte) => *byte,
            None => {
                return Err(Error::Malformed(
                    "a varint runs past the end of the page".into(),
                ))
            }
        };
        value = (value << 7) | u64::from(byte & 0x7f);
        if byte & 0x80 == 0 {
            return Ok((value, &bytes[index + 1..]));
        }
    }
    let byte = match bytes.get(8) {
        Some(byte) => *byte,
        None => {
            return Err(Error::Malformed(
                "a varint runs past the end of the page".into(),
            ))
        }
    };
    Ok(((value << 8) | u64::from(byte), &bytes[9..]))
}

/// The text of a value, as a string.
fn text_of(values: &[Value], index: usize) -> Result<String, Error> {
    match values.get(index) {
        Some(Value::Text(bytes)) => String::from_utf8(bytes.clone())
            .map_err(|_| Error::Malformed("the database holds text that is not UTF-8".into())),
        _ => Err(Error::Malformed("a record of the schema is short".into())),
    }
}

/// Whether a name is one of the tables SQLite keeps for itself.
///
/// The filter the adapter used is `NOT LIKE 'sqlite_%'`, and `LIKE` is
/// case-insensitive and treats `_` as any one character: the name is one
/// of SQLite's own when it starts with `sqlite` and has at least one more
/// character after it.
///
/// The first six BYTES are what the name is compared on, and a name whose
/// sixth byte falls inside a character has no six bytes to compare. The
/// slice is taken with `get`, which says "there is no such slice" — and a
/// name that came out of the file is not the tree's own doing, so it does
/// not get to stop the answer.
fn is_internal_name(name: &str) -> bool {
    // The six bytes are taken with `get`: a name whose sixth byte is
    // inside a character has no six bytes to compare, and `get` says so
    // where a plain slice would panic.
    let Some(head) = name.get(..6) else {
        return false;
    };
    name.len() > 6 && head.eq_ignore_ascii_case("sqlite")
}

fn be_i8(bytes: &[u8]) -> Result<i8, Error> {
    bytes
        .first()
        .map(|byte| *byte as i8)
        .ok_or_else(|| Error::Malformed("a record is shorter than its header says".into()))
}

fn be_i16(bytes: &[u8]) -> Result<i16, Error> {
    let bytes = bytes
        .get(..2)
        .ok_or_else(|| Error::Malformed("a record is shorter than its header says".into()))?;
    Ok(i16::from_be_bytes([bytes[0], bytes[1]]))
}

fn be_i24(bytes: &[u8]) -> Result<i32, Error> {
    let bytes = bytes
        .get(..3)
        .ok_or_else(|| Error::Malformed("a record is shorter than its header says".into()))?;
    let value = i32::from_be_bytes([0, bytes[0], bytes[1], bytes[2]]);
    Ok(value >> 8)
}

fn be_i32(bytes: &[u8]) -> Result<i32, Error> {
    let bytes = bytes
        .get(..4)
        .ok_or_else(|| Error::Malformed("a record is shorter than its header says".into()))?;
    Ok(i32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}

fn be_i48(bytes: &[u8]) -> Result<i64, Error> {
    let bytes = bytes
        .get(..6)
        .ok_or_else(|| Error::Malformed("a record is shorter than its header says".into()))?;
    let value = i64::from_be_bytes([
        0, 0, bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5],
    ]);
    Ok(value >> 16)
}

fn be_i64(bytes: &[u8]) -> Result<i64, Error> {
    let bytes = bytes
        .get(..8)
        .ok_or_else(|| Error::Malformed("a record is shorter than its header says".into()))?;
    Ok(i64::from_be_bytes([
        bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
    ]))
}

fn be_f64(bytes: &[u8]) -> Result<f64, Error> {
    let bytes = bytes
        .get(..8)
        .ok_or_else(|| Error::Malformed("a record is shorter than its header says".into()))?;
    Ok(f64::from_be_bytes([
        bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
    ]))
}

/// UTF-16 to UTF-8.
///
/// A code unit that is not a character on its own -- a surrogate without
/// its pair -- becomes the replacement character: SQLite would refuse the
/// database, and a reader that guesses is better than one that stops.
fn utf16_to_utf8(bytes: &[u8], big_endian: bool) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len());
    let mut at = 0usize;
    while at + 1 < bytes.len() {
        let unit = if big_endian {
            u16::from_be_bytes([bytes[at], bytes[at + 1]])
        } else {
            u16::from_le_bytes([bytes[at], bytes[at + 1]])
        };
        at += 2;
        let character = match unit {
            0xd800..=0xdbff => {
                if at + 1 < bytes.len() {
                    let low = if big_endian {
                        u16::from_be_bytes([bytes[at], bytes[at + 1]])
                    } else {
                        u16::from_le_bytes([bytes[at], bytes[at + 1]])
                    };
                    if (0xdc00..=0xdfff).contains(&low) {
                        at += 2;
                        let code = 0x1_0000
                            + ((u32::from(unit) - 0xd800) << 10)
                            + (u32::from(low) - 0xdc00);
                        char::from_u32(code)
                    } else {
                        Some('\u{fffd}')
                    }
                } else {
                    Some('\u{fffd}')
                }
            }
            0xdc00..=0xdfff => Some('\u{fffd}'),
            _ => char::from_u32(u32::from(unit)),
        };
        if let Some(character) = character {
            let mut buffer = [0u8; 4];
            out.extend_from_slice(character.encode_utf8(&mut buffer).as_bytes());
        }
    }
    out
}

// --- the CREATE TABLE statement ------------------------------------------

/// The definition of a table, as its `CREATE TABLE` statement says it.
#[derive(Clone, Debug, Default)]
pub struct TableDef {
    /// The columns, in the order the statement declares them.
    pub columns: Vec<ColumnDef>,
    /// The primary key, as indices into the columns.
    pub primary_key: Vec<usize>,
    /// The table stores its rows in an index b-tree.
    pub without_rowid: bool,
    /// The table is a virtual table: its rows live in a module.
    pub virtual_table: bool,
}

/// One column of a `CREATE TABLE` statement.
#[derive(Clone, Debug)]
pub struct ColumnDef {
    /// The name of the column.
    pub name: String,
    /// The type the column was declared with.
    pub type_name: String,
    /// The column is computed when it is read.
    pub computed: bool,
    /// The literal default of the column, when it has one this reader
    /// understands.
    pub default: Option<Value>,
    /// The column is declared `PRIMARY KEY`.
    pub primary_key: bool,
    /// The `PRIMARY KEY` of the column is declared `DESC`, which takes
    /// the column out of the rowid alias.
    pub descending: bool,
}

/// The words that end the type name of a column and begin its
/// constraints.
const CONSTRAINT_WORDS: [&str; 11] = [
    "CONSTRAINT",
    "PRIMARY",
    "NOT",
    "NULL",
    "UNIQUE",
    "CHECK",
    "DEFAULT",
    "COLLATE",
    "REFERENCES",
    "GENERATED",
    "AS",
];

/// Read the `CREATE TABLE` statement of a table.
///
/// This is not a SQL parser: it reads the shape of the statement -- the
/// column list, the primary key, the generated columns and the
/// `WITHOUT ROWID` clause -- and stops where the shape ends. A statement
/// it cannot read gives a table with no columns, which is what a table
/// whose definition is not a `CREATE TABLE` at all gives.
pub fn parse_create_table(sql: &str) -> TableDef {
    let mut cursor = Sql::new(sql.as_bytes());
    let mut def = TableDef::default();
    cursor.skip_space();
    if !cursor.eat_word("CREATE") {
        return def;
    }
    cursor.skip_space();
    if cursor.eat_word("VIRTUAL") {
        def.virtual_table = true;
        cursor.skip_space();
    }
    if !cursor.eat_word("TABLE") {
        return def;
    }
    cursor.skip_space();
    if cursor.eat_word("IF") {
        cursor.skip_space();
        if !cursor.eat_word("NOT") {
            return def;
        }
        cursor.skip_space();
        if !cursor.eat_word("EXISTS") {
            return def;
        }
        cursor.skip_space();
    }
    // The name of the table, which may be qualified by the schema it
    // lives in.
    if cursor.read_ident().is_none() {
        return def;
    }
    cursor.skip_space();
    if cursor.eat_char(b'.') {
        cursor.skip_space();
        if cursor.read_ident().is_none() {
            return def;
        }
        cursor.skip_space();
    }
    // The column list is the parenthesised group after the name. The
    // cursor stays ON the parenthesis: the group reader counts the
    // nesting, and it counts from the parenthesis it is given.
    if !cursor.peek_char(b'(') {
        // A table created from a query has no column list of its own.
        // SQLite rewrites the statement it stores, so this is rare.
        return def;
    }
    let body = match cursor.balanced() {
        Some(body) => body,
        None => return def,
    };
    cursor.skip_space();
    if cursor.eat_word("WITHOUT") {
        cursor.skip_space();
        cursor.eat_word("ROWID");
        def.without_rowid = true;
    } else {
        cursor.skip_space();
        cursor.eat_word("STRICT");
    }

    let mut primary: Vec<usize> = Vec::new();
    for item in split_items(body) {
        let mut cursor = Sql::new(item);
        cursor.skip_space();
        if cursor.starts_constraint() {
            // A table constraint. Only the primary key says anything the
            // reader needs; the rest are about the rows and not about
            // where they are stored.
            if let Some((columns, descending)) = cursor.primary_key_columns() {
                primary = columns
                    .iter()
                    .filter_map(|name| def.columns.iter().position(|c| &c.name == name))
                    .collect();
                if descending {
                    // A `PRIMARY KEY(col DESC)` takes the column out of
                    // the rowid alias; the flag is kept on the column.
                    if let Some(index) = primary.first() {
                        if let Some(column) = def.columns.get_mut(*index) {
                            column.descending = true;
                        }
                    }
                }
            }
            continue;
        }
        // A column the reader cannot read is not a column: the statement
        // says something this reader does not know how to read.
        if let Some(column) = cursor.column() {
            def.columns.push(column);
        }
    }
    if primary.is_empty() {
        // A column declared `PRIMARY KEY` is the primary key when the
        // statement does not name one.
        primary = def
            .columns
            .iter()
            .enumerate()
            .filter(|(_, column)| column.primary_key)
            .map(|(index, _)| index)
            .collect();
    }
    def.primary_key = primary;
    def
}

/// Build the table a schema row describes.
fn build_table(name: &str, root_page: u32, def: TableDef, sql: &str) -> Table {
    let TableDef {
        columns,
        primary_key,
        without_rowid,
        virtual_table,
    } = def;

    // The columns of a table that is not `WITHOUT ROWID` are stored in the
    // order they are declared; a table that is stores the primary key
    // columns first, in the order the primary key names them.
    let stored: Vec<usize> = if without_rowid {
        let mut order: Vec<usize> = Vec::new();
        for index in &primary_key {
            if !columns[*index].computed && !order.contains(index) {
                order.push(*index);
            }
        }
        for (index, column) in columns.iter().enumerate() {
            if !column.computed && !order.contains(&index) {
                order.push(index);
            }
        }
        order
    } else {
        (0..columns.len())
            .filter(|index| !columns[*index].computed)
            .collect()
    };

    let mut record_fields = vec![None; columns.len()];
    for (field, index) in stored.iter().enumerate() {
        if let Some(slot) = record_fields.get_mut(*index) {
            *slot = Some(field);
        }
    }

    // The rowid alias: a column of type INTEGER that is the primary key
    // of a table that has a rowid. The record stores NULL for it and the
    // rowid is the value.
    let rowid_alias = if without_rowid || virtual_table {
        None
    } else if primary_key.len() == 1 {
        let index = primary_key[0];
        let column = columns.get(index);
        let is_integer = column
            .map(|column| column.type_name.eq_ignore_ascii_case("INTEGER"))
            .unwrap_or(false);
        let descending = column.map(|column| column.descending).unwrap_or(false);
        if is_integer && !descending {
            Some(index)
        } else {
            None
        }
    } else {
        None
    };

    let columns = columns
        .into_iter()
        .map(|column| Column {
            name: column.name,
            default: column.default,
            computed: column.computed,
        })
        .collect();

    let _ = sql;
    Table {
        name: name.to_string(),
        root_page,
        columns,
        record_fields,
        rowid_alias,
        without_rowid,
        virtual_table,
    }
}

/// A cursor over a SQL statement.
struct Sql<'a> {
    text: &'a [u8],
    at: usize,
}

impl<'a> Sql<'a> {
    fn new(text: &'a [u8]) -> Self {
        Sql { text, at: 0 }
    }

    /// Skip the spaces and the comments.
    fn skip_space(&mut self) {
        loop {
            while self.at < self.text.len() && self.text[self.at].is_ascii_whitespace() {
                self.at += 1;
            }
            if self.text.get(self.at) == Some(&b'-') && self.text.get(self.at + 1) == Some(&b'-') {
                while self.at < self.text.len() && self.text[self.at] != b'\n' {
                    self.at += 1;
                }
                continue;
            }
            if self.text.get(self.at) == Some(&b'/') && self.text.get(self.at + 1) == Some(&b'*') {
                self.at += 2;
                while self.at < self.text.len() {
                    if self.text[self.at] == b'*' && self.text.get(self.at + 1) == Some(&b'/') {
                        self.at += 2;
                        break;
                    }
                    self.at += 1;
                }
                continue;
            }
            return;
        }
    }

    /// The word at the cursor, if the cursor is at one.
    fn word(&self) -> Option<&'a str> {
        let first = *self.text.get(self.at)?;
        if !(first.is_ascii_alphabetic() || first == b'_' || first >= 0x80) {
            return None;
        }
        let mut end = self.at;
        while end < self.text.len() {
            let byte = self.text[end];
            if byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'$' || byte >= 0x80 {
                end += 1;
            } else {
                break;
            }
        }
        core::str::from_utf8(&self.text[self.at..end]).ok()
    }

    /// Eat a word, whatever its case.
    fn eat_word(&mut self, word: &str) -> bool {
        match self.word() {
            Some(found) if found.eq_ignore_ascii_case(word) => {
                self.at += found.len();
                true
            }
            _ => false,
        }
    }

    /// Is the cursor at this character?
    fn peek_char(&self, byte: u8) -> bool {
        self.text.get(self.at) == Some(&byte)
    }

    /// Eat a character.
    fn eat_char(&mut self, byte: u8) -> bool {
        if self.text.get(self.at) == Some(&byte) {
            self.at += 1;
            true
        } else {
            false
        }
    }

    /// An identifier: a word, or a quoted one.
    fn read_ident(&mut self) -> Option<String> {
        self.skip_space();
        match self.text.get(self.at) {
            Some(b'"') => {
                self.at += 1;
                let mut out = String::new();
                loop {
                    match self.text.get(self.at) {
                        Some(b'"') => {
                            if self.text.get(self.at + 1) == Some(&b'"') {
                                out.push('"');
                                self.at += 2;
                            } else {
                                self.at += 1;
                                return Some(out);
                            }
                        }
                        Some(byte) => {
                            out.push(*byte as char);
                            self.at += 1;
                        }
                        None => return Some(out),
                    }
                }
            }
            Some(b'`') => {
                self.at += 1;
                let mut out = String::new();
                loop {
                    match self.text.get(self.at) {
                        Some(b'`') => {
                            if self.text.get(self.at + 1) == Some(&b'`') {
                                out.push('`');
                                self.at += 2;
                            } else {
                                self.at += 1;
                                return Some(out);
                            }
                        }
                        Some(byte) => {
                            out.push(*byte as char);
                            self.at += 1;
                        }
                        None => return Some(out),
                    }
                }
            }
            Some(b'[') => {
                self.at += 1;
                let start = self.at;
                while self.at < self.text.len() && self.text[self.at] != b']' {
                    self.at += 1;
                }
                let out = String::from_utf8_lossy(&self.text[start..self.at]).into_owned();
                self.at += 1;
                Some(out)
            }
            Some(b'\'') => {
                // SQLite writes the name of a shadow table in single
                // quotes -- `CREATE TABLE 'vt_data'(...)` -- so a schema
                // that carries a virtual table has to be read.
                self.at += 1;
                let mut out = String::new();
                loop {
                    match self.text.get(self.at) {
                        Some(b'\'') => {
                            if self.text.get(self.at + 1) == Some(&b'\'') {
                                out.push('\'');
                                self.at += 2;
                            } else {
                                self.at += 1;
                                return Some(out);
                            }
                        }
                        Some(byte) => {
                            out.push(*byte as char);
                            self.at += 1;
                        }
                        None => return Some(out),
                    }
                }
            }
            _ => {
                let word = self.word()?;
                self.at += word.len();
                Some(word.to_string())
            }
        }
    }

    /// The bytes inside the parenthesised group at the cursor, or `None`
    /// when there is none.
    fn balanced(&mut self) -> Option<&'a [u8]> {
        if self.text.get(self.at) != Some(&b'(') {
            return None;
        }
        let start = self.at + 1;
        let mut depth = 0usize;
        while self.at < self.text.len() {
            match self.text[self.at] {
                b'(' => depth += 1,
                b')' => {
                    depth = depth.saturating_sub(1);
                    if depth == 0 {
                        let body = &self.text[start..self.at];
                        self.at += 1;
                        return Some(body);
                    }
                }
                b'\'' => self.skip_quoted(b'\''),
                b'"' => self.skip_quoted(b'"'),
                b'`' => self.skip_quoted(b'`'),
                _ => {}
            }
            self.at += 1;
        }
        None
    }

    /// Skip a quoted run, where the quote is doubled inside it.
    fn skip_quoted(&mut self, quote: u8) {
        self.at += 1;
        while self.at < self.text.len() {
            if self.text[self.at] == quote {
                if self.text.get(self.at + 1) == Some(&quote) {
                    self.at += 2;
                    continue;
                }
                return;
            }
            self.at += 1;
        }
    }

    /// Whether the item at the cursor is a table constraint rather than
    /// a column definition.
    fn starts_constraint(&self) -> bool {
        const TABLE_CONSTRAINTS: [&str; 5] =
            ["CONSTRAINT", "PRIMARY", "UNIQUE", "CHECK", "FOREIGN"];
        match self.word() {
            Some(word) => TABLE_CONSTRAINTS
                .iter()
                .any(|candidate| word.eq_ignore_ascii_case(candidate)),
            None => false,
        }
    }

    /// The columns of a `PRIMARY KEY` table constraint, and whether the
    /// key is declared `DESC`.
    fn primary_key_columns(&mut self) -> Option<(Vec<String>, bool)> {
        if self.eat_word("CONSTRAINT") {
            self.skip_space();
            self.read_ident();
            self.skip_space();
        }
        if !self.eat_word("PRIMARY") {
            return None;
        }
        self.skip_space();
        if !self.eat_word("KEY") {
            return None;
        }
        self.skip_space();
        let body = self.balanced()?;
        let mut columns = Vec::new();
        let mut descending = false;
        for item in split_items(body) {
            let mut cursor = Sql::new(item);
            cursor.skip_space();
            if let Some(name) = cursor.read_ident() {
                columns.push(name);
            }
            cursor.skip_space();
            if cursor.eat_word("DESC") {
                descending = true;
            }
        }
        Some((columns, descending))
    }

    /// A column definition.
    fn column(&mut self) -> Option<ColumnDef> {
        let name = self.read_ident()?;
        let mut type_name = String::new();
        // The type of a column is a run of words, each of which may carry
        // a parenthesised argument list: `VARCHAR(20)`, `DECIMAL(10, 2)`.
        loop {
            self.skip_space();
            let part = match self.type_part() {
                Some(part) => part,
                None => break,
            };
            if !type_name.is_empty() {
                type_name.push(' ');
            }
            type_name.push_str(&part.1);
            self.at += part.0;
            self.skip_space();
            if self.text.get(self.at) == Some(&b'(') {
                let _ = self.balanced();
            }
        }
        let mut column = ColumnDef {
            name,
            type_name,
            computed: false,
            default: None,
            primary_key: false,
            descending: false,
        };
        // The constraints of the column.
        loop {
            self.skip_space();
            if self.at >= self.text.len() {
                break;
            }
            if self.eat_word("CONSTRAINT") {
                self.skip_space();
                self.read_ident();
                continue;
            }
            if self.eat_word("PRIMARY") {
                self.skip_space();
                self.eat_word("KEY");
                column.primary_key = true;
                self.skip_space();
                if self.eat_word("DESC") {
                    column.descending = true;
                } else {
                    self.eat_word("ASC");
                }
                continue;
            }
            if self.eat_word("NOT") {
                self.skip_space();
                self.eat_word("NULL");
                continue;
            }
            if self.eat_word("NULL") {
                continue;
            }
            if self.eat_word("UNIQUE") {
                continue;
            }
            if self.eat_word("CHECK") {
                self.skip_space();
                let _ = self.balanced();
                continue;
            }
            if self.eat_word("DEFAULT") {
                column.default = self.default_value();
                continue;
            }
            if self.eat_word("COLLATE") {
                self.skip_space();
                self.read_ident();
                continue;
            }
            if self.eat_word("REFERENCES") {
                self.skip_space();
                self.read_ident();
                self.skip_space();
                if self.text.get(self.at) == Some(&b'(') {
                    let _ = self.balanced();
                }
                // The rest of the clause is about what happens when the
                // key is broken, and it says nothing about the column.
                continue;
            }
            if self.eat_word("GENERATED") {
                self.skip_space();
                self.eat_word("ALWAYS");
                self.skip_space();
                self.eat_word("AS");
                self.skip_space();
                let _ = self.balanced();
                self.skip_space();
                column.computed = !self.eat_word("STORED");
                self.skip_space();
                self.eat_word("VIRTUAL");
                continue;
            }
            if self.eat_word("AS") {
                self.skip_space();
                let _ = self.balanced();
                self.skip_space();
                column.computed = !self.eat_word("STORED");
                self.skip_space();
                self.eat_word("VIRTUAL");
                continue;
            }
            // A word this reader does not know: step over it.
            self.at += 1;
        }
        Some(column)
    }

    /// The next part of a type name, with the length of the text it took.
    fn type_part(&self) -> Option<(usize, String)> {
        match self.text.get(self.at) {
            Some(b'"') | Some(b'`') | Some(b'[') => {
                let mut cursor = Sql::new(&self.text[self.at..]);
                let name = cursor.read_ident()?;
                Some((cursor.at, name))
            }
            _ => {
                let word = self.word()?;
                if CONSTRAINT_WORDS
                    .iter()
                    .any(|candidate| word.eq_ignore_ascii_case(candidate))
                {
                    return None;
                }
                Some((word.len(), word.to_string()))
            }
        }
    }

    /// The default a column was declared with, when it is a literal this
    /// reader understands.
    fn default_value(&mut self) -> Option<Value> {
        self.skip_space();
        let start = self.at;
        if self.text.get(self.at) == Some(&b'(') {
            let _ = self.balanced();
        } else {
            // A term: a sign, a number, a word or a quoted string.
            if self.text.get(self.at) == Some(&b'-') || self.text.get(self.at) == Some(&b'+') {
                self.at += 1;
            }
            match self.text.get(self.at) {
                Some(b'\'') | Some(b'"') => {
                    let quote = self.text[self.at];
                    self.skip_quoted(quote);
                    // The quoted run ends on its closing quote, and the
                    // default is the whole run -- the closing quote
                    // included. An unterminated run has no quote to step
                    // over, and the text it left is not a literal.
                    if self.at < self.text.len() {
                        self.at += 1;
                    }
                }
                Some(byte) if byte.is_ascii_digit() => {
                    while self.at < self.text.len()
                        && (self.text[self.at].is_ascii_digit()
                            || self.text[self.at] == b'.'
                            || self.text[self.at] == b'e'
                            || self.text[self.at] == b'E'
                            || self.text[self.at] == b'+'
                            || self.text[self.at] == b'-')
                    {
                        self.at += 1;
                    }
                }
                Some(_) => {
                    let word = self.word()?;
                    self.at += word.len();
                }
                None => return None,
            }
        }
        let text = core::str::from_utf8(&self.text[start..self.at]).ok()?;
        literal(text)
    }
}

/// A literal value, as the default of a column.
fn literal(text: &str) -> Option<Value> {
    let text = text.trim();
    let text = match text
        .strip_prefix('(')
        .and_then(|rest| rest.strip_suffix(')'))
    {
        Some(inner) => inner.trim(),
        None => text,
    };
    if text.eq_ignore_ascii_case("NULL") {
        return Some(Value::Null);
    }
    if text.eq_ignore_ascii_case("TRUE") {
        return Some(Value::Integer(1));
    }
    if text.eq_ignore_ascii_case("FALSE") {
        return Some(Value::Integer(0));
    }
    if text.len() >= 2 && text.starts_with('\'') && text.ends_with('\'') {
        return Some(Value::Text(
            text[1..text.len() - 1].replace("''", "'").into_bytes(),
        ));
    }
    if text.parse::<i64>().is_ok() {
        return Some(Value::Integer(text.parse::<i64>().ok()?));
    }
    if let Ok(real) = text.parse::<f64>() {
        if text.chars().any(|c| c.is_ascii_digit()) {
            return Some(Value::Real(real));
        }
    }
    None
}

/// Split a statement into the items separated by its top-level commas.
fn split_items(body: &[u8]) -> Vec<&[u8]> {
    let mut items = Vec::new();
    let mut start = 0usize;
    let mut depth = 0usize;
    let mut at = 0usize;
    while at < body.len() {
        match body[at] {
            b'(' => depth += 1,
            b')' => depth = depth.saturating_sub(1),
            b',' if depth == 0 => {
                items.push(&body[start..at]);
                start = at + 1;
            }
            b'\'' | b'"' | b'`' => {
                let quote = body[at];
                at += 1;
                while at < body.len() {
                    if body[at] == quote {
                        if body.get(at + 1) == Some(&quote) {
                            at += 2;
                            continue;
                        }
                        break;
                    }
                    at += 1;
                }
            }
            b'[' => {
                while at < body.len() && body[at] != b']' {
                    at += 1;
                }
            }
            b'-' if body.get(at + 1) == Some(&b'-') => {
                while at < body.len() && body[at] != b'\n' {
                    at += 1;
                }
            }
            b'/' if body.get(at + 1) == Some(&b'*') => {
                at += 2;
                while at < body.len() {
                    if body[at] == b'*' && body.get(at + 1) == Some(&b'/') {
                        at += 2;
                        break;
                    }
                    at += 1;
                }
                continue;
            }
            _ => {}
        }
        at += 1;
    }
    items.push(&body[start..]);
    items
}

#[cfg(test)]
mod tests {
    use super::is_internal_name;

    /// A table name whose sixth byte is inside a character.
    ///
    /// The name comes out of the file, and a name is not the tree's own
    /// doing. The slice of the first six bytes is taken with `get`, which
    /// says "there is no such slice" where a plain slice would panic — and
    /// a name that has no six bytes to compare is not one of SQLite's own
    /// tables: it is a table, and the answer says so.
    #[test]
    fn a_table_name_that_is_not_on_a_character_boundary_is_not_internal() {
        // The emoji is four bytes, so byte six is inside it.
        assert!(!is_internal_name("sql\u{1F600}"));
        // And a two-byte character in the same place.
        assert!(!is_internal_name("sql\u{e9}"));
        // The names that ARE SQLite's own still are.
        assert!(is_internal_name("sqlite_sequence"));
        assert!(is_internal_name("SQLITE_stat1"));
        // And the ones that are not still are not.
        assert!(!is_internal_name("sqlite"));
        assert!(!is_internal_name("mytable"));
    }
}
