// SPDX-License-Identifier: Apache-2.0

//! Format definitions for rgx-tv packs and manifests.
//!
//! The codec is bytes in and bytes out: [`PackHeader::decode`] takes the 32
//! bytes at the front of a pack, [`Manifest::encode`] produces a whole
//! manifest, [`RawManifest::from_bytes`] takes one back. Opening, publishing
//! and the wall clock belong to `rgx-tv`, which is `std` and owns the
//! filesystem.
//!
//! The reader/writer split is the reason [`RawManifest`] exists. Reading the
//! catalog whole and decoding it later is what lets pack reads start while
//! the coordinator is still turning file records into a plan.
//!
//! - **Pack (`.tvp`)**: Fixed-capacity aggregate file containing raw sequential
//!   file slices. Payload bytes are verified by CRC32.
//! - **Manifest (`manifest.tvm`)**: Versioned, generation-stamped catalog mapping
//!   every file path to `(pack_id, offset, length, metadata)`. Published
//!   atomically via write-and-rename.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use crate::error::Error;
use crate::error::Result;

/// Magic bytes for pack files.
pub const PACK_MAGIC: &[u8; 8] = b"RGXTVP01";
/// Magic bytes for manifest file.
pub const MANIFEST_MAGIC: &[u8; 8] = b"RGXTVM01";
/// Format version of a pack.
pub const FORMAT_VERSION: u32 = 1;
/// Format version of a manifest. Separate from [`FORMAT_VERSION`] because the
/// two evolve for different reasons: v3 changed what a file record's
/// `is_binary` flag means, from "the first kilobyte held a NUL" to "the file
/// does", which is the rule the searchers apply. A v2 store ingested a
/// file with a NUL at byte 4,000 as text and answered for it while both
/// reference engines suppressed it, so a v2 catalog is a different question
/// and is refused with its version named.
pub const MANIFEST_VERSION: u32 = 4;
/// Default target pack size (16 MiB).
pub const DEFAULT_PACK_SIZE: u64 = 16 * 1024 * 1024;

/// Pack header written at the start of each `.tvp` file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackHeader {
    pub magic: [u8; 8],
    pub version: u32,
    pub pack_id: u32,
    pub file_count: u32,
    pub content_len: u64,
    pub content_crc32: u32,
}

impl PackHeader {
    pub const SIZE: usize = 32;

    pub fn new(pack_id: u32, file_count: u32, content_len: u64, content_crc32: u32) -> Self {
        Self {
            magic: *PACK_MAGIC,
            version: FORMAT_VERSION,
            pack_id,
            file_count,
            content_len,
            content_crc32,
        }
    }

    /// Append the header to `buf`. A pack is written as `header.extend(&mut buf)`
    /// followed by one `write_all` of the payload.
    pub fn extend(&self, buf: &mut Vec<u8>) {
        buf.extend_from_slice(&self.magic);
        buf.extend_from_slice(&self.version.to_le_bytes());
        buf.extend_from_slice(&self.pack_id.to_le_bytes());
        buf.extend_from_slice(&self.file_count.to_le_bytes());
        buf.extend_from_slice(&self.content_len.to_le_bytes());
        buf.extend_from_slice(&self.content_crc32.to_le_bytes());
    }

    /// The 32 bytes at the front of a pack. A short slice is a truncated pack,
    /// and a wrong magic is a path that does not lead to a store.
    pub fn decode(b: &[u8]) -> Result<Self> {
        if b.len() < Self::SIZE {
            return Err(Error::msg("pack header truncated"));
        }
        let mut c = Cursor::new(b);
        let magic = c.magic()?;
        if &magic != PACK_MAGIC {
            return Err(Error::msg("invalid pack magic"));
        }
        let version = c.u32()?;
        if version != FORMAT_VERSION {
            return Err(Error::msg(format!(
                "unsupported pack version {version} (this build reads {FORMAT_VERSION})"
            )));
        }
        let pack_id = c.u32()?;
        let file_count = c.u32()?;
        let content_len = c.u64()?;
        let content_crc32 = c.u32()?;

        Ok(Self {
            magic,
            version,
            pack_id,
            file_count,
            content_len,
            content_crc32,
        })
    }
}

