// SPDX-License-Identifier: Apache-2.0

//! zen-rs - an NTFS file indexer built on the MFT and the USN journal.
//!
//! The monitor drives `FSCTL_READ_USN_JOURNAL` (0x900bb) with a 40-byte
//! input and a 64 KiB output buffer, and tails journal deltas via overlapped
//! reads -- the shape the Win32 USN journal interface documents.
//!
//! One on-disk index format: the name index blob (FSIDX015). A scan builds
//! it from MFT rows; `save` persists it with the
//! root stamp beside it; `load`, `ui`, `search` and `list` reopen it.

// Windows only. This is the NTFS MFT/USN indexer and `discovery-windows` is
// empty off-Windows (see its `lib.rs`: the `windows` crate family does not
// cross-compile). Everything that reads a volume is gated; what is left
// below is the index writing and its tests, which are platform-independent.
#[cfg(windows)]
use rgy_discovery_windows::{indexer, ntfs, seam, volume};
#[cfg(windows)]
use rgy_name_core::live::Live;
#[cfg(windows)]
use rgy_name_core::name_index::Index;
#[cfg(windows)]
use rgy_name_core::name_query::{Alternatives, Searcher, merge_hits};
#[cfg(windows)]
use rgy_name_core::persist;
#[cfg(windows)]
use rgy_search_ui as ui;

// One allocator story (plan 4.4/4.10): mimalloc for the general heap, matching
// searchctl/searchd -- it measured 5-12% on parallel walks on Windows.
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

#[cfg(windows)]
use indexer::Indexer;
#[cfg(windows)]
use volume::Volume;

/// Seconds since the epoch -- the index's timestamp base, same as searchd.
#[cfg(windows)]
fn now_secs() -> u32 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as u32)
        .unwrap_or(0)
}

/// The display path for one hit: the root plus the index-relative path, the
/// same composition `searchd` puts on the wire.
#[cfg(windows)]
fn full_path(root: &str, rel: &str) -> String {
    format!("{root}{}", rel.trim_start_matches(['/', '\\']))
}

/// The home directory `name_query` resolves `~` against.
#[cfg(windows)]
fn query_home() -> String {
    std::env::var("USERPROFILE").unwrap_or_default()
}

/// Sorted display paths matching `query_str`, deduped across `|` alternatives.
#[cfg(windows)]
fn search_paths(live: &Live, root: &str, query_str: &str) -> Result<Vec<String>, String> {
    let home = query_home();
    let alts = Alternatives::parse(query_str, &home)?;
    for w in &alts.warnings {
        eprintln!("warning: {w}");
    }

    let searcher = Searcher { live };
    // The merge is the searcher's, and the identity it keys on is the one
    // that matters: keying it on `Hit::idx` keeps one entry and drops the
    // rest, because every live entry shares the `u32::MAX` sentinel. Each
    // alternative keeps the limit it was parsed with.
    let mut all = Vec::new();
    for alt in &alts.alts {
        all.extend(searcher.search(alt));
    }
    // The union of the alternatives, not their sum: an entry two of them
    // match is one result. `usize::MAX` is the same "no cut" the CLI uses.
    let hits = merge_hits(all, usize::MAX);

    let mut out = Vec::new();
    let mut buf = Vec::new();
    for h in &hits {
        let full = match &h.over {
            // An overlay hit already carries its own path: it is an entry
            // the base index does not have, so reading it back from the
            // base would index `parent[u32::MAX]` -- out of bounds -- and
            // panic. The path is the index's internal form (`/`); the
            // answer is the platform's.
            Some(path) => {
                rgy_engine::console::spell(path).replace('/', std::path::MAIN_SEPARATOR_STR)
            }
            None => {
                live.base.path(h.idx as usize, &mut buf);
                let rel = rgy_engine::console::spell(&buf);
                buf.clear();
                full_path(root, &rel)
            }
        };
        out.push(full);
    }
    out.sort_unstable();
    Ok(out)
}

