// SPDX-License-Identifier: Apache-2.0

//! SQLite itself, as the oracle the reader this crate ships is held to.
//!
//! The reader reads the file format out of the bytes. SQLite is the
//! reference: the corpus below is a set of real databases, and every one
//! of them is read through both -- once by the reader that ships and once
//! by SQLite -- and the two answers are compared.
//!
//! `rusqlite` is a dev-dependency because of this file. It does not
//! enter the crate a user installs: the reader that ships is the one this
//! crate wrote, and the oracle is here to keep it honest.

#![cfg(all(test, feature = "sqlite"))]

use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::fmt::Write as _;
use rgx_core::{Context, Field, Outcome};
use std::io::Write as _;
use std::path::{Path, PathBuf};

/// The reader this crate had before: SQLite itself, in memory.
fn old_dump(data: &[u8]) -> rgx_core::Result<Outcome> {
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

/// The facts this crate had before.
fn old_facts(data: &[u8]) -> Vec<Field> {
    let mut facts = Vec::new();
    if data.len() < 16 || &data[0..15] != b"SQLite format 3" {
        return facts;
    }
    let Ok(mut conn) = rusqlite::Connection::open_in_memory() else {
        return facts;
    };
    let static_data: &'static [u8] = unsafe { std::mem::transmute::<&[u8], &'static [u8]>(data) };
    if conn.deserialize_bytes("main", static_data).is_err() {
        return facts;
    }
    let tables: Vec<String> = {
        let Ok(mut stmt) = conn.prepare(
            "SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%'",
        ) else {
            return facts;
        };
        let Ok(rows) = stmt.query_map([], |r| r.get::<_, String>(0)) else {
            return facts;
        };
        rows.flatten().collect()
    };
    for table in tables {
        facts.push(Field::new("table.name", &table));
        let sql = format!("SELECT * FROM \"{}\"", table.replace('"', "\"\""));
        let Ok(stmt) = conn.prepare(&sql) else {
            continue;
        };
        for i in 0..stmt.column_count() {
            let name = stmt.column_name(i).unwrap_or("");
            if !name.is_empty() {
                facts.push(Field::new("table.column", name));
            }
        }
    }
    facts
}

/// The part of a dump that holds one table.
fn section(dump: &str, table: &str) -> String {
    let header = format!("-- table: {table} --");
    let start = dump
        .find(&header)
        .unwrap_or_else(|| panic!("the dump has no {header}"));
    let rest = &dump[start..];
    // The next table starts the next section; the search starts one
    // character in, so the header of this one is not the one it finds.
    let end = rest[1..]
        .find("-- table: ")
        .map(|at| at + 1)
        .unwrap_or(rest.len());
    rest[..end].to_string()
}

/// What a reader said, for a message that says what differs.
fn describe(result: &rgx_core::Result<Outcome>) -> String {
    match result {
        Ok(Outcome::Text(text)) => format!("{} bytes of text", text.len()),
        Ok(_) => "something that is not text".to_string(),
        Err(e) => format!("an error: {e}"),
    }
}

/// The corpus: the databases the two readers are compared on.
///
/// It is built once per run, by SQLite itself, in a folder of its own.
/// The oracle builds the databases it is compared with, so the two
/// readers always read the same bytes -- and a folder left over from an
/// earlier run cannot stand in for one.
fn corpus() -> &'static Path {
    static CORPUS: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    CORPUS.get_or_init(|| {
        let dir = std::env::temp_dir().join("rgy-sqlite-corpus");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        build_corpus(&dir);
        dir
    })
}