/// Metadata describing one pack inside the manifest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackRecord {
    pub pack_id: u32,
    pub filename: String,
    pub file_count: u32,
    pub content_len: u64,
    pub content_crc32: u32,
}

/// Where a file's bytes are, without its name.
///
/// The path is the only part of a file record that costs a heap allocation,
/// so a name is decoded when a result needs to print one and not before.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileLoc {
    pub file_id: u64,
    pub pack_id: u32,
    pub offset: u64,
    pub length: u64,
    /// Offset of this record's path bytes within the manifest payload.
    pub path_at: u32,
    pub path_len: u32,
    /// The file's 512-bit n-gram signature, as
    /// [`crate::ngram::build_stored_sig512`] computes it at build time over
    /// the text the searcher will see -- which for a byte-order-marked file
    /// is its decoded form, not the stored bytes.
    ///
    /// It rides along in the locator so the query path can reject a file
    /// without touching its bytes: a subset test against the pattern's own
    /// signature, before the slice is borrowed. `false` is proof the file
    /// cannot hold the needle; `true` proves nothing and costs a search.
    /// All-zero is "unknown" (an archive member, an empty file) and never
    /// rejects; see [`crate::ngram::NgramSig512::contains_query`].
    pub ngram: [u64; 8],
    flags: u8,
}

impl FileLoc {
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.flags & 1 != 0
    }
    #[inline]
    pub fn is_deleted(&self) -> bool {
        self.flags & 2 != 0
    }
    #[inline]
    pub fn is_binary(&self) -> bool {
        self.flags & 4 != 0
    }
}

/// Metadata describing one file inside the manifest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileRecord {
    pub file_id: u64,
    pub rel_path: String,
    pub pack_id: u32,
    pub offset: u64,
    pub length: u64,
    pub mtime_secs: u64,
    pub mtime_nanos: u32,
    pub crc32: u32,
    pub is_empty: bool,
    pub is_deleted: bool,
    pub is_binary: bool,
    /// The file's 512-bit n-gram signature; see [`FileLoc::ngram`].
    pub ngram: [u64; 8],
}

/// A manifest file that has been read and has not been decoded.
///
/// Splitting read from decode is what lets a search start fetching pack bytes
/// while the coordinator is still decoding the file records. The checksum
/// still gates the answer — [`decode`](Self::decode) verifies before it
/// returns — so a corrupt manifest is still an error and never a wrong
/// result; the work done ahead of the check is work nobody sees if it
/// fails.
pub struct RawManifest {
    bytes: Vec<u8>,
    payload_len: usize,
    expected_crc: u32,
    /// Whether [`verify`](RawManifest::verify) has already passed on these
    /// bytes. The checksum is the same bytes both `head()` and `decode()`
    /// need; remembering costs one atomic load and saves one pass over the
    /// whole manifest.
    verified: core::sync::atomic::AtomicBool,
}

/// The part of a manifest that precedes the file records: the generation, the
/// root path and the pack table. A pack reader needs nothing else to know which
/// files to fetch and how much of each to fetch.
#[derive(Debug, Clone)]
pub struct ManifestHead {
    pub generation: u64,
    pub timestamp_secs: u64,
    /// Fingerprint of the walk configuration that decided which files this
    /// store contains. An update that would answer a different question must
    /// refuse rather than tombstone everything the answer excluded.
    pub corpus: u64,
    pub root_path: String,
    pub packs: Vec<PackRecord>,
    /// Offset into the raw buffer where the file records begin.
    pub files_at: usize,
}

/// A bounds-checked reader over a buffer that is already in memory.
///
/// The manifest is read whole and its CRC is verified before anything is
/// decoded, so decoding does not need `Read` error handling per field — it
/// needs an offset that cannot walk off the end. This is that, and it is
/// what `head()` and `decode()` use so the two cannot drift.
struct Cursor<'a> {
    buf: &'a [u8],
    at: usize,
}

impl<'a> Cursor<'a> {
    #[inline]
    fn new(buf: &'a [u8]) -> Self {
        Cursor { buf, at: 0 }
    }

