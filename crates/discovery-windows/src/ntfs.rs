// SPDX-License-Identifier: Apache-2.0

//! NTFS on-disk structure parsing.
//!
//! Everything here reads little-endian values out of raw buffers. NTFS internals
//! are not exposed by the `windows` crate, so the layouts below are parsed by hand
//! against the documented on-disk formats.

use alloc::string::{String, ToString};
use alloc::vec::Vec;

// Kept as a complete reference set for the attribute types and namespaces NTFS
// defines, even where this indexer only needs one of them today.
pub const ATTR_STANDARD_INFORMATION: u32 = 0x10;
pub const ATTR_ATTRIBUTE_LIST: u32 = 0x20;
pub const ATTR_FILE_NAME: u32 = 0x30;
pub const ATTR_DATA: u32 = 0x80;

/// `$FILE_NAME` namespace values. Higher-quality names win.
pub const NAMESPACE_POSIX: u8 = 0;
pub const NAMESPACE_WIN32: u8 = 1;
pub const NAMESPACE_DOS: u8 = 2;
pub const NAMESPACE_WIN32_AND_DOS: u8 = 3;

/// `FILE_ATTRIBUTE_REPARSE_POINT` and the USN reason flags live in
/// `discovery` (the shared change vocabulary — one type, one home). These
/// re-exports keep the `ntfs::…` paths working.
pub use rgy_discovery::{
    FILE_ATTRIBUTE_REPARSE_POINT, USN_REASON_FILE_CREATE, USN_REASON_FILE_DELETE,
    USN_REASON_JOURNAL_DELETE, USN_REASON_PATH_RELEVANT, USN_REASON_RENAME_DUPLICATE_SOURCE,
    USN_REASON_RENAME_DUPLICATE_TARGET, USN_REASON_RENAME_NEW_NAME, USN_REASON_RENAME_OLD_NAME,
    UsnRecord,
};

#[inline]
pub fn u16le(b: &[u8], off: usize) -> u16 {
    if off + 2 > b.len() {
        return 0;
    }
    u16::from_le_bytes([b[off], b[off + 1]])
}

#[inline]
pub fn u32le(b: &[u8], off: usize) -> u32 {
    if off + 4 > b.len() {
        return 0;
    }
    u32::from_le_bytes([b[off], b[off + 1], b[off + 2], b[off + 3]])
}

#[inline]
pub fn u64le(b: &[u8], off: usize) -> u64 {
    if off + 8 > b.len() {
        return 0;
    }
    let mut a = [0u8; 8];
    a.copy_from_slice(&b[off..off + 8]);
    u64::from_le_bytes(a)
}

/// Decode a little-endian UTF-16 run (the on-disk form NTFS uses for names).
fn utf16le_to_string(bytes: &[u8]) -> String {
    let units: Vec<u16> = bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|c| u16::from_le_bytes(*c))
        .collect();
    String::from_utf16_lossy(&units)
}

/// A single name for a file record, straight from a `$FILE_NAME` attribute.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileNameEntry {
    pub parent_frn: u64,
    pub namespace: u8,
    pub file_size: u64,
    pub file_attributes: u32,
    /// `ReparsePointTag` from the same value. Zero unless
    /// `FILE_ATTRIBUTE_REPARSE_POINT` is set in `file_attributes`.
    pub reparse_tag: u32,
    pub name: String,
}

impl FileNameEntry {
    /// Lower is better. Win32 names are the ones Explorer shows.
    fn namespace_rank(&self) -> u8 {
        match self.namespace {
            NAMESPACE_WIN32 => 0,
            NAMESPACE_WIN32_AND_DOS => 1,
            NAMESPACE_POSIX => 2,
            _ => 3,
        }
    }

    /// A junction, symlink, or mount point — its path string is the *target*
    /// name recorded by the reparse tag, not something the indexer can resolve.
    pub fn is_reparse_point(&self) -> bool {
        self.file_attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0
    }
}

/// Header of an NTFS file record. The attribute array follows at `attrs_offset`.
///
/// A faithful mirror of the on-disk header; the unused fields are kept so the
/// struct still round-trips what NTFS stores.
#[derive(Debug, Clone, Copy)]
pub struct FileRecordHeader {
    pub attrs_offset: u16,
    pub bytes_in_use: u32,
    pub bytes_allocated: u32,
    pub flags: u16,
    pub sequence_number: u16,
    /// Base file reference at 0x20: nonzero on an extension record
    /// (`$ATTRIBUTE_LIST` spill target) and names the owning base FRN.
    pub base_frn: u64,
}

pub const MFT_RECORD_IN_USE: u16 = 0x0001;
pub const MFT_RECORD_IS_DIRECTORY: u16 = 0x0002;

pub fn parse_file_record(buf: &[u8]) -> Option<FileRecordHeader> {
    if buf.len() < 48 || &buf[0..4] != b"FILE" {
        return None;
    }
    Some(FileRecordHeader {
        attrs_offset: u16le(buf, 0x14),
        bytes_in_use: u32le(buf, 0x18),
        bytes_allocated: u32le(buf, 0x1c),
        flags: u16le(buf, 0x16),
        sequence_number: u16le(buf, 0x10),
        base_frn: u64le(buf, 0x20),
    })
}

/// Bytes per sector used by MFT update-sequence fixups.
///
/// Always 512, regardless of the volume's own sector or cluster size. NTFS
/// protects `$MFT` records at the record's granularity, not the volume's, so a
/// volume reporting 4 KiB `BytesPerSector` still scrambles the last two bytes
/// of each 512-byte half.
///
/// This was a parameter once, and that was a real defect. `scan_via_runs`
/// passed the volume's `BytesPerSector`, so `apply_fixups` became a **silent
/// no-op** on any volume reporting 512 bytes or more: `end = i * sector_size`
/// overshot the record on the very first iteration and the loop broke having
/// restored nothing. It was invisible because the parent FRN lives at offset 0,
/// inside the untouched first sector, so paths still resolved — only the name
/// at the tail of the `$FILE_NAME` value was scrambled, which is exactly the
/// "plausible but wrong" failure an index cannot afford. Correcting the call
/// site would have left the mistake one edit away from returning, so the
/// constant is internal and the caller can no longer get it wrong.
pub const MFT_SECTOR_SIZE: usize = 512;

/// Reapply the MFT update-sequence fixups.
///
/// NTFS parks the last two bytes of every sector at the head of the record and
/// overwrites each sector's tail with an update sequence number. Any attribute
/// value that straddles a sector boundary reads as scrambled bytes until those
/// are restored, which shows up as garbled filenames.
///
/// Takes no sector size: see [`MFT_SECTOR_SIZE`] for why that is not a choice.
///
/// Returns `false` when a protected sector tail does not carry the record's
/// USN (Zen's bulk walk logs "bad US %x != %x" and abandons the record
/// — `FUN_1400d0820`), or when the USA claims more sectors than the buffer
/// holds. Matching sectors are still restored before returning `false`, so the
/// single-record FSCTL path can ignore the result and keep its historically
/// tolerant behaviour; the bulk walk must skip the record.
#[must_use]
pub fn apply_fixups(buf: &mut [u8]) -> bool {
    if buf.len() < 8 {
        return true;
    }
    let usa_off = u16le(buf, 0x04) as usize;
    let usa_count = u16le(buf, 0x06) as usize;
    if usa_count == 0 {
        return true;
    }
    if usa_off + usa_count * 2 > buf.len() {
        return false;
    }

    let usn = u16le(buf, usa_off);
    let mut ok = true;
    for i in 1..usa_count {
        let end = i * MFT_SECTOR_SIZE;
        if end < 2 || end > buf.len() {
            // The USA promised more sectors than the record holds.
            ok = false;
            break;
        }
        let pos = end - 2;
        if u16le(buf, pos) == usn {
            let saved = u16le(buf, usa_off + i * 2);
            buf[pos..pos + 2].copy_from_slice(&saved.to_le_bytes());
        } else {
            // RE abandons the whole record on a USN tail mismatch.
            ok = false;
        }
    }
    ok
}

/// One `$FILE_NAME` attribute value.
///
/// The in-value `FileNameLength` byte at 0x40 is a **character** count (SDK
/// `FILE_NAME_INFORMATION`); the name itself is UTF-16LE, so the byte length is
/// `2 * FileNameLength`. That is authoritative when it fits — unlike
/// `ValueLength - 66` it ignores trailing padding past the name. When the count
/// is missing, zero, or does not fit the value (corrupt record), fall back to
/// `ValueLength - 66` (even-rounded).
fn parse_file_name_value(v: &[u8], value_len: usize) -> Option<FileNameEntry> {
    const FILE_NAME_HEADER_LEN: usize = 66;
    if v.len() < FILE_NAME_HEADER_LEN {
        return None;
    }
    let namespace = v[65];

    // FileNameLength is at 0x40 (64), namespace at 0x41 (65).
    let mut name_len = (v[0x40] as usize).saturating_mul(2);
    if name_len == 0
        || !name_len.is_multiple_of(2)
        || name_len > value_len.saturating_sub(FILE_NAME_HEADER_LEN)
        || name_len > v.len() - FILE_NAME_HEADER_LEN
    {
        name_len = value_len.saturating_sub(FILE_NAME_HEADER_LEN);
        name_len = name_len.min(v.len() - FILE_NAME_HEADER_LEN);
        // Names are UTF-16, so the length is always even.
        name_len -= name_len % 2;
    }
    if name_len == 0 {
        return None;
    }

    let name = utf16le_to_string(&v[FILE_NAME_HEADER_LEN..FILE_NAME_HEADER_LEN + name_len]);
    if name.is_empty() {
        return None;
    }
    Some(FileNameEntry {
        parent_frn: u64le(v, 0),
        namespace,
        file_size: u64le(v, 0x30),
        file_attributes: u32le(v, 0x38),
        // 0x3C is ReparsePointTag for reparse entries and EaSize for the rest;
        // only meaningful when FILE_ATTRIBUTE_REPARSE_POINT is set.
        reparse_tag: u32le(v, 0x3C),
        name,
    })
}

/// A resident attribute located inside a record buffer.
struct ResidentAttr<'a> {
    attr_type: u32,
    value: &'a [u8],
    value_len: usize,
}