/// Build one database and record the reader's answer for it.
fn record(dir: &Path, name: &str, pragmas: &[&str], sql: &str) {
    let path = dir.join(format!("{name}.db"));
    let _ = std::fs::remove_file(&path);
    {
        let conn = rusqlite::Connection::open(&path).unwrap();
        for p in pragmas {
            conn.execute_batch(p).unwrap();
        }
        conn.execute_batch(sql).unwrap();
    }
    let bytes = std::fs::read(&path).unwrap();
    std::fs::write(dir.join(format!("{name}.db")), &bytes).unwrap();

    let dump = match old_dump(&bytes) {
        Ok(Outcome::Text(t)) => t,
        Ok(_) => b"<not text>".to_vec(),
        Err(e) => format!("<error: {e}>").into_bytes(),
    };
    std::fs::write(dir.join(format!("{name}.dump")), &dump).unwrap();

    let mut facts = String::new();
    for f in old_facts(&bytes) {
        facts.push_str(&f.name);
        facts.push('\t');
        facts.push_str(&f.value);
        facts.push('\n');
    }
    std::fs::write(dir.join(format!("{name}.facts")), facts.as_bytes()).unwrap();
}

/// Build the corpus: a database per shape the reader has to answer.
fn build_corpus(dir: &Path) {
    record(
        dir,
        "simple",
        &[],
        "CREATE TABLE t(id INTEGER, name TEXT); INSERT INTO t VALUES (1, 'ZIRCALOY');",
    );

    record(
        dir,
        "types",
        &[],
        "CREATE TABLE t(i INTEGER, r REAL, s TEXT, b BLOB, n TEXT);
         INSERT INTO t VALUES (42, 3.5, 'hello', x'00ff10', NULL);
         INSERT INTO t VALUES (-7, 0.1, '', x'', 'x');
         INSERT INTO t VALUES (0, 1.0, 'tab\there', x'4142', 'tail');",
    );

    record(
        dir,
        "multi",
        &[],
        "CREATE TABLE users(id INTEGER PRIMARY KEY, name TEXT, email TEXT);
         CREATE TABLE posts(id INTEGER, user_id INTEGER, title TEXT);
         CREATE INDEX idx_posts_user ON posts(user_id);
         CREATE VIEW v AS SELECT id, name FROM users;
         CREATE TRIGGER tr AFTER INSERT ON users BEGIN SELECT 1; END;
         INSERT INTO users VALUES (1, 'ann', 'a@x');
         INSERT INTO posts VALUES (1, 1, 'hello');",
    );

    record(
        dir,
        "big",
        &[],
        "CREATE TABLE t(id INTEGER, body TEXT);
         WITH RECURSIVE c(i) AS (SELECT 1 UNION ALL SELECT i+1 FROM c WHERE i<200)
         INSERT INTO t SELECT i, 'body-' || i || '-' || replace(hex(zeroblob(2000)), '00', 'x')
         FROM c;",
    );

    // A row whose payload spills onto the overflow pages: a little over
    // the 4061 bytes a page holds.
    record(
        dir,
        "overflow",
        &[],
        "CREATE TABLE t(id INTEGER, body TEXT);
         INSERT INTO t VALUES (1, replace(hex(zeroblob(5000)), '00', 'x'));",
    );

    record(
        dir,
        "smallpage",
        &["PRAGMA page_size=512;"],
        "CREATE TABLE t(id INTEGER, body TEXT);
         INSERT INTO t VALUES (1, 'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa');",
    );

    record(
        dir,
        "bigpage",
        &["PRAGMA page_size=65536;"],
        "CREATE TABLE t(id INTEGER, body TEXT); INSERT INTO t VALUES (1, 'hello');",
    );

    record(
        dir,
        "utf16le",
        &["PRAGMA encoding='UTF-16le';"],
        "CREATE TABLE t(id INTEGER, name TEXT); INSERT INTO t VALUES (1, 'ZIRCALOY');",
    );

    record(
        dir,
        "utf16be",
        &["PRAGMA encoding='UTF-16be';"],
        "CREATE TABLE t(id INTEGER, name TEXT); INSERT INTO t VALUES (1, 'ZIRCALOY');",
    );

    record(
        dir,
        "withoutrowid",
        &[],
        "CREATE TABLE t(a TEXT, b INTEGER, PRIMARY KEY(a)) WITHOUT ROWID;
         INSERT INTO t VALUES ('one', 1); INSERT INTO t VALUES ('two', 2);",
    );

    record(
        dir,
        "withoutrowid2",
        &[],
        "CREATE TABLE t(a TEXT, b INTEGER, c TEXT, PRIMARY KEY(b, c)) WITHOUT ROWID;
         INSERT INTO t VALUES ('one', 1, 'x'); INSERT INTO t VALUES ('two', 2, 'y');",
    );

    record(
        dir,
        "generated",
        &[],
        "CREATE TABLE t(a INTEGER, b INTEGER GENERATED ALWAYS AS (a*2) VIRTUAL,
                        c INTEGER GENERATED ALWAYS AS (a+1) STORED);
         INSERT INTO t(a) VALUES (3);",
    );

    record(
        dir,
        "quoted",
        &[],
        "CREATE TABLE \"my table\"(\"col one\" TEXT, \"col\"\"two\" INTEGER);
         INSERT INTO \"my table\" VALUES ('v', 2);",
    );

    record(
        dir,
        "autoincrement",
        &[],
        "CREATE TABLE t(id INTEGER PRIMARY KEY AUTOINCREMENT, v TEXT);
         INSERT INTO t(v) VALUES ('a'); INSERT INTO t(v) VALUES ('b');",
    );

    record(dir, "empty", &[], "CREATE TABLE t(id INTEGER);");

    // A virtual table: its rows live in a module, not in the file. SQLite
    // answers it by running the module; the reader has no module to run,
    // so the two have to agree that the rows are not in the file.
    record(
        dir,
        "virtual",
        &[],
        "CREATE VIRTUAL TABLE vt USING fts5(body);
         INSERT INTO vt(body) VALUES ('hello');",
    );

    record(
        dir,
        "wal",
        &["PRAGMA journal_mode=WAL;"],
        "CREATE TABLE t(id INTEGER, v TEXT); INSERT INTO t VALUES (1, 'w');",
    );

    // The rowid alias: which spellings of `INTEGER PRIMARY KEY` make the
    // column an alias for the rowid (the record stores NULL and the rowid
    // is shown) and which do not (the record stores the value).
    record(
        dir,
        "alias",
        &[],
        "CREATE TABLE t(a INTEGER PRIMARY KEY, b INTEGER);
         INSERT INTO t(b) VALUES (7);",
    );

    record(
        dir,
        "alias_int",
        &[],
        "CREATE TABLE t(a INT PRIMARY KEY, b INTEGER);
         INSERT INTO t(b) VALUES (7);",
    );

    record(
        dir,
        "alias_desc",
        &[],
        "CREATE TABLE t(a INTEGER PRIMARY KEY DESC, b INTEGER);
         INSERT INTO t(b) VALUES (7);",
    );

    record(
        dir,
        "alias_quoted",
        &[],
        "CREATE TABLE t(a \"INTEGER\" PRIMARY KEY, b INTEGER);
         INSERT INTO t(b) VALUES (7);",
    );

    record(
        dir,
        "alias2",
        &[],
        "CREATE TABLE t(a INTEGER, b TEXT, PRIMARY KEY(a));
         INSERT INTO t(b) VALUES ('x');",
    );

    record(
        dir,
        "asselect",
        &[],
        "CREATE TABLE t AS SELECT 1 AS x, 'a' AS y;",
    );

    record(
        dir,
        "strict",
        &[],
        "CREATE TABLE t(a INTEGER, b TEXT) STRICT;
         INSERT INTO t VALUES (1, 'x');",
    );

    record(
        dir,
        "alterdefault",
        &[],
        "CREATE TABLE t(a INTEGER);
         ALTER TABLE t ADD COLUMN b TEXT DEFAULT 'dflt';
         INSERT INTO t(a) VALUES (1);",
    );

    record(
        dir,
        "alterdefault2",
        &[],
        "CREATE TABLE t(a INTEGER);
         INSERT INTO t(a) VALUES (1);
         ALTER TABLE t ADD COLUMN b TEXT DEFAULT 'dflt';",
    );

    record(
        dir,
        "types2",
        &[],
        "CREATE TABLE t(
             a VARCHAR(20) NOT NULL DEFAULT 'z',
             b DECIMAL(10,2),
             c,
             d TEXT COLLATE NOCASE,
             e INTEGER,
             f TEXT CHECK(length(f) < 10),
             g TEXT UNIQUE,
             CONSTRAINT pk PRIMARY KEY (a)
         );
         INSERT INTO t(a,b,c,d,e,f,g) VALUES ('x', 1.25, 3, 'D', 4, 'short', 'u');",
    );

    record(
        dir,
        "keywords",
        &[],
        "CREATE TABLE t(\"select\" TEXT, \"from\" INTEGER, \"index\" REAL, [group] BLOB);
         INSERT INTO t VALUES ('s', 1, 2.5, x'0102');",
    );

    record(
        dir,
        "bignumbers",
        &[],
        "CREATE TABLE t(i INTEGER, r REAL);
         INSERT INTO t VALUES (9223372036854775807, 1e308);
         INSERT INTO t VALUES (-9223372036854775808, -0.0);
         INSERT INTO t VALUES (0, 1.0);",
    );

    record(
        dir,
        "rowidgaps",
        &[],
        "CREATE TABLE t(id INTEGER, v TEXT);
         INSERT INTO t(rowid, v) VALUES (1, 'a');
         INSERT INTO t(rowid, v) VALUES (100, 'b');
         INSERT INTO t(rowid, v) VALUES (100000, 'c');",
    );

    record(
        dir,
        "manycolumns",
        &[],
        "CREATE TABLE t(c0 INTEGER, c1 TEXT, c2 REAL, c3 BLOB, c4 TEXT, c5 INTEGER,
                         c6 TEXT, c7 REAL, c8 BLOB, c9 TEXT, c10 INTEGER, c11 TEXT,
                         c12 REAL, c13 BLOB, c14 TEXT, c15 INTEGER, c16 TEXT, c17 REAL,
                         c18 BLOB, c19 TEXT);
         INSERT INTO t(c0, c19) VALUES (1, 'last');",
    );

    record(
        dir,
        "textnewline",
        &[],
        "CREATE TABLE t(id INTEGER, v TEXT);
         INSERT INTO t VALUES (1, 'two
lines');",
    );

    // The same database with the WAL flags set in the header: the file is
    // checkpointed and complete, and the old reader still refuses it.
    {
        let mut bytes = std::fs::read(dir.join("simple.db")).unwrap();
        bytes[18] = 2;
        bytes[19] = 2;
        std::fs::write(dir.join("walflag.db"), &bytes).unwrap();
        let dump = match old_dump(&bytes) {
            Ok(Outcome::Text(t)) => t,
            Ok(_) => b"<not text>".to_vec(),
            Err(e) => format!("<error: {e}>").into_bytes(),
        };
        std::fs::write(dir.join("walflag.dump"), &dump).unwrap();
        let mut facts = String::new();
        for f in old_facts(&bytes) {
            facts.push_str(&f.name);
            facts.push('\t');
            facts.push_str(&f.value);
            facts.push('\n');
        }
        std::fs::write(dir.join("walflag.facts"), facts.as_bytes()).unwrap();
    }

    // A database whose page 2 is garbage: the old reader refuses it.
    let path = dir.join("corrupt.db");
    let _ = std::fs::remove_file(&path);
    {
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute_batch("CREATE TABLE t(id INTEGER, v TEXT); INSERT INTO t VALUES (1, 'x');")
            .unwrap();
    }
    let mut bytes = std::fs::read(&path).unwrap();
    for b in bytes[4096..4100].iter_mut() {
        *b = 0xff;
    }
    std::fs::write(&path, &bytes).unwrap();
    let dump = match old_dump(&bytes) {
        Ok(Outcome::Text(t)) => t,
        Ok(_) => b"<not text>".to_vec(),
        Err(e) => format!("<error: {e}>").into_bytes(),
    };
    std::fs::write(dir.join("corrupt.dump"), &dump).unwrap();
    let mut facts = String::new();
    for f in old_facts(&bytes) {
        facts.push_str(&f.name);
        facts.push('\t');
        facts.push_str(&f.value);
        facts.push('\n');
    }
    std::fs::write(dir.join("corrupt.facts"), facts.as_bytes()).unwrap();
}