    #[inline]
    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        let end = match self.at.checked_add(n) {
            Some(e) if e <= self.buf.len() => e,
            _ => return Err(eof()),
        };
        let s = &self.buf[self.at..end];
        self.at = end;
        Ok(s)
    }

    #[inline]
    fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }

    #[inline]
    fn u64(&mut self) -> Result<u64> {
        Ok(u64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }

    /// A length-prefixed UTF-8 string. One allocation, and only the bytes
    /// that are actually stored.
    #[inline]
    fn string(&mut self) -> Result<String> {
        let n = self.u32()? as usize;
        let s = self.take(n)?;
        String::from_utf8(s.to_vec()).map_err(Error::msg)
    }

    #[inline]
    fn magic(&mut self) -> Result<[u8; 8]> {
        Ok(self.take(8)?.try_into().unwrap())
    }

    #[inline]
    fn seek(&mut self, at: usize) -> Result<()> {
        if at > self.buf.len() {
            return Err(eof());
        }
        self.at = at;
        Ok(())
    }

    #[inline]
    fn position(&self) -> usize {
        self.at
    }
}

fn eof() -> Error {
    Error::msg("manifest truncated")
}

impl RawManifest {
    /// Takes a `manifest.tvm` that has already been read into memory. No
    /// verification yet — [`verify`](Self::verify) is the gate, and it is
    /// reached through every decode path, so a corrupt catalog is still an
    /// error and never a wrong answer.
    pub fn from_bytes(bytes: Vec<u8>) -> Result<Self> {
        if bytes.len() < MIN_MANIFEST_LEN {
            return Err(Error::msg("manifest file too small"));
        }
        let payload_len = bytes.len() - 4;
        let expected_crc = u32::from_le_bytes(bytes[payload_len..].try_into().unwrap());
        Ok(RawManifest {
            bytes,
            payload_len,
            expected_crc,
            verified: core::sync::atomic::AtomicBool::new(false),
        })
    }

    /// Decodes the header and the pack table, skipping every file record.
    pub fn head(&self) -> Result<ManifestHead> {
        self.verify()?;
        self.head_unchecked()
    }

    /// The header decode itself. [`head`](Self::head) gates it on the
    /// checksum; [`scan`](Self::scan) has already done that.
    fn head_unchecked(&self) -> Result<ManifestHead> {
        let mut c = Cursor::new(&self.bytes[..self.payload_len]);

        if &c.magic()? != MANIFEST_MAGIC {
            return Err(Error::msg("invalid manifest magic"));
        }

        let version = c.u32()?;
        if version != MANIFEST_VERSION {
            return Err(Error::msg(format!(
                "unsupported manifest version {version} (this build reads {MANIFEST_VERSION})"
            )));
        }

        let generation = c.u64()?;
        let timestamp_secs = c.u64()?;
        let corpus = c.u64()?;
        let root_path = c.string()?;

        let pack_count = c.u32()? as usize;
        let mut packs = Vec::with_capacity(pack_count);
        for _ in 0..pack_count {
            let pack_id = c.u32()?;
            let filename = c.string()?;
            let file_count = c.u32()?;
            let content_len = c.u64()?;
            let content_crc32 = c.u32()?;
            packs.push(PackRecord {
                pack_id,
                filename,
                file_count,
                content_len,
                content_crc32,
            });
        }

        Ok(ManifestHead {
            generation,
            timestamp_secs,
            corpus,
            root_path,
            packs,
            files_at: c.position(),
        })
    }

    /// The header plus every file's fixed fields, with no path decoded.
    ///
    /// Same checksum gate as [`decode`](Self::decode), and the same records in
    /// the same order — this is a different projection of the same bytes, not a
    /// second format. A query that prints results decodes the names of the
    /// files that produced them through [`path_of`](Self::path_of).
    pub fn scan(&self) -> Result<(ManifestHead, Vec<FileLoc>)> {
        self.verify()?;
        let head = self.head_unchecked()?;

        let mut c = Cursor::new(&self.bytes[..self.payload_len]);
        c.seek(head.files_at)?;

        let file_count = c.u64()? as usize;
        if self.payload_len > u32::MAX as usize {
            return Err(Error::msg(
                "manifest larger than 4 GiB: a file record's path offset would not fit",
            ));
        }
        let mut locs = Vec::with_capacity(file_count);
        for _ in 0..file_count {
            let hdr = c.take(12)?;
            let file_id = u64::from_le_bytes(hdr[..8].try_into().unwrap());
            let path_len = u32::from_le_bytes(hdr[8..12].try_into().unwrap()) as usize;
            let path_at = c.position();
            c.take(path_len)?;
            let trailer = c.take(101)?;
            let pack_id = u32::from_le_bytes(trailer[..4].try_into().unwrap());
            let offset = u64::from_le_bytes(trailer[4..12].try_into().unwrap());
            let length = u64::from_le_bytes(trailer[12..20].try_into().unwrap());
            let mut ngram = [0u64; 8];
            for (i, w) in ngram.iter_mut().enumerate() {
                let s = 36 + i * 8;
                *w = u64::from_le_bytes(trailer[s..s + 8].try_into().unwrap());
            }
            let flags = trailer[100];
            locs.push(FileLoc {
                file_id,
                pack_id,
                offset,
                length,
                path_at: path_at as u32,
                path_len: path_len as u32,
                ngram,
                flags,
            });
        }
        Ok((head, locs))
    }

