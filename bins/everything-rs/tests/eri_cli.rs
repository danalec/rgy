//! End-to-end coverage for the `.eri` index file: the binary must be able to
//! read a hand-built ERI2 stream off disk and answer queries against it.
//!
//! The stream is assembled here byte by byte rather than round-tripped through
//! `Index::write_to`, so the on-disk layout is pinned independently of the
//! writer. A round-trip test passes even when both sides share the same
//! misreading of the format; this one does not.

use std::io::Write;
use std::path::PathBuf;
use std::process::Command;

const ERI_MAGIC: &[u8; 4] = b"ERI2";
const ERI_VERSION: u32 = 2;

struct Record<'a> {
    frn: u64,
    parent_frn: u64,
    size: u64,
    attrs: u32,
    is_dir: bool,
    name: &'a str,
    path: &'a str,
}

/// Encode an ERI2 stream. `roots` is the root table: `(volume_ordinal, root)`
/// pairs, which is what makes a multi-volume index survive a save/load cycle.
fn encode_eri(roots: &[(u16, &str)], records: &[Record<'_>]) -> Vec<u8> {
    let mut out = Vec::new();

    out.extend_from_slice(ERI_MAGIC);
    out.extend_from_slice(&ERI_VERSION.to_le_bytes());

    out.extend_from_slice(&(roots.len() as u16).to_le_bytes());
    for (ordinal, root) in roots {
        out.extend_from_slice(&ordinal.to_le_bytes());
        out.extend_from_slice(&(root.len() as u16).to_le_bytes());
        out.extend_from_slice(root.as_bytes());
    }

    out.extend_from_slice(&(records.len() as u64).to_le_bytes());

    for r in records {
        out.extend_from_slice(&r.frn.to_le_bytes());
        out.extend_from_slice(&r.parent_frn.to_le_bytes());
        out.extend_from_slice(&r.size.to_le_bytes());
        out.extend_from_slice(&r.attrs.to_le_bytes());
        out.push(u8::from(r.is_dir));
        out.extend_from_slice(&(r.name.len() as u16).to_le_bytes());
        out.extend_from_slice(r.name.as_bytes());
        out.extend_from_slice(&(r.path.len() as u16).to_le_bytes());
        out.extend_from_slice(r.path.as_bytes());
    }

    out
}

/// A small non-C volume, to prove the stored root is what drives the paths.
const ROOT: &str = "D:\\";

fn sample_records() -> Vec<Record<'static>> {
    vec![
        Record {
            frn: 100,
            parent_frn: 5,
            size: 0,
            attrs: 0x10,
            is_dir: true,
            name: "projects",
            path: "D:\\projects",
        },
        Record {
            frn: 101,
            parent_frn: 100,
            size: 0,
            attrs: 0x10,
            is_dir: true,
            name: "src",
            path: "D:\\projects\\src",
        },
        Record {
            frn: 102,
            parent_frn: 101,
            size: 2048,
            attrs: 0x20,
            is_dir: false,
            name: "main.rs",
            path: "D:\\projects\\src\\main.rs",
        },
        Record {
            frn: 103,
            parent_frn: 101,
            size: 17,
            attrs: 0x20,
            is_dir: false,
            name: "notes.txt",
            path: "D:\\projects\\src\\notes.txt",
        },
    ]
}

fn write_temp_eri(bytes: &[u8], label: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!(
        "everything-rs-{label}-{}-{:?}.eri",
        std::process::id(),
        std::thread::current().id()
    ));
    let mut f = std::fs::File::create(&path).expect("create temp .eri");
    f.write_all(bytes).expect("write temp .eri");
    path
}

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_everything-rs")
}

#[test]
fn load_reports_stats_for_an_eri_file() {
    let path = write_temp_eri(&encode_eri(&[(0, ROOT)], &sample_records()), "load");
    let out = Command::new(bin())
        .arg("load")
        .arg(&path)
        .output()
        .expect("run everything-rs load");

    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "load failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    // Root FRN 5 is seeded by Index::new, plus the four records written out.
    assert!(
        stdout.contains("indexed paths     : 5"),
        "stdout was:\n{stdout}"
    );
    assert!(
        stdout.contains("root              : D:\\"),
        "stdout was:\n{stdout}"
    );
    assert!(
        stdout.contains("directories       : 3"),
        "stdout was:\n{stdout}"
    );
    // Two files, 2048 + 17 bytes.
    assert!(
        stdout.contains("total file bytes  : 2065"),
        "stdout was:\n{stdout}"
    );

    let _ = std::fs::remove_file(&path);
}

#[test]
fn search_queries_the_loaded_index() {
    let path = write_temp_eri(&encode_eri(&[(0, ROOT)], &sample_records()), "search");
    let out = Command::new(bin())
        .arg("search")
        .arg(&path)
        .arg("ext:rs")
        .output()
        .expect("run everything-rs search");

    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "search failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(stdout.lines().count(), 1, "stdout was:\n{stdout}");
    assert_eq!(stdout.trim(), "D:\\projects\\src\\main.rs");

    let _ = std::fs::remove_file(&path);
}

#[test]
fn search_negative_term_excludes_matches() {
    let path = write_temp_eri(&encode_eri(&[(0, ROOT)], &sample_records()), "neg");
    let out = Command::new(bin())
        .arg("search")
        .arg(&path)
        .arg("src !notes")
        .output()
        .expect("run everything-rs search");

    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success());
    // `src` matches the directory and everything under it; `!notes` drops only
    // notes.txt, so the directory itself and main.rs both survive.
    let mut lines: Vec<&str> = stdout.lines().collect();
    lines.sort_unstable();
    assert_eq!(lines, ["D:\\projects\\src", "D:\\projects\\src\\main.rs"]);

    let _ = std::fs::remove_file(&path);
}

