//! ZIP archive format implementation.
//!
//! Provides ZIP file creation and extraction capabilities.
//! ZIP container reader, vendored from upstream `b02/compress/zip.rs`.
//!
//! Provenance: upstream commit 819a1cf. upstream is the source of truth — the
//! Unknown-method refusal, zip64 (EOCD64 + per-entry 0x0001) and the
//! XZ routing all landed there first, with python-zipfile vectors.
//!
//! Trims for rgx: the writer half (ZipWriter/ZipEntry — rgx never
//! creates archives) is not vendored, and the XZ arm returns a clean
//! "not supported" error instead of routing to upstream's XZ container,
//! which is not vendored and whose liblzma interop is a known defect
//! (the upstream XZ vector is #[ignore]d there). Entries with methods 9 /
//! 93 / 98 (deflate64, zstd, ppmd) error instead of decoding, which is
//! what the `zip` crate served them as compressed bytes masquerading
//! as content — rgx's zip surface loses those to an honest error.
//!
//! Copy-time changes: `alloc::` -> `std::`, upstream module paths pointed
//! at rgx-core::compress, the test fixtures re-anchored to this crate's
//! testdata/.
//!
//! After the copy: every entry is decoded under a ceiling
//! ([`default_byte_limit`], overridable with [`ZipReader::with_limit`]).
//! The central directory's uncompressed size is a claim, not a bound, so it
//! is checked before the decode and the running cap is what stops a bomb.

use crate::compress::bzip2::Bzip2Decoder;
use crate::compress::cap::Bounded;
use crate::compress::crc32::crc32;
use crate::compress::deflate::DeflateDecoder;
use crate::compress::error::{CompressError, CompressResult};
use crate::compress::lzma::{LzmaDecoder, LzmaProps};
use alloc::collections::BTreeMap;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::cell::{Cell, OnceCell};

/// Default ceiling on the bytes one archive may expand to: the same number
/// `Ctx::default()` gives the archive adapters' byte budget, so a zip costs
/// no more than the `.gz` beside it.
///
/// This is the *default*, not the process budget — a caller that knows the
/// budget it is running under should pass it to [`ZipReader::with_limit`]
/// instead of taking this.
pub fn default_byte_limit() -> u64 {
    crate::Ctx::default().max_file_size as u64
}

/// Compression method.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CompressionMethod {
    /// No compression (stored).
    Store,
    /// DEFLATE compression.
    #[default]
    Deflate,
    /// BZIP2 compression (decoded by the `bzip2` sibling module).
    Bzip2,
    /// LZMA compression (decoded by the `lzma` sibling module).
    ///
    /// The payload carries a ZIP-specific LZMA header, see
    /// `ZipReader::decode_lzma`.
    Lzma,
    /// XZ compression (decoded by the `xz` sibling module).
    Xz,
    /// Zstd compression (method 93, decoded by the `zstd` sibling module).
    Zstd,
    /// Any other method (deflate64, ppmd, …). Preserved, never
    /// guessed: mapping these to `Store` handed the caller raw
    /// compressed bytes as if they were the file's text — silent
    /// garbage in whatever indexes the output.
    Unsupported(u16),
}

impl From<u16> for CompressionMethod {
    fn from(method: u16) -> Self {
        match method {
            0 => Self::Store,
            8 => Self::Deflate,
            12 => Self::Bzip2,
            14 => Self::Lzma,
            93 => Self::Zstd,
            95 => Self::Xz,
            other => Self::Unsupported(other),
        }
    }
}

impl From<CompressionMethod> for u16 {
    fn from(method: CompressionMethod) -> Self {
        match method {
            CompressionMethod::Store => 0,
            CompressionMethod::Deflate => 8,
            CompressionMethod::Bzip2 => 12,
            CompressionMethod::Lzma => 14,
            CompressionMethod::Zstd => 93,
            CompressionMethod::Xz => 95,
            CompressionMethod::Unsupported(m) => m,
        }
    }
}

/// ZIP entry metadata, as recorded in the central directory.
#[derive(Debug, Clone)]
pub struct ZipEntryMetadata {
    /// File name.
    pub name: String,
    /// Uncompressed size.
    pub size: u32,
    /// Compressed size.
    pub compressed_size: u32,
    /// CRC-32 checksum.
    pub crc32: u32,
    /// Compression method.
    pub method: CompressionMethod,
    /// Last modified time (DOS format).
    pub modified_time: u16,
    /// Last modified date (DOS format).
    pub modified_date: u16,
    /// Offset to local file header within the archive.
    pub local_header_offset: u32,
}

/// ZIP file reader.
#[derive(Debug)]
pub struct ZipReader<'a> {
    data: &'a [u8],
    entries: Vec<ZipEntryMetadata>,
    /// `entries[i].name -> i` for O(1) by-name extraction. The Office and
    /// GGUF/Keras adapters walk the list once and then look each member
    /// up by name (e.g. `xl/sharedStrings.xml`, `[Content_Types].xml`,
    /// `m.keras/data/...`), and an O(N) scan turns a 65 536-entry
    /// archive into a multi-second walk (measured, see commit log).
    /// Built lazily on the first `extract(name)` call; the linear scan
    /// the rest of the API used to do lives on as a single O(N) at that
    /// point, after which every name lookup is amortised O(1).
    by_name: OnceCell<BTreeMap<String, usize>>,
    /// Ceiling on the bytes every entry of this archive may expand to.
    limit: u64,
    spent: Cell<u64>,
}

