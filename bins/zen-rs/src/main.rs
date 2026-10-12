// SPDX-License-Identifier: Apache-2.0

//! zen-rs — an NTFS file indexer built on the MFT and the USN journal.
//!
//! Grounding for the approach came from the decompiled Zen 1.4.1.1032
//! binary, specifically its NTFS monitor state machine, which drives
//! `FSCTL_READ_USN_JOURNAL` (0x900bb) with a 40-byte input and a 64 KiB output
//! buffer and tails journal deltas via overlapped reads.

// Old bin, new homes (IMPLEMENTATION_PLAN.md first-PR sequence #3: "old bins
// compile against them unchanged" — behaviour unchanged, imports only).
use discovery_windows::{indexer, ntfs, volume};
use name_core::index;
use name_core::live::Live;
use search_ui as ui;

use indexer::Indexer;
use volume::Volume;

/// Build the fsearch name index the UI's [`name_core::name_query::Searcher`]
/// runs on, from the zen path index (the `.eri` / MFT view).
///
/// The name index is keyed by parent, not by NTFS FRN, so this assigns
/// synthetic ids along the path tree and hands [`indexer::listings_from_rows`]
/// a consistent parent map. The UI and `searchctl` then share one matcher.
fn live_from_zen(index: &index::Index) -> Live {
    use name_core::index::ROOT_FRN;
    use name_core::name_index;
    use std::collections::HashMap;

    let mut id_of: HashMap<String, u64> = HashMap::new();
    let mut next_id = ROOT_FRN + 1;
    let mut rows: Vec<indexer::NameRow> = Vec::new();

    let root_str = index.root().trim_end_matches(['\\', '/']).to_string();
    id_of.insert(root_str.clone(), ROOT_FRN);

    let mut entries: Vec<(String, bool, u64)> = index
        .iter_all_paths()
        .map(|(frn, path)| {
            let meta = index.meta_of(frn);
            (
                path.trim_end_matches(['\\', '/']).to_string(),
                meta.is_dir,
                meta.size,
            )
        })
        .collect();
    // Deterministic id assignment, so two runs of the same index agree.
    entries.sort();

    for (trimmed, is_dir, size) in entries {
        let (parent_key, name) = match trimmed.rfind(['\\', '/']) {
            Some(i) => {
                let p = trimmed[..i].to_string();
                (
                    if p.is_empty() || p == root_str {
                        root_str.clone()
                    } else {
                        p
                    },
                    trimmed[i + 1..].to_string(),
                )
            }
            None => (root_str.clone(), trimmed.clone()),
        };
        let parent_frn = *id_of.entry(parent_key).or_insert_with(|| {
            let id = next_id;
            next_id += 1;
            id
        });
        let frn = *id_of.entry(trimmed).or_insert_with(|| {
            let id = next_id;
            next_id += 1;
            id
        });
        rows.push(indexer::NameRow {
            frn,
            parent_frn,
            name: name.into_bytes(),
            is_dir,
            link: false,
            size,
            mtime: 0,
        });
    }

    let listings = indexer::listings_from_rows(&rows, ROOT_FRN);
    Live::new(name_index::Index::build(listings, 0, 0, b""))
}

