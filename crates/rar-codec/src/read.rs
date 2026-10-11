//! Archive reader: full RAR4 + RAR5 header parsing with stored-entry
//! extraction.
//!
//! Layout reference: RARLAB's `technote.txt` (RAR 5.x archive format) and
//! the RAR 4.x block layout. Everything here is bounds-checked; malformed or
//! truncated input yields an [`Error`], never a panic.

use alloc::borrow::Cow;
use alloc::string::String;
use alloc::vec::Vec;

use crate::crc32::crc32;
use crate::{vint, Error, Result};

/// Compression method value for "store" (no compression) in **RAR4** file
/// headers. In RAR5 the method is the value 0..=5 from the compression
/// information field, where 0 means store.
pub const METHOD_STORE: u8 = 0x30;

const MAGIC4: &[u8; 7] = b"Rar!\x1a\x07\x00";
const MAGIC5: &[u8; 8] = b"Rar!\x1a\x07\x01\x00";

/// Which RAR generation an archive uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    /// `Rar!\x1a\x07\x00` — the "RAR4"/RAR 2.9-4.x family.
    Rar4,
    /// `Rar!\x1a\x07\x01\x00` — RAR 5.x.
    Rar5,
}

/// What an entry holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    /// A regular file with (possibly empty) payload.
    File,
    /// A directory. Has no payload.
    Directory,
    /// A symbolic link (RAR5 redirection record, types 1..=3). The payload
    /// is the link target.
    Symlink,
    /// A hard link (RAR5 redirection record, type 4).
    Hardlink,
    /// A RAR5 service block (e.g. archive comment). Payload is present but
    /// not a user file.
    Service,
}

/// One archive member, borrowed from the input buffer.
#[derive(Debug, Clone)]
pub struct Entry<'a> {
    /// Member name (UTF-8, lossy-decoded).
    pub name: Cow<'a, str>,
    pub kind: EntryKind,
    /// Uncompressed size in bytes. Only meaningful when
    /// [`Entry::size_unknown`] is false.
    pub unpacked_size: u64,
    /// True when the 0x0008 file flag was set: `unpacked_size` must be
    /// ignored and extraction runs until the end of the data area.
    pub size_unknown: bool,
    /// Size of the packed data area in the archive.
    pub packed_size: u64,
    /// Compression method, 0..=5 (`(compression_info >> 8) & 7`).
    /// 0 = store → extractable; 1..=5 need the decompressor.
    pub method: u8,
    /// Compression algorithm version (low 6 bits of compression info):
    /// 0 = classic RAR5 (LZ77+Huffman/PPMd), 1 = RAR 7.x variant.
    pub compression_version: u8,
    /// Required dictionary size in bytes (128 KB << N from compression info).
    pub dictionary_size: u64,
    /// Solid flag: dictionary persists from the previous file.
    pub solid: bool,
    /// Host OS: 0 = Windows, 1 = Unix.
    pub host_os: u8,
    /// CRC-32 of the *unpacked* data, when recorded.
    pub crc32: Option<u32>,
    /// Raw mtime (Unix time when host is Unix), when recorded.
    pub mtime: Option<u32>,
    /// True when the entry's payload is encrypted (needs a password and the
    /// proprietary algorithm).
    pub encrypted: bool,
    /// True when the entry's packed data started in a previous volume of a
    /// multivolume set (header flag 0x08, RAR4: 0x0001).
    pub split_before: bool,
    /// True when the entry's packed data continues in the next volume of a
    /// multivolume set (header flag 0x10, RAR4: 0x0002). Such entries are
    /// incomplete when the archive was opened as a single volume; use
    /// [`Archive::open_multi`] to reassemble them.
    pub split_after: bool,
    /// Absolute offset of the payload area in the archive, `None` when the
    /// entry carries no data (directories, zero-byte stored files still
    /// have a 1-byte area in RAR5 — offset is `Some` then).
    pub data_offset: Option<usize>,
}

/// One contiguous piece of an entry's packed data area. In a multivolume
/// archive an entry's data is split into one chunk per volume.
#[derive(Debug, Clone, Copy)]
struct Chunk {
    /// Index into the volume list (`0` = the slice `Archive` was opened with).
    volume: usize,
    offset: usize,
    len: usize,
}

/// Decoder state cached between `extract` calls for solid archives:
/// decoding a solid member requires decoding the whole solid group that
/// precedes it, so the unpacker position is remembered per group.
// Only one variant is ever live and it is private to the crate, so boxing
// it would buy nothing but an allocation per archive.
#[allow(clippy::large_enum_variant)]
enum SolidCache {
    Empty,
    Decoded {
        /// Entry index of the first group member.
        start: usize,
        /// Next entry index to decode (all entries before it are done).
        through: usize,
        decoder: SolidDecoder,
        /// Decoded outputs by entry index.
        outputs: alloc::collections::BTreeMap<usize, Vec<u8>>,
    },
}

/// The format-specific unpacker kept alive for a solid group.
// Same reasoning as `SolidCache`: one live variant, crate-private, and the
// rare3 window alone is 4 MB, so boxing would only add an allocation.
#[allow(clippy::large_enum_variant)]
enum SolidDecoder {
    Rar5(crate::unpack::Unpacker),
    Rar4(crate::unpack3::Unpacker3),
}

impl core::fmt::Debug for SolidCache {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            SolidCache::Empty => f.write_str("SolidCache::Empty"),
            SolidCache::Decoded {
                start,
                through,
                outputs,
                ..
            } => f
                .debug_struct("SolidCache::Decoded")
                .field("start", start)
                .field("through", through)
                .field("outputs", outputs)
                .finish(),
        }
    }
}