/// Build the name index for one open volume: MFT rows grouped into the DFS
/// listings the index consumes. Paths inside are root-relative; the caller
/// rejoins the root for display (the UI does it too).
#[cfg(windows)]
fn build_index(volume: &Volume) -> Result<(Index, indexer::IndexStats), String> {
    // The escape hatch that forces the record path instead of the streamed
    // one is read here, not inside the backend: the backend is a library,
    // and a library that reads the environment is a library that cannot be
    // tested.
    let streamed = std::env::var_os("ERS_USE_RECORDS").is_none();
    let indexer = Indexer::with_records(volume, streamed);
    let mut last = std::time::Instant::now();
    let (rows, stats) = indexer
        .scan_rows(&mut |done, total, _limit| {
            if last.elapsed().as_secs() >= 2 {
                last = std::time::Instant::now();
                eprint!("\rrecords {done} / {total}");
                let _ = std::io::Write::flush(&mut std::io::stderr());
            }
        })
        .map_err(|e| format!("failed to scan {}: {e:?}", volume.root))?;
    eprintln!();
    let listings = indexer::listings_from_rows(&rows, ntfs::ROOT_FRN);
    Ok((Index::build(listings, 0, now_secs(), b""), stats))
}

/// A saved name index or a volume to scan: what `ui`/`search`/`list`/`index`
/// run against. A saved blob's root comes from its sidecar, so a file named
/// by the user still serves the paths it was built with.
#[cfg(windows)]
fn load_or_build(source: &str) -> Result<(Index, String), String> {
    if std::path::Path::new(source).is_file() {
        persist::load_saved(std::path::Path::new(source)).map_err(|e| format!("{source}: {e}"))
    } else {
        let volume = Volume::open(source).map_err(|e| e.to_string())?;
        let (idx, _) = build_index(&volume)?;
        Ok((idx, volume.root.clone()))
    }
}

/// Print the usage and leave. `code` is the exit status: 0 when the
/// user asked for help, 2 for the rg convention's "bad invocation".
///
/// The stream follows the status: the help is an answer, and an answer
/// goes to stdout. The complaint about a bad invocation goes to
/// stderr, because that one is not an answer.
fn usage(code: i32) -> ! {
    if code == 0 {
        println!("{}", usage_text());
    } else {
        eprintln!("{}", usage_text());
    }
    std::process::exit(code);
}

/// The usage text.
///
/// The text is the same on every platform, and it says what the tool
/// is where it cannot run: a program that hides its own commands on
/// the platforms that have none is a program nobody can read.
fn usage_text() -> String {
    format!(
        "zen-rs <command> [args]\n\n\
         commands:\n  \
           index <volume|file>      inspect volume or saved index statistics\n  \
           index-all               index every fixed NTFS volume\n  \
           volumes                 list the fixed NTFS volumes index-all covers\n  \
           save <volume> <file>     scan volume and save the name index\n  \
           load <file>              load a saved name index and print statistics\n  \
           ui <volume|file>         interactive search UI (enter opens the result)\n  \
           list <volume|file>       list indexed paths\n  \
           search <volume|file> <q> search paths using query filters\n  \
           search-all <q>           search every fixed NTFS volume\n  \
           tail <volume>            walk the MFT, then apply pending USN journal records\n  \
           record <volume> <frn>    hexdump and parse a single raw MFT record\n\n\
         options:\n  \
           -h, --help              this text\n  \
           -V, --version           the version\n\n\
         volume examples: C:\\  D:  \\\\?\\E:\\\n\
         query examples:  \"ext:rs dir:src main\"  \"size:>100mb !draft\"  \"*.rs | *.ts\"\n\n\
         in the ui, Esc or ctrl-c quits, enter prints the selected path,\n\
         ctrl-u clears the query, ctrl-w deletes the previous word\n{note}",
        note = if cfg!(windows) {
            ""
        } else {
            "\nthe indexer is Windows-only: on the other platforms searchctl is the search.\n"
        },
    )
}

/// The version, as the answer to a question.
fn version() -> ! {
    println!("zen-rs {}", env!("CARGO_PKG_VERSION"));
    std::process::exit(0);
}

/// On any platform but Windows there is no NTFS MFT and no USN journal to
/// read, so there is no indexer to run. Saying so is better than pretending:
/// `searchctl` is the name/content search on those platforms, and this bin
/// exists for the Windows fast path.
#[cfg(not(windows))]
fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("-h") | Some("--help") | Some("help") => usage(0),
        Some("-V") | Some("--version") => version(),
        _ => {
            eprintln!(
                "zen-rs: the NTFS MFT/USN indexer is Windows-only; use searchctl on this platform"
            );
            std::process::exit(2);
        }
    }
}

#[cfg(windows)]
fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() {
        usage(2);
    }
    match args[0].as_str() {
        "-h" | "--help" | "help" => usage(0),
        "-V" | "--version" => version(),
        _ => {}
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
        _ => usage(2),
    };

    if let Err(e) = result {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}

/// Hexdump one raw MFT record and its parsed attributes.
#[cfg(windows)]
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

