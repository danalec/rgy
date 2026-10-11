//! Incremental update and generation publishing engine for rgx-tv.
//!
//! Compares the live filesystem state against the stored generation, appending
//! modified/added files into new packs and publishing an updated generation atomically.

use alloc::string::{String, ToString};
use alloc::vec::Vec;
use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::Path;
use std::time::{Instant, UNIX_EPOCH};

use crate::compress::crc32::crc32;

use super::builder::BuildConfig;
use super::format::{FileRecord, Manifest, PackHeader, PackRecord};

/// Statistics reported after an incremental update.
#[derive(Debug, Clone)]
pub struct UpdateStats {
    pub files_added: usize,
    pub files_modified: usize,
    pub files_deleted: usize,
    pub new_packs_written: usize,
    pub new_bytes_written: u64,
    pub new_generation: u64,
    pub elapsed_ms: f64,
}

/// Incrementally updates `store_dir` against `root_dir`.
pub fn update_store(
    root_dir: &Path,
    store_dir: &Path,
    config: &BuildConfig,
) -> io::Result<UpdateStats> {
    let t0 = Instant::now();

    let mut manifest = super::format::load_manifest(store_dir)?;
    let canon_root = root_dir.canonicalize()?;
    let canon_store = store_dir.canonicalize()?;

    // An update that walks a different tree than the build did does not find
    // the files the build excluded, and concludes they were deleted — a
    // tombstone per excluded file, published atomically, not recoverable
    // by rebuilding the packs. Refuse instead.
    let want = config.fingerprint();
    if manifest.corpus != want {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "rgx-tv update: this store was built with a different file selection \
                 (store corpus {:#018x}, this invocation {:#018x}). An update must walk \
                 the same tree the build walked; pass the same --hidden/--no-ignore/\
                 --max-depth/-g/--follow, or rebuild the store from scratch.",
                manifest.corpus, want
            ),
        ));
    }

    // Map existing active files by relative path
    let mut file_map: HashMap<String, usize> = HashMap::new();
    for (i, f) in manifest.files.iter().enumerate() {
        if !f.is_deleted {
            file_map.insert(f.rel_path.clone(), i);
        }
    }

    let mut seen_paths: HashSet<String> = HashSet::new();

    let walker = super::builder::walk_root(&canon_root, &canon_store, config)?;

    let mut next_pack_id = manifest.packs.iter().map(|p| p.pack_id).max().unwrap_or(0) + 1;

    let mut new_pack_files: Vec<FileRecord> = Vec::new();
    let mut new_pack_payload: Vec<u8> = Vec::new();

    let mut files_added = 0usize;
    let mut files_modified = 0usize;
    let mut files_deleted = 0usize;
    let mut new_bytes = 0u64;
    let mut file_id_counter = manifest.files.iter().map(|f| f.file_id).max().unwrap_or(0) + 1;

    let flush_pack = |pack_id: u32,
                      payload: &mut Vec<u8>,
                      files: &mut Vec<FileRecord>,
                      manifest: &mut Manifest,
                      store_dir: &Path|
     -> io::Result<()> {
        if files.is_empty() && payload.is_empty() {
            return Ok(());
        }
        let filename = format!("pack_{:05}.tvp", pack_id);
        let pack_path = store_dir.join(&filename);

        let content_len = payload.len() as u64;
        let content_crc = crc32(payload);

        let header = PackHeader::new(pack_id, files.len() as u32, content_len, content_crc);

        let mut head = Vec::with_capacity(PackHeader::SIZE);
        header.extend(&mut head);
        let mut file = File::create(&pack_path)?;
        file.write_all(&head)?;
        file.write_all(payload)?;
        file.sync_all()?;

        manifest.packs.push(PackRecord {
            pack_id,
            filename,
            file_count: files.len() as u32,
            content_len,
            content_crc32: content_crc,
        });

        manifest.files.append(files);
        payload.clear();
        Ok(())
    };

    for result in walker {
        let entry = match result {
            Ok(e) => e,
            Err(_) => continue,
        };

        let path = entry.path();
        if !path.is_file() {
            continue;
        }

        // The store is not ingested into itself: the builder prunes the
        // store's directory.

        let metadata = match entry.metadata() {
            Ok(m) => m,
            Err(_) => continue,
        };

        let file_len = metadata.len();
        let (mtime_secs, mtime_nanos) = metadata
            .modified()
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| (d.as_secs(), d.subsec_nanos()))
            .unwrap_or((0, 0));

        let rel_path = match path.strip_prefix(&canon_root) {
            Ok(p) => p.to_string_lossy().to_string(),
            Err(_) => path.to_string_lossy().to_string(),
        };

        seen_paths.insert(rel_path.clone());

        let is_existing = file_map.get(&rel_path).copied();

        if let Some(existing_idx) = is_existing {
            let existing = &manifest.files[existing_idx];
            // If length and mtime match, file is unchanged
            if existing.length == file_len
                && existing.mtime_secs == mtime_secs
                && existing.mtime_nanos == mtime_nanos
            {
                continue;
            }
            // Otherwise, mark old record as deleted (tombstone)
            manifest.files[existing_idx].is_deleted = true;
            files_modified += 1;
        } else {
            files_added += 1;
        }

        if file_len == 0 {
            manifest.files.push(FileRecord {
                file_id: file_id_counter,
                rel_path,
                pack_id: 0,
                offset: 0,
                length: 0,
                mtime_secs,
                mtime_nanos,
                crc32: 0,
                is_empty: true,
                is_deleted: false,
                is_binary: false,
            });
            file_id_counter += 1;
            continue;
        }

        let content = match fs::read(path) {
            Ok(c) => c,
            Err(_) => continue,
        };

        let file_crc = crc32(&content);
        // The same rule the build applies, for the same reason: an update
        // that decides differently from the build it is updating is the
        // two-axes defect this crate keeps running into. A UTF-16LE file
        // -- half NUL bytes from its first one -- used to come back
        // binary from an update and text from a build of the same tree,
        // and a file whose NUL sat at byte 4,000 came back the other way.
        let is_binary = crate::encoding::is_binary(&content);

        if !new_pack_payload.is_empty()
            && (new_pack_payload.len() as u64 + file_len > config.target_pack_size)
        {
            flush_pack(
                next_pack_id,
                &mut new_pack_payload,
                &mut new_pack_files,
                &mut manifest,
                store_dir,
            )?;
            next_pack_id += 1;
        }

        let offset = new_pack_payload.len() as u64;
        new_pack_payload.extend_from_slice(&content);

        new_pack_files.push(FileRecord {
            file_id: file_id_counter,
            rel_path,
            pack_id: next_pack_id,
            offset,
            length: file_len,
            mtime_secs,
            mtime_nanos,
            crc32: file_crc,
            is_empty: false,
            is_deleted: false,
            is_binary,
        });

        file_id_counter += 1;
        new_bytes += file_len;
    }

    // Detect deleted files
    for (rel_path, &idx) in &file_map {
        if !seen_paths.contains(rel_path) {
            manifest.files[idx].is_deleted = true;
            files_deleted += 1;
        }
    }

    let packs_before = manifest.packs.len();
    flush_pack(
        next_pack_id,
        &mut new_pack_payload,
        &mut new_pack_files,
        &mut manifest,
        store_dir,
    )?;
    let new_packs_written = manifest.packs.len() - packs_before;

    // Increment generation
    manifest.generation += 1;
    manifest.timestamp_secs = std::time::SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    // Atomically publish updated generation
    super::format::publish_atomic(&manifest, store_dir)?;

    let elapsed = t0.elapsed().as_secs_f64() * 1000.0;

    Ok(UpdateStats {
        files_added,
        files_modified,
        files_deleted,
        new_packs_written,
        new_bytes_written: new_bytes,
        new_generation: manifest.generation,
        elapsed_ms: elapsed,
    })
}