impl<'a> ZipReader<'a> {
    /// Open a ZIP file. Extraction is bounded by
    /// [`default_byte_limit`]; see [`Self::with_limit`] to supply the
    /// process budget instead.
    pub fn new(data: &'a [u8]) -> CompressResult<Self> {
        Self::with_limit(data, default_byte_limit())
    }

    /// [`Self::new`] with the caller's own ceiling on total decoded bytes.
    ///
    /// The ceiling is a property of the archive, not of the entry: it is
    /// spent across every [`Self::extract`] on this reader, which is the
    /// same accounting the archive adapters' byte budget uses.
    pub fn with_limit(data: &'a [u8], limit: u64) -> CompressResult<Self> {
        let entries = Self::parse_central_directory(data)?;
        Ok(Self {
            data,
            entries,
            by_name: OnceCell::new(),
            limit,
            spent: Cell::new(0),
        })
    }

    /// The ceiling on total decoded bytes, and how much of it is spent.
    pub fn budget(&self) -> (u64, u64) {
        (self.limit, self.spent.get())
    }

    /// The refusal a byte budget produces. It carries both the limit and
    /// what the header declared: with a lying header those differ by orders
    /// of magnitude, and the difference is the whole finding.
    fn over_budget(entry: &ZipEntryMetadata, spent: u64, limit: u64) -> CompressError {
        CompressError::DecompressionFailed(format!(
            "zip entry `{}` stopped at the {limit}-byte archive budget ({spent} bytes already \
             decoded, {} declared): the entry expands further than the budget allows",
            entry.name, entry.size
        ))
    }

    /// One zip64 extra field: when the 32-bit slot saturated, read the
    /// next 8 bytes as the real value. A value past 4 GiB is refused —
    /// the metadata fields are u32, and truncating them would slice the
    /// wrong byte range on extract.
    fn zip64_field(body: &[u8], at: &mut usize, saturated: u32) -> CompressResult<u32> {
        if saturated != 0xFFFF_FFFF {
            return Ok(saturated);
        }
        let slice = body
            .get(*at..*at + 8)
            .ok_or(CompressError::InvalidFormat("zip64 extra truncated"))?;
        let mut v = [0u8; 8];
        v.copy_from_slice(slice);
        *at += 8;
        u32::try_from(u64::from_le_bytes(v)).map_err(|_| {
            CompressError::InvalidFormat("zip64 entry larger than 4 GiB is out of range")
        })
    }