/// Scan a volume and save its name index, with the root stamp beside it.
#[cfg(windows)]
fn cmd_save(volume_path: &str, file: &str) -> Result<(), String> {
    let volume = Volume::open(volume_path).map_err(|e| e.to_string())?;
    let (mut idx, _stats) = build_index(&volume)?;
    let saved = persist::save(
        &mut idx,
        std::path::Path::new(&volume.root),
        std::path::Path::new(file),
    )
    .map_err(|e| format!("failed to save {file}: {e}"))?;
    println!(
        "saved {} ({} entries) for root {}",
        saved.index.display(),
        idx.n,
        volume.root
    );
    Ok(())
}

/// Statistics for a volume scan or a saved index.
#[cfg(windows)]
fn cmd_index(target: &str) -> Result<(), String> {
    if std::path::Path::new(target).is_file() {
        let (idx, root) = load_or_build(target)?;
        println!("root            : {root}");
        println!("index           : {target}");
        println!("entries         : {}", idx.n);
        println!("directories     : {}", idx.d);
        println!("names           : {}", idx.u);
        println!("index bytes     : {}", idx.bytes());
        return Ok(());
    }
    let volume = Volume::open(target).map_err(|e| e.to_string())?;
    let (idx, stats) = build_index(&volume)?;
    println!("root            : {}", volume.root);
    println!("records read    : {}", stats.records_read);
    println!("records in use  : {}", stats.records_in_use);
    println!("$FILE_NAME vals : {}", stats.names_seen);
    println!("files           : {}", stats.files);
    println!("directories     : {}", stats.directories);
    println!("reparse points  : {}", stats.reparse_points);
    println!("read errors     : {}", stats.read_errors);
    println!("entries         : {}", idx.n);
    println!("names           : {}", idx.u);
    Ok(())
}

/// Scan every fixed NTFS volume -- one name index per volume, the blob's
/// single root being the volume root.
#[cfg(windows)]
fn cmd_index_all() -> Result<(), String> {
    let roots = volume::list_fixed_ntfs_roots();
    if roots.is_empty() {
        return Err("no fixed NTFS volume found".into());
    }
    for root in roots {
        let volume = Volume::open(&root).map_err(|e| e.to_string())?;
        let (idx, stats) = build_index(&volume)?;
        println!("\n{root}");
        println!("  records read    : {}", stats.records_read);
        println!("  records in use  : {}", stats.records_in_use);
        println!("  $FILE_NAME vals : {}", stats.names_seen);
        println!("  files           : {}", stats.files);
        println!("  directories     : {}", stats.directories);
        println!("  reparse points  : {}", stats.reparse_points);
        println!("  read errors     : {}", stats.read_errors);
        println!("  entries         : {}", idx.n);
        println!("  names           : {}", idx.u);
    }
    Ok(())
}

/// List the fixed NTFS volumes a multi-volume scan would cover.
#[cfg(windows)]
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

/// Interactive full-screen search over a volume or a saved index.
#[cfg(windows)]
fn cmd_ui(source: &str) -> Result<(), String> {
    let (idx, root) = load_or_build(source)?;
    let indexed = idx.n;
    ui::run(Live::with_root(idx, root.as_bytes()), root, indexed)
}

#[cfg(windows)]
fn cmd_search(source: &str, query_str: &str) -> Result<(), String> {
    let (idx, root) = load_or_build(source)?;
    let start = std::time::Instant::now();
    let matches = search_paths(&Live::with_root(idx, root.as_bytes()), &root, query_str)?;
    let elapsed = start.elapsed();

    let mut dropped = 0usize;
    for path in &matches {
        if rgy_pick::carries_line(path.as_bytes()) {
            println!("{path}");
        } else {
            dropped += 1;
        }
    }
    if dropped > 0 {
        eprintln!("{}", rgy_pick::uncarryable_paths(dropped));
    }
    eprintln!("\n{} path(s) matched in {:.2?}", matches.len(), elapsed);
    Ok(())
}

#[cfg(windows)]
fn cmd_search_all(query_str: &str) -> Result<(), String> {
    let roots = volume::list_fixed_ntfs_roots();
    if roots.is_empty() {
        return Err("no fixed NTFS volume found".into());
    }
    for root in roots {
        eprintln!("\n# {root}");
        let (idx, r) = load_or_build(&root)?;
        let mut dropped = 0usize;
        for path in search_paths(&Live::with_root(idx, r.as_bytes()), &r, query_str)? {
            if rgy_pick::carries_line(path.as_bytes()) {
                println!("{path}");
            } else {
                dropped += 1;
            }
        }
        if dropped > 0 {
            eprintln!("{}", rgy_pick::uncarryable_paths(dropped));
        }
    }
    Ok(())
}