/// Linear scan of the attribute array in a single-record buffer.
///
/// This is correct for records that fit in one MFT record (the overwhelming
/// majority). Multi-block records additionally need `$ATTRIBUTE_LIST` chasing,
/// which `parse_multi_block` handles; callers should prefer both and dedupe.
fn scan_attributes<'a>(buf: &'a [u8], start: usize, end: usize) -> Vec<ResidentAttr<'a>> {
    let mut out = Vec::new();
    let mut off = start;
    while off + 16 <= end && off + 16 <= buf.len() {
        let attr_type = u32le(buf, off);
        if attr_type == 0xFFFF_FFFF {
            break; // end-of-attributes marker
        }
        let length = u32le(buf, off + 4) as usize;
        if length < 16 || off + length > buf.len() || off + length > end {
            break;
        }
        let non_resident = buf[off + 8];
        if non_resident == 0 {
            let name_len = buf[off + 9] as usize;
            let value_len = u32le(buf, off + 16) as usize;
            // ValueOffset (attr+0x14) is the authoritative value position, never
            // NameOffset arithmetic. That matters for nameless attributes whose
            // NameOffset is 0 (a real layout: C: FRN 620545) and for attributes
            // with padding before the value. ValueOffset == 0x18 for the
            // canonical nameless case; records like FRN 620545 leave BOTH
            // offsets 0, and the old rule hardcoded 0x18 only after checking
            // NameLength — trusting `name_off + name_len * 2` put the pointer on
            // the attribute header and read the type code as a parent FRN.
            let mut value_off = u16le(buf, off + 20) as usize;
            if value_off == 0 && name_len == 0 {
                value_off = 0x18;
            }
            let value_start = off + value_off;
            if value_off != 0 && value_start + value_len <= buf.len() {
                out.push(ResidentAttr {
                    attr_type,
                    value: &buf[value_start..value_start + value_len],
                    value_len,
                });
            }
        }
        // Length is padded up to 8 bytes.
        off += (length + 7) & !7;
    }
    out
}

/// Extract every `$FILE_NAME` value from a single-record buffer.
///
/// Only the linear attribute array is walked. Records too large for one block
/// keep their names behind an `$ATTRIBUTE_LIST` in sibling records; chasing those
/// needs a second record read, which the caller drives via `attrs_split()`.
/// Attempting it here without that read produced entries whose parent field held
/// an attribute type code instead of a file reference.
pub fn parse_file_names(buf: &[u8], _own_frn: u64) -> Vec<FileNameEntry> {
    let Some(hdr) = parse_file_record(buf) else {
        return Vec::new();
    };
    let end = (hdr.bytes_in_use as usize).min(buf.len());
    scan_attributes(buf, hdr.attrs_offset as usize, end)
        .into_iter()
        .filter(|a| a.attr_type == ATTR_FILE_NAME)
        .filter_map(|a| parse_file_name_value(a.value, a.value_len))
        .collect()
}

/// The record's last-write time from its resident `$STANDARD_INFORMATION`
/// (value offset 0x08, a FILETIME), as unix seconds — the `mtime:` filter
/// and the name index's per-entry mtime. `None` when the attribute is
/// missing or non-resident (rare; a non-resident SI would need the run
/// chase, which the index path does not do).
pub fn standard_info_mtime(buf: &[u8]) -> Option<u32> {
    let hdr = parse_file_record(buf)?;
    let end = (hdr.bytes_in_use as usize).min(buf.len());
    let si = scan_attributes(buf, hdr.attrs_offset as usize, end)
        .into_iter()
        .find(|a| a.attr_type == ATTR_STANDARD_INFORMATION)?;
    if si.value_len < 0x10 {
        return None;
    }
    let ft = u64le(si.value, 0x08);
    // FILETIME is 100 ns ticks since 1601-01-01; unix epoch is 11644473600 s later.
    const EPOCH_DIFF: u64 = 11_644_473_600;
    let secs = ft / 10_000_000;
    Some(secs.saturating_sub(EPOCH_DIFF).min(u32::MAX as u64) as u32)
}

/// True when the record's attributes spill into sibling records.
pub fn attrs_split(buf: &[u8]) -> bool {
    let Some(hdr) = parse_file_record(buf) else {
        return false;
    };
    if hdr.bytes_allocated as usize > buf.len() {
        return true;
    }
    let end = (hdr.bytes_in_use as usize).min(buf.len());
    scan_attributes(buf, hdr.attrs_offset as usize, end)
        .into_iter()
        .any(|a| a.attr_type == ATTR_ATTRIBUTE_LIST)
}

/// One row of an `$ATTRIBUTE_LIST`.
///
/// The list names every attribute of a record, including those that no longer fit
/// in the record and live in a sibling "base" record. Following those siblings is
/// how a file whose name set is too large still gets indexed.
///
/// The trailing `AttrId` field is an instance id, **not** a byte offset into the
/// target record. The attribute is located there by type and name, which is why
/// this struct does not carry an offset.
#[derive(Debug, Clone, Copy)]
pub struct AttrListEntry {
    pub attr_type: u32,
    /// Low 48 bits are the record number, high 16 the sequence.
    pub mft_reference: u64,
    /// Zero for the attributes that hold the names; nonzero only for run data.
    pub start_vcn: u64,
}

/// The in-record `$ATTRIBUTE_LIST` value, if the record has one.
pub fn attr_list_value(buf: &[u8]) -> Option<&[u8]> {
    let hdr = parse_file_record(buf)?;
    let end = (hdr.bytes_in_use as usize).min(buf.len());
    scan_attributes(buf, hdr.attrs_offset as usize, end)
        .into_iter()
        .find(|a| a.attr_type == ATTR_ATTRIBUTE_LIST)
        .map(|a| a.value)
}

/// Parse an `$ATTRIBUTE_LIST` value into its entries.
///
/// Each entry is 26 bytes minimum: type(4) + length(2) + nameLength(1) +
/// nameOffset(1) + startVcn(8) + mftReference(8) + attrId(2).
pub fn parse_attr_list(v: &[u8]) -> Vec<AttrListEntry> {
    let mut out = Vec::new();
    let mut off = 0usize;
    while off + 26 <= v.len() {
        let attr_type = u32le(v, off);
        if attr_type == 0xFFFF_FFFF {
            break;
        }
        let entry_len = u16le(v, off + 4) as usize;
        if entry_len < 26 || off + entry_len > v.len() {
            break;
        }
        out.push(AttrListEntry {
            attr_type,
            start_vcn: u64le(v, off + 8),
            mft_reference: u64le(v, off + 16),
        });
        off += entry_len;
    }
    out
}

/// Pick the best `$FILE_NAME` from a set: prefer Win32 namespace, then shorter name.
pub fn best_name(names: &[FileNameEntry]) -> Option<FileNameEntry> {
    names
        .iter()
        .min_by(|a, b| {
            a.namespace_rank()
                .cmp(&b.namespace_rank())
                .then_with(|| a.name.len().cmp(&b.name.len()))
        })
        .cloned()
}

/// Root of the volume; file reference 5 is the NTFS root directory.
pub const ROOT_FRN: u64 = 5;

/// Strip the 16-bit sequence number NTFS stores in the high bits of file references.
#[inline]
pub fn frn_number(frn: u64) -> u64 {
    frn & 0x0000_FFFF_FFFF_FFFF
}

// A decoded USN journal record (V2 and V3 share this layout; V4's header
// matches too, with trailing fields we do not need). The type is `UsnRecord`
// in `discovery`, re-exported at the top of this module.

pub const USN_HEADER_LEN: usize = 60;

pub fn parse_usn_record(buf: &[u8]) -> Option<UsnRecord> {
    if buf.len() < USN_HEADER_LEN {
        return None;
    }
    let record_len = u32le(buf, 0) as usize;
    let major = u16le(buf, 4);
    // V0/V1 are unused by NTFS; V2, V3 and V4 all share this header layout.
    if major < 2 || record_len > buf.len() {
        return None;
    }
    // USN_RECORD_V2 tail fields: FileAttributes@52, FileNameLength@56,
    // FileNameOffset@58 (SDK layout, confirmed against Zen's own
    // reader). The previous 52/54 read FileAttributes' two halves as the name
    // fields — names decoded as header garbage on live journals, and every
    // fixture used an empty name so the misread stayed invisible.
    let name_len = u16le(buf, 56) as usize;
    let name_off = u16le(buf, 58) as usize;
    // The name ends inside the record: a record that says otherwise is not
    // a record this reader can trust.
    //
    // A name that is not empty starts after the header. Without that lower
    // bound, an offset of zero decodes the 60-byte header as the file name
    // -- a name that is not a name. An empty name has no offset to check:
    // the journal writes one for a record that renamed nothing.
    if name_off + name_len > buf.len()
        || name_off + name_len > record_len
        || (name_len > 0 && name_off < USN_HEADER_LEN)
    {
        return None;
    }
    let name = utf16le_to_string(&buf[name_off..name_off + name_len]);
    Some(UsnRecord {
        file_reference: u64le(buf, 8),
        parent_reference: u64le(buf, 16),
        usn: u64le(buf, 24),
        reason: u32le(buf, 40),
        file_attributes: u32le(buf, 52),
        name,
    })
}

/// Split a `FSCTL_READ_USN_JOURNAL` output buffer into its resume USN and the
/// record stream that follows it.
///
/// The output is **not** a bare run of records: the first 8 bytes are the USN to
/// resume from. Parsing from offset 0 reads that USN as a record length — on a
/// live journal it is a large number, exceeds the buffer, and the walk stops
/// immediately with zero records, which looks like "nothing changed" rather than
/// a parsing error. The resume USN is authoritative; deriving it as
/// `max(record.usn) + 1` loses the case where a read returns no matching records
/// but still advances.
pub fn split_usn_output(buf: &[u8]) -> Option<(i64, &[u8])> {
    if buf.len() < 8 {
        return None;
    }
    let next = i64::from_le_bytes(buf[0..8].try_into().ok()?);
    Some((next, &buf[8..]))
}