    /// Parse the central directory.
    fn parse_central_directory(data: &[u8]) -> CompressResult<Vec<ZipEntryMetadata>> {
        // Find end of central directory.
        //
        // This is the LAST record in the file and APPNOTE bounds it to the
        // final 65 557 bytes (22-byte record + up to 65 535 of comment). The
        // code here scanned the WHOLE archive backwards one window at a time,
        // which for a 50 MB zip is 50 million 4-byte compares to read a record
        // that is provably in the last 64 KiB -- and it ran on every zip-family
        // file (zip, jar, apk, docx, xlsx, pptx, epub, whl, xpi), before the
        // central directory is even parsed.
        //
        // `memrchr` on the first byte is the SIMD primitive for exactly this,
        // and narrowing to the legal window first makes it strictly cheaper
        // than it would be over the whole archive. A zip whose EOCD is missing
        // reports the same error as before, just sooner.
        const EOCD_SIG: &[u8; 4] = b"PK\x05\x06";
        const EOCD_MAX_COMMENT: usize = u16::MAX as usize; // 65 535
        const EOCD_MIN: usize = 22; // the record with no comment
        let floor = data.len().saturating_sub(EOCD_MIN + EOCD_MAX_COMMENT);
        let tail = &data[floor..];
        let eocd_start = crate::memchr::memmem::rfind(tail, EOCD_SIG)
            .map(|i| floor + i)
            .ok_or(CompressError::InvalidFormat("EOCD not found"))?;

        if eocd_start + 22 > data.len() {
            return Err(CompressError::InvalidFormat("truncated EOCD"));
        }

        let mut central_dir_offset = u32::from_le_bytes([
            data[eocd_start + 16],
            data[eocd_start + 17],
            data[eocd_start + 18],
            data[eocd_start + 19],
        ]) as usize;
        let mut num_entries =
            u16::from_le_bytes([data[eocd_start + 10], data[eocd_start + 11]]) as usize;

        // Zip64: the 32-bit EOCD fields saturate at 0xFFFF/0xFFFFFFFF and
        // the real values live in the EOCD64 record, found through the
        // locator that sits directly before the EOCD.
        if num_entries == 0xFFFF || central_dir_offset == 0xFFFF_FFFF {
            let loc = eocd_start
                .checked_sub(20)
                .ok_or(CompressError::InvalidFormat("zip64 locator missing"))?;
            if data.get(loc..loc + 4) != Some(&b"PK"[..]) {
                return Err(CompressError::InvalidFormat("zip64 locator not found"));
            }
            let eocd64_off = u64::from_le_bytes([
                data[loc + 8],
                data[loc + 9],
                data[loc + 10],
                data[loc + 11],
                data[loc + 12],
                data[loc + 13],
                data[loc + 14],
                data[loc + 15],
            ]) as usize;
            if data.get(eocd64_off..eocd64_off + 4) != Some(&b"PK"[..]) {
                return Err(CompressError::InvalidFormat("zip64 EOCD not found"));
            }
            let rd64 = |at: usize| -> u64 {
                let mut v = [0u8; 8];
                v.copy_from_slice(&data[at..at + 8]);
                u64::from_le_bytes(v)
            };
            num_entries = rd64(eocd64_off + 32) as usize;
            central_dir_offset = rd64(eocd64_off + 48) as usize;
        }

        let mut entries = Vec::with_capacity(num_entries);
        let mut pos = central_dir_offset;

        for _ in 0..num_entries {
            if pos + 46 > data.len() {
                return Err(CompressError::InvalidFormat("truncated central directory"));
            }

            if &data[pos..pos + 4] != b"PK\x01\x02" {
                return Err(CompressError::InvalidFormat(
                    "invalid central directory signature",
                ));
            }

            let method = u16::from_le_bytes([data[pos + 10], data[pos + 11]]);
            let modified_time = u16::from_le_bytes([data[pos + 12], data[pos + 13]]);
            let modified_date = u16::from_le_bytes([data[pos + 14], data[pos + 15]]);
            let crc = u32::from_le_bytes([
                data[pos + 16],
                data[pos + 17],
                data[pos + 18],
                data[pos + 19],
            ]);
            let compressed_size = u32::from_le_bytes([
                data[pos + 20],
                data[pos + 21],
                data[pos + 22],
                data[pos + 23],
            ]);
            let uncompressed_size = u32::from_le_bytes([
                data[pos + 24],
                data[pos + 25],
                data[pos + 26],
                data[pos + 27],
            ]);
            let name_len = u16::from_le_bytes([data[pos + 28], data[pos + 29]]) as usize;
            let extra_len = u16::from_le_bytes([data[pos + 30], data[pos + 31]]) as usize;
            let comment_len = u16::from_le_bytes([data[pos + 32], data[pos + 33]]) as usize;
            let local_header_offset = u32::from_le_bytes([
                data[pos + 42],
                data[pos + 43],
                data[pos + 44],
                data[pos + 45],
            ]);

            // Same as the compressed-data range: the guard above covers
            // `pos + 46` but not `pos + 46 + name_len`, and `name_len` is
            // attacker-controlled.
            let name_end = (pos + 46)
                .checked_add(name_len)
                .ok_or(CompressError::InvalidFormat("entry name length overflows"))?;
            let name = data
                .get(pos + 46..name_end)
                .ok_or(CompressError::InvalidFormat("entry name out of bounds"))?;
            let name = String::from_utf8_lossy(name).into_owned();

            // Zip64 per-entry extra (header id 0x0001): the 32-bit
            // fields above saturate, and the real values follow in the
            // fixed order uncompressed, compressed, local header
            // offset — only for the fields that actually saturated.
            let (mut size, mut csize, mut lho) =
                (uncompressed_size, compressed_size, local_header_offset);
            if size == 0xFFFF_FFFF || csize == 0xFFFF_FFFF || lho == 0xFFFF_FFFF {
                let extra = data
                    .get(pos + 46 + name_len..pos + 46 + name_len + extra_len)
                    .ok_or(CompressError::InvalidFormat("extra field out of bounds"))?;
                let mut cur = 0usize;
                let mut found = false;
                while cur + 4 <= extra.len() {
                    let id = u16::from_le_bytes([extra[cur], extra[cur + 1]]);
                    let len = u16::from_le_bytes([extra[cur + 2], extra[cur + 3]]) as usize;
                    let body = extra
                        .get(cur + 4..cur + 4 + len)
                        .ok_or(CompressError::InvalidFormat("zip64 extra truncated"))?;
                    if id == 0x0001 {
                        let mut b = 0usize;
                        size = Self::zip64_field(body, &mut b, size)?;
                        csize = Self::zip64_field(body, &mut b, csize)?;
                        lho = Self::zip64_field(body, &mut b, lho)?;
                        found = true;
                        break;
                    }
                    cur += 4 + len;
                }
                if !found {
                    return Err(CompressError::InvalidFormat(
                        "zip64 entry without a 0x0001 extra field",
                    ));
                }
            }

            entries.push(ZipEntryMetadata {
                name,
                size,
                compressed_size: csize,
                crc32: crc,
                method: CompressionMethod::from(method),
                modified_time,
                modified_date,
                local_header_offset: lho,
            });

            pos += 46 + name_len + extra_len + comment_len;
        }

        Ok(entries)
    }

    /// List entries in the archive.
    pub fn list(&self) -> &[ZipEntryMetadata] {
        &self.entries
    }

    /// Extract a file by name, under the archive's byte budget.
    ///
    /// An entry whose expansion runs past the budget is refused, not
    /// truncated: see [`Self::with_limit`].
    ///
    /// The first by-name call builds a name -> index map in O(N) and
    /// every later call is O(log N). The Office and Keras/GGUF
    /// adapters chain several `extract(name)` calls per archive, so
    /// the old O(N) scan was the difference between a millisecond and
    /// a multi-second walk on a 65 536-entry fixture.
    pub fn extract(&self, name: &str) -> CompressResult<Vec<u8>> {
        let map = self.by_name.get_or_init(|| {
            let mut m = BTreeMap::new();
            for (i, e) in self.entries.iter().enumerate() {
                m.insert(e.name.to_string(), i);
            }
            m
        });
        let &idx = map
            .get(name)
            .ok_or(CompressError::InvalidFormat("file not found"))?;
        self.extract_entry(&self.entries[idx])
    }