/// The one shape the reader leaves out and SQLite answers.
///
/// A virtual table keeps its rows in a module, not in the file: SQLite
/// runs the module to answer a question about it, and the reader has no
/// module to run. So the reader leaves the table out of the dump -- it
/// does not print a table with no rows in it, which would be a lie about
/// the file -- and the shadow tables the module keeps are ordinary
/// tables, and those it reads.
#[test]
fn a_virtual_table_is_left_out_and_its_shadow_tables_are_not() {
    let dir = corpus();
    let bytes = std::fs::read(dir.join("virtual.db")).unwrap();

    // SQLite answers the virtual table: the module is in the library.
    let Outcome::Text(old) = old_dump(&bytes).unwrap() else {
        panic!("the old reader did not answer with text");
    };
    let old = String::from_utf8_lossy(&old).into_owned();
    assert!(old.contains("-- table: vt --"), "{old}");

    // The reader leaves it out, and dumps the shadow tables.
    let Outcome::Text(new) = crate::dump_sqlite_in_memory(&bytes).unwrap() else {
        panic!("the reader did not answer with text");
    };
    let new = String::from_utf8_lossy(&new).into_owned();
    assert!(!new.contains("-- table: vt --"), "{new}");
    for shadow in ["vt_data", "vt_idx", "vt_content", "vt_docsize", "vt_config"] {
        assert!(
            new.contains(&format!("-- table: {shadow} --")),
            "the reader left out {shadow}: {new}"
        );
        // The shadow tables are ordinary tables, and the reader reads them
        // the way SQLite does: the same bytes.
        assert_eq!(
            section(&old, shadow),
            section(&new, shadow),
            "the dump of {shadow} differs"
        );
    }

    // The facts name the tables, and the shadow tables carry the columns
    // their own `CREATE TABLE` statement gives them. The virtual table
    // has no columns to report: they belong to the module.
    let facts = crate::sqlite_facts(&bytes);
    let names: Vec<String> = facts
        .iter()
        .map(|fact| format!("{}\t{}", fact.name, fact.value))
        .collect();
    assert_eq!(
        names,
        vec![
            "table.name\tvt",
            "table.name\tvt_data",
            "table.column\tid",
            "table.column\tblock",
            "table.name\tvt_idx",
            "table.column\tsegid",
            "table.column\tterm",
            "table.column\tpgno",
            "table.name\tvt_content",
            "table.column\tid",
            "table.column\tc0",
            "table.name\tvt_docsize",
            "table.column\tid",
            "table.column\tsz",
            "table.name\tvt_config",
            "table.column\tk",
            "table.column\tv",
        ]
    );
}