    /// One file's path, decoded on demand. Cheap enough to call per result and
    /// never called for a file that produced none.
    pub fn path_of(&self, loc: &FileLoc) -> Result<String> {
        let at = loc.path_at as usize;
        let end = at + loc.path_len as usize;
        if end > self.payload_len {
            return Err(Error::msg("manifest truncated"));
        }
        String::from_utf8(self.bytes[at..end].to_vec()).map_err(Error::msg)
    }

    /// Verifies the checksum and decodes every record. The checksum gate.
    /// Every decode path goes through this one, so a manifest that fails
    /// it cannot be read by any of them.
    pub fn verify(&self) -> Result<()> {
        if self.verified.load(core::sync::atomic::Ordering::Relaxed) {
            return Ok(());
        }
        let computed_crc = crate::compress::crc32::crc32(&self.bytes[..self.payload_len]);
        if self.expected_crc != computed_crc {
            return Err(Error::msg(format!(
                "manifest crc mismatch: expected {:#x}, found {:#x}",
                self.expected_crc, computed_crc
            )));
        }
        self.verified
            .store(true, core::sync::atomic::Ordering::Relaxed);
        Ok(())
    }

    pub fn decode(&self) -> Result<Manifest> {
        self.verify()?;

        let mut c = Cursor::new(&self.bytes[..self.payload_len]);

        if &c.magic()? != MANIFEST_MAGIC {
            return Err(Error::msg("invalid manifest magic"));
        }

        let version = c.u32()?;
        if version != MANIFEST_VERSION {
            return Err(Error::msg(format!(
                "unsupported manifest version {version} (this build reads {MANIFEST_VERSION})"
            )));
        }

        let generation = c.u64()?;
        let timestamp_secs = c.u64()?;
        let corpus = c.u64()?;
        let root_path = c.string()?;

        let pack_count = c.u32()? as usize;
        let mut packs = Vec::with_capacity(pack_count);
        for _ in 0..pack_count {
            let pack_id = c.u32()?;
            let filename = c.string()?;
            let file_count = c.u32()?;
            let content_len = c.u64()?;
            let content_crc32 = c.u32()?;
            packs.push(PackRecord {
                pack_id,
                filename,
                file_count,
                content_len,
                content_crc32,
            });
        }

        let file_count = c.u64()? as usize;
        let mut files = Vec::with_capacity(file_count);
        for _ in 0..file_count {
            let file_id = c.u64()?;
            let rel_path = c.string()?;
            let pack_id = c.u32()?;
            let offset = c.u64()?;
            let length = c.u64()?;
            let mtime_secs = c.u64()?;
            let mtime_nanos = c.u32()?;
            let crc32 = c.u32()?;
            let mut ngram = [0u64; 8];
            for w in ngram.iter_mut() {
                *w = c.u64()?;
            }
            let flags = c.take(1)?[0];

            files.push(FileRecord {
                file_id,
                rel_path,
                pack_id,
                offset,
                length,
                mtime_secs,
                mtime_nanos,
                crc32,
                is_empty: flags & 1 != 0,
                is_deleted: flags & 2 != 0,
                is_binary: flags & 4 != 0,
                ngram,
            });
        }

        Ok(Manifest {
            generation,
            timestamp_secs,
            corpus,
            root_path,
            packs,
            files,
        })
    }
}