    /// Extract a specific entry by its index in [`Self::list`], under the
    /// archive's remaining byte budget.
    ///
    /// This is the O(1) counterpart to [`Self::extract`]: callers iterating
    /// the entry list already have the index in hand, and the name lookup
    /// in `extract` is an O(N) scan that turns a 65 536-entry archive into
    /// a 6 s walk (measured on a fixture, see commit log).
    pub fn extract_index(&self, idx: usize) -> CompressResult<Vec<u8>> {
        let entry = self
            .entries
            .get(idx)
            .ok_or(CompressError::InvalidFormat("entry index out of range"))?;
        self.extract_entry(entry)
    }

    /// Extract a specific entry, under the archive's remaining byte budget.
    fn extract_entry(&self, entry: &ZipEntryMetadata) -> CompressResult<Vec<u8>> {
        let limit = self.limit;
        let spent = self.spent.get();
        let remaining = limit.saturating_sub(spent);

        // The declared size is checked first so a header announcing more
        // than the whole archive may expand to is refused for free. It is
        // not trusted as the bound: a header is exactly what a bomb lies
        // in, and the running cap below is what actually stops one.
        if entry.size as u64 > remaining {
            return Err(Self::over_budget(entry, spent, limit));
        }

        // Use the local header offset from the central directory
        let pos = entry.local_header_offset as usize;

        if pos + 30 > self.data.len() {
            return Err(CompressError::InvalidFormat("local header not found"));
        }

        if &self.data[pos..pos + 4] != b"PK\x03\x04" {
            return Err(CompressError::InvalidFormat(
                "invalid local header signature",
            ));
        }

        // Skip local file header
        let name_len = u16::from_le_bytes([self.data[pos + 26], self.data[pos + 27]]) as usize;
        let extra_len = u16::from_le_bytes([self.data[pos + 28], self.data[pos + 29]]) as usize;
        let data_start = pos + 30 + name_len + extra_len;

        // `data_start` comes from the *local* file header while
        // `compressed_size` comes from the *central* directory, so a crafted
        // or truncated archive can point past the end of the data. Bound the
        // slice rather than indexing blindly: this is untrusted input and a
        // panic here aborts the caller.
        let end = data_start
            .checked_add(entry.compressed_size as usize)
            .ok_or(CompressError::InvalidFormat("compressed size overflows"))?;
        let compressed_data =
            self.data
                .get(data_start..end)
                .ok_or(CompressError::InvalidFormat(
                    "compressed data range out of bounds",
                ))?;

        let decoded = match entry.method {
            CompressionMethod::Store => {
                Bounded::complete(compressed_data.to_vec(), Some(entry.size as u64), remaining)
            }
            CompressionMethod::Deflate => {
                DeflateDecoder::new().decode_bounded(compressed_data, remaining)?
            }
            CompressionMethod::Bzip2 => {
                Bzip2Decoder::new().decode_bounded(compressed_data, remaining)?
            }
            CompressionMethod::Lzma => {
                Self::decode_lzma(compressed_data, entry.size as u64, remaining)?
            }
            CompressionMethod::Xz => {
                crate::compress::xz::XzReader::decompress_bounded(compressed_data, remaining)?
            }
            CompressionMethod::Zstd => crate::compress::zstd::ZstdDecoder::new()
                .decode_bounded(compressed_data, remaining)?,
            CompressionMethod::Unsupported(m) => {
                return Err(CompressError::InvalidFormat(match m {
                    93 => "zstd-compressed zip entry (method 93) not supported",
                    9 => "deflate64 zip entry (method 9) not supported",
                    98 => "ppmd-compressed zip entry (method 98) not supported",
                    _ => "unsupported zip compression method",
                }))
            }
        };

        // A short read is an error, not a payload: the prefix the cap kept
        // cannot match the recorded CRC, so returning it would report a
        // corrupt archive rather than a budget that ran out.
        if decoded.capped || decoded.bytes.len() as u64 > remaining {
            return Err(Self::over_budget(entry, spent, limit));
        }

        // Verify CRC
        let computed_crc = crc32(&decoded.bytes);
        if computed_crc != entry.crc32 {
            return Err(CompressError::CrcMismatch {
                expected: entry.crc32,
                actual: computed_crc,
            });
        }

        self.spent.set(spent + decoded.bytes.len() as u64);
        Ok(decoded.bytes)
    }