/// Walk a 64 KiB USN read buffer, yielding every well-formed record in it.
pub fn walk_usn_buffer(buf: &[u8]) -> Vec<UsnRecord> {
    let mut out = Vec::new();
    let mut off = 0usize;
    while off + 8 <= buf.len() {
        let record_len = u32le(buf, off) as usize;
        // Guard against a garbage length stalling the walk.
        if record_len < 8 {
            break;
        }
        if off + record_len > buf.len() {
            break;
        }
        if let Some(rec) = parse_usn_record(&buf[off..off + record_len]) {
            out.push(rec);
        }
        off += record_len;
    }
    out
}

/// `READ_USN_JOURNAL_DATA_V1`, the `DeviceIoControl` input for
/// `FSCTL_READ_USN_JOURNAL`.
///
/// Layout, from the `windows` crate's `READ_USN_JOURNAL_DATA_V1`:
/// `StartUsn` is **i64**, `Timeout` and `BytesToWaitFor` are **u64**, and
/// `UsnJournalID` is **u64**. V0 is the same without the trailing version
/// window and is 40 bytes — the 0x28 Zen passes. This struct was
/// previously built from an all-DWORD layout and produced 32 bytes, which the
/// driver rejects with `ERROR_INVALID_USER_BUFFER`.
#[derive(Debug, Clone, Copy, Default)]
pub struct ReadUsnJournalData {
    /// `-1` is not a sentinel here; `0` is clamped to the journal's
    /// `lowest_valid_usn` by the caller before it is used.
    pub start_usn: i64,
    pub reason_mask: u32,
    pub return_only_on_close: u32,
    pub timeout: u64,
    pub bytes_to_wait_for: u64,
    pub usn_journal_id: u64,
    pub min_major_version: u16,
    pub max_major_version: u16,
}

impl ReadUsnJournalData {
    pub fn to_bytes(self) -> [u8; 44] {
        let mut b = [0u8; 44];
        b[0x00..0x08].copy_from_slice(&self.start_usn.to_le_bytes());
        b[0x08..0x0c].copy_from_slice(&self.reason_mask.to_le_bytes());
        b[0x0c..0x10].copy_from_slice(&self.return_only_on_close.to_le_bytes());
        b[0x10..0x18].copy_from_slice(&self.timeout.to_le_bytes());
        b[0x18..0x20].copy_from_slice(&self.bytes_to_wait_for.to_le_bytes());
        b[0x20..0x28].copy_from_slice(&self.usn_journal_id.to_le_bytes());
        b[0x28..0x2a].copy_from_slice(&self.min_major_version.to_le_bytes());
        b[0x2a..0x2c].copy_from_slice(&self.max_major_version.to_le_bytes());
        b
    }
}

/// Journal state from `FSCTL_QUERY_USN_JOURNAL` (0x900b4).
///
/// The journal identity and the USN window it currently covers. A reader that
/// resumes from a stale `start_usn` outside this window sees a gap it cannot
/// recover from, which is why the identity is checked before every read.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct UsnJournalData {
    /// Changes when the journal is deleted and recreated.
    pub usn_journal_id: u64,
    pub first_usn: i64,
    pub next_usn: i64,
    pub lowest_valid_usn: i64,
    pub max_usn: i64,
    pub min_supported_major_version: u16,
    pub max_supported_major_version: u16,
}

impl UsnJournalData {
    /// Parse the `USN_JOURNAL_DATA` buffer. Fields past the buffer's end are
    /// left at their defaults rather than reading past it.
    pub fn parse(buf: &[u8]) -> Option<Self> {
        if buf.len() < 0x38 {
            return None;
        }
        Some(UsnJournalData {
            usn_journal_id: u64le(buf, 0x00),
            first_usn: u64le(buf, 0x08) as i64,
            next_usn: u64le(buf, 0x10) as i64,
            lowest_valid_usn: u64le(buf, 0x18) as i64,
            max_usn: u64le(buf, 0x20) as i64,
            // V1 and later report the supported version window; V0 does not.
            min_supported_major_version: if buf.len() >= 0x3c {
                u16le(buf, 0x38)
            } else {
                0
            },
            max_supported_major_version: if buf.len() >= 0x3c {
                u16le(buf, 0x3a)
            } else {
                0
            },
        })
    }

    /// A `start_usn` below `lowest_valid_usn` sits in a hole the journal has
    /// already recycled — reading from it returns nothing useful.
    pub fn covers(&self, start_usn: i64) -> bool {
        start_usn >= self.lowest_valid_usn && start_usn <= self.next_usn
    }
}

/// The documented "first available record" sentinel.
///
/// It is not a position: it is the kernel's way of saying "the first record
/// you can read". The feed starts there when there is no saved position, and
/// a resume point that equals it has lost nothing — the journal may well
/// have recycled records below it.
pub const FIRST_AVAILABLE_USN: i64 = 0x7FFF_FFFF;

/// `NTFS_FILE_RECORD_INPUT` for `FSCTL_GET_NTFS_FILE_RECORD`.
///
/// Despite the conventional `VolumeSerialNumber` + `FileReferenceNumber` layout,
/// the NTFS driver actually takes a single 64-bit file reference number. This
/// matches Zen's `FUN_1400cfe80`, which loads `frn & 0xFFFFFFFFFFFF` into
/// a `DWORDLONG` and passes 8 bytes.
#[derive(Debug, Clone, Copy)]
pub struct FileRecordInput {
    pub file_reference: u64,
}

impl FileRecordInput {
    pub fn to_bytes(self) -> [u8; 8] {
        self.file_reference.to_le_bytes()
    }
}

/// Size of the header the driver prepends to the returned `FILE_RECORD`.
///
/// `FSCTL_GET_NTFS_FILE_RECORD` writes an 8-byte file reference and 4 bytes of
/// context, then the MFT record itself at offset 12. Zen reads the record
/// magic at output+0x0C and the in-use flag at 0x22 = 0x0C + 0x16.
pub const FILE_RECORD_OUTPUT_PREFIX: usize = 12;

/// `NTFS_VOLUME_DATA_BUFFER`, the response to `FSCTL_GET_NTFS_VOLUME_DATA` (0x90064).
///
/// Field offsets follow the SDK struct exactly. Two of them were wrong here for
/// a long time and the damage was silent: `BytesPerFileRecordSegment` was read
/// from 0x5c — the high DWORD of `MftZoneEnd`, which is 0 on any volume whose
/// MFT zone sits below 4 G clusters — and `ClustersPerFileRecordSegment` from
/// 0x25, inside `TotalReserved`. The record size then fell through to a
/// fallback that computed `4096 << 0` and every scan on a 1 KiB-record volume
/// walked four times too few records with a four-times-too-wide stride.
#[derive(Debug, Clone, Copy)]
pub struct NtfsVolumeData {
    pub volume_serial_number: u64,
    /// `BytesPerSector` at 0x28.
    pub bytes_per_sector: u32,
    /// `BytesPerCluster` at 0x2c — verified against the SDK layout.
    pub bytes_per_cluster: u32,
    /// `BytesPerFileRecordSegment` at 0x30. The authoritative MFT record size.
    pub bytes_per_file_record_segment: u32,
    /// `ClustersPerFileRecordSegment` at 0x34. A signed exponent of the
    /// cluster size: `0xFE` means 2⁻² clusters, not 254 clusters.
    pub clusters_per_file_record_segment: u32,
    /// `MftValidDataLength` at 0x38. Bytes of the MFT actually in use; bounds
    /// the record walk.
    pub mft_valid_data_length: u64,
}

impl NtfsVolumeData {
    /// Smallest and largest cluster size NTFS can be formatted with.
    const MIN_CLUSTER: u32 = 512;
    const MAX_CLUSTER: u32 = 65536;
    const DEFAULT_CLUSTER: u32 = 4096;
    /// Fallback MFT record size when the volume's own value is missing or
    /// implausible. 1 KiB is the NTFS default and works on both record sizes.
    const DEFAULT_MFT_RECORD: usize = 1024;

    /// Reject a cluster size the filesystem could not possibly have.
    ///
    /// `BytesPerCluster` is at offset 0x2c — confirmed against the SDK
    /// `NTFS_VOLUME_DATA_BUFFER` layout. The guard stays anyway: a
    /// wrong-but-plausible cluster size would silently mis-scale every
    /// LCN→byte conversion in the run-streaming path, and a value the
    /// filesystem could not have been formatted with is not worth trusting.
    fn valid_cluster(v: u32) -> u32 {
        if (Self::MIN_CLUSTER..=Self::MAX_CLUSTER).contains(&v) && v.is_power_of_two() {
            v
        } else {
            Self::DEFAULT_CLUSTER
        }
    }

    pub fn parse(buf: &[u8]) -> Option<Self> {
        if buf.len() < 0x3c {
            return None;
        }
        let bytes_per_sector = u32le(buf, 0x28);
        let bytes_per_cluster = u32le(buf, 0x2c);
        Some(NtfsVolumeData {
            volume_serial_number: u64le(buf, 0x08),
            bytes_per_sector: if bytes_per_sector == 0 {
                512
            } else {
                bytes_per_sector
            },
            bytes_per_cluster: Self::valid_cluster(bytes_per_cluster),
            bytes_per_file_record_segment: u32le(buf, 0x30),
            clusters_per_file_record_segment: u32le(buf, 0x34),
            mft_valid_data_length: u64le(buf, 0x38),
        })
    }

    /// Number of MFT record slots the volume has allocated.
    pub fn mft_record_count(&self) -> u64 {
        let size = self.bytes_per_file_record_segment();
        if size == 0 {
            0
        } else {
            self.mft_valid_data_length / size as u64
        }
    }

    /// Size in bytes of one MFT file record segment.
    ///
    /// `BytesPerFileRecordSegment` (offset 0x30) is authoritative and is what
    /// the driver reports on every real volume. The `ClustersPerFileRecordSegment`
    /// fallback exists only for a buffer that left the field zero.
    ///
    /// That fallback follows Zen's boot reader (`FUN_1400cfa70`):
    /// `val < 1` means `2^(-val)` **bytes** (0xF6 = -10 -> 1024), `val >= 1`
    /// means that many **clusters** (1 -> cluster, 2 -> 2*cluster). The old
    /// formula read it as a signed exponent of the *cluster size* in both
    /// directions (`cluster * 2^val` / `cluster >> -val`), which computes 8192
    /// where the record is 4096 and "quarter cluster = 1 KiB" where the encoding
    /// actually means 4 bytes — `0xFFFFFFFE` is 2^2 = 4 bytes here, not 1024.
    ///
    /// The candidate is always filtered through [`Self::plausible_record_size`]:
    /// a real record is a power of two in [512, 64 KiB].
    pub fn bytes_per_file_record_segment(&self) -> usize {
        let candidate = if self.bytes_per_file_record_segment > 0 {
            self.bytes_per_file_record_segment as usize
        } else {
            let cluster = self.bytes_per_cluster();
            let val = self.clusters_per_file_record_segment as i32;
            if val < 1 {
                let shift = (-(val as i64)).min(31) as u32;
                1usize << shift
            } else {
                cluster.saturating_mul(val as usize)
            }
        };
        Self::plausible_record_size(candidate)
    }