/// Smallest byte count that can hold a header, a pack table and a checksum.
const MIN_MANIFEST_LEN: usize = 8 + 4 + 8 + 8 + 8 + 4 + 4 + 8 + 4;

/// The corpus manifest versioned catalog.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Manifest {
    pub generation: u64,
    pub timestamp_secs: u64,
    /// Fingerprint of the walk configuration that decided which files belong to
    /// this store. An update carrying a different one is refused.
    pub corpus: u64,
    pub root_path: String,
    pub packs: Vec<PackRecord>,
    pub files: Vec<FileRecord>,
}

impl Manifest {
    /// A fresh catalog stamped with the time the caller says it is. The wall
    /// clock is `std` and lives in `rgx-tv`; the codec here takes the seconds
    /// it measured and puts them where the format wants them.
    pub fn new_at(root_path: String, generation: u64, timestamp_secs: u64) -> Self {
        Self::with_corpus_at(root_path, generation, 0, timestamp_secs)
    }

    /// The store's definition of which files it contains, hashed. `0` means
    /// "not stated", which is what a store built before v2 carries.
    pub fn with_corpus_at(
        root_path: String,
        generation: u64,
        corpus: u64,
        timestamp_secs: u64,
    ) -> Self {
        Self {
            generation,
            timestamp_secs,
            corpus,
            root_path,
            packs: Vec::new(),
            files: Vec::new(),
        }
    }

    /// The whole `manifest.tvm`, with its terminating CRC32 checksum. What a
    /// caller writes, and what [`RawManifest::from_bytes`] reads back.
    pub fn encode(&self) -> Vec<u8> {
        let mut buf = Vec::new();

        buf.extend_from_slice(MANIFEST_MAGIC);
        buf.extend_from_slice(&MANIFEST_VERSION.to_le_bytes());
        buf.extend_from_slice(&self.generation.to_le_bytes());
        buf.extend_from_slice(&self.timestamp_secs.to_le_bytes());
        buf.extend_from_slice(&self.corpus.to_le_bytes());

        // Root path
        let root_bytes = self.root_path.as_bytes();
        buf.extend_from_slice(&(root_bytes.len() as u32).to_le_bytes());
        buf.extend_from_slice(root_bytes);

        // Packs
        buf.extend_from_slice(&(self.packs.len() as u32).to_le_bytes());
        for p in &self.packs {
            buf.extend_from_slice(&p.pack_id.to_le_bytes());
            let fn_bytes = p.filename.as_bytes();
            buf.extend_from_slice(&(fn_bytes.len() as u32).to_le_bytes());
            buf.extend_from_slice(fn_bytes);
            buf.extend_from_slice(&p.file_count.to_le_bytes());
            buf.extend_from_slice(&p.content_len.to_le_bytes());
            buf.extend_from_slice(&p.content_crc32.to_le_bytes());
        }

        // Files
        buf.extend_from_slice(&(self.files.len() as u64).to_le_bytes());
        for f in &self.files {
            buf.extend_from_slice(&f.file_id.to_le_bytes());
            let path_bytes = f.rel_path.as_bytes();
            buf.extend_from_slice(&(path_bytes.len() as u32).to_le_bytes());
            buf.extend_from_slice(path_bytes);
            buf.extend_from_slice(&f.pack_id.to_le_bytes());
            buf.extend_from_slice(&f.offset.to_le_bytes());
            buf.extend_from_slice(&f.length.to_le_bytes());
            buf.extend_from_slice(&f.mtime_secs.to_le_bytes());
            buf.extend_from_slice(&f.mtime_nanos.to_le_bytes());
            buf.extend_from_slice(&f.crc32.to_le_bytes());
            for w in f.ngram {
                buf.extend_from_slice(&w.to_le_bytes());
            }
            let flags: u8 = (if f.is_empty { 1 } else { 0 })
                | (if f.is_deleted { 2 } else { 0 })
                | (if f.is_binary { 4 } else { 0 });
            buf.extend_from_slice(&[flags]);
        }

        // Checksum of the entire manifest buffer
        let checksum = crate::compress::crc32::crc32(&buf);
        buf.extend_from_slice(&checksum.to_le_bytes());

        buf
    }
}