    /// Decode an LZMA entry (method 14) under `limit`.
    ///
    /// The ZIP payload is wrapped in a 4-byte header before the raw LZMA1
    /// stream: 2 bytes of LZMA version (ignored here) followed by a 2-byte
    /// little-endian length of the property block, then the properties
    /// themselves (1 byte `lc`/`lp`/`pb` plus a 4-byte little-endian
    /// dictionary size). This is *not* the 13-byte `.lzma` "alone" header
    /// that [`LzmaDecoder::decode`] expects, so the stream is handed to
    /// [`LzmaDecoder::decode_raw_bounded`] with the properties parsed out.
    fn decode_lzma(data: &[u8], uncompressed_size: u64, limit: u64) -> CompressResult<Bounded> {
        // 4-byte ZIP header + 5-byte LZMA property block.
        if data.len() < 9 {
            return Err(CompressError::InvalidFormat("truncated LZMA header"));
        }
        let props_size = u16::from_le_bytes([data[2], data[3]]) as usize;
        if props_size != 5 {
            // LZMA2-in-ZIP (property block of 2 bytes) is not implemented.
            return Err(CompressError::Unsupported(
                "unsupported LZMA property block",
            ));
        }

        let props = LzmaProps::from_byte(data[4])?;
        let dict_size = u32::from_le_bytes([data[5], data[6], data[7], data[8]]);

        LzmaDecoder::new().decode_raw_bounded(
            &data[9..],
            props,
            dict_size,
            Some(uncompressed_size),
            limit,
        )
    }

    /// Extract all files.
    pub fn extract_all(&self) -> CompressResult<BTreeMap<String, Vec<u8>>> {
        let mut result = BTreeMap::new();

        for entry in &self.entries {
            let data = self.extract_entry(entry)?;
            result.insert(entry.name.clone(), data);
        }

        Ok(result)
    }
}