#[test]
fn search_type_filter_narrows_to_one_kind() {
    let path = write_temp_eri(&encode_eri(&[(0, ROOT)], &sample_records()), "type");

    for (filter, expected) in [
        (
            "src type:file",
            "D:\\projects\\src\\main.rs\nD:\\projects\\src\\notes.txt",
        ),
        ("src type:dir", "D:\\projects\\src"),
    ] {
        let out = Command::new(bin())
            .arg("search")
            .arg(&path)
            .arg(filter)
            .output()
            .expect("run everything-rs search");

        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(out.status.success());
        let mut lines: Vec<&str> = stdout.lines().collect();
        lines.sort_unstable();
        assert_eq!(lines.join("\n"), expected, "filter {filter:?}");
    }

    let _ = std::fs::remove_file(&path);
}

#[test]
fn load_rejects_a_file_with_the_wrong_magic() {
    let path = write_temp_eri(b"NOPE\x00\x00\x00\x00\x00\x00", "bad-magic");
    let out = Command::new(bin())
        .arg("load")
        .arg(&path)
        .output()
        .expect("run everything-rs load");

    assert!(!out.status.success(), "a bad magic must not exit 0");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("not an ERI file"), "stderr was:\n{stderr}");

    let _ = std::fs::remove_file(&path);
}

#[test]
fn load_rejects_an_unsupported_version() {
    let mut bytes = encode_eri(&[(0, ROOT)], &sample_records());
    bytes[4..8].copy_from_slice(&99u32.to_le_bytes());

    let path = write_temp_eri(&bytes, "bad-ver");
    let out = Command::new(bin())
        .arg("load")
        .arg(&path)
        .output()
        .expect("run everything-rs load");

    assert!(!out.status.success(), "a bad version must not exit 0");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("unsupported index version"),
        "stderr was:\n{stderr}"
    );

    let _ = std::fs::remove_file(&path);
}

/// A multi-volume index must round-trip through `save`/`load`: each volume's
/// root string has to come back, or volume 1's records resolve against volume
/// 0's prefix. The record keys carry the volume ordinal in their high 16 bits.
#[test]
fn load_restores_every_volume_root() {
    // Volume-key helper mirrored here so the fixture does not depend on the
    // library: ordinal in the high 16 bits, raw FRN masked to 48.
    let key = |ordinal: u16, frn: u64| ((ordinal as u64) << 48) | (frn & 0x0000_FFFF_FFFF_FFFF);
    let root_key = |ordinal: u16| key(ordinal, 5);

    let records = vec![
        Record {
            frn: key(0, 100),
            parent_frn: root_key(0),
            size: 1,
            attrs: 0x20,
            is_dir: false,
            name: "a.txt",
            path: "C:\\a.txt",
        },
        // The same raw FRN on the other volume names a different file.
        Record {
            frn: key(1, 100),
            parent_frn: root_key(1),
            size: 2,
            attrs: 0x20,
            is_dir: false,
            name: "b.txt",
            path: "D:\\b.txt",
        },
    ];
    let bytes = encode_eri(&[(0, "C:\\"), (1, "D:\\")], &records);
    let path = write_temp_eri(&bytes, "multi-root");

    let out = Command::new(bin())
        .arg("search")
        .arg(&path)
        .arg(".txt")
        .output()
        .expect("run everything-rs search");

    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "search failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let mut lines: Vec<&str> = stdout.lines().collect();
    lines.sort_unstable();
    assert_eq!(
        lines,
        ["C:\\a.txt", "D:\\b.txt"],
        "both volume roots must survive the load; stdout was:\n{stdout}"
    );

    let _ = std::fs::remove_file(&path);
}

/// An ERI2 stream with no root table is unusable: nothing can resolve against
/// a missing prefix, so it must fail cleanly rather than load an empty shell.
#[test]
fn load_rejects_an_index_with_no_volume_roots() {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(ERI_MAGIC);
    bytes.extend_from_slice(&ERI_VERSION.to_le_bytes());
    bytes.extend_from_slice(&0u16.to_le_bytes()); // root_count = 0
    bytes.extend_from_slice(&0u64.to_le_bytes()); // zero records

    let path = write_temp_eri(&bytes, "no-roots");
    let out = Command::new(bin())
        .arg("load")
        .arg(&path)
        .output()
        .expect("run everything-rs load");

    assert!(!out.status.success(), "a root-less index must not exit 0");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("no volume roots"), "stderr was:\n{stderr}");

    let _ = std::fs::remove_file(&path);
}

/// A truncated file whose record count is nonsense must fail cleanly.
///
/// The count field is read straight off disk, so an unbounded reservation here
/// aborts the process on allocation failure instead of returning an error — a
/// 21-byte file declaring a trillion records used to abort with
/// "memory allocation of 72567767433232 bytes failed".
#[test]
fn load_rejects_a_truncated_file_with_an_absurd_record_count() {
    let mut bytes = encode_eri(&[(0, ROOT)], &[]);
    // Overwrite the (currently zero) count with 1e12, leaving no records after it.
    let count_at = bytes.len() - 8;
    bytes[count_at..].copy_from_slice(&1_000_000_000_000u64.to_le_bytes());

    let path = write_temp_eri(&bytes, "bad-count");
    let out = Command::new(bin())
        .arg("load")
        .arg(&path)
        .output()
        .expect("run everything-rs load");

    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(
        out.status.code(),
        Some(1),
        "load must exit cleanly, not abort; status was {:?}, stderr:\n{stderr}",
        out.status
    );
    assert!(
        stderr.contains("failed to read index"),
        "expected a clean error, stderr was:\n{stderr}"
    );

    let _ = std::fs::remove_file(&path);
}