fn usage() -> ! {
    eprintln!(
        "zen-rs <command> [args]\n\n\
         commands:\n  \
           index <volume|file>      inspect volume or .eri file statistics\n  \
           index-all               index every fixed NTFS volume into one index\n  \
           volumes                 list the fixed NTFS volumes index-all covers\n  \
           save <volume> <file>     scan volume and save index to .eri file\n  \
           load <file>              load an .eri file and print statistics\n  \
           ui <volume|file>         interactive search UI (enter opens the result)\n  \
           list <volume|file>       list indexed paths\n  \
           search <volume|file> <q> search paths using query filters\n  \
           search-all <q>           search every fixed NTFS volume\n  \
           tail <volume>            walk the MFT, then apply pending USN journal records\n  \
           record <volume> <frn>    hexdump and parse a single raw MFT record\n\n\
         volume examples: C:\\  D:  \\\\?\\E:\\\n\
         query examples:  \"ext:rs dir:src main\"  \"size:>100mb !draft\"  \"*.rs | *.ts\"\n\n\
         in the ui, Esc or ctrl-c quits, enter prints the selected path,\n\
         ctrl-u clears the query, ctrl-w deletes the previous word"
    );
    std::process::exit(2);
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() {
        usage();
    }

    let result = match args[0].as_str() {
        "index" if args.len() == 2 => cmd_index(&args[1]),
        "index-all" if args.len() == 1 => cmd_index_all(),
        "volumes" if args.len() == 1 => cmd_volumes(),
        "search-all" if args.len() == 2 => cmd_search_all(&args[1]),
        "save" if args.len() == 3 => cmd_save(&args[1], &args[2]),
        "load" if args.len() == 2 => cmd_index(&args[1]),
        "ui" if args.len() == 2 => cmd_ui(&args[1]),
        "record" if args.len() == 3 => cmd_record(&args[1], args[2].parse().unwrap_or(5)),
        "list" if args.len() == 2 => cmd_list(&args[1], None),
        "search" if args.len() == 3 => cmd_search(&args[1], &args[2]),
        "tail" if args.len() == 2 => cmd_tail(&args[1]),
        _ => usage(),
    };

    if let Err(e) = result {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}

fn scan(volume_path: &str) -> Result<(Volume, index::Index, indexer::IndexStats), String> {
    let volume = Volume::open(volume_path).map_err(|e| e.to_string())?;
    eprintln!("opened {}", volume.path);

    let indexer = Indexer::new(&volume, 0);
    let mut index = index::Index::new(&volume.root);
    let start = std::time::Instant::now();

    let stats = indexer
        .scan_all(&mut index, &mut |frn, edges, limit| {
            eprint!("\rscanning MFT... record {frn}/{limit}, {edges} named entries   ");
        })
        .map_err(|e| e.to_string())?;

    eprintln!(
        "\rscanned {} records in {:.1?} — {} files, {} directories, {} live records        ",
        stats.records_read,
        start.elapsed(),
        stats.files,
        stats.directories,
        stats.records_in_use
    );

    Ok((volume, index, stats))
}

/// Scan every fixed NTFS volume into one combined index.
///
/// Each volume gets its own ordinal so its file reference numbers cannot
/// collide with another volume's — both volumes have root at FRN 5, and the
/// same FRN names unrelated files on each.
fn scan_all_volumes() -> Result<(index::Index, indexer::IndexStats), String> {
    let roots = volume::list_fixed_ntfs_roots();
    if roots.is_empty() {
        return Err("no fixed NTFS volume found".to_string());
    }

    let mut index = index::Index::new(&roots[0]);
    let mut totals = indexer::IndexStats::default();

    for (ordinal, root) in roots.iter().enumerate() {
        let ordinal = ordinal as u16;
        let volume = Volume::open(root).map_err(|e| format!("{root}: {e}"))?;
        eprintln!(
            "[{ordinal}] opened {} (record size {})",
            volume.root, volume.record_size
        );

        let indexer = Indexer::new(&volume, ordinal);
        let start = std::time::Instant::now();
        let stats = indexer
            .scan_all(&mut index, &mut |frn, edges, limit| {
                eprint!("\r[{ordinal}] record {frn}/{limit}, {edges} entries   ");
            })
            .map_err(|e| format!("{root}: {e}"))?;
        eprintln!(
            "\r[{ordinal}] scanned {} records in {:.1?} — {} files, {} directories        ",
            stats.records_read,
            start.elapsed(),
            stats.files,
            stats.directories
        );

        totals.records_read += stats.records_read;
        totals.records_in_use += stats.records_in_use;
        totals.names_seen += stats.names_seen;
        totals.files += stats.files;
        totals.directories += stats.directories;
        totals.resolved_paths += stats.resolved_paths;
        totals.orphan_edges += stats.orphan_edges;
        totals.cycle_edges += stats.cycle_edges;
        totals.read_errors += stats.read_errors;
        totals.reparse_points += stats.reparse_points;
    }

    Ok((index, totals))
}

/// Hexdump one raw MFT record and its parsed attributes.
fn cmd_record(volume_path: &str, frn: u64) -> Result<(), String> {
    let volume = Volume::open(volume_path).map_err(|e| e.to_string())?;
    let Some(rec) = volume.read_file_record(frn).map_err(|e| e.to_string())? else {
        eprintln!("record {frn}: not in use");
        return Ok(());
    };

    println!("record {frn}: {} bytes", rec.len());
    for (i, chunk) in rec.chunks(16).enumerate().take(24) {
        let hex: Vec<String> = chunk.iter().map(|b| format!("{b:02x}")).collect();
        let ascii: String = chunk
            .iter()
            .map(|b| {
                if (0x20..0x7f).contains(b) {
                    *b as char
                } else {
                    '.'
                }
            })
            .collect();
        println!("{:04x}  {:<47}  {}", i * 16, hex.join(" "), ascii);
    }

    let hdr = ntfs::parse_file_record(&rec);
    if let Some(h) = hdr {
        println!(
            "\nattrs_offset=0x{:x} bytes_in_use={} flags=0x{:x} in_use={} is_dir={}",
            h.attrs_offset,
            h.bytes_in_use,
            h.flags,
            h.flags & ntfs::MFT_RECORD_IN_USE != 0,
            h.flags & ntfs::MFT_RECORD_IS_DIRECTORY != 0
        );
    }

    let names = ntfs::parse_file_names(&rec, frn);
    println!("\nparsed {} $FILE_NAME value(s):", names.len());
    for n in &names {
        println!(
            "  name={:?} namespace={} parent=0x{:x} size={}",
            n.name, n.namespace, n.parent_frn, n.file_size
        );
    }
    Ok(())
}

fn load_or_scan(source: &str) -> Result<index::Index, String> {
    if source.ends_with(".eri") || std::path::Path::new(source).is_file() {
        let start = std::time::Instant::now();
        let file = std::fs::File::open(source)
            .map_err(|e| format!("cannot open index file {source}: {e}"))?;
        let mut reader = std::io::BufReader::new(file);
        let idx = index::Index::read_from(&mut reader)
            .map_err(|e| format!("failed to read index {source}: {e}"))?;
        eprintln!(
            "loaded index from {source} in {:.2?} ({} entries)",
            start.elapsed(),
            idx.len()
        );
        Ok(idx)
    } else {
        let (_volume, idx, _stats) = scan(source)?;
        Ok(idx)
    }
}

/// Write `index` to `file_path` so that a crash mid-write can never leave a
/// partial `.eri` where a complete one used to be.
///
/// The stream goes to a sibling `.tmp` file, is flushed all the way to disk, and
/// only then replaces the target. `rename` within a volume is atomic, so a reader
/// sees either the previous complete index or the new complete one — never a
/// half-written one. An interrupted scan leaves a stray `.tmp`, which is ignored
/// by `load_or_scan` and overwritten by the next `save`.
fn write_index_atomically(index: &index::Index, file_path: &str) -> std::io::Result<()> {
    let tmp_path = format!("{file_path}.tmp");

    let result = (|| -> std::io::Result<()> {
        let file = std::fs::File::create(&tmp_path)?;
        let mut writer = std::io::BufWriter::new(file);
        index.write_to(&mut writer)?;
        let file = writer.into_inner().map_err(|e| e.into_error())?;
        // Durability before the rename: otherwise a crash could leave the rename
        // durable but the contents not, which is the same broken state.
        file.sync_all()?;
        Ok(())
    })();

    if let Err(e) = result {
        let _ = std::fs::remove_file(&tmp_path);
        return Err(e);
    }

    if let Err(e) = std::fs::rename(&tmp_path, file_path) {
        let _ = std::fs::remove_file(&tmp_path);
        return Err(e);
    }

    Ok(())
}

fn cmd_save(volume_path: &str, file_path: &str) -> Result<(), String> {
    let (_volume, index, _stats) = scan(volume_path)?;
    let start = std::time::Instant::now();

    write_index_atomically(&index, file_path)
        .map_err(|e| format!("failed to write index {file_path}: {e}"))?;

    eprintln!(
        "saved {} paths to {file_path} in {:.2?}",
        index.len(),
        start.elapsed()
    );
    Ok(())
}

fn cmd_index(target: &str) -> Result<(), String> {
    if target.ends_with(".eri") || std::path::Path::new(target).is_file() {
        let index = load_or_scan(target)?;
        println!("index file        : {}", target);
        println!("root              : {}", index.root());
        println!("indexed paths     : {}", index.len());
        println!("directories       : {}", index.directory_count());
        println!(
            "indexed files     : {}",
            index.len().saturating_sub(index.directory_count())
        );
        println!("total file bytes  : {}", index.total_bytes());
        return Ok(());
    }

    let (_volume, index, stats) = scan(target)?;

    println!("volume            : {}", target);
    println!("records read      : {}", stats.records_read);
    println!("records in use    : {}", stats.records_in_use);
    println!("$FILE_NAME values : {}", stats.names_seen);
    println!("files             : {}", stats.files);
    println!("directories       : {}", stats.directories);
    println!("reparse points    : {}", stats.reparse_points);
    // `entry_count` excludes the synthetic root; `len` includes it.
    println!("canonical paths   : {}", index.entry_count());
    println!("alias paths       : {}", index.alias_count());
    println!("  resolved        : {}", stats.resolved_paths);
    println!("  orphan edges    : {}", stats.orphan_edges);
    println!("  cycle edges     : {}", stats.cycle_edges);
    println!("read errors       : {}", stats.read_errors);
    println!("total file bytes  : {}", index.total_bytes());
    Ok(())
}

/// List the fixed NTFS volumes a multi-volume scan would cover.
fn cmd_volumes() -> Result<(), String> {
    let roots = volume::list_fixed_ntfs_roots();
    if roots.is_empty() {
        println!("no fixed NTFS volume found");
        return Ok(());
    }
    for (ordinal, root) in roots.iter().enumerate() {
        match Volume::open(root) {
            Ok(v) => {
                let raw = v.volume_data;
                println!(
                    "[{ordinal}] {}  serial=0x{:08X}  record_size={}  records={}",
                    v.root, v.serial, v.record_size, v.record_count
                );
                if let Some(d) = raw {
                    println!(
                        "      raw: rec_size={} cluster={} cpf={:x} mft_valid={}",
                        d.bytes_per_file_record_segment,
                        d.bytes_per_cluster(),
                        d.clusters_per_file_record_segment,
                        d.mft_valid_data_length
                    );
                }
            }
            Err(e) => println!("[{ordinal}] {root}  (unavailable: {e})"),
        }
    }
    Ok(())
}

/// Scan every fixed NTFS volume and print the combined index statistics.
fn cmd_index_all() -> Result<(), String> {
    let (index, stats) = scan_all_volumes()?;

    println!("volumes           : all fixed NTFS");
    println!("records read      : {}", stats.records_read);
    println!("records in use    : {}", stats.records_in_use);
    println!("$FILE_NAME values : {}", stats.names_seen);
    println!("files             : {}", stats.files);
    println!("directories       : {}", stats.directories);
    println!("reparse points    : {}", stats.reparse_points);
    println!("canonical paths   : {}", index.entry_count());
    println!("alias paths       : {}", index.alias_count());
    println!("  resolved        : {}", stats.resolved_paths);
    println!("  orphan edges    : {}", stats.orphan_edges);
    println!("  cycle edges     : {}", stats.cycle_edges);
    println!("read errors       : {}", stats.read_errors);
    println!("total file bytes  : {}", index.total_bytes());
    Ok(())
}

/// Print one warning per query token the parser could not honour: a `re:`
/// pattern that failed to compile, or a `type:` naming no known kind. Either
/// way the alternative matches nothing, so without this the user just sees
/// silent zero (or, for `type:`, a silently wrong result set).
fn warn_query_errors(query: &index::OrQuery) {
    for alt in &query.alternatives {
        for e in &alt.regex_errors {
            eprintln!("warning: {e}");
        }
        for e in &alt.type_errors {
            eprintln!("warning: {e}");
        }
    }
}

/// Search the combined index of every fixed NTFS volume.
fn cmd_search_all(query_str: &str) -> Result<(), String> {
    let (index, _stats) = scan_all_volumes()?;
    let query = index::OrQuery::parse(query_str);
    warn_query_errors(&query);
    let results = index.search(&query);
    for (_, path) in &results {
        println!("{path}");
    }
    eprintln!("\n{} path(s) matched", results.len());
    Ok(())
}

/// Interactive full-screen search over an index.
fn cmd_ui(source: &str) -> Result<(), String> {
    let index = load_or_scan(source)?;
    let root = index.root().to_string();
    let indexed = index.entry_count();
    let live = live_from_zen(&index);
    ui::run(live, root, indexed)
}

fn cmd_search(source: &str, query_str: &str) -> Result<(), String> {
    let index = load_or_scan(source)?;
    let query = index::OrQuery::parse(query_str);
    warn_query_errors(&query);

    let start = std::time::Instant::now();
    let mut matches = index.search(&query);
    let elapsed = start.elapsed();

    matches.sort_by(|a, b| a.1.cmp(b.1));

    for (_frn, path) in &matches {
        println!("{path}");
    }
    eprintln!("\n{} path(s) matched in {:.2?}", matches.len(), elapsed);
    Ok(())
}

fn cmd_list(source: &str, filter: Option<&str>) -> Result<(), String> {
    let index = load_or_scan(source)?;

    let needle = filter.map(|f| f.to_lowercase());
    // Canonical paths and aliases, so a hard link is listed under both names.
    let mut rows: Vec<&str> = index.iter_all_paths().map(|(_, p)| p).collect();
    rows.sort_unstable();

    let mut shown = 0usize;
    for path in rows {
        if needle
            .as_ref()
            .is_some_and(|n| !path.to_lowercase().contains(n))
        {
            continue;
        }
        println!("{path}");
        shown += 1;
    }
    eprintln!("\n{} path(s) shown", shown);
    Ok(())
}

fn cmd_tail(volume_path: &str) -> Result<(), String> {
    let (volume, mut index, _stats) = scan(volume_path)?;

    let indexer = Indexer::new(&volume, 0);
    // Start below the journal's live window; `tail_usn` clamps it to
    // `lowest_valid_usn` and reports `start_usn_was_stale`, which is how "from
    // the oldest record still available" is expressed. `StartUsn` is i64 in
    // READ_USN_JOURNAL_DATA, not a 32-bit field.
    let mut start_usn: i64 = 0;
    let before = index.len();

    let mut rounds = 0u32;
    let mut known_journal_id: u64 = 0;
    loop {
        let outcome = indexer
            .tail_usn(&mut index, &mut start_usn, &mut known_journal_id, 100_000)
            .map_err(|e| e.to_string())?;
        rounds += 1;

        eprintln!(
            "round {rounds}: applied {} change(s), index now {} entries, next USN 0x{:X}",
            outcome.applied,
            index.len(),
            outcome.next_start_usn
        );
        eprintln!(
            "  journal id=0x{:X} lowest=0x{:X} next=0x{:X}",
            outcome.journal_id, outcome.journal_lowest_valid_usn, outcome.journal_next_usn
        );
        if outcome.journal_id_changed {
            eprintln!("  journal recreated: restarted from the new journal's first USN");
        }
        if outcome.start_usn_was_stale {
            eprintln!("  resume point was outside the live window; moved to lowest_valid_usn");
        }
        if outcome.journal_wrapped {
            eprintln!("  journal wrapped: history between the resume point and here is gone");
        }

        if outcome.applied == 0 || rounds >= 10 {
            break;
        }
    }

    println!("paths before tail : {before}");
    println!("paths after tail  : {}", index.len());
    println!("net change        : {}", index.len() as i64 - before as i64);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use index::{Index, NodeKind};
    use std::io::Read;

    fn sample_index() -> Index {
        let mut idx = Index::new("D:\\");
        idx.insert(100, 5, "projects", NodeKind::Directory, 0, 0x10);
        idx.insert(101, 100, "notes.txt", NodeKind::File, 17, 0x20);
        idx
    }

    fn temp_path(label: &str) -> String {
        let p = std::env::temp_dir().join(format!(
            "zen-rs-{label}-{}-{:?}.eri",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_file(&p);
        p.to_string_lossy().into_owned()
    }

    #[test]
    fn atomic_write_produces_a_readable_index_and_leaves_no_tmp() {
        let path = temp_path("atomic");
        let idx = sample_index();

        write_index_atomically(&idx, &path).expect("write");

        assert!(std::path::Path::new(&path).exists(), "target must exist");
        assert!(
            !std::path::Path::new(&format!("{path}.tmp")).exists(),
            "no temp file may survive a successful write"
        );

        let mut f = std::fs::File::open(&path).expect("reopen");
        let loaded = Index::read_from(&mut f).expect("read back");

        assert_eq!(loaded.root(), "D:\\");
        assert_eq!(loaded.len(), idx.len());
        assert_eq!(loaded.path_of(101), Some("D:\\projects\\notes.txt"));

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn atomic_write_replaces_an_existing_index() {
        let path = temp_path("replace");

        let mut first = Index::new("D:\\");
        first.insert(100, 5, "old", NodeKind::File, 1, 0x20);
        write_index_atomically(&first, &path).expect("first write");

        let second = sample_index();
        write_index_atomically(&second, &path).expect("second write");

        let mut f = std::fs::File::open(&path).expect("reopen");
        let loaded = Index::read_from(&mut f).expect("read back");

        assert_eq!(loaded.len(), second.len());
        assert_eq!(loaded.path_of(101), Some("D:\\projects\\notes.txt"));

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn atomic_write_failure_leaves_no_partial_target() {
        // A path inside a directory that does not exist: the create of the temp
        // file fails, so nothing should be left behind and the error must
        // surface rather than panic.
        let path = format!(
            "{}\\no-such-dir-{}\\out.eri",
            std::env::temp_dir().display(),
            std::process::id()
        );

        let err = write_index_atomically(&sample_index(), &path)
            .expect_err("writing into a missing directory must fail");
        assert!(
            !std::path::Path::new(&path).exists(),
            "no target may appear on failure"
        );
        assert!(!err.to_string().is_empty());
    }

    #[test]
    fn a_truncated_eri_reads_back_as_an_error_not_a_partial_index() {
        // The property that makes atomic writing worth doing: a half-written
        // stream must be rejected outright rather than decoded into a partial
        // index that looks plausible.
        let path = temp_path("truncated");
        let idx = sample_index();
        write_index_atomically(&idx, &path).expect("write");

        let full = std::fs::read(&path).expect("read bytes");
        let cut = &full[..full.len() / 2];
        std::fs::write(&path, cut).expect("truncate");

        let mut f = std::fs::File::open(&path).expect("reopen");
        let mut cursor = std::io::BufReader::new(&mut f);
        let mut sink = Vec::new();
        let _ = cursor.read_to_end(&mut sink);
        let err = Index::read_from(&mut cursor).expect_err("truncated stream must fail");
        assert!(
            err.to_string().to_lowercase().contains("buffer")
                || err.to_string().to_lowercase().contains("read"),
            "unexpected error: {err}"
        );

        let _ = std::fs::remove_file(&path);
    }
}