/// The dump of a known database, byte for byte.
///
/// The comparison above says that the reader and SQLite agree; this says
/// what the reader answers, in the bytes, so the shape of the output --
/// the sections, the tab, the spelling of a value -- is pinned in the
/// tree and not only in a comparison with another reader.
#[test]
fn the_dump_of_a_known_database_is_the_expected_text() {
    let dir = corpus();
    let bytes = std::fs::read(dir.join("simple.db")).unwrap();
    let Outcome::Text(text) = crate::dump_sqlite_in_memory(&bytes).unwrap() else {
        panic!("the reader did not answer with text");
    };
    assert_eq!(
        String::from_utf8_lossy(&text),
        "-- sqlite_master --\n\
         table\tt\tt\tCREATE TABLE t(id INTEGER, name TEXT)\n\
         -- tables --\n\
         -- table: t --\n\
         id\tname\n\
         1\tZIRCALOY\n"
    );
}

/// The facts of a known database, byte for byte.
///
/// The facts are what a `--filter` asks about, and they carry the names
/// the database gives its columns -- names that live in the schema, not
/// in the pages.
#[test]
fn the_facts_of_a_known_database_are_the_expected_text() {
    let dir = corpus();
    let bytes = std::fs::read(dir.join("simple.db")).unwrap();
    let facts = crate::sqlite_facts(&bytes);
    let names: Vec<String> = facts
        .iter()
        .map(|fact| format!("{}\t{}", fact.name, fact.value))
        .collect();
    assert_eq!(
        names,
        vec!["table.name\tt", "table.column\tid", "table.column\tname"]
    );
}