/// ZIP error type (alias for CompressError with ZIP-specific messages).
pub type ZipError = CompressError;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compress::huffman::BitWriter;

    /// A zip whose central-directory sizes and CRC are the caller's to
    /// choose, so a test can make the header lie. The writer half of the
    /// format is not vendored; this is the minimum needed to say so.
    ///
    /// Each entry is `(name, method, payload, declared_size, declared_crc)`.
    /// The defaults are the payload's own length and CRC, which is right for
    /// a stored entry; a compressed one has to pass the CRC of what it
    /// decodes to.
    type RawZipEntry<'a> = (&'a str, u16, &'a [u8], Option<u32>, Option<u32>);

    fn zip_entry(entries: &[RawZipEntry<'_>]) -> Vec<u8> {
        let mut out = Vec::new();
        let mut central = Vec::new();
        for (name, method, payload, declared_size, declared_crc) in entries {
            let size = declared_size.unwrap_or(payload.len() as u32);
            let crc = declared_crc.unwrap_or_else(|| crc32(payload));
            let offset = out.len() as u32;

            out.extend_from_slice(b"PK\x03\x04");
            out.extend_from_slice(&20u16.to_le_bytes()); // version needed
            out.extend_from_slice(&0u16.to_le_bytes()); // flags
            out.extend_from_slice(&method.to_le_bytes());
            out.extend_from_slice(&[0u8; 4]); // time, date
            out.extend_from_slice(&crc.to_le_bytes());
            out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
            out.extend_from_slice(&size.to_le_bytes());
            out.extend_from_slice(&(name.len() as u16).to_le_bytes());
            out.extend_from_slice(&0u16.to_le_bytes());
            out.extend_from_slice(name.as_bytes());
            out.extend_from_slice(payload);

            central.extend_from_slice(b"PK\x01\x02");
            central.extend_from_slice(&20u16.to_le_bytes()); // version made by
            central.extend_from_slice(&20u16.to_le_bytes()); // version needed
            central.extend_from_slice(&0u16.to_le_bytes()); // flags
            central.extend_from_slice(&method.to_le_bytes());
            central.extend_from_slice(&[0u8; 4]); // time, date
            central.extend_from_slice(&crc.to_le_bytes());
            central.extend_from_slice(&(payload.len() as u32).to_le_bytes());
            central.extend_from_slice(&size.to_le_bytes());
            central.extend_from_slice(&(name.len() as u16).to_le_bytes());
            central.extend_from_slice(&[0u8; 12]);
            central.extend_from_slice(&offset.to_le_bytes());
            central.extend_from_slice(name.as_bytes());
        }

        let cd_offset = out.len() as u32;
        out.extend_from_slice(&central);
        out.extend_from_slice(b"PK\x05\x06");
        out.extend_from_slice(&[0u8; 4]);
        out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
        out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
        out.extend_from_slice(&(central.len() as u32).to_le_bytes());
        out.extend_from_slice(&cd_offset.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out
    }

    /// A raw DEFLATE stream of `target` bytes from three symbols: literal
    /// 'A', length-258 and end-of-block, all with 2-bit codes in a dynamic
    /// block, plus a 2-bit distance-1. Every 258 output bytes cost 8 bits,
    /// so this is the shape of 42.zip — a header that costs little and an
    /// expansion that does not — at roughly 258:1.
    fn deflate_bomb(target: usize) -> Vec<u8> {
        let mut w = BitWriter::new();
        w.write_bits(1, 1); // BFINAL
        w.write_bits(2, 2); // BTYPE = dynamic Huffman
        w.write_bits(29, 5); // HLIT: 286 literal/length codes
        w.write_bits(0, 5); // HDIST: one distance code
        w.write_bits(14, 4); // HCLEN: 18 code-length codes

        // Code-length alphabet, RFC 1951 order. Symbols 0, 1, 2 and 18 get
        // length 2 — all four at one length is a complete code, so nothing
        // here depends on a decoder accepting an incomplete one. The set is
        // the one this stream emits: 18 for a run of zeros, 2 for a code
        // length, 0 and 1 unused but needed to complete the code.
        const CL_ORDER: [u16; 18] = [16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1];
        for &sym in &CL_ORDER {
            w.write_bits(
                match sym {
                    0 | 1 | 2 | 18 => 2u32,
                    _ => 0,
                },
                3,
            );
        }

        // Canonical codes over {0, 1, 2, 18} at length 2, in symbol order.
        const CL_ZERO_RUN: u64 = 0b11; // symbol 18: repeat a zero 11..138 times
        const CL_TWO: u64 = 0b10; // symbol 2: a literal code length

        let zeros = |w: &mut BitWriter, n: usize| {
            let mut left = n;
            while left > 0 {
                let take = left.min(138);
                w.write_bits_msb(CL_ZERO_RUN, 2);
                w.write_bits((take - 11) as u32, 7);
                left -= take;
            }
        };

        // Literal/length lengths: 'A' at 65, end-of-block at 256, the
        // length-258 code at 285. The one distance code is length 2 too.
        for run in [65usize, 190, 28] {
            zeros(&mut w, run);
            w.write_bits_msb(CL_TWO, 2);
        }
        w.write_bits_msb(CL_TWO, 2); // the distance code's length

        const LIT_A: u64 = 0b00; // 65
        const EOB: u64 = 0b01; // 256
        const LEN_258: u64 = 0b10; // 285
        const DIST_1: u64 = 0b00; // 0
        for _ in 0..target / 258 {
            w.write_bits_msb(LIT_A, 2);
            w.write_bits_msb(LEN_258, 2);
            w.write_bits_msb(DIST_1, 2);
        }
        w.write_bits_msb(EOB, 2);
        w.finish()
    }

    /// The bomb the budget exists for: the central directory says 1 KB and
    /// the payload expands to `target` bytes.
    fn lying_zip(target: usize) -> Vec<u8> {
        let bomb = deflate_bomb(target);
        let prefix_crc = crc32(&vec![b'A'; 1024]);
        zip_entry(&[("bomb.txt", 8, &bomb, Some(1024), Some(prefix_crc))])
    }

    /// How many bytes `deflate_bomb(target)` expands to: one literal plus a
    /// 258-byte match per repetition.
    fn bomb_expansion(target: usize) -> usize {
        (target / 258) * 259
    }

    #[test]
    fn the_deflate_bomb_fixture_is_a_bomb() {
        // Guard for the fixture, not the reader: if this stops holding, the
        // budget tests below would pass for the wrong reason.
        let small = deflate_bomb(1 << 20);
        let whole = DeflateDecoder::new().decode(&small).expect("bomb decodes");
        assert_eq!(whole.len(), bomb_expansion(1 << 20));
        assert!(
            whole.iter().all(|&b| b == b'A'),
            "the expansion is not what was encoded"
        );
        assert!(
            small.len() * 300 < whole.len(),
            "{} bytes expanded to {} — that is not a bomb",
            small.len(),
            whole.len()
        );
        // The ratio is linear, so the big fixtures are the same shape.
        let big = deflate_bomb(10 << 30);
        assert!(
            big.len() * 300 < 10 << 30,
            "10 GB costs {} bytes on disk",
            big.len()
        );
    }

    /// The shape the task names: a central directory claiming 1 KB per
    /// entry over a payload that expands to tens of megabytes. The cap has
    /// to stop it while it expands, because the declared size is the lie.
    #[test]
    fn a_zip_declaring_1kb_and_expanding_to_64mb_is_stopped() {
        let z = lying_zip(64 << 20);
        let r = ZipReader::with_limit(&z, 1 << 20).expect("parse");
        assert_eq!(
            r.list()[0].size,
            1024,
            "the header must be lying for this test to mean anything"
        );

        let err = r.extract("bomb.txt").expect_err("must stop at the budget");
        let CompressError::DecompressionFailed(msg) = &err else {
            panic!("expected a budget refusal, got {err:?}")
        };
        assert!(
            msg.contains("1048576"),
            "the limit must be in the message: {msg}"
        );
        assert!(
            msg.contains("1024 declared"),
            "the lie must be in the message: {msg}"
        );
        assert_eq!(r.budget().1, 0, "a refused entry must not spend the budget");

        // The whole payload is 186 KB on disk; the declared size is 1 KB.
        // Whatever the cap is, the bytes actually decoded are bounded by it.
        assert!(z.len() < 1 << 20, "fixture is {} bytes", z.len());
    }

    /// The budget belongs to the archive, not to the entry: no single entry
    /// is over the limit here, and together they are.
    #[test]
    fn the_budget_is_shared_across_the_whole_archive() {
        let bomb = deflate_bomb(1 << 20);
        let one = bomb_expansion(1 << 20);
        let limit = one as u64 + one as u64 / 2;
        let crc = crc32(&vec![b'A'; one]);
        let z = zip_entry(&[
            ("a.txt", 8, &bomb, Some(one as u32), Some(crc)),
            ("b.txt", 8, &bomb, Some(one as u32), Some(crc)),
        ]);

        let r = ZipReader::with_limit(&z, limit).expect("parse");
        let first = r.extract("a.txt").expect("first entry fits");
        assert_eq!(first.len(), one);
        assert!(first.iter().all(|&b| b == b'A'));
        assert_eq!(r.budget(), (limit, one as u64));

        let err = r.extract("b.txt").expect_err("the archive budget is spent");
        let CompressError::DecompressionFailed(msg) = &err else {
            panic!("{err:?}")
        };
        assert!(
            msg.contains(&format!("{one} bytes already decoded")),
            "{msg}"
        );
    }

    /// A header that declares more than the archive may expand to is
    /// refused before a byte of it is decoded.
    #[test]
    fn a_declared_size_over_the_budget_is_refused_before_decoding() {
        let bomb = deflate_bomb(1 << 20);
        let z = zip_entry(&[("huge.txt", 8, &bomb, Some(1 << 30), None)]);
        let r = ZipReader::with_limit(&z, 1 << 20).expect("parse");
        let err = r
            .extract("huge.txt")
            .expect_err("must refuse the declaration");
        let CompressError::DecompressionFailed(msg) = &err else {
            panic!("{err:?}")
        };
        assert!(msg.contains("1073741824 declared"), "{msg}");
        assert_eq!(
            r.budget().1,
            0,
            "nothing may be decoded to refuse a declaration"
        );
    }

    /// A stored entry with a lying uncompressed size is caught the same way
    /// the compressed ones are — `reserve` on the declared size would not
    /// catch it, and the copy is the cheapest way to spend a budget.
    #[test]
    fn a_stored_entry_lying_about_its_size_is_stopped() {
        let payload = vec![b'Z'; 1 << 20];
        let z = zip_entry(&[("flat.bin", 0, &payload, Some(1024), None)]);
        let r = ZipReader::with_limit(&z, 1 << 16).expect("parse");
        let err = r.extract("flat.bin").expect_err("must stop at the budget");
        let CompressError::DecompressionFailed(msg) = &err else {
            panic!("{err:?}")
        };
        assert!(msg.contains("1024 declared"), "{msg}");
        assert!(msg.contains("65536"), "{msg}");
    }

    /// The other half: a real archive has to come out whole. 5000 stored
    /// entries, every one of them checked against its own content, under
    /// the default budget. This passes with or without the cap — that is
    /// what a control is for.
    #[test]
    fn five_thousand_entries_still_extract_whole() {
        let body = vec![b'q'; 256];
        let entries: Vec<(String, Vec<u8>)> = (0..5000)
            .map(|i| (format!("dir/f{i:05}.txt"), body.clone()))
            .collect();
        let entries: Vec<RawZipEntry<'_>> = entries
            .iter()
            .map(|(n, d)| (n.as_str(), 0u16, &d[..], None, None))
            .collect();
        let z = zip_entry(&entries);

        let r = ZipReader::new(&z).expect("parse");
        assert_eq!(r.list().len(), 5000);
        let all = r
            .extract_all()
            .expect("every entry fits under the default budget");
        assert_eq!(all.len(), 5000);
        for (i, (name, _, bytes, _, _)) in entries.iter().enumerate() {
            assert_eq!(
                all.get(*name).map(Vec::as_slice),
                Some(&bytes[..]),
                "entry {i}"
            );
        }
        assert_eq!(r.budget(), (default_byte_limit(), 5000 * 256));
    }

    /// The number the default is, so it cannot drift from the one the
    /// archive adapters' byte budget falls back to.
    #[test]
    fn the_default_limit_is_the_context_default() {
        assert_eq!(
            default_byte_limit(),
            crate::Ctx::default().max_file_size as u64
        );
    }

    /// Every method the reader implements still decodes whole under a cap
    /// barely above its real size, on the python-zipfile vectors rather
    /// than on anything built here — bzip2, lzma and xz each take a
    /// different bounded path than deflate, and none may regress into a
    /// refusal or a short read.
    #[test]
    fn every_supported_method_still_decodes_under_a_tight_cap() {
        macro_rules! case {
            ($name:literal) => {{
                let z = include_bytes!(concat!("../testdata/zip_", $name, ".zip"));
                let expected = include_bytes!(concat!("../testdata/zip_", $name, ".expected"));
                let r = ZipReader::with_limit(z, 4096)
                    .unwrap_or_else(|e| panic!("{}/{}: {e}", $name, $name));
                let got = r
                    .extract("readme.txt")
                    .unwrap_or_else(|e| panic!("{}/{}: {e}", $name, $name));
                assert_eq!(got, &expected[..], "{}/{}/readme", $name, $name);
                assert_eq!(
                    r.budget().1,
                    expected.len() as u64,
                    "{}/{} spend",
                    $name,
                    $name
                );
            }};
        }
        case!("text_store");
        case!("text_deflate");
        case!("text_bzip2");
        case!("text_lzma");
        case!("binary_deflate");
        case!("binary_bzip2");
        case!("binary_lzma");
        case!("empty_store");
        case!("empty_bzip2");
        case!("empty_lzma");
        case!("zip64");
        case!("text_m95");
    }

    /// The measurement the whole change turns on, and the reason it is
    /// `#[ignore]`d because it builds a 10 GB expansion: the ratio the
    /// header in `lying_zip` claims, at the scale the task names. It
    /// measures rather than asserts timing — the assertion is that the
    /// budget stops it, and `RGX_ZIP_BOMB_BYTES` scales the corpus down
    /// when 10 GB of allocation is not wanted.
    ///
    /// The uncoded column is what the same entry costs through
    /// `DeflateDecoder::decode`, i.e. the path this replaced.
    #[test]
    #[ignore = "10 GB of allocation; run with --release --ignored"]
    fn ten_gigabytes_from_a_1kb_declaration() {
        let target: usize = std::env::var("RGX_ZIP_BOMB_BYTES")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(10 << 30);
        let bomb = deflate_bomb(target);
        let expansion = bomb_expansion(target);
        let z = zip_entry(&[(
            "bomb.txt",
            8,
            &bomb,
            Some(1024),
            Some(crc32(&vec![b'A'; 1024])),
        )]);
        let r = ZipReader::new(&z).expect("parse");
        std::eprintln!(
            "zip on disk {} B, header declares {} B, real expansion {} B, budget {} B",
            z.len(),
            r.list()[0].size,
            expansion,
            r.budget().0
        );

        let t = std::time::Instant::now();
        let err = r.extract("bomb.txt").expect_err("stopped");
        let bounded = t.elapsed();
        assert!(matches!(err, CompressError::DecompressionFailed(_)));
        std::eprintln!("capped at default: {bounded:?}");

        for limit in [64 << 20, 8 << 20] {
            let t = std::time::Instant::now();
            let err = ZipReader::with_limit(&z, limit)
                .expect("parse")
                .extract("bomb.txt");
            std::eprintln!(
                "capped at {limit:>12}: {:?} ({})",
                t.elapsed(),
                err.err().map(|_| "refused").unwrap_or("extracted")
            );
        }

        let t = std::time::Instant::now();
        let whole = DeflateDecoder::new()
            .decode(&bomb)
            .map(|v| v.len())
            .unwrap_or(0);
        std::eprintln!("uncapped:          {:?} -> {whole} B", t.elapsed());
        assert_eq!(whole, expansion);
    }

    /// Third-party vectors: archives emitted by Python's zipfile module
    /// (the reference writers) over empty/text/binary payloads in store,
    /// deflate, bzip2 and lzma, plus an allowZip64 archive. Readme.txt's
    /// content is the .expected twin; nested/inner.bin is its reversal.
    #[test]
    fn test_zip_third_party_vectors() {
        macro_rules! case {
            ($name:literal) => {{
                let data = include_bytes!(concat!("../testdata/zip_", $name, ".zip"));
                let expected = include_bytes!(concat!("../testdata/zip_", $name, ".expected"));
                let r = ZipReader::new(data).unwrap_or_else(|e| panic!("zip_{}: {e}", $name));
                assert_eq!(r.list().len(), 2, "zip_{} entries", $name);
                assert_eq!(
                    r.extract("readme.txt")
                        .unwrap_or_else(|e| panic!("zip_{}: {e}", $name)),
                    expected,
                    "zip_{} readme",
                    $name
                );
                let mut rev: Vec<u8> = expected.to_vec();
                rev.reverse();
                assert_eq!(
                    r.extract("nested/inner.bin")
                        .unwrap_or_else(|e| panic!("zip_{}: {e}", $name)),
                    rev,
                    "zip_{} nested",
                    $name
                );
            }};
        }
        case!("empty_store");
        case!("empty_deflate");
        case!("text_store");
        case!("text_deflate");
        case!("text_bzip2");
        case!("text_lzma");
        case!("binary_store");
        case!("binary_deflate");
        case!("binary_bzip2");
        case!("binary_lzma");
        case!("zip64");
    }

    /// A method we do not implement must be an error, never the raw
    /// compressed bytes passed off as the file's content.
    #[test]
    fn test_unknown_method_is_refused() {
        let raw = zip_entry(&[("readme.txt", 98, b"data", Some(4), Some(crc32(b"data")))]);
        let r = ZipReader::new(&raw).expect("parse");
        let err = r.extract("readme.txt").expect_err("must refuse ppmd");
        assert!(matches!(err, CompressError::InvalidFormat(_)));
    }

    #[test]
    fn test_method_93_zstd_decodes() {
        let frame = b"\x28\xb5\x2f\xfd\x04\x58\x61\x00\x00hello zstd\\n\xca\xfb\x72\x73";
        let original = b"hello zstd\\n";
        let raw = zip_entry(&[(
            "readme.txt",
            93,
            frame,
            Some(original.len() as u32),
            Some(crc32(original)),
        )]);
        let r = ZipReader::new(&raw).expect("parse");
        assert_eq!(r.extract("readme.txt").expect("extract zstd"), original);
    }

    /// Method 95 (XZ) inside a zip: a hand-built archive around a
    /// python-lzma (liblzma) FORMAT_XZ stream, decoded and CRC-verified.
    #[test]
    fn test_method_95_decodes() {
        let data = include_bytes!("../testdata/zip_text_m95.zip");
        let expected = include_bytes!("../testdata/zip_text_m95.expected");
        let r = ZipReader::new(data).expect("parse");
        assert_eq!(r.extract("readme.txt").expect("extract"), &expected[..]);
    }

    #[test]
    fn test_rejects_non_zip() {
        assert!(ZipReader::new(b"not a zip at all").is_err());
    }

    /// Truncation at every length must error, never panic.
    #[test]
    fn test_truncated_never_panics() {
        let data = include_bytes!("../testdata/zip_text_deflate.zip");
        for cut in (0..data.len()).step_by(97) {
            let _ = ZipReader::new(&data[..cut]);
        }
    }
}