    /// A record size the filesystem could actually have, or 1 KiB.
    ///
    /// Checked on *both* paths. The tail of `NTFS_VOLUME_DATA_BUFFER` arrives
    /// zeroed on some volumes, so `mft_record_size` reads as 0 and the fallback
    /// runs on an unchecked `ClusterSize` exponent — `1 << 31` computed to
    /// 2 GiB, which made `record_count` come out as 10 and truncated the index
    /// to a handful of records. A real MFT record is a power of two between
    /// 512 bytes and 64 KiB; anything outside that is not a record size.
    fn plausible_record_size(candidate: usize) -> usize {
        if candidate.is_power_of_two() && (512..=65536).contains(&candidate) {
            candidate
        } else {
            Self::DEFAULT_MFT_RECORD
        }
    }

    pub fn bytes_per_cluster(&self) -> usize {
        self.bytes_per_cluster as usize
    }

    /// Bytes per sector, as reported by `FSCTL_GET_NTFS_VOLUME_DATA`.
    ///
    /// Unused, and deliberately left off the `Volume` struct. Nothing in the
    /// indexer may consume it: MFT fixups are fixed at 512 regardless of the
    /// volume (see [`MFT_SECTOR_SIZE`]), so the only thing a caller can do with
    /// this value is pass it somewhere it must not be parameterised. Kept
    /// because it mirrors the on-disk structure, per the housekeeping rule
    /// about the other format constants, and because it is public API of a
    /// published crate: a caller reading the volume's own geometry has
    /// nowhere else to get it.
    pub fn bytes_per_sector(&self) -> usize {
        self.bytes_per_sector as usize
    }
}

/// An extent run of a non-resident attribute, in clusters.
///
/// `start_vcn` is the run's *logical* position in the attribute's data stream.
/// It is what makes the physical extent list usable as a byte-accurate map of
/// the stream: file record N lives at logical offset `N * record_size`, and the
/// physical concatenation of runs is **not** that stream whenever extents are
/// missing, sparse, or out of allocation order. Deriving a record number from a
/// running counter over the physical runs is exactly the bug this field exists
/// to prevent.
///
/// `start_lcn` is the physical start cluster, or `None` for a sparse hole. A
/// hole is real stream space (it reads as zeros) and must advance the logical
/// position without being read — and must never be recorded at the previous
/// run's LCN.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MftRun {
    pub start_vcn: u64,
    pub clusters: u64,
    pub start_lcn: Option<i64>,
}

/// Decodes an NTFS data run list into cluster runs.
///
/// `base_vcn` is the `StartingVCN` of the attribute the run list belongs to.
/// The primary extent's list starts at 0; a run list that overflowed into a
/// sibling MFT record continues at the `StartingVCN` written in that record's
/// attribute header, and its deltas are relative to the previous run's LCN
/// across the whole attribute, not across the fragment.
pub fn decode_data_runs(runs: &[u8], base_vcn: u64) -> Result<Vec<MftRun>, String> {
    let mut out = Vec::new();
    let mut prev_lcn = 0i64;
    let mut vcn = base_vcn;
    let mut i = 0usize;

    while i < runs.len() {
        let header = runs[i];
        if header == 0 {
            break;
        }
        let len_bytes = (header & 0x0f) as usize;
        let off_bytes = (header >> 4) as usize;
        i += 1;

        if len_bytes == 0 || i + len_bytes + off_bytes > runs.len() {
            break;
        }

        let length = decode_run_int(&runs[i..i + len_bytes], false);
        i += len_bytes;

        let mut lcn_delta = 0i64;
        if off_bytes > 0 {
            lcn_delta = decode_run_int(&runs[i..i + off_bytes], true);
            i += off_bytes;
        }

        if length <= 0 {
            continue;
        }

        if off_bytes == 0 {
            // Sparse: stream space with no backing clusters. It occupies VCNs
            // and it does not move the LCN chain — the next run's delta stays
            // relative to the last real LCN.
            out.push(MftRun {
                start_vcn: vcn,
                clusters: length as u64,
                start_lcn: None,
            });
        } else {
            prev_lcn += lcn_delta;
            if prev_lcn < 0 {
                return Err("data run has a negative LCN".to_string());
            }
            out.push(MftRun {
                start_vcn: vcn,
                clusters: length as u64,
                start_lcn: Some(prev_lcn),
            });
        }
        vcn += length as u64;
    }

    if out.is_empty() {
        return Err("empty $MFT data run list".to_string());
    }
    Ok(out)
}

/// Decode a little-endian variable-length integer used in NTFS data runs.
///
/// The length and the offset in a run header are nibbles, so a corrupt
/// record can claim up to 15 bytes. The value only holds eight, so the
/// bytes past the eighth are dropped: shifting by 64 would panic in a debug
/// build, and in a release build the shift wraps and the byte lands in the
/// low bits, which is worse -- it is a wrong number that looks like a right
/// one.
fn decode_run_int(b: &[u8], signed: bool) -> i64 {
    let mut v = 0i64;
    for (i, &byte) in b.iter().enumerate() {
        let shift = 8 * i;
        if shift >= 64 {
            break;
        }
        v |= (byte as i64) << shift;
    }
    if signed && !b.is_empty() && (b[b.len() - 1] & 0x80) != 0 {
        let shift = 8 * b.len().min(8);
        if shift < 64 {
            v -= 1i64 << shift;
        }
    }
    v
}

/// Extract the data runs of the unnamed non-resident `$DATA` attribute from
/// one MFT record.
///
/// Works on any record that holds an extent of that attribute, not just MFT
/// record 0: when the run list overflows, the continuation records each carry
/// an extent whose attribute header's `StartingVCN` says where in the stream
/// its runs begin. The run deltas inside one extent are relative to LCN 0
/// (the previous LCN starts at zero for each extent) — zen's own MFT
/// walker resets its LCN accumulator per extent the same way.
pub fn extract_data_runs(record: &[u8]) -> Result<Vec<MftRun>, String> {
    let hdr = parse_file_record(record).ok_or_else(|| "invalid MFT record header".to_string())?;

    let end = (hdr.bytes_in_use as usize).min(record.len());
    let mut off = hdr.attrs_offset as usize;

    while off + 16 <= end && off + 16 <= record.len() {
        let attr_type = u32le(record, off);
        if attr_type == 0xFFFF_FFFF {
            break;
        }
        let length = u32le(record, off + 4) as usize;
        if length < 16 || off + length > record.len() || off + length > end {
            break;
        }

        let non_resident = record[off + 8];
        let name_len = record[off + 9];

        // Unnamed non-resident $DATA attribute
        if attr_type == ATTR_DATA
            && name_len == 0
            && non_resident != 0
            && off + 0x22 <= record.len()
        {
            // A continuation extent does not start at VCN 0; the header says
            // where it does. Without this every record past the first extent
            // gets the wrong file reference number.
            let base_vcn = u64le(record, off + 0x10);
            let run_off = u16le(record, off + 0x20) as usize;
            if run_off > 0 && off + run_off < off + length {
                let runs_slice = &record[off + run_off..off + length];
                return decode_data_runs(runs_slice, base_vcn);
            }
        }

        off += (length + 7) & !7;
    }

    Err("$DATA data run list not found".to_string())
}

/// Hand-built MFT records for the tests that need them.
///
/// The bytes are the layout the kernel writes: a FILE record with one
/// nameless `$FILE_NAME` at `attrs_offset` 0x38, and `base_frn` at 0x20
/// (0 for a base record, the owner's FRN for an extension record). The
/// indexer and the path resolver are held against these, so a parser that
/// stops understanding the real layout fails here.
#[cfg(test)]
pub(crate) mod testutil {
    use alloc::vec;
    use alloc::vec::Vec;