/// The in-house reader and SQLite itself must answer the same thing.
#[test]
fn the_in_house_reader_agrees_with_sqlite() {
    let dir = corpus();
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name.ends_with(".db"))
        .collect();
    names.sort();
    assert!(!names.is_empty(), "the corpus is empty");
    for name in names {
        let stem = name.trim_end_matches(".db");
        // The virtual table is the one shape where the two cannot agree:
        // SQLite runs the module, the reader has no module to run. The
        // test below says what each of them does.
        if stem == "virtual" {
            continue;
        }
        let bytes = std::fs::read(dir.join(&name)).unwrap();

        // The dump: the same bytes, or the same refusal.
        let old = old_dump(&bytes);
        let new = crate::dump_sqlite_in_memory(&bytes);
        match (&old, &new) {
            (Ok(Outcome::Text(a)), Ok(Outcome::Text(b))) => {
                let tables = crate::sqlite::Db::open(&bytes)
                    .ok()
                    .and_then(|db| db.tables().ok())
                    .unwrap_or_default();
                the_dumps_agree(a, b, &tables, stem);
            }
            (Err(_), Err(_)) => {}
            (a, b) => panic!(
                "{stem}: the old reader says {} and the new one says {}",
                describe(a),
                describe(b)
            ),
        }

        // The facts: the same list.
        let old = old_facts(&bytes);
        let new = crate::sqlite_facts(&bytes);
        assert_eq!(old, new, "the facts of {stem} differ");
    }
}