/// A parsed archive. Borrows the input bytes.
#[derive(Debug)]
pub struct Archive<'a> {
    data: &'a [u8],
    /// Further volumes of a multivolume set, in order (volume 1 is `data`).
    /// Empty unless opened via [`Archive::open_multi`].
    extra_volumes: Vec<&'a [u8]>,
    /// Packed-data chunks per entry, parallel to `entries`. `Some` for
    /// archives opened with [`Archive::open_multi`] (entries may span
    /// volumes); `None` entries extract straight from `data`.
    entry_chunks: Vec<Option<Vec<Chunk>>>,
    solid: core::cell::RefCell<SolidCache>,
    /// Detected format.
    pub format: Format,
    entries: Vec<Entry<'a>>,
    /// True when the archive declares itself encrypted (RAR5 encryption
    /// header / RAR4 password-protected members).
    pub encrypted: bool,
    /// True when the main header declares this a volume of a multivolume
    /// set (RAR5 archive flag 0x0001).
    pub multivolume: bool,
    /// Volume number from the RAR5 main header (present on all volumes
    /// except, typically, the first).
    pub volume_number: Option<u64>,
}

impl<'a> Archive<'a> {
    /// Strict open: verifies every RAR5 header CRC. Corrupt archives fail
    /// with [`Error::BadCrc`].
    pub fn open(data: &'a [u8]) -> Result<Self> {
        Self::parse(data, true)
    }

    /// Lenient open: parses structure but skips header CRC verification.
    /// Use for listing/recovery of damaged archives.
    pub fn open_lenient(data: &'a [u8]) -> Result<Self> {
        Self::parse(data, false)
    }

    /// Open a multivolume (split) archive given all its volumes in order.
    ///
    /// The first volume must carry the multivolume flag (RAR5 archive flag
    /// 0x0001 / RAR4 MHD_VOLUME); every volume is parsed with strict header
    /// CRC verification (RAR4 header CRCs are not verified, as in `open`),
    /// split file entries are reassembled across volumes (RAR5 header flags
    /// 0x08/0x10, RAR4 LHD_SPLIT_BEFORE/AFTER mark data continuing from the
    /// previous / into the next volume), and [`Archive::extract`]
    /// transparently concatenates the packed chunks.
    ///
    /// Passing a single slice is equivalent to [`Archive::open`].
    pub fn open_multi(volumes: &[&'a [u8]]) -> Result<Self> {
        let (&first, rest) = volumes
            .split_first()
            .ok_or(Error::Malformed("no volumes given"))?;
        let mut archive = Self::parse(first, true)?;
        if rest.is_empty() {
            // A lone volume of a multivolume set is incomplete when any
            // entry's data continues into the (missing) next volume.
            if archive.entries.iter().any(|e| e.split_after) {
                return Err(Error::Truncated);
            }
            return Ok(archive);
        }
        if !archive.multivolume {
            return Err(Error::Malformed(
                "first volume does not declare the multivolume flag",
            ));
        }
        if let Some(num) = archive.volume_number {
            if num != 0 {
                return Err(Error::Malformed("volumes out of order"));
            }
        }
        archive.extra_volumes = rest.to_vec();
        // Chunk lists for the first volume's entries.
        archive.entry_chunks = archive
            .entries
            .iter()
            .map(|e| {
                Some(match e.data_offset {
                    Some(off) => alloc::vec![Chunk {
                        volume: 0,
                        offset: off,
                        len: e.packed_size as usize,
                    }],
                    None => alloc::vec![],
                })
            })
            .collect();

        // The first volume may already have a file whose data continues
        // into volume 2: that entry is the initial continuation target.
        let mut pending: Option<usize> = archive
            .entries
            .iter()
            .position(|e| e.split_after && e.data_offset.is_some());
        for (vi, vol) in rest.iter().enumerate() {
            let vi = vi + 1;
            let parsed = Self::parse(vol, true)?;
            if parsed.format != archive.format {
                return Err(Error::Unsupported("multivolume set mixes RAR formats"));
            }
            if !parsed.multivolume {
                return Err(Error::Malformed(
                    "volume does not declare the multivolume flag",
                ));
            }
            if let Some(num) = parsed.volume_number {
                // Volume numbers are 0-based; the first volume typically
                // omits the field, later ones carry their index.
                if num != vi as u64 {
                    return Err(Error::Malformed("volumes out of order"));
                }
            }
            for entry in parsed.entries {
                let is_continuation = entry.split_before
                    && entry.data_offset.is_some()
                    && pending.is_some_and(|pi| {
                        archive.entries[pi].name == entry.name
                            && archive.entries[pi].kind == entry.kind
                    });
                if is_continuation {
                    let pi = pending.unwrap();
                    archive.entry_chunks[pi].as_mut().unwrap().push(Chunk {
                        volume: vi,
                        offset: entry.data_offset.unwrap(),
                        len: entry.packed_size as usize,
                    });
                    archive.entries[pi].packed_size += entry.packed_size;
                    if !entry.split_after {
                        let target = &mut archive.entries[pi];
                        target.split_after = false;
                        // Empirically (WinRAR 7.x), a split file's repeated
                        // headers carry the CRC32 of the *packed chunk* in
                        // that volume while data continues; only the closing
                        // volume's header carries the CRC32 of the whole
                        // unpacked data. Adopt the closing header's CRC.
                        target.crc32 = entry.crc32;
                        pending = None;
                    }
                    continue;
                }
                if entry.split_before && entry.data_offset.is_some() {
                    return Err(Error::Malformed(
                        "continuation entry without matching previous volume",
                    ));
                }
                // Per-volume service records (e.g. the "QO" quick-open block)
                // never carry file chunks and are skipped: only the first
                // volume's are reported.
                if entry.kind == EntryKind::Service {
                    continue;
                }
                if pending.take().is_some() {
                    // The previous file was never closed: the set is
                    // incomplete or a volume is missing.
                    return Err(Error::Truncated);
                }
                if entry.split_after && entry.data_offset.is_none() {
                    return Err(Error::Malformed("split entry without a data area"));
                }
                archive.entry_chunks.push(Some(match entry.data_offset {
                    Some(off) => alloc::vec![Chunk {
                        volume: vi,
                        offset: off,
                        len: entry.packed_size as usize,
                    }],
                    None => alloc::vec![],
                }));
                if entry.split_after {
                    pending = Some(archive.entries.len());
                }
                archive.entries.push(entry);
            }
        }
        if pending.is_some() {
            return Err(Error::Truncated);
        }
        Ok(archive)
    }

    /// All member entries in archive order.
    pub fn entries(&self) -> &[Entry<'a>] {
        &self.entries
    }

    /// Extract the unpacked bytes of `entry`.
    ///
    /// Only stored entries are extractable; anything compressed or
    /// encrypted returns [`Error::Unsupported`]. Entries whose data spans
    /// volumes require an archive opened via [`Archive::open_multi`].
    pub fn extract(&self, entry: &Entry<'a>) -> Result<Vec<u8>> {
        if entry.kind == EntryKind::Directory {
            return Err(Error::Malformed("directory has no payload"));
        }
        if entry.encrypted {
            return Err(Error::Unsupported("encrypted payload (password required)"));
        }
        if (entry.split_before || entry.split_after) && self.entry_chunks.is_empty() {
            return Err(Error::Unsupported(
                "entry spans multiple volumes; open the set with Archive::open_multi",
            ));
        }
        // Solid members go through the group decoder even when stored, so
        // the shared window/tables stay in sync with the rest of the group.
        if entry.solid {
            return self.extract_solid(entry);
        }
        let store = match self.format {
            Format::Rar5 => entry.method == 0,
            Format::Rar4 => entry.method == METHOD_STORE,
        };
        if !store {
            return match self.format {
                Format::Rar5 => self.extract_compressed_rar5(entry),
                Format::Rar4 => self.extract_compressed_rar4(entry),
            };
        }
        let packed = self.packed_bytes(entry)?;
        let out: Vec<u8> = match self.format {
            Format::Rar5 => {
                // Stored payload: raw bytes, no flag byte (RAR5 store = plain).
                if entry.size_unknown {
                    // extract until the end of the data area
                    packed.into_owned()
                } else {
                    let end = entry.unpacked_size as usize;
                    if end > packed.len() {
                        return Err(Error::Truncated);
                    }
                    packed[..end].to_vec()
                }
            }
            Format::Rar4 => {
                let end = entry.unpacked_size as usize;
                if end > packed.len() {
                    return Err(Error::Truncated);
                }
                packed[..end].to_vec()
            }
        };
        if let Some(want) = entry.crc32 {
            if crc32(&out) != want {
                return Err(Error::BadCrc("file data"));
            }
        }
        Ok(out)
    }

    /// Gather the entry's packed data by its index in `entries`.
    /// Single-volume archives read a slice of `data`; multivolume archives
    /// concatenate the recorded chunks.
    fn packed_bytes_at(&self, idx: usize) -> Result<Cow<'_, [u8]>> {
        let entry = &self.entries[idx];
        if let Some(Some(chunks)) = self.entry_chunks.get(idx) {
            let mut buf = Vec::with_capacity(entry.packed_size as usize);
            for c in chunks {
                let vol = if c.volume == 0 {
                    self.data
                } else {
                    self.extra_volumes
                        .get(c.volume - 1)
                        .ok_or(Error::Malformed("chunk references missing volume"))?
                };
                let end = c
                    .offset
                    .checked_add(c.len)
                    .ok_or(Error::Malformed("size overflow"))?;
                if end > vol.len() {
                    return Err(Error::Truncated);
                }
                buf.extend_from_slice(&vol[c.offset..end]);
            }
            return Ok(Cow::Owned(buf));
        }
        let off = entry
            .data_offset
            .ok_or(Error::Malformed("entry has no data area"))?;
        let end = off
            .checked_add(entry.packed_size as usize)
            .ok_or(Error::Malformed("size overflow"))?;
        if end > self.data.len() {
            return Err(Error::Truncated);
        }
        Ok(Cow::Borrowed(&self.data[off..end]))
    }