#[cfg(windows)]
fn cmd_list(source: &str, filter: Option<&str>) -> Result<(), String> {
    let (idx, root) = load_or_build(source)?;

    let needle = filter.map(|f| f.to_lowercase());
    // Every entry, so a hard link is listed under both names.
    let mut rows: Vec<String> = Vec::with_capacity(idx.n.saturating_sub(1));
    let mut buf = Vec::new();
    for i in 1..idx.n {
        idx.path(i, &mut buf);
        rows.push(full_path(&root, &rgy_engine::console::spell(&buf)));
        buf.clear();
    }
    rows.sort_unstable();

    let mut shown = 0usize;
    let mut dropped = 0usize;
    for path in rows {
        if needle
            .as_ref()
            .is_some_and(|n| !path.to_lowercase().contains(n))
        {
            continue;
        }
        if rgy_pick::carries_line(path.as_bytes()) {
            println!("{path}");
            shown += 1;
        } else {
            dropped += 1;
        }
    }
    if dropped > 0 {
        eprintln!("{}", rgy_pick::uncarryable_paths(dropped));
    }
    eprintln!("\n{} path(s) shown", shown);
    Ok(())
}

/// Walk the MFT, then feed pending USN journal records to the live index --
/// the same feed `searchd` holds open, run to its end here as a demo.
#[cfg(windows)]
fn cmd_tail(volume_path: &str) -> Result<(), String> {
    use rgy_discovery::Watch;

    let volume = Volume::open(volume_path).map_err(|e| e.to_string())?;
    let (idx, _stats) = build_index(&volume)?;
    let before = idx.n;
    let mut live = Live::with_root(idx, volume_path.as_bytes());
    let mut watch = seam::UsnWatch::new(Indexer::new(&volume));

    let mut rounds = 0u32;
    let mut total = 0usize;
    while rounds < 10 {
        rounds += 1;
        match watch.next_batch()? {
            rgy_discovery::ChangeBatch::Events(events) => {
                let n = events.len();
                if n == 0 {
                    break;
                }
                live.apply_events(&events);
                total += n;
                eprintln!("round {rounds}: applied {n} change(s)");
            }
            rgy_discovery::ChangeBatch::Rescan => {
                eprintln!("round {rounds}: history lost at the root - a full rescan is due");
                break;
            }
        }
    }

    println!("entries before tail : {before}");
    println!("changes applied     : {total}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use rgy_discovery::walk::{KIND_DIR, KIND_FILE, Listing, NONE, RawEnt};
    // The save/load round trip runs off Windows too, so the index format and
    // its persistence are imported here rather than through the gated
    // imports at the top of the file.
    use rgy_name_core::name_index::Index;
    use rgy_name_core::persist;

    /// root (listing 0) has "projects" (dir, listing 1) with "notes.txt".
    /// Built from `rgy_discovery::walk` listings rather than an MFT scan, so the
    /// save/load round trip tests run off Windows too.
    fn sample_index() -> Index {
        let mut root_names = Vec::new();
        let off = root_names.len() as u32;
        root_names.extend_from_slice(b"projects");
        let root = Listing {
            id: 0,
            names: root_names,
            ents: vec![RawEnt {
                name_off: off,
                name_len: 8,
                kind: KIND_DIR,
                size: 0,
                mtime: 1,
                child: 1,
            }],
        };
        let mut dir_names = Vec::new();
        let off = dir_names.len() as u32;
        dir_names.extend_from_slice(b"notes.txt");
        let dir = Listing {
            id: 1,
            names: dir_names,
            ents: vec![RawEnt {
                name_off: off,
                name_len: 9,
                kind: KIND_FILE,
                size: 17,
                mtime: 2,
                child: NONE,
            }],
        };
        Index::build(vec![root, dir], 0, 0, b"")
    }

    fn temp_path(label: &str) -> String {
        let p = std::env::temp_dir().join(format!(
            "zen-rs-{label}-{}-{:?}.idx",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_file(&p);
        p.to_string_lossy().into_owned()
    }

    #[test]
    fn a_saved_index_round_trips_with_its_root() {
        let dir = std::env::temp_dir().join(format!("zen-rs-root-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("root dir");
        let root = std::fs::canonicalize(&dir).expect("canonical root");

        let path = temp_path("roundtrip");
        let mut idx = sample_index();
        persist::save(&mut idx, &root, std::path::Path::new(&path)).expect("save");

        assert!(std::path::Path::new(&path).exists(), "target must exist");
        assert!(
            !std::path::Path::new(&path).with_extension("tmp").exists(),
            "no temp file may survive a successful write"
        );

        let (loaded, saved_root) =
            persist::load_saved(std::path::Path::new(&path)).expect("read back");
        assert_eq!(saved_root, root.to_string_lossy().into_owned());
        assert_eq!(loaded.n, idx.n);

        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(format!("{path}.meta"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_truncated_index_fails_to_load() {
        // The property that makes atomic writing worth doing: a half-written
        // blob must be rejected outright rather than decoded into a partial
        // index that looks plausible.
        let path = temp_path("truncated");
        let mut idx = sample_index();
        persist::save(
            &mut idx,
            std::env::temp_dir().as_path(),
            std::path::Path::new(&path),
        )
        .expect("save");

        let full = std::fs::read(&path).expect("read bytes");
        std::fs::write(&path, &full[..full.len() / 2]).expect("truncate");

        assert!(
            persist::load_saved(std::path::Path::new(&path)).is_err(),
            "a truncated blob must not load"
        );

        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(format!("{path}.meta"));
    }

    #[test]
    fn a_failed_save_leaves_no_partial_target() {
        // The temp file's parent is a plain file, so the write must fail and
        // leave nothing behind. Built with real separators: the Windows
        // spelling (`\` joins) once made this one file *name* at the temp
        // root on Unix, where the test only passed because an unprivileged
        // user cannot create files at `/` -- run as root (a VM suite) the
        // write succeeded and the assertion was about permissions the test
        // never meant to use.
        let dir = std::env::temp_dir().join(format!("zen-rs-not-a-dir-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::write(&dir, b"file, not a directory").expect("make the blocker");
        let path = dir.join("out.idx");

        let mut idx = sample_index();
        let err = persist::save(&mut idx, std::path::Path::new("C:\\"), &path)
            .expect_err("saving under a plain file must fail");
        assert!(!path.exists(), "no target may appear on failure");
        assert!(!err.to_string().is_empty());

        let _ = std::fs::remove_file(&dir);
    }

    /// Every live change is answered, and the answer is the overlay's own
    /// path.
    ///
    /// The overlay holds the entries the base does not have, and they all
    /// carry the same `Hit::idx` -- the `u32::MAX` sentinel. A merge keyed
    /// on that key keeps one of them and drops the rest, and a path read
    /// back from the base would index `parent[u32::MAX]`, out of bounds,
    /// and panic. The daemon's answer is where this shows first; this is
    /// the same shape in the CLI.
    #[cfg(windows)]
    #[test]
    fn every_live_change_is_answered() {
        use rgy_discovery::{PortableSource, Source};
        use rgy_name_core::live::Live;

        let dir = std::env::temp_dir().join(format!(
            "zen-rs-live-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).expect("the root dir");
        std::fs::write(dir.join("hello.txt"), b"hello\n").expect("the first file");
        let root_bytes = dir.as_os_str().as_encoded_bytes().to_vec();

        // The index as it was before the files appeared.
        let source = PortableSource::new(root_bytes.clone(), 2);
        let mut live = Live::with_root(
            Index::build(source.enumerate().expect("the walk"), 0, 1, b"/"),
            &root_bytes,
        );

        // Two files appear, and the feed says the folder whose contents
        // changed (what a directory-granular feed says).
        std::fs::write(dir.join("late-one.txt"), b"one\n").expect("the first arrival");
        std::fs::write(dir.join("late-two.txt"), b"two\n").expect("the second arrival");
        live.apply_events(&[rgy_discovery::UsnRecord {
            file_reference: 0,
            parent_reference: 0,
            usn: 0,
            reason: 0,
            file_attributes: rgy_discovery::FILE_ATTRIBUTE_DIRECTORY,
            name: rgy_engine::console::spell(&root_bytes),
        }]);

        // The display root carries its trailing separator: the index's own
        // paths are relative to it.
        let root = format!("{}\\", dir.to_string_lossy());
        let paths = super::search_paths(&live, &root, "late").expect("the search");
        assert!(
            paths.iter().any(|p| p.ends_with("late-one.txt")),
            "the first new file must be answered; paths={paths:?}"
        );
        assert!(
            paths.iter().any(|p| p.ends_with("late-two.txt")),
            "the second new file must be answered too, not collapsed into the first; paths={paths:?}"
        );
    }
}