/// The two dumps have to agree, with one difference this names.
///
/// A generated column's value is computed by SQLite when the row is read:
/// the expression is in the schema, not in the file. The reader reads
/// the file, so it has no answer for that column and the column comes
/// out empty. Every other column has to match, byte for byte.
fn the_dumps_agree(old: &[u8], new: &[u8], tables: &[crate::sqlite::Table], stem: &str) {
    let old_text = String::from_utf8_lossy(old);
    let new_text = String::from_utf8_lossy(new);
    let old_lines: Vec<&str> = old_text.lines().collect();
    let new_lines: Vec<&str> = new_text.lines().collect();
    assert_eq!(
        old_lines.len(),
        new_lines.len(),
        "the dump of {stem} has a different number of lines"
    );
    // Where the dump is: the schema, then the column names of a table,
    // then the rows of that table.
    #[derive(Clone, Copy, PartialEq)]
    enum Where {
        Schema,
        Columns,
        Rows,
    }
    let mut place = Where::Schema;
    let mut computed: Vec<usize> = Vec::new();
    for (number, (old_line, new_line)) in old_lines.iter().zip(new_lines.iter()).enumerate() {
        if let Some(name) = old_line.strip_prefix("-- table: ") {
            let name = name.trim_end_matches(" --");
            let table = tables
                .iter()
                .find(|table| table.name == name)
                .unwrap_or_else(|| {
                    panic!("the dump of {stem} names a table the reader does not have: {name}")
                });
            computed = table
                .columns
                .iter()
                .enumerate()
                .filter(|(_, column)| column.computed)
                .map(|(index, _)| index)
                .collect();
            place = Where::Columns;
            continue;
        }
        match place {
            Where::Schema => {
                assert_eq!(
                    old_line, new_line,
                    "the dump of {stem} differs at line {number}"
                );
            }
            Where::Columns => {
                assert_eq!(
                    old_line, new_line,
                    "the column names of {stem} differ at line {number}"
                );
                place = Where::Rows;
            }
            Where::Rows => {
                let old_fields: Vec<&str> = old_line.split('\t').collect();
                let new_fields: Vec<&str> = new_line.split('\t').collect();
                assert_eq!(
                    old_fields.len(),
                    new_fields.len(),
                    "the row at line {number} of {stem} has a different number of columns"
                );
                for (index, (old_field, new_field)) in
                    old_fields.iter().zip(new_fields.iter()).enumerate()
                {
                    if computed.contains(&index) {
                        assert!(
                            new_field.is_empty(),
                            "the reader answered the computed column {index} of {stem}"
                        );
                    } else {
                        assert_eq!(
                            old_field, new_field,
                            "the dump of {stem} differs at line {number}, column {index}"
                        );
                    }
                }
            }
        }
    }
}