    /// Whether the entry carries a packed data area.
    fn has_data(&self, idx: usize) -> bool {
        let e = &self.entries[idx];
        e.data_offset.is_some()
            || matches!(self.entry_chunks.get(idx), Some(Some(c)) if !c.is_empty())
    }

    /// Gather the entry's packed data, locating it by identity.
    fn packed_bytes(&self, entry: &Entry<'a>) -> Result<Cow<'_, [u8]>> {
        let idx = self
            .entries
            .iter()
            .position(|e| core::ptr::eq(e, entry))
            .ok_or(Error::Malformed("entry does not belong to this archive"))?;
        self.packed_bytes_at(idx)
    }

    /// Extract a RAR5 member compressed with LZ77+Huffman (method 1..=5,
    /// compression algorithm version 0).
    fn extract_compressed_rar5(&self, entry: &Entry<'a>) -> Result<Vec<u8>> {
        if entry.compression_version != 0 {
            return Err(Error::Unsupported(
                "RAR 7.x compression algorithm (version 1)",
            ));
        }
        if entry.solid {
            return self.extract_solid(entry);
        }
        let packed = self.packed_bytes(entry)?;
        let target = if entry.size_unknown {
            None
        } else {
            Some(entry.unpacked_size)
        };
        let out = crate::unpack::unpack(&packed, target, entry.dictionary_size)?;
        if let Some(want) = entry.crc32 {
            if crc32(&out) != want {
                return Err(Error::BadCrc("file data"));
            }
        }
        Ok(out)
    }

    /// Extract a RAR4 member compressed with the LZ77/Huffman method
    /// (0x31..=0x35, "RAR 2.9/3.x" compression).
    fn extract_compressed_rar4(&self, entry: &Entry<'a>) -> Result<Vec<u8>> {
        if !(0x31..=0x35).contains(&entry.method) {
            return Err(Error::Unsupported("unknown RAR4 compression method"));
        }
        let packed = self.packed_bytes(entry)?;
        let out = crate::unpack3::unpack(&packed, entry.unpacked_size)?;
        if let Some(want) = entry.crc32 {
            if crc32(&out) != want {
                return Err(Error::BadCrc("file data"));
            }
        }
        Ok(out)
    }

    /// Decode the solid group ending at `entry` and return its output.
    ///
    /// A solid group is a run of data-bearing entries in which every member
    /// after the first carries the solid flag: the LZ77 window, Huffman
    /// tables and distance cache persist between members, so extracting any
    /// member requires decoding the group from its first entry. Progress is
    /// cached in the archive so repeated extracts resume where the previous
    /// one stopped; stored members inside the group still occupy the
    /// dictionary.
    fn extract_solid(&self, entry: &Entry<'a>) -> Result<Vec<u8>> {
        let idx = self
            .entries
            .iter()
            .position(|e| core::ptr::eq(e, entry))
            .ok_or(Error::Malformed("entry does not belong to this archive"))?;
        // First member of the solid run: every solid-flagged member
        // continues the previous entry's dictionary, so walk back over
        // solid members (and data-less entries like directories, which do
        // not break the run) to the run's first member — which itself is
        // not solid-flagged (or has nothing before it).
        let mut start = idx;
        while start > 0 && (self.entries[start].solid || !self.has_data(start)) {
            start -= 1;
        }
        let mut cache = self.solid.borrow_mut();
        let same_group = matches!(&*cache, SolidCache::Decoded { start: s, .. } if *s == start);
        if !same_group {
            let decoder = match self.format {
                Format::Rar5 => {
                    let dict = self.entries[start..=idx]
                        .iter()
                        .map(|e| e.dictionary_size)
                        .max()
                        .unwrap_or(0);
                    SolidDecoder::Rar5(crate::unpack::Unpacker::new_solid(dict))
                }
                Format::Rar4 => {
                    // The rar3 window must cover the whole group output
                    // (matches may reference any earlier member): size it
                    // from the sum of the group's unpacked sizes, capped at
                    // the 4 MB rar3 dictionary and rounded up to a power of
                    // two (Unpacker3 indexes it with a mask).
                    const RAR4_MAX_WINDOW: u64 = 0x40000 * 16;
                    let total: u64 = self.entries[start..=idx]
                        .iter()
                        .map(|e| e.unpacked_size)
                        .sum::<u64>()
                        .min(RAR4_MAX_WINDOW);
                    let window = if total >= RAR4_MAX_WINDOW {
                        RAR4_MAX_WINDOW as usize
                    } else {
                        1usize << (64 - (total | 1).leading_zeros())
                    };
                    SolidDecoder::Rar4(crate::unpack3::Unpacker3::new(window))
                }
            };
            *cache = SolidCache::Decoded {
                start,
                through: start,
                decoder,
                outputs: alloc::collections::BTreeMap::new(),
            };
        }
        loop {
            let (through, decoder, outputs) = match &mut *cache {
                SolidCache::Decoded {
                    through,
                    decoder,
                    outputs,
                    ..
                } => (through, decoder, outputs),
                SolidCache::Empty => unreachable!(),
            };
            if *through > idx {
                break;
            }
            let i = *through;
            if !self.has_data(i) {
                *through = i + 1;
                if i == idx {
                    break;
                }
                continue;
            }
            let e = &self.entries[i];
            let decoded = match decoder {
                SolidDecoder::Rar5(unpacker) => {
                    let target = if e.size_unknown {
                        None
                    } else {
                        Some(e.unpacked_size)
                    };
                    if e.method == 0 {
                        // Stored member: raw bytes that still occupy the dictionary.
                        let packed = self.packed_bytes_at(i)?;
                        unpacker.write_stored(&packed);
                        Ok(packed.into_owned())
                    } else {
                        let packed = self.packed_bytes_at(i)?;
                        unpacker.begin_member(target);
                        unpacker.run(&packed).and_then(|()| unpacker.finish())
                    }
                }
                SolidDecoder::Rar4(unpacker) => {
                    if e.method == METHOD_STORE {
                        // Stored member: raw bytes that still occupy the window.
                        let packed = self.packed_bytes_at(i)?;
                        unpacker.write_stored(&packed)?;
                        Ok(packed.into_owned())
                    } else {
                        if !(0x31..=0x35).contains(&e.method) {
                            Err(Error::Unsupported("unknown RAR4 compression method"))
                        } else {
                            let packed = self.packed_bytes_at(i)?;
                            unpacker.run_member(&packed, e.unpacked_size)
                        }
                    }
                }
            };
            let out = match decoded {
                Ok(out) => out,
                Err(err) => {
                    *cache = SolidCache::Empty;
                    return Err(err);
                }
            };
            if let Some(want) = e.crc32 {
                if crc32(&out) != want {
                    return Err(Error::BadCrc("file data"));
                }
            }
            outputs.insert(i, out);
            *through = i + 1;
            if i == idx {
                break;
            }
        }
        match &*cache {
            SolidCache::Decoded { outputs, .. } => outputs
                .get(&idx)
                .cloned()
                .ok_or(Error::Malformed("solid member was not decoded")),
            SolidCache::Empty => unreachable!(),
        }
    }

    fn parse(data: &'a [u8], verify_crc: bool) -> Result<Self> {
        if data.len() < 8 {
            return Err(Error::Truncated);
        }
        let format = if &data[..8] == MAGIC5 {
            Format::Rar5
        } else if &data[..7] == MAGIC4 {
            Format::Rar4
        } else {
            return Err(Error::BadMagic);
        };
        let mut archive = Archive {
            data,
            extra_volumes: Vec::new(),
            entry_chunks: Vec::new(),
            solid: core::cell::RefCell::new(SolidCache::Empty),
            format,
            entries: Vec::new(),
            encrypted: false,
            multivolume: false,
            volume_number: None,
        };
        match format {
            Format::Rar5 => archive.parse_rar5(verify_crc)?,
            Format::Rar4 => archive.parse_rar4()?,
        }
        Ok(archive)
    }

    // ---------------------------------------------------------------- RAR5

    fn parse_rar5(&mut self, verify_crc: bool) -> Result<()> {
        let data = self.data;
        let mut p = 8usize; // past magic
        loop {
            // Header: CRC32(4). HSIZE counts "starting from Header type
            // field", but the CRC covers "starting from Header size field"
            // — both up to and including the optional extra area. So:
            //   crc region = [hsize_field .. type_field + hsize]
            if p.checked_add(5)
                .ok_or(Error::Malformed("offset overflow"))?
                > data.len()
            {
                return Err(Error::Truncated);
            }
            let crc_start = p + 4;
            let mut q = crc_start;
            let hsize = vint::read_r(data, &mut q)? as usize;
            let body_end = q
                .checked_add(hsize)
                .ok_or(Error::Malformed("header size overflow"))?;
            if body_end > data.len() {
                return Err(Error::Truncated);
            }
            if verify_crc {
                let want = u32::from_le_bytes(data[p..p + 4].try_into().unwrap());
                if crc32(&data[crc_start..body_end]) != want {
                    return Err(Error::BadCrc("rar5 header"));
                }
            }
            let htype = vint::read_r(data, &mut q)?;
            let hflags = vint::read_r(data, &mut q)?;
            let has_extra = hflags & 0x01 != 0;
            let has_data = hflags & 0x02 != 0;
            // Header flags 0x08/0x10 mark packed data split across volumes
            // (same semantics as RAR4's LHD_SPLIT_BEFORE/AFTER). Written by
            // WinRAR for multivolume sets; each volume repeats the file
            // header with one chunk of the data area.
            let split_before = hflags & 0x08 != 0;
            let split_after = hflags & 0x10 != 0;
            let extra_size = if has_extra {
                vint::read_r(data, &mut q)? as usize
            } else {
                0
            };
            let data_size = if has_data {
                vint::read_r(data, &mut q)?
            } else {
                0
            };

            match htype {
                1 => {
                    // main header: archive flags then optional volume number
                    if q.checked_add(1)
                        .ok_or(Error::Malformed("offset overflow"))?
                        > body_end
                    {
                        return Err(Error::Truncated);
                    }
                    let aflags = vint::read_r(data, &mut q)?;
                    self.multivolume = aflags & 0x01 != 0;
                    if aflags & 0x02 != 0 {
                        if q.checked_add(1)
                            .ok_or(Error::Malformed("offset overflow"))?
                            > body_end
                        {
                            return Err(Error::Truncated);
                        }
                        self.volume_number = Some(vint::read_r(data, &mut q)?);
                    }
                }
                2 | 3 => {
                    // file / service header
                    let kind = if htype == 3 {
                        EntryKind::Service
                    } else {
                        EntryKind::File
                    };
                    let data_off = if has_data { Some(body_end) } else { None };
                    let entry = self.parse_rar5_file(
                        data,
                        &mut q,
                        body_end,
                        extra_size,
                        kind,
                        data_size,
                        data_off,
                        split_before,
                        split_after,
                    )?;
                    self.entries.push(entry);
                }
                4 => {
                    // archive encryption header
                    self.encrypted = true;
                }
                5 => break, // end of archive
                _ => {}     // unknown block: skip by size, forward-compatible
            }

            p = body_end
                .checked_add(data_size as usize)
                .ok_or(Error::Malformed("offset overflow"))?;
        }
        Ok(())
    }

    // The parameters are the block header fields the caller already
    // decoded; a one-use struct would rename them, not remove them.
    #[allow(clippy::too_many_arguments)]
    fn parse_rar5_file(
        &self,
        data: &'a [u8],
        q: &mut usize,
        body_end: usize,
        extra_size: usize,
        mut kind: EntryKind,
        data_size: u64,
        data_offset: Option<usize>,
        split_before: bool,
        split_after: bool,
    ) -> Result<Entry<'a>> {
        let fflags = vint::read_r(data, q)?;
        let size_unknown = fflags & 0x08 != 0;
        let mut unpacked = vint::read_r(data, q)?;
        if size_unknown {
            // 0x0008: unpacked size field is present but must be ignored;
            // extraction runs until the end of the compression stream.
            unpacked = 0;
        }
        let _attrs = vint::read_r(data, q)?;
        let mtime = if fflags & 0x02 != 0 {
            let end = q
                .checked_add(4)
                .ok_or(Error::Malformed("offset overflow"))?;
            if end > body_end {
                return Err(Error::Truncated);
            }
            let m = u32::from_le_bytes(data[*q..end].try_into().unwrap());
            *q = end;
            Some(m)
        } else {
            None
        };
        let file_crc = if fflags & 0x04 != 0 {
            let end = q
                .checked_add(4)
                .ok_or(Error::Malformed("offset overflow"))?;
            if end > body_end {
                return Err(Error::Truncated);
            }
            let c = u32::from_le_bytes(data[*q..end].try_into().unwrap());
            *q = end;
            Some(c)
        } else {
            None
        };
        // Compression information vint (technote: version/solid/method/dict).
        // Masks per RARLAB technote: 0x003f version, 0x0040 solid,
        // 0x0380 method (bits 7-9), 0x7c00 dictionary bits 10-14,
        // 0xf8000 dictionary refinement bits 15-19 (compression v1 only),
        // 0x100000 "v1 fields, v0 algorithm" bit (v1 only).
        let cinfo = vint::read_r(data, q)?;
        let compression_version = (cinfo & 0x3f) as u8;
        let solid = cinfo & 0x40 != 0;
        let method = ((cinfo >> 7) & 0x07) as u8;
        // Dictionary size: 128 KB base, shifted by the 4-bit field
        // (libarchive masks & 15 and rejects > 64 MB).
        let mut dictionary_size = (128u64 * 1024) << ((cinfo >> 10) & 0x0f);
        if compression_version >= 1 {
            // bits 15-19 refine the dictionary size (RAR 7.x)
            dictionary_size += dictionary_size * ((cinfo >> 15) & 0x1f) / 32;
        }
        if dictionary_size > 64 * 1024 * 1024 {
            return Err(Error::Malformed("declared dictionary size not supported"));
        }
        let host_os = vint::read_r(data, q)? as u8;

        let name_len = vint::read_r(data, q)? as usize;
        let name_end = q
            .checked_add(name_len)
            .ok_or(Error::Malformed("offset overflow"))?;
        if name_end > body_end {
            return Err(Error::Truncated);
        }
        let name = String::from_utf8_lossy(&data[*q..name_end]);
        *q = name_end;

        // Extra area sits at the END of the header (after the name).
        let mut encrypted = false;
        if extra_size > 0 {
            let extra_end = q
                .checked_add(extra_size)
                .ok_or(Error::Malformed("extra area overflow"))?;
            if extra_end != body_end {
                return Err(Error::Malformed("extra area must end at header end"));
            }
            let mut e = *q;
            while e < extra_end {
                let size = vint::read_r(data, &mut e)? as usize;
                let rec_end = e
                    .checked_add(size)
                    .ok_or(Error::Malformed("extra record overflow"))?;
                if rec_end > extra_end {
                    return Err(Error::Malformed("extra record overruns area"));
                }
                let rec_type = vint::read_r(data, &mut e)?;
                match rec_type {
                    0x01 => {
                        // file encryption record
                        encrypted = true;
                    }
                    0x05
                    // redirection record: symlink / junction / hardlink
                    if e < rec_end =>
                    {
                        let redir_type = vint::read_r(data, &mut e)?;
                        match redir_type {
                            1..=3 => kind = EntryKind::Symlink,
                            4 => kind = EntryKind::Hardlink,
                            _ => {}
                        }
                    }
                    _ => {} // hash, time, version, owner, ...
                }
                e = rec_end;
            }
            *q = extra_end;
        }

        let is_dir = fflags & 0x01 != 0;
        Ok(Entry {
            name,
            kind: if is_dir { EntryKind::Directory } else { kind },
            unpacked_size: unpacked,
            size_unknown,
            packed_size: data_size,
            method,
            compression_version,
            dictionary_size,
            solid,
            host_os,
            crc32: file_crc,
            mtime,
            encrypted,
            split_before,
            split_after,
            data_offset,
        })
    }