    pub(crate) fn build_record(base_frn: u64, parent_frn: u64, ns: u8, name: &str) -> Vec<u8> {
        let name_bytes = name.encode_utf16().count() * 2;
        let value_len = 66 + name_bytes;
        let attr_len = 0x18 + value_len;
        let need = 0x38 + attr_len;
        let mut rec = vec![0u8; need];
        rec[0..4].copy_from_slice(b"FILE");
        rec[0x14..0x16].copy_from_slice(&0x38u16.to_le_bytes()); // attrs_offset
        rec[0x16..0x18].copy_from_slice(&1u16.to_le_bytes()); // in use
        rec[0x18..0x1c].copy_from_slice(&(need as u32).to_le_bytes()); // bytes_in_use
        rec[0x20..0x28].copy_from_slice(&base_frn.to_le_bytes()); // base file reference

        let attr = &mut rec[0x38..];
        attr[0x00..0x04].copy_from_slice(&super::ATTR_FILE_NAME.to_le_bytes());
        attr[0x04..0x08].copy_from_slice(&(attr_len as u32).to_le_bytes());
        // NameLength/NameOffset stay 0: the canonical nameless layout.
        attr[0x10..0x14].copy_from_slice(&(value_len as u32).to_le_bytes());
        attr[0x18..0x20].copy_from_slice(&parent_frn.to_le_bytes());
        attr[0x18 + 64] = (name_bytes / 2) as u8; // FileNameLength, chars
        attr[0x18 + 65] = ns;
        for (i, c) in name.encode_utf16().enumerate() {
            attr[0x18 + 66 + i * 2..0x18 + 68 + i * 2].copy_from_slice(&c.to_le_bytes());
        }
        rec
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    /// A run header's length and offset are nibbles, so a corrupt record can
    /// claim up to 15 bytes for either. The value only holds eight, and the
    /// bytes past the eighth must be dropped: shifting by 64 panics in a
    /// debug build, and in a release build the shift wraps and the byte
    /// lands in the low bits -- a wrong number that looks like a right one.
    #[test]
    fn a_run_int_longer_than_eight_bytes_does_not_overflow() {
        let nine = [0x01u8, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09];
        assert_eq!(
            decode_run_int(&nine, false),
            0x0807_0605_0403_0201,
            "the first eight bytes are the value"
        );
        assert_eq!(
            decode_run_int(&[0xff; 15], false),
            -1,
            "a full-length field saturates"
        );
        // The eight-byte value is the last one that fits.
        let eight = [0x01u8, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08];
        assert_eq!(decode_run_int(&eight, false), 0x0807_0605_0403_0201);
    }

    /// The run list is read from the disk, so its lengths are the disk's
    /// word: a corrupt record must not make the reader panic.
    #[test]
    fn a_corrupt_run_list_is_refused_not_panicked() {
        // A header that claims 15 length bytes and 15 offset bytes, with
        // only a few bytes behind it.
        let runs = vec![0xFFu8, 0x01, 0x02, 0x03];
        let out = decode_data_runs(&runs, 0);
        assert!(
            out.is_err() || out.is_ok(),
            "the reader answers; it does not panic"
        );
    }

    /// The USN record's name starts after the header. A record that says
    /// otherwise is not a record this reader can trust: an offset of zero
    /// decodes the 60-byte header as the file name.
    #[test]
    fn a_usn_name_inside_the_header_is_refused() {
        let mut buf = vec![0u8; USN_HEADER_LEN + 16];
        buf[0..4].copy_from_slice(&(USN_HEADER_LEN as u32 + 16).to_le_bytes()); // RecordLength
        buf[4..6].copy_from_slice(&3u16.to_le_bytes()); // MajorVersion
        buf[56..58].copy_from_slice(&8u16.to_le_bytes()); // FileNameLength
        buf[58..60].copy_from_slice(&0u16.to_le_bytes()); // FileNameOffset: the trap
        assert!(
            parse_usn_record(&buf).is_none(),
            "a name that starts inside the header is refused"
        );
        // The same record with the name where it belongs parses.
        buf[58..60].copy_from_slice(&(USN_HEADER_LEN as u16).to_le_bytes());
        assert!(
            parse_usn_record(&buf).is_some(),
            "a name after the header is read"
        );
        // An empty name has no offset to check: the journal writes one for
        // a record that renamed nothing.
        let mut empty = vec![0u8; USN_HEADER_LEN];
        empty[0..4].copy_from_slice(&(USN_HEADER_LEN as u32).to_le_bytes());
        empty[4..6].copy_from_slice(&2u16.to_le_bytes());
        assert!(
            parse_usn_record(&empty).is_some(),
            "a record with an empty name is still a record"
        );
    }

    #[test]
    fn file_name_value_roundtrip() {
        let mut v = vec![0u8; 74];
        v[0x30..0x38].copy_from_slice(&1234u64.to_le_bytes());
        v[0x38..0x3c].copy_from_slice(&0x20u32.to_le_bytes());
        // Claims 8 chars but only 4 are there: exercises the ValueLength-66
        // fallback when the in-value count lies.
        v[64] = 8;
        v[65] = NAMESPACE_WIN32;
        let encoded: Vec<u8> = "test".encode_utf16().flat_map(u16::to_le_bytes).collect();
        v[66..66 + encoded.len()].copy_from_slice(&encoded);
        let parsed = parse_file_name_value(&v, v.len()).expect("should parse");
        assert_eq!(parsed.name, "test");
        assert_eq!(parsed.namespace, NAMESPACE_WIN32);
        assert_eq!(parsed.file_size, 1234);
    }

    #[test]
    fn rejects_non_file_magic() {
        assert!(parse_file_record(&[0u8; 128]).is_none());
        let mut b = vec![0u8; 128];
        b[0..4].copy_from_slice(b"FILE");
        assert!(parse_file_record(&b).is_some());
    }

    #[test]
    fn nameless_attribute_with_zero_nameoffset_still_finds_its_value() {
        // Mirrors C: MFT record 620545, whose $FILE_NAME NTFS wrote with
        // NameOffset = 0. An implementation that trusts the field points the
        // value at the attribute header and takes the type code 0x30 for a
        // parent reference.
        let mut attr = vec![0u8; 120];
        attr[0x00..0x04].copy_from_slice(&ATTR_FILE_NAME.to_le_bytes());
        attr[0x04..0x08].copy_from_slice(&120u32.to_le_bytes()); // Length
        // NonResident = 0, NameLength = 0, NameOffset = 0 -- the trap.
        attr[0x0e..0x10].copy_from_slice(&5u16.to_le_bytes()); // Instance
        attr[0x10..0x14].copy_from_slice(&90u32.to_le_bytes()); // ValueLength

        const VALUE: usize = 0x18;
        attr[VALUE..VALUE + 8].copy_from_slice(&0x000A_0000_000A_78C0u64.to_le_bytes());
        attr[VALUE + 64] = 12; // FileNameLength
        attr[VALUE + 65] = NAMESPACE_WIN32;
        for (i, c) in "NEXTMO~1.PNG".encode_utf16().enumerate() {
            attr[VALUE + 66 + i * 2..VALUE + 68 + i * 2].copy_from_slice(&c.to_le_bytes());
        }

        let found = scan_attributes(&attr, 0, attr.len());
        let file_name = found
            .iter()
            .find(|a| a.attr_type == ATTR_FILE_NAME)
            .expect("should expose the $FILE_NAME value");

        let parsed =
            parse_file_name_value(file_name.value, file_name.value_len).expect("should parse");
        assert_eq!(parsed.name, "NEXTMO~1.PNG");
        assert_eq!(frn_number(parsed.parent_frn), 0x0A78C0);
        assert_eq!(parsed.namespace, NAMESPACE_WIN32);
    }

    #[test]
    fn attribute_with_explicit_nameoffset_uses_it() {
        // Record 5's $FILE_NAME does carry NameOffset = 0x18.
        let mut attr = vec![0u8; 96];
        attr[0x00..0x04].copy_from_slice(&ATTR_FILE_NAME.to_le_bytes());
        attr[0x04..0x08].copy_from_slice(&96u32.to_le_bytes());
        attr[0x0a..0x0c].copy_from_slice(&0x18u16.to_le_bytes()); // NameOffset
        attr[0x10..0x14].copy_from_slice(&68u32.to_le_bytes()); // ValueLength
        attr[0x18..0x20].copy_from_slice(&0x0005_0000_0000_0005u64.to_le_bytes());

        let found = scan_attributes(&attr, 0, attr.len());
        let file_name = found
            .iter()
            .find(|a| a.attr_type == ATTR_FILE_NAME)
            .expect("should expose the $FILE_NAME value");
        assert_eq!(frn_number(u64le(file_name.value, 0)), 5);
    }

    #[test]
    fn value_offset_locates_the_value_past_padding() {
        // ValueOffset at attr+0x14 is authoritative. Here it points past
        // padding (0x28, not the 0x18 default); trusting 0x18 or NameOffset
        // arithmetic would read the padding as a parent reference.
        let mut attr = vec![0u8; 128];
        attr[0x00..0x04].copy_from_slice(&ATTR_FILE_NAME.to_le_bytes());
        attr[0x04..0x08].copy_from_slice(&128u32.to_le_bytes()); // Length
        attr[0x14..0x16].copy_from_slice(&0x28u16.to_le_bytes()); // ValueOffset
        attr[0x10..0x14].copy_from_slice(&68u32.to_le_bytes()); // ValueLength
        // Poison the 0x18 gap: a parser that hardcodes 0x18 takes this as the
        // parent reference.
        attr[0x18..0x20].copy_from_slice(&0xDEAD_BEEFu64.to_le_bytes());
        const VALUE: usize = 0x28;
        attr[VALUE..VALUE + 8].copy_from_slice(&0x0005_0000_0000_0005u64.to_le_bytes());
        attr[VALUE + 64] = 1; // FileNameLength: 1 char
        attr[VALUE + 65] = NAMESPACE_WIN32;
        let encoded: Vec<u8> = "x".encode_utf16().flat_map(u16::to_le_bytes).collect();
        attr[VALUE + 66..VALUE + 66 + encoded.len()].copy_from_slice(&encoded);

        let found = scan_attributes(&attr, 0, attr.len());
        let file_name = found
            .iter()
            .find(|a| a.attr_type == ATTR_FILE_NAME)
            .expect("should expose the $FILE_NAME value");
        let parsed =
            parse_file_name_value(file_name.value, file_name.value_len).expect("should parse");
        assert_eq!(frn_number(parsed.parent_frn), 5);
        assert_eq!(parsed.name, "x");
    }

    #[test]
    fn frn_masks_sequence_number() {
        assert_eq!(frn_number(0x0001_0000_0000_0042), 0x42);
        assert_eq!(frn_number(0xABCD_0000_0000_0042), 0x42);
    }

    #[test]
    fn fixups_restore_sector_tails() {
        // Two 512-byte sectors; USA sits at 0x30 and saves the value 0x4242 that
        // was overwritten at offset 510.
        let mut buf = vec![0u8; 1024];
        buf[0x04..0x06].copy_from_slice(&0x0030u16.to_le_bytes()); // usa_offset
        buf[0x06..0x08].copy_from_slice(&3u16.to_le_bytes()); // usa_count: 2 sectors + usn
        buf[0x30..0x32].copy_from_slice(&0xBEEFu16.to_le_bytes()); // update sequence number
        buf[0x32..0x34].copy_from_slice(&0x4242u16.to_le_bytes()); // saved tail of sector 0
        buf[0x34..0x36].copy_from_slice(&0x4343u16.to_le_bytes()); // saved tail of sector 1

        buf[510..512].copy_from_slice(&0xBEEFu16.to_le_bytes()); // scrambled
        buf[1022..1024].copy_from_slice(&0xBEEFu16.to_le_bytes()); // scrambled

        assert!(apply_fixups(&mut buf));

        assert_eq!(u16le(&buf, 510), 0x4242);
        assert_eq!(u16le(&buf, 1022), 0x4343);
    }

    #[test]
    fn fixups_ignore_unmarked_sector_tails() {
        // No update sequence number in the tail. The bytes are left alone, but
        // the bulk walk treats this as a bad record ("bad US %x != %x" in
        // FUN_1400d0820) and abandons it — hence the `false` return. The FSCTL
        // single-record path ignores the result and keeps the historically
        // tolerant behaviour.
        let mut buf = vec![0u8; 1024];
        buf[0x04..0x06].copy_from_slice(&0x0030u16.to_le_bytes());
        buf[0x06..0x08].copy_from_slice(&3u16.to_le_bytes());
        buf[0x30..0x32].copy_from_slice(&0xBEEFu16.to_le_bytes());
        buf[0x32..0x34].copy_from_slice(&0x4242u16.to_le_bytes());
        buf[510..512].copy_from_slice(&0x1111u16.to_le_bytes());

        assert!(!apply_fixups(&mut buf));
        assert_eq!(u16le(&buf, 510), 0x1111);
    }

    #[test]
    fn fixups_return_true_on_clean_records() {
        let mut buf = vec![0u8; 1024];
        buf[0x04..0x06].copy_from_slice(&0x0030u16.to_le_bytes());
        buf[0x06..0x08].copy_from_slice(&3u16.to_le_bytes());
        buf[0x30..0x32].copy_from_slice(&0xBEEFu16.to_le_bytes());
        buf[0x32..0x34].copy_from_slice(&0x4242u16.to_le_bytes());
        buf[0x34..0x36].copy_from_slice(&0x4343u16.to_le_bytes());
        buf[510..512].copy_from_slice(&0xBEEFu16.to_le_bytes());
        buf[1022..1024].copy_from_slice(&0xBEEFu16.to_le_bytes());

        assert!(apply_fixups(&mut buf));

        // A record with no USA at all is fine: nothing to restore.
        let mut empty = vec![0u8; 16];
        assert!(apply_fixups(&mut empty));
    }

    #[test]
    fn fixups_fail_on_a_usa_that_promises_too_many_sectors() {
        // usa_count says 5 sectors but the buffer is one record long: the
        // record is truncated or corrupt.
        let mut buf = vec![0u8; 1024];
        buf[0x04..0x06].copy_from_slice(&0x0030u16.to_le_bytes());
        buf[0x06..0x08].copy_from_slice(&6u16.to_le_bytes()); // 5 sectors + usn
        buf[0x30..0x32].copy_from_slice(&0xBEEFu16.to_le_bytes());

        assert!(!apply_fixups(&mut buf));
    }

    #[test]
    fn fixups_restore_every_tail_of_a_4k_sector_volume() {
        // The regression that was missing. Both existing fixup tests passed 512
        // explicitly, so nothing pinned the *invariant* — which is how
        // `scan_via_runs` shipped a call passing the volume's `BytesPerSector`
        // and got away with it: on a 4 KiB-sector volume that made the whole
        // function a no-op, silently, on the primary scan path.
        //
        // A 4096-byte-sector volume is the common case on modern disks, and it
        // is the one that was broken. A 1024-byte MFT record is still protected
        // at 512, so both tails here must come back regardless of what the
        // volume reports. This test cannot take a sector size, which is the
        // point: the mistake is unrepresentable rather than merely fixed.
        let mut buf = vec![0u8; 1024];
        buf[0x04..0x06].copy_from_slice(&0x0030u16.to_le_bytes()); // usa_offset
        buf[0x06..0x08].copy_from_slice(&3u16.to_le_bytes()); // usa_count: 2 sectors + usn
        buf[0x30..0x32].copy_from_slice(&0xBEEFu16.to_le_bytes()); // update sequence number
        buf[0x32..0x34].copy_from_slice(&0x4242u16.to_le_bytes()); // saved tail of sector 0
        buf[0x34..0x36].copy_from_slice(&0x4343u16.to_le_bytes()); // saved tail of sector 1

        // Both sector tails carry the update sequence number, i.e. both were
        // scrambled on disk. These are the bytes a $FILE_NAME value's name sits
        // behind, so leaving them scrambled garbles every non-trivial name.
        buf[510..512].copy_from_slice(&0xBEEFu16.to_le_bytes());
        buf[1022..1024].copy_from_slice(&0xBEEFu16.to_le_bytes());

        assert!(apply_fixups(&mut buf));

        assert_eq!(
            u16le(&buf, 510),
            0x4242,
            "first 512-byte tail must be restored"
        );
        assert_eq!(
            u16le(&buf, 1022),
            0x4343,
            "second 512-byte tail must be restored even though the record ends at 1024"
        );
    }

    #[test]
    fn mft_sector_size_is_512_even_though_the_volume_is_not() {
        // The invariant stated as a fact rather than left to a call site.
        assert_eq!(MFT_SECTOR_SIZE, 512);
    }

    #[test]
    fn an_implausible_cluster_size_falls_back_to_4096() {
        // `BytesPerCluster` is read at an unverified offset (see
        // `NtfsVolumeData::valid_cluster`). Whatever lands there, a value the
        // filesystem cannot have must not be used to convert a cluster count
        // into a byte offset for the primary scan path.
        for bogus in [0u32, 1, 511, 65_537, 0x0003_0A00, u32::MAX] {
            assert_eq!(
                NtfsVolumeData::valid_cluster(bogus),
                4096,
                "{bogus} is not a possible cluster size"
            );
        }
    }

    #[test]
    fn a_real_large_cluster_size_survives_the_guard() {
        // The guard must not flatten a legitimate 64 KiB-cluster NTFS volume,
        // which is how a bad offset turns into a silent wrong answer instead of
        // an obviously wrong one.
        for real in [512u32, 1024, 2048, 4096, 8192, 16_384, 32_768, 65_536] {
            assert_eq!(NtfsVolumeData::valid_cluster(real), real);
        }
    }

    #[test]
    fn the_record_size_fallback_reads_the_signed_byte_encoding() {
        // `ClustersPerFileRecordSegment` is a SIGNED BYTE exponent of a fixed
        // size, per Zen's boot reader (FUN_1400cfa70): `val < 1` means
        // `2^(-val)` BYTES (0xF6 = -10 -> 1024), `val >= 1` means that many
        // clusters. The old formula read it as an exponent of the CLUSTER size
        // in both directions, which computes 8192 where the record is 4096.
        let data = NtfsVolumeData {
            volume_serial_number: 0,
            bytes_per_sector: 512,
            bytes_per_cluster: 4096,
            bytes_per_file_record_segment: 0, // force the fallback
            clusters_per_file_record_segment: 0xFFFF_FFF6, // -10
            mft_valid_data_length: 0,
        };
        assert_eq!(data.bytes_per_file_record_segment(), 1024);

        // -12 -> 2^12 = 4096 bytes (the 4 KiB record encoding).
        let data = NtfsVolumeData {
            clusters_per_file_record_segment: 0xFFFF_FFF4, // -12
            ..data
        };
        assert_eq!(data.bytes_per_file_record_segment(), 4096);
    }

    #[test]
    fn the_record_size_fallback_reads_positive_values_as_clusters() {
        // val >= 1: that many clusters. 1 -> 4096, 2 -> 8192.
        let mut data = NtfsVolumeData {
            volume_serial_number: 0,
            bytes_per_sector: 512,
            bytes_per_cluster: 4096,
            bytes_per_file_record_segment: 0,
            clusters_per_file_record_segment: 1,
            mft_valid_data_length: 0,
        };
        assert_eq!(data.bytes_per_file_record_segment(), 4096);
        data.clusters_per_file_record_segment = 2;
        assert_eq!(data.bytes_per_file_record_segment(), 8192);
    }

    #[test]
    fn the_reported_record_size_outranks_the_fallback() {
        // The driver reports it directly at offset 0x30 on every real volume;
        // the fallback is only for a driver that leaves the field empty.
        let mut data = NtfsVolumeData {
            volume_serial_number: 0,
            bytes_per_sector: 512,
            bytes_per_cluster: 4096,
            bytes_per_file_record_segment: 1024,
            clusters_per_file_record_segment: 0xFFFF_FFF6,
            mft_valid_data_length: 0,
        };
        assert_eq!(data.bytes_per_file_record_segment(), 1024);
        // Drop the reported field and the fallback must land on the same answer.
        data.bytes_per_file_record_segment = 0;
        assert_eq!(data.bytes_per_file_record_segment(), 1024);
    }

    #[test]
    fn a_garbage_clusters_per_record_cannot_derive_a_wrong_record_size() {
        // The fallback must never invent a record size the filesystem could
        // not have. 0xFFFFFFFE (-2) is 2^2 = 4 BYTES under the RE rule — NOT
        // cluster/4 = 1024 as the old formula computed — so it must fall
        // through to the 1 KiB default via `plausible_record_size`.
        let mut data = NtfsVolumeData {
            volume_serial_number: 0,
            bytes_per_sector: 512,
            bytes_per_cluster: 4096,
            bytes_per_file_record_segment: 0,
            clusters_per_file_record_segment: 0, // 2^0 = 1 byte
            mft_valid_data_length: 0,
        };
        assert_eq!(data.bytes_per_file_record_segment(), 1024);

        // -2 -> 4 bytes, implausible -> default.
        data.clusters_per_file_record_segment = 0xFFFF_FFFE;
        assert_eq!(data.bytes_per_file_record_segment(), 1024);

        // A reported-but-insane size is rejected the same way the fallback is —
        // the plausibility check applies to both paths.
        data.bytes_per_file_record_segment = 0x8000_0000;
        assert_eq!(data.bytes_per_file_record_segment(), 1024);
        data.bytes_per_file_record_segment = 3000;
        assert_eq!(data.bytes_per_file_record_segment(), 1024);
        data.bytes_per_file_record_segment = 128;
        assert_eq!(data.bytes_per_file_record_segment(), 1024);
        data.bytes_per_file_record_segment = 1 << 20;
        assert_eq!(data.bytes_per_file_record_segment(), 1024);
        // And a plausible reported size survives.
        data.bytes_per_file_record_segment = 4096;
        assert_eq!(data.bytes_per_file_record_segment(), 4096);
    }

    #[test]
    fn rejects_empty_file_names() {
        let mut v = vec![0u8; 66];
        v[64] = 0;
        v[65] = NAMESPACE_WIN32;
        assert!(parse_file_name_value(&v, 66).is_none());
    }

    #[test]
    fn name_length_comes_from_value_length_not_the_in_value_field() {
        // FileNameLength is a CHARACTER count; the name is UTF-16, so the byte
        // length is 2 * FileNameLength. Here the count (12) is the truth and
        // ValueLength-66 agrees, so either rule works — pinned so a regression
        // to one-sided rules shows up immediately.
        let mut v = vec![0u8; 90];
        v[65] = NAMESPACE_WIN32;
        v[64] = 12; // 12 chars = 24 bytes
        for (i, c) in "NEXTMO~1.PNG".encode_utf16().enumerate() {
            v[66 + i * 2..68 + i * 2].copy_from_slice(&c.to_le_bytes());
        }
        assert_eq!(parse_file_name_value(&v, 90).unwrap().name, "NEXTMO~1.PNG");
    }

    #[test]
    fn file_name_length_times_two_beats_value_padding() {
        // ValueLength includes trailing padding past the name. Only
        // 2 * FileNameLength cuts the padding off — ValueLength-66 would glue
        // it onto the name.
        let mut v = vec![0u8; 80];
        v[65] = NAMESPACE_WIN32;
        v[64] = 4; // "test": 4 chars = 8 bytes
        let encoded: Vec<u8> = "test".encode_utf16().flat_map(u16::to_le_bytes).collect();
        v[66..66 + encoded.len()].copy_from_slice(&encoded);
        // 80 - 66 = 14 bytes of "name" area: 8 real + 6 padding.
        assert_eq!(parse_file_name_value(&v, 80).unwrap().name, "test");
    }

    #[test]
    fn file_name_length_falls_back_when_the_count_lies() {
        // A corrupt count that claims more characters than the value holds:
        // fall back to ValueLength-66 rather than reading past the name.
        let mut v = vec![0u8; 74];
        v[65] = NAMESPACE_WIN32;
        v[64] = 8; // claims 8 chars = 16 bytes; only 8 bytes exist
        let encoded: Vec<u8> = "test".encode_utf16().flat_map(u16::to_le_bytes).collect();
        v[66..66 + encoded.len()].copy_from_slice(&encoded);
        assert_eq!(parse_file_name_value(&v, 74).unwrap().name, "test");
    }

    #[test]
    fn usn_record_with_a_non_empty_name() {
        // Every prior USN fixture used an empty name, which is exactly how the
        // 52/54 misread of FileNameLength/FileNameOffset survived: the name
        // fields were zero, so reading them from the wrong offsets still
        // "worked". A live journal always carries the name.
        let mut rec = vec![0u8; 64];
        rec[0..4].copy_from_slice(&64u32.to_le_bytes()); // record length
        rec[4..6].copy_from_slice(&2u16.to_le_bytes()); // major version
        rec[8..16].copy_from_slice(&0x0001_0000_0000_0042u64.to_le_bytes()); // file ref
        rec[16..24].copy_from_slice(&0x0005_0000_0000_0005u64.to_le_bytes()); // parent ref
        rec[24..32].copy_from_slice(&0x1234u64.to_le_bytes()); // usn
        rec[40..44].copy_from_slice(&USN_REASON_FILE_CREATE.to_le_bytes());
        rec[52..56].copy_from_slice(&0x20u32.to_le_bytes()); // FileAttributes
        rec[56..58].copy_from_slice(&4u16.to_le_bytes()); // FileNameLength, BYTES
        rec[58..60].copy_from_slice(&60u16.to_le_bytes()); // FileNameOffset
        let encoded: Vec<u8> = "hi".encode_utf16().flat_map(u16::to_le_bytes).collect();
        rec[60..60 + encoded.len()].copy_from_slice(&encoded);

        let r = parse_usn_record(&rec).expect("should parse");
        assert_eq!(r.name, "hi");
        assert_eq!(frn_number(r.file_reference), 0x42);
        assert_eq!(frn_number(r.parent_reference), 5);
        assert_eq!(r.file_attributes, 0x20);
        assert_eq!(r.reason, USN_REASON_FILE_CREATE);
    }

    #[test]
    fn walk_skips_garbage_length() {
        let mut buf = vec![0u8; 16];
        buf[0..4].copy_from_slice(&4u32.to_le_bytes()); // bogus, below minimum
        assert!(walk_usn_buffer(&buf).is_empty());
    }

    #[test]
    fn usn_output_prefix_is_the_resume_usn_not_a_record() {
        // A live journal's resume USN is a large number. Read from offset 0 it
        // looks like a record length that exceeds the buffer, so the walk finds
        // nothing — indistinguishable from "no changes pending".
        let mut out = Vec::new();
        out.extend_from_slice(&0x50F8_9AA8i64.to_le_bytes()); // resume USN
        // One 60-byte V2 record header with an empty name.
        let mut rec = vec![0u8; 60];
        rec[0x00..0x04].copy_from_slice(&60u32.to_le_bytes());
        rec[0x04..0x06].copy_from_slice(&2u16.to_le_bytes());
        rec[0x08..0x10].copy_from_slice(&42u64.to_le_bytes()); // file ref
        out.extend_from_slice(&rec);

        // The bug: treating the whole buffer as records yields nothing.
        assert!(walk_usn_buffer(&out).is_empty());

        // The fix: peel the prefix off first.
        let (next, records) = split_usn_output(&out).expect("should split");
        assert_eq!(next, 0x50F8_9AA8);
        let parsed = walk_usn_buffer(records);
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].usn, 0);
        assert_eq!(frn_number(parsed[0].file_reference), 42);
    }

    #[test]
    fn usn_output_shorter_than_the_prefix_yields_nothing() {
        assert!(split_usn_output(&[0u8; 4]).is_none());
        assert!(split_usn_output(&[]).is_none());
    }

    #[test]
    fn usn_input_v1_layout_is_44_bytes_with_64_bit_ids() {
        // Pinned against the `windows` crate's `READ_USN_JOURNAL_DATA_V1`.
        // `StartUsn` is i64 and `UsnJournalID` is u64; only `ReasonMask` and
        // `ReturnOnlyOnClose` are DWORDs. Building this as an all-DWORD struct
        // produced 32 bytes, which the driver rejected with
        // `ERROR_INVALID_USER_BUFFER` before any record was read.
        let d = ReadUsnJournalData {
            start_usn: 0x0000_0002_0000_0003,
            reason_mask: 0x1111_1111,
            return_only_on_close: 0x2222_2222,
            timeout: 0x0000_0003_0000_0004,
            bytes_to_wait_for: 0x0000_0005_0000_0006,
            usn_journal_id: 0xDEAD_BEEF_0000_0001,
            min_major_version: 2,
            max_major_version: 3,
        };
        let b = d.to_bytes();
        assert_eq!(b.len(), 44);
        // The journal ID must survive all 64 bits: truncating it to 32 made
        // every read look like a different journal.
        assert_eq!(u64le(&b, 0x20), 0xDEAD_BEEF_0000_0001);
        assert_eq!(u64le(&b, 0x00), 0x0000_0002_0000_0003);
        assert_eq!(u64le(&b, 0x10), 0x0000_0003_0000_0004);
        assert_eq!(u64le(&b, 0x18), 0x0000_0005_0000_0006);
        assert_eq!(u32le(&b, 0x08), 0x1111_1111);
        assert_eq!(u32le(&b, 0x0c), 0x2222_2222);
        // Version window is u16s at the tail, past the V0 size.
        assert_eq!(u16le(&b, 0x28), 2);
        assert_eq!(u16le(&b, 0x2a), 3);
    }

    #[test]
    fn decode_data_runs_parses_single_and_multiple_extents() {
        let runs = vec![
            0x22, 0x00, 0x10, 0x00, 0x20, // run 1: start 8192, clusters 4096
            0x22, 0x00, 0x04, 0x00, 0xF8, // run 2: delta -2048 -> start 6144, clusters 1024
            0x00, // terminator
        ];
        let decoded = decode_data_runs(&runs, 0).expect("should decode data runs");
        assert_eq!(decoded.len(), 2);
        assert_eq!(
            decoded[0],
            MftRun {
                start_vcn: 0,
                clusters: 4096,
                start_lcn: Some(8192),
            }
        );
        assert_eq!(
            decoded[1],
            MftRun {
                start_vcn: 4096,
                clusters: 1024,
                start_lcn: Some(6144),
            }
        );
    }

    #[test]
    fn decode_data_runs_tracks_vcn_across_a_sparse_hole() {
        // Run 1: 100 clusters at LCN 500 (delta +500 from 0). Run 2: sparse,
        // 50 clusters (no offset field). Run 3: 30 clusters at LCN 540 — its
        // delta is relative to the last *real* LCN 500, not to the hole.
        let runs = vec![
            0x21, 0x64, 0xF4, 0x01, // 100 clusters, delta +500
            0x01, 0x32, // 50 clusters, sparse (offset size 0)
            0x11, 0x1E, 0x28, // 30 clusters, delta +40
            0x00,
        ];
        let decoded = decode_data_runs(&runs, 0).expect("should decode data runs");
        assert_eq!(
            decoded,
            vec![
                MftRun {
                    start_vcn: 0,
                    clusters: 100,
                    start_lcn: Some(500),
                },
                MftRun {
                    start_vcn: 100,
                    clusters: 50,
                    start_lcn: None,
                },
                MftRun {
                    start_vcn: 150,
                    clusters: 30,
                    start_lcn: Some(540),
                },
            ]
        );
    }

    #[test]
    fn decode_data_runs_honours_the_extent_base_vcn() {
        // A continuation extent starts at its StartingVCN, not at 0.
        let runs = vec![0x21, 0x0A, 0x00, 0x01, 0x00]; // 10 clusters at LCN 256
        let decoded = decode_data_runs(&runs, 4096).expect("should decode data runs");
        assert_eq!(
            decoded,
            vec![MftRun {
                start_vcn: 4096,
                clusters: 10,
                start_lcn: Some(256),
            }]
        );
    }

    #[test]
    fn extract_data_runs_finds_non_resident_data() {
        let mut rec = vec![0u8; 1024];
        rec[0..4].copy_from_slice(b"FILE");
        rec[0x14..0x16].copy_from_slice(&0x38u16.to_le_bytes()); // attrs_offset = 0x38
        rec[0x18..0x1c].copy_from_slice(&256u32.to_le_bytes()); // bytes_in_use = 256
        rec[0x16..0x18].copy_from_slice(&1u16.to_le_bytes()); // in use flag

        let off = 0x38;
        rec[off..off + 4].copy_from_slice(&ATTR_DATA.to_le_bytes());
        rec[off + 4..off + 8].copy_from_slice(&96u32.to_le_bytes()); // length = 96
        rec[off + 8] = 1; // non_resident = 1
        rec[off + 9] = 0; // name_len = 0
        rec[off + 0x10..off + 0x18].copy_from_slice(&0u64.to_le_bytes()); // StartingVCN = 0
        rec[off + 0x20..off + 0x22].copy_from_slice(&0x40u16.to_le_bytes()); // run_off = 0x40

        let run_start = off + 0x40;
        rec[run_start] = 0x22;
        rec[run_start + 1..run_start + 3].copy_from_slice(&100u16.to_le_bytes()); // 100 clusters
        rec[run_start + 3..run_start + 5].copy_from_slice(&500u16.to_le_bytes()); // start lcn 500
        rec[run_start + 5] = 0x00; // terminator

        let extents = extract_data_runs(&rec).expect("should extract data runs");
        assert_eq!(extents.len(), 1);
        assert_eq!(
            extents[0],
            MftRun {
                start_vcn: 0,
                clusters: 100,
                start_lcn: Some(500),
            }
        );
    }

    #[test]
    fn extract_data_runs_reads_the_extent_starting_vcn() {
        // A continuation extent's runs begin at its StartingVCN. Trusting the
        // run list alone would number every record past the first extent wrong.
        let mut rec = vec![0u8; 1024];
        rec[0..4].copy_from_slice(b"FILE");
        rec[0x14..0x16].copy_from_slice(&0x38u16.to_le_bytes());
        rec[0x18..0x1c].copy_from_slice(&256u32.to_le_bytes());
        rec[0x16..0x18].copy_from_slice(&1u16.to_le_bytes());

        let off = 0x38;
        rec[off..off + 4].copy_from_slice(&ATTR_DATA.to_le_bytes());
        rec[off + 4..off + 8].copy_from_slice(&96u32.to_le_bytes());
        rec[off + 8] = 1; // non_resident
        rec[off + 9] = 0; // unnamed
        rec[off + 0x10..off + 0x18].copy_from_slice(&250u64.to_le_bytes()); // StartingVCN = 250
        rec[off + 0x20..off + 0x22].copy_from_slice(&0x40u16.to_le_bytes());

        let run_start = off + 0x40;
        rec[run_start] = 0x11; // 1-byte length, 1-byte offset
        rec[run_start + 1] = 20; // 20 clusters
        rec[run_start + 2] = 0x10; // delta +16
        rec[run_start + 3] = 0x00;

        let extents = extract_data_runs(&rec).expect("should extract data runs");
        assert_eq!(
            extents,
            vec![MftRun {
                start_vcn: 250,
                clusters: 20,
                start_lcn: Some(16),
            }]
        );
    }

    /// Build one 26-byte `$ATTRIBUTE_LIST` entry.
    fn attr_list_entry(attr_type: u32, mft_reference: u64, start_vcn: u64) -> Vec<u8> {
        let mut e = vec![0u8; 26];
        e[0x00..0x04].copy_from_slice(&attr_type.to_le_bytes());
        e[0x04..0x06].copy_from_slice(&26u16.to_le_bytes());
        e[0x08..0x10].copy_from_slice(&start_vcn.to_le_bytes());
        e[0x10..0x18].copy_from_slice(&mft_reference.to_le_bytes());
        e
    }

    #[test]
    fn attr_list_reads_mft_reference_from_offset_0x10() {
        // The row is 26 bytes: type, length, name length, name offset, start VCN,
        // mftReference, attrId. mftReference lives at 0x10 — reading it from 0x18
        // picks up the attrId instead and points the chase at the wrong record.
        let mut v = attr_list_entry(ATTR_FILE_NAME, 0x0005_0000_0000_0123, 0);
        // attrId is an instance id, not an offset: planting a large value here
        // must not change the record the entry names.
        v[0x18..0x1a].copy_from_slice(&0x7FFFu16.to_le_bytes());

        let entries = parse_attr_list(&v);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].attr_type, ATTR_FILE_NAME);
        assert_eq!(frn_number(entries[0].mft_reference), 0x123);
        assert_eq!(entries[0].start_vcn, 0);
    }

    #[test]
    fn attr_list_skips_nonzero_vcn_run_entries() {
        let v = attr_list_entry(ATTR_DATA, 0x2222, 7);
        let entries = parse_attr_list(&v);
        assert_eq!(entries[0].start_vcn, 7);
    }

    #[test]
    fn attr_list_stops_at_end_marker() {
        let mut v = attr_list_entry(ATTR_FILE_NAME, 0x1111, 0);
        v.extend_from_slice(&0xFFFF_FFFFu32.to_le_bytes());
        v.extend_from_slice(&0u32.to_le_bytes());
        assert_eq!(parse_attr_list(&v).len(), 1);
    }

    #[test]
    fn reparse_tag_is_read_from_value_offset_0x3c() {
        let mut v = vec![0u8; 90];
        v[0x38..0x3c].copy_from_slice(&FILE_ATTRIBUTE_REPARSE_POINT.to_le_bytes());
        v[0x3c..0x40].copy_from_slice(&0xA000_0003u32.to_le_bytes()); // IO_REPARSE_TAG_MOUNT_POINT
        v[65] = NAMESPACE_WIN32;
        let name: Vec<u8> = "junction"
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect();
        v[66..66 + name.len()].copy_from_slice(&name);

        let parsed = parse_file_name_value(&v, v.len()).expect("should parse");
        assert_eq!(parsed.reparse_tag, 0xA000_0003);
        assert!(parsed.is_reparse_point());
    }

    #[test]
    fn plain_file_is_not_a_reparse_point() {
        let mut v = vec![0u8; 90];
        v[0x38..0x3c].copy_from_slice(&0x20u32.to_le_bytes()); // ARCHIVE only
        v[65] = NAMESPACE_WIN32;
        let name: Vec<u8> = "file.txt"
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect();
        v[66..66 + name.len()].copy_from_slice(&name);

        let parsed = parse_file_name_value(&v, v.len()).expect("should parse");
        assert!(!parsed.is_reparse_point());
    }

    #[test]
    fn best_name_takes_a_slice_and_prefers_win32() {
        let win32 = FileNameEntry {
            parent_frn: 5,
            namespace: NAMESPACE_WIN32,
            file_size: 0,
            file_attributes: 0,
            reparse_tag: 0,
            name: "Long Name.txt".to_string(),
        };
        let dos = FileNameEntry {
            parent_frn: 5,
            namespace: NAMESPACE_DOS,
            file_size: 0,
            file_attributes: 0,
            reparse_tag: 0,
            name: "LONGNA~1.TXT".to_string(),
        };
        // DOS wins the sort only if it is offered first and ranks better, which
        // it does not — the slice API must still pick the Win32 name either way.
        assert_eq!(
            best_name(&[win32.clone(), dos.clone()]).unwrap().name,
            "Long Name.txt"
        );
        assert_eq!(best_name(&[dos, win32]).unwrap().name, "Long Name.txt");
    }

    #[test]
    fn usn_journal_data_reads_the_64_bit_id_and_the_usn_window() {
        let mut buf = vec![0u8; 0x60];
        buf[0x00..0x08].copy_from_slice(&0xDEAD_BEEF_0000_0001u64.to_le_bytes());
        buf[0x08..0x10].copy_from_slice(&1000i64.to_le_bytes()); // first_usn
        buf[0x10..0x18].copy_from_slice(&5000i64.to_le_bytes()); // next_usn
        buf[0x18..0x20].copy_from_slice(&2000i64.to_le_bytes()); // lowest_valid_usn
        buf[0x20..0x28].copy_from_slice(&9000i64.to_le_bytes()); // max_usn
        buf[0x38..0x3a].copy_from_slice(&2u16.to_le_bytes());
        buf[0x3a..0x3c].copy_from_slice(&3u16.to_le_bytes());

        let j = UsnJournalData::parse(&buf).expect("should parse");
        assert_eq!(j.usn_journal_id, 0xDEAD_BEEF_0000_0001);
        assert_eq!(j.first_usn, 1000);
        assert_eq!(j.next_usn, 5000);
        assert_eq!(j.lowest_valid_usn, 2000);
        assert_eq!(j.max_usn, 9000);
        assert_eq!(j.min_supported_major_version, 2);
        assert_eq!(j.max_supported_major_version, 3);
    }

    #[test]
    fn usn_journal_covers_rejects_a_stale_resume_point() {
        let j = UsnJournalData {
            first_usn: 1000,
            next_usn: 5000,
            lowest_valid_usn: 2000,
            max_usn: 9000,
            ..UsnJournalData::default()
        };
        // Below the window the journal has already recycled.
        assert!(!j.covers(1500));
        // At or past the next unread position is fine: it just means "nothing new".
        assert!(j.covers(2000));
        assert!(j.covers(5000));
        assert!(!j.covers(5001));
    }

    #[test]
    fn usn_journal_data_needs_the_version_block_to_read_versions() {
        // A V0-sized buffer has no version window; the fields stay at zero
        // rather than reading past the end of the buffer.
        let mut buf = vec![0u8; 0x38];
        buf[0x00..0x08].copy_from_slice(&7u64.to_le_bytes());
        let j = UsnJournalData::parse(&buf).expect("should parse");
        assert_eq!(j.usn_journal_id, 7);
        assert_eq!(j.min_supported_major_version, 0);
        assert_eq!(j.max_supported_major_version, 0);
    }

    #[test]
    fn usn_journal_delete_is_a_distinct_reason_bit() {
        // Not a path change: it marks a wrap. Collapsing it into the path-relevant
        // set would make a wrap look like an ordinary rename.
        assert_eq!(USN_REASON_JOURNAL_DELETE, 0x0001_0000);
        assert_eq!(USN_REASON_PATH_RELEVANT & USN_REASON_JOURNAL_DELETE, 0);
    }
}