    // ---------------------------------------------------------------- RAR4

    fn parse_rar4(&mut self) -> Result<()> {
        // RAR4 header CRCs are CRC-16 (low half of CRC-32) over a region
        // whose exact length depends on block flags; real-world damage here
        // is common and non-fatal, so we parse without verifying.
        let data = self.data;
        let mut p = 7usize; // past marker block
        while p
            .checked_add(7)
            .ok_or(Error::Malformed("offset overflow"))?
            <= data.len()
        {
            let btype = data[p + 2];
            let flags = u16::from_le_bytes([data[p + 3], data[p + 4]]) as u32;
            let hsize = u16::from_le_bytes([data[p + 5], data[p + 6]]) as usize;
            if hsize < 7 {
                return Err(Error::Malformed("rar4 header too small"));
            }
            let body_end = p
                .checked_add(hsize)
                .ok_or(Error::Malformed("offset overflow"))?;
            if body_end > data.len() {
                return Err(Error::Truncated);
            }
            // HD_ADD_SIZE_PRESENT appends data to *non-file* headers
            // (COMM/SUB/...). File headers carry pack_size explicitly and
            // never contain an ADD_SIZE field, even when bit 0x8000 is set;
            // their data area is pack_size bytes right after the header.
            let mut skip = if flags & 0x8000 != 0 && btype != 0x74 {
                if p + 11 > data.len() {
                    return Err(Error::Truncated);
                }
                u32::from_le_bytes([data[p + 7], data[p + 8], data[p + 9], data[p + 10]]) as usize
            } else {
                0
            };

            if btype == 0x73 {
                // main header: MHD_VOLUME marks a multivolume set member
                self.multivolume = flags & 0x0001 != 0;
            } else if btype == 0x74 {
                // file header
                if p + 32 > body_end {
                    return Err(Error::Truncated);
                }
                let mut pack =
                    u32::from_le_bytes([data[p + 7], data[p + 8], data[p + 9], data[p + 10]])
                        as u64;
                let mut unp =
                    u32::from_le_bytes([data[p + 11], data[p + 12], data[p + 13], data[p + 14]])
                        as u64;
                let file_crc =
                    u32::from_le_bytes([data[p + 16], data[p + 17], data[p + 18], data[p + 19]]);
                let ftime =
                    u32::from_le_bytes([data[p + 20], data[p + 21], data[p + 22], data[p + 23]]);
                let method = data[p + 25];
                let name_len = u16::from_le_bytes([data[p + 26], data[p + 27]]) as usize;
                let attrs =
                    u32::from_le_bytes([data[p + 28], data[p + 29], data[p + 30], data[p + 31]]);
                let mut name_off = p + 32;
                if flags & 0x0100 != 0 {
                    // LHD_LARGE: high halves of pack/unp sizes
                    if name_off + 8 > body_end {
                        return Err(Error::Truncated);
                    }
                    pack |= (u32::from_le_bytes([
                        data[name_off],
                        data[name_off + 1],
                        data[name_off + 2],
                        data[name_off + 3],
                    ]) as u64)
                        << 32;
                    unp |= (u32::from_le_bytes([
                        data[name_off + 4],
                        data[name_off + 5],
                        data[name_off + 6],
                        data[name_off + 7],
                    ]) as u64)
                        << 32;
                    name_off += 8;
                }
                let name_end = name_off
                    .checked_add(name_len)
                    .ok_or(Error::Malformed("offset overflow"))?;
                if name_end > body_end {
                    return Err(Error::Truncated);
                }
                let name = String::from_utf8_lossy(&data[name_off..name_end]);
                // Directory detection: DOS dir attribute, Unix mode dir bit
                // (mode lives in the low 16 bits of attrs when host is Unix),
                // or a trailing '/' in the name. Unix symlink mode 0xA000
                // becomes EntryKind::Symlink.
                let unix_mode = attrs & 0xF000;
                let is_dir = attrs & 0x10 != 0
                    || unix_mode == 0x4000
                    || name.as_bytes().last() == Some(&b'/');
                let rar4_kind = if is_dir {
                    EntryKind::Directory
                } else if unix_mode == 0xA000 {
                    EntryKind::Symlink
                } else {
                    EntryKind::File
                };
                // RAR4 password protection rides on the file header flag
                let encrypted = flags & 0x04 != 0;
                if encrypted {
                    self.encrypted = true;
                }
                self.entries.push(Entry {
                    name,
                    kind: rar4_kind,
                    unpacked_size: unp,
                    size_unknown: false,
                    packed_size: pack,
                    method,
                    compression_version: 0,
                    dictionary_size: 0,
                    // FHD_SOLID
                    solid: flags & 0x10 != 0,
                    host_os: data[p + 15],
                    crc32: Some(file_crc),
                    mtime: Some(ftime),
                    encrypted,
                    // RAR4 LHD_SPLIT_BEFORE/LHD_SPLIT_AFTER
                    split_before: flags & 0x01 != 0,
                    split_after: flags & 0x02 != 0,
                    // File data always follows the header directly.
                    data_offset: Some(body_end),
                });
                // The next header sits after this file's packed data.
                skip = pack as usize;
            } else if btype == 0x7b {
                break; // end of archive
            }

            p = body_end
                .checked_add(skip)
                .ok_or(Error::Malformed("offset overflow"))?;
        }
        Ok(())
    }
}

#[cfg(test)]
#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec::Vec;

    /// Build one RAR5 block: CRC32 | hsize | header | data area.
    /// `fields` are the body fields after the optional data-size vint.
    fn block(htype: u64, hflags: u64, fields: &[u8], data: Option<&[u8]>) -> Vec<u8> {
        let mut full = Vec::new();
        vint::write(&mut full, htype);
        vint::write(&mut full, hflags);
        if let Some(d) = data {
            vint::write(&mut full, d.len() as u64);
        }
        full.extend_from_slice(fields);
        let mut crc_region = Vec::new();
        vint::write(&mut crc_region, full.len() as u64);
        crc_region.extend_from_slice(&full);
        let mut out = Vec::new();
        out.extend_from_slice(&crc32(&crc_region).to_le_bytes());
        out.extend_from_slice(&crc_region);
        if let Some(d) = data {
            out.extend_from_slice(d);
        }
        out
    }

    fn main_block(volnum: Option<u64>) -> Vec<u8> {
        let mut fields = Vec::new();
        match volnum {
            None => vint::write(&mut fields, 0x01),
            Some(n) => {
                vint::write(&mut fields, 0x03);
                vint::write(&mut fields, n);
            }
        }
        block(1, 0, &fields, None)
    }

    fn file_block(name: &str, crc: u32, total: u64, chunk: &[u8], split: u64) -> Vec<u8> {
        let mut fields = Vec::new();
        vint::write(&mut fields, 0x04); // fflags: file CRC present
        vint::write(&mut fields, total); // unpacked size (total, in every volume)
        vint::write(&mut fields, 0); // attrs
        fields.extend_from_slice(&crc.to_le_bytes());
        vint::write(&mut fields, 0); // cinfo: version 0, store, 128 KB dict
        vint::write(&mut fields, 0); // host os: windows
        vint::write(&mut fields, name.len() as u64);
        fields.extend_from_slice(name.as_bytes());
        block(2, 0x02 | split, &fields, Some(chunk))
    }

    fn end_block() -> Vec<u8> {
        let mut fields = Vec::new();
        vint::write(&mut fields, 0);
        block(5, 0, &fields, None)
    }

    fn two_volumes() -> (Vec<u8>, Vec<u8>, Vec<u8>) {
        let data: Vec<u8> = (0..100u8).map(|i| i.wrapping_mul(37)).collect();
        let crc = crc32(&data);
        let mut v1 = Vec::new();
        v1.extend_from_slice(MAGIC5);
        v1.extend_from_slice(&main_block(None));
        v1.extend_from_slice(&file_block(
            "a.bin",
            crc,
            data.len() as u64,
            &data[..60],
            0x10,
        ));
        v1.extend_from_slice(&end_block());
        let mut v2 = Vec::new();
        v2.extend_from_slice(MAGIC5);
        v2.extend_from_slice(&main_block(Some(1)));
        v2.extend_from_slice(&file_block(
            "a.bin",
            crc,
            data.len() as u64,
            &data[60..],
            0x08,
        ));
        v2.extend_from_slice(&end_block());
        (v1, v2, data)
    }

    #[test]
    fn multivolume_stored_reassembly() {
        let (v1, v2, data) = two_volumes();
        let arc = Archive::open_multi(&[&v1, &v2]).unwrap();
        assert!(arc.multivolume);
        assert_eq!(arc.volume_number, None);
        assert_eq!(arc.entries().len(), 1);
        let e = &arc.entries()[0];
        assert_eq!(e.name, "a.bin");
        assert!(
            !e.split_before && !e.split_after,
            "reassembled entry must be whole"
        );
        assert_eq!(e.packed_size, 100);
        assert_eq!(arc.extract(e).unwrap(), data);
    }

    #[test]
    fn multivolume_single_volume_reports_split() {
        let (v1, _v2, _data) = two_volumes();
        let arc = Archive::open(&v1).unwrap();
        assert!(arc.multivolume);
        assert!(arc.entries()[0].split_after);
        assert!(matches!(
            arc.extract(&arc.entries()[0]),
            Err(Error::Unsupported(_))
        ));
    }

    #[test]
    fn multivolume_rejects_out_of_order() {
        let (v1, v2, _data) = two_volumes();
        assert!(matches!(
            Archive::open_multi(&[&v2, &v1]),
            Err(Error::Malformed("volumes out of order"))
        ));
    }

    #[test]
    fn multivolume_rejects_incomplete_set() {
        let (v1, _v2, _data) = two_volumes();
        assert!(matches!(Archive::open_multi(&[&v1]), Err(Error::Truncated)));
    }

    // ------------------------------------------------------------ RAR4

    /// RAR4 block: CRC-16 (low half of CRC-32) | type | flags | hsize | body.
    fn rar4_block(btype: u8, flags: u16, body: &[u8]) -> Vec<u8> {
        let hsize = (7 + body.len()) as u16;
        let mut hdr = Vec::new();
        hdr.push(btype);
        hdr.extend_from_slice(&flags.to_le_bytes());
        hdr.extend_from_slice(&hsize.to_le_bytes());
        hdr.extend_from_slice(body);
        let crc = (crc32(&hdr) & 0xFFFF) as u16;
        let mut out = crc.to_le_bytes().to_vec();
        out.extend_from_slice(&hdr);
        out
    }

    /// RAR4 main header declaring a multivolume set (MHD_VOLUME).
    fn rar4_main() -> Vec<u8> {
        rar4_block(0x73, 0x0001, &[0; 6])
    }

    /// RAR4 file header body: pack/unp sizes, host, crc, time, ver, method,
    /// name length, attrs, name.
    fn rar4_file_body(name: &str, pack: u32, unp: u32, method: u8, crc: u32) -> Vec<u8> {
        let mut b = Vec::new();
        b.extend_from_slice(&pack.to_le_bytes());
        b.extend_from_slice(&unp.to_le_bytes());
        b.push(2); // host os: unix
        b.extend_from_slice(&crc.to_le_bytes());
        b.extend_from_slice(&0u32.to_le_bytes()); // file time
        b.push(29); // unpack version 2.9
        b.push(method);
        b.extend_from_slice(&(name.len() as u16).to_le_bytes());
        b.extend_from_slice(&0u32.to_le_bytes()); // attrs
        b.extend_from_slice(name.as_bytes());
        b
    }

    fn rar4_end() -> Vec<u8> {
        rar4_block(0x7b, 0, &[])
    }

    fn rar4_two_volumes() -> (Vec<u8>, Vec<u8>, Vec<u8>) {
        let data: Vec<u8> = (0..100u8).map(|i| i.wrapping_mul(37)).collect();
        let crc = crc32(&data);
        let mut v1 = Vec::new();
        v1.extend_from_slice(MAGIC4);
        v1.extend_from_slice(&rar4_main());
        v1.extend_from_slice(&rar4_block(
            0x74,
            0x0002, // LHD_SPLIT_AFTER
            &rar4_file_body("a.bin", 60, 100, METHOD_STORE, crc),
        ));
        v1.extend_from_slice(&data[..60]);
        v1.extend_from_slice(&rar4_end());
        let mut v2 = Vec::new();
        v2.extend_from_slice(MAGIC4);
        v2.extend_from_slice(&rar4_main());
        v2.extend_from_slice(&rar4_block(
            0x74,
            0x0001, // LHD_SPLIT_BEFORE
            &rar4_file_body("a.bin", 40, 100, METHOD_STORE, crc),
        ));
        v2.extend_from_slice(&data[60..]);
        v2.extend_from_slice(&rar4_end());
        (v1, v2, data)
    }

    #[test]
    fn multivolume_rar4_stored_reassembly() {
        let (v1, v2, data) = rar4_two_volumes();
        let arc = Archive::open_multi(&[&v1, &v2]).unwrap();
        assert!(arc.multivolume);
        assert_eq!(arc.entries().len(), 1);
        let e = &arc.entries()[0];
        assert_eq!(e.name, "a.bin");
        assert!(
            !e.split_before && !e.split_after,
            "reassembled entry must be whole"
        );
        assert_eq!(e.packed_size, 100);
        assert_eq!(arc.extract(e).unwrap(), data);
    }

    #[test]
    fn multivolume_rar4_single_volume_reports_split() {
        let (v1, _v2, _data) = rar4_two_volumes();
        let arc = Archive::open(&v1).unwrap();
        assert!(arc.multivolume);
        assert!(arc.entries()[0].split_after);
        assert!(matches!(
            arc.extract(&arc.entries()[0]),
            Err(Error::Unsupported(_))
        ));
    }

    #[test]
    fn multivolume_rar4_rejects_incomplete_set() {
        let (v1, _v2, _data) = rar4_two_volumes();
        assert!(matches!(Archive::open_multi(&[&v1]), Err(Error::Truncated)));
    }
}
