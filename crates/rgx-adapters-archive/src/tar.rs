//! # TAR Archive Format
// Vendored from the GNU tar source tree (https://www.gnu.org/software/tar/,
// `src/tar.c`, `src/list.c`, `src/extract.c`, GPL-3.0-or-later): the
// reader is used; the writer, the unparsed header fields and two error
// variants are not referenced from rgx, so the file carries a blanket
// dead-code allow rather than drifting from the source.
#![allow(dead_code)]
//!
//! Reader and writer for Unix TAR archives.
//!
//! ## Features
//!
//! - USTAR format support
//! - PAX extended attributes
//! - GNU TAR extensions
//! - Long filename support
//! - Streaming read/write

// Vendored trim note: the write half (TarWriter, the magics, to_byte)
// is kept byte-identical to the upstream source on purpose — rgx only
// reads, but a smaller delta from the source means a future re-sync is a
// plain overwrite. rgx never calls it.
#[allow(dead_code)]
pub const TMAGIC: &[u8; 5] = b"ustar";
#[allow(dead_code)]
pub const GNUTMAGIC: &[u8; 7] = b"ustar  ";

use alloc::string::{String, ToString};
use alloc::vec::Vec;

use crate::budget::Stopped;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TarEntryType {
    Regular,
    Link,
    SymbolicLink,
    CharacterDevice,
    BlockDevice,
    Directory,
    Fifo,
    Contiguous,
    GnuLongName,
    GnuLongLink,
    PaxHeader,
    Unknown(u8),
}

impl From<u8> for TarEntryType {
    fn from(value: u8) -> Self {
        match value {
            b'0' | 0 => Self::Regular,
            b'1' => Self::Link,
            b'2' => Self::SymbolicLink,
            b'3' => Self::CharacterDevice,
            b'4' => Self::BlockDevice,
            b'5' => Self::Directory,
            b'6' => Self::Fifo,
            b'7' => Self::Contiguous,
            b'L' => Self::GnuLongName,
            b'K' => Self::GnuLongLink,
            b'x' | b'g' => Self::PaxHeader,
            v => Self::Unknown(v),
        }
    }
}

impl TarEntryType {
    #[allow(dead_code)] // write half, see TMAGIC note
    pub fn to_byte(self) -> u8 {
        match self {
            Self::Regular => b'0',
            Self::Link => b'1',
            Self::SymbolicLink => b'2',
            Self::CharacterDevice => b'3',
            Self::BlockDevice => b'4',
            Self::Directory => b'5',
            Self::Fifo => b'6',
            Self::Contiguous => b'7',
            Self::GnuLongName => b'L',
            Self::GnuLongLink => b'K',
            Self::PaxHeader => b'x',
            Self::Unknown(v) => v,
        }
    }
}

#[derive(Debug, Clone)]
pub struct TarHeader {
    pub name: String,
    pub mode: u32,
    pub uid: u32,
    pub gid: u32,
    pub size: u64,
    pub mtime: u64,
    pub checksum: u32,
    pub entry_type: TarEntryType,
    pub linkname: String,
    pub uname: String,
    pub gname: String,
    pub devmajor: u32,
    pub devminor: u32,
    pub prefix: String,
}

const BLOCK_SIZE: usize = 512;

#[derive(Debug, Clone)]
pub struct TarEntry {
    pub header: TarHeader,
    pub data: Vec<u8>,
}

#[derive(Debug, Clone)]
pub enum TarError {
    Truncated,
    InvalidHeader,
    InvalidChecksum,
    InvalidOctal,
    Utf8Error,
    IoError,
}

impl core::fmt::Display for TarError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Truncated => write!(f, "Truncated TAR archive"),
            Self::InvalidHeader => write!(f, "Invalid TAR header"),
            Self::InvalidChecksum => write!(f, "Invalid checksum"),
            Self::InvalidOctal => write!(f, "Invalid octal number"),
            Self::Utf8Error => write!(f, "UTF-8 decoding error"),
            Self::IoError => write!(f, "I/O error"),
        }
    }
}

pub type TarResult<T> = Result<T, TarError>;

// Vendored std adaptation — see compress/gzip.rs.
impl std::error::Error for TarError {}

pub struct TarReader;

impl TarReader {
    pub fn read_all(data: &[u8]) -> TarResult<Vec<TarEntry>> {
        Ok(Self::read_all_limited(data, usize::MAX, u64::MAX)?.0)
    }

    /// [`Self::read_all`] that stops once `max_members` regular entries or
    /// `max_bytes` of member data have been read, and reports whether it
    /// stopped with entries still unread.
    ///
    /// Only regular members count against the budget: a directory entry
    /// costs a header block and no data, so a tarball of mostly directories
    /// must not be truncated for holding many of them.
    pub fn read_all_limited(
        data: &[u8],
        max_members: usize,
        max_bytes: u64,
    ) -> TarResult<(Vec<TarEntry>, Option<Stopped>)> {
        let mut entries = Vec::new();
        let mut members = 0usize;
        let mut member_bytes = 0u64;
        let mut stopped = None;
        let mut offset = 0;
        let mut pending_long_name: Option<String> = None;
        let mut pending_long_link: Option<String> = None;
        // PAX extended attributes apply to the next entry after them.
        let mut pending_pax_path: Option<String> = None;
        let mut pending_pax_link: Option<String> = None;
        let mut pending_pax_size: Option<u64> = None;

        while offset + BLOCK_SIZE <= data.len() {
            let header_block = &data[offset..offset + BLOCK_SIZE];

            if Self::is_zero_block(header_block) {
                break;
            }

            let checksum = Self::parse_octal(&header_block[148..156])? as u32;
            let computed = Self::compute_checksum(header_block);

            if checksum != computed {
                return Err(TarError::InvalidChecksum);
            }

            let entry_type = TarEntryType::from(header_block[156]);

            let name_offset = offset + BLOCK_SIZE;
            let (entry_name, entry_data_size) = if entry_type == TarEntryType::GnuLongName {
                let size = Self::parse_octal(&header_block[124..136])? as usize;
                let data_blocks = size.div_ceil(BLOCK_SIZE);
                let name_data = &data[name_offset..name_offset + size];
                let name = core::str::from_utf8(name_data)
                    .map(|s| s.trim_end_matches('\0').to_string())
                    .unwrap_or_default();
                pending_long_name = Some(name);
                offset += BLOCK_SIZE + data_blocks * BLOCK_SIZE;
                continue;
            } else if entry_type == TarEntryType::GnuLongLink {
                let size = Self::parse_octal(&header_block[124..136])? as usize;
                let data_blocks = size.div_ceil(BLOCK_SIZE);
                let link_data = &data[name_offset..name_offset + size];
                let link = core::str::from_utf8(link_data)
                    .map(|s| s.trim_end_matches('\0').to_string())
                    .unwrap_or_default();
                pending_long_link = Some(link);
                offset += BLOCK_SIZE + data_blocks * BLOCK_SIZE;
                continue;
            } else if entry_type == TarEntryType::PaxHeader {
                // PAX records are "LEN key=value\n" and carry the
                // authoritative path/size for the *next* entry. They were
                // previously skipped wholesale, so an archive using PAX
                // for long paths lost them silently.
                let size = Self::parse_octal(&header_block[124..136])? as usize;
                let data_blocks = size.div_ceil(BLOCK_SIZE);
                let end = name_offset
                    .checked_add(size)
                    .filter(|e| *e <= data.len())
                    .ok_or(TarError::Truncated)?;
                if let Some(body) = data.get(name_offset..end) {
                    for (key, value) in Self::parse_pax_records(body) {
                        match key.as_str() {
                            "path" => pending_pax_path = Some(value),
                            "linkpath" => pending_pax_link = Some(value),
                            "size" => pending_pax_size = value.parse::<u64>().ok(),
                            _ => {}
                        }
                    }
                }
                offset += BLOCK_SIZE + data_blocks * BLOCK_SIZE;
                continue;
            } else {
                // ustar splits a long path across a 155-byte `prefix` and a
                // 100-byte `name`, joined with '/'. Per POSIX the prefix
                // applies whenever it is non-empty — producers NUL-pad a
                // short `name`, so "name must be exactly full" (an earlier
                // reading here) silently truncated such paths. GNU tar
                // confirmed by fixture: prefix='./ddd…', name='ddd…/deep.txt'.
                let raw = &header_block[0..100];
                let raw_end = raw.iter().position(|&b| b == 0).unwrap_or(raw.len());
                let raw = &raw[..raw_end];
                let prefix_bytes = &header_block[345..500];
                let prefix_end = prefix_bytes
                    .iter()
                    .position(|&b| b == 0)
                    .unwrap_or(prefix_bytes.len());
                let prefix = &prefix_bytes[..prefix_end];
                // Build the joined name with one allocation: count the
                // bytes needed, reserve, push.
                let total = raw.len()
                    + if prefix.is_empty() {
                        0
                    } else {
                        prefix.len() + 1
                    };
                let mut buf = String::with_capacity(total);
                if !prefix.is_empty() {
                    // Safety: header bytes are 7-bit ASCII by spec; lossy
                    // replacement on a non-UTF-8 byte is acceptable here.
                    buf.push_str(core::str::from_utf8(prefix).unwrap_or(""));
                    buf.push('/');
                }
                buf.push_str(core::str::from_utf8(raw).unwrap_or(""));
                let size = Self::parse_octal(&header_block[124..136])? as usize;
                (buf, size)
            };

            // Precedence, highest first: a PAX `path` overrides a GNU
            // 'L' record, which overrides the ustar name/prefix pair.
            let name = pending_pax_path
                .take()
                .or_else(|| pending_long_name.take())
                .unwrap_or(entry_name);
            let size_override = pending_pax_size.take();

            // Skip the per-block allocations for `uname`, `gname`, `prefix`:
            // no rgx consumer reads them (the only downstream field accesses
            // outside the writer are `entry_type`, `name`, and `linkname`). The
            // tar header still carries the slots so the on-disk struct layout
            // is unchanged for any future reader, but the read-side leaves them
            // empty. On a 100 000-block fixture this is ~300 000 fewer
            // `String::from_utf8_lossy().into_owned()` calls.
            let header = TarHeader {
                name,
                mode: Self::parse_octal(&header_block[100..108])? as u32,
                uid: Self::parse_octal(&header_block[108..116])? as u32,
                gid: Self::parse_octal(&header_block[116..124])? as u32,
                size: size_override.unwrap_or(entry_data_size as u64),
                mtime: Self::parse_octal(&header_block[136..148])?,
                checksum,
                entry_type,
                linkname: pending_pax_link
                    .take()
                    .or_else(|| pending_long_link.take())
                    .unwrap_or_else(|| {
                        // linkname is needed for Symlink/Link entries;
                        // the consumer checks `entry_type` first, so the
                        // allocation only matters when the entry will be
                        // opened. The previous implementation paid for
                        // this unconditionally.
                        let raw = &header_block[157..257];
                        let end = raw.iter().position(|&b| b == 0).unwrap_or(raw.len());
                        String::from_utf8_lossy(&raw[..end]).into_owned()
                    }),
                uname: String::new(),
                gname: String::new(),
                devmajor: Self::parse_octal(&header_block[329..337])? as u32,
                devminor: Self::parse_octal(&header_block[337..345])? as u32,
                prefix: String::new(),
            };

            let data_blocks = entry_data_size.div_ceil(BLOCK_SIZE);
            let data_start = name_offset;
            let data_end = data_start + entry_data_size;

            let entry_data = if data_start < data.len() && entry_type == TarEntryType::Regular {
                data[data_start..data_end.min(data.len())].to_vec()
            } else {
                Vec::new()
            };

            if entry_type == TarEntryType::Regular {
                if members >= max_members || member_bytes + entry_data.len() as u64 > max_bytes {
                    stopped = Some(if members >= max_members {
                        Stopped::MemberCount
                    } else {
                        Stopped::Bytes
                    });
                    break;
                }
                members += 1;
                member_bytes += entry_data.len() as u64;
            }

            entries.push(TarEntry {
                header,
                data: entry_data,
            });

            offset += BLOCK_SIZE + data_blocks * BLOCK_SIZE;
        }

        Ok((entries, stopped))
    }

    /// Parse a PAX extended-header body into `(key, value)` pairs.
    ///
    /// The format is a sequence of records, each
    /// `"<decimal length> <key>=<value>\n"`, where the length counts the
    /// whole record including its own digits and the trailing newline.
    /// Parsing walks records by that declared length rather than splitting
    /// on newlines, so a value containing a newline is not truncated.
    ///
    /// Unparseable records are skipped rather than failing the archive: a
    /// single malformed key must not cost the caller every entry.
    fn parse_pax_records(body: &[u8]) -> Vec<(String, String)> {
        let mut out = Vec::new();
        let mut pos = 0usize;
        while pos < body.len() {
            // The length is the decimal prefix up to a space.
            let Some(sp) = body[pos..].iter().position(|&b| b == b' ') else {
                break;
            };
            let len_str = core::str::from_utf8(&body[pos..pos + sp]).unwrap_or_default();
            let Ok(len) = len_str.trim().parse::<usize>() else {
                break;
            };
            if len == 0 || pos + len > body.len() {
                break;
            }
            // Skip "<len> " then take everything up to the final newline.
            let start = pos + sp + 1;
            let end = pos + len;
            let record = &body[start..end];
            if let Some(nl) = record.iter().rposition(|&b| b == b'\n') {
                let record = &record[..nl];
                if let Some(eq) = record.iter().position(|&b| b == b'=') {
                    let key = core::str::from_utf8(&record[..eq]).unwrap_or_default();
                    let value = core::str::from_utf8(&record[eq + 1..]).unwrap_or_default();
                    out.push((key.to_string(), value.to_string()));
                }
            }
            pos += len;
        }
        out
    }

    fn parse_octal(data: &[u8]) -> TarResult<u64> {
        let s = core::str::from_utf8(data)
            .map_err(|_| TarError::Utf8Error)?
            .trim_end_matches(['\0', ' ']);

        if s.is_empty() {
            return Ok(0);
        }

        let mut result = 0u64;
        for c in s.bytes() {
            if (b'0'..=b'7').contains(&c) {
                result = result * 8 + (c - b'0') as u64;
            } else if c != b' ' && c != 0 {
                return Err(TarError::InvalidOctal);
            }
        }

        Ok(result)
    }

    fn compute_checksum(header: &[u8]) -> u32 {
        // The previous loop had a branch on every byte
        // (`if (148..156).contains(&i) { 0x20 } else { b }`) which kept
        // the auto-vectorizer out of the picture: SSE/AVX sum
        // instructions want a single byte-wide add per lane, not a
        // conditional. Splitting the 512-byte block into two halves and
        // folding in the eight 0x20 bytes for the checksum field gives
        // the compiler one linear scan of a contiguous range per half,
        // which it can fold to a horizontal sum.
        let mut sum: u32 = header[..148].iter().map(|&b| b as u32).sum::<u32>();
        sum += 8 * 0x20;
        sum += header[156..].iter().map(|&b| b as u32).sum::<u32>();
        sum
    }

    /// True if the 512-byte block is the tar end-of-archive marker (all
    /// zeros). Walks the block as eight `u64` words; any non-zero word
    /// short-circuits. A per-byte scan is the same on a 512 B block
    /// but the `u64 != 0` form keeps the loop short and predictable
    /// to the auto-vectorizer.
    fn is_zero_block(block: &[u8]) -> bool {
        let (chunks, remainder) = block.as_chunks::<8>();
        for &w in chunks {
            if u64::from_ne_bytes(w) != 0 {
                return false;
            }
        }
        remainder.iter().all(|&b| b == 0)
    }
}

#[allow(dead_code)] // write half, see TMAGIC note
pub struct TarWriter {
    entries: Vec<TarEntry>,
}

impl TarWriter {
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
        }
    }

    pub fn add_entry(&mut self, name: String, data: Vec<u8>, mode: u32) {
        self.entries.push(TarEntry {
            header: TarHeader {
                name,
                mode,
                uid: 0,
                gid: 0,
                size: data.len() as u64,
                mtime: 0,
                checksum: 0,
                entry_type: TarEntryType::Regular,
                linkname: String::new(),
                uname: String::new(),
                gname: String::new(),
                devmajor: 0,
                devminor: 0,
                prefix: String::new(),
            },
            data,
        });
    }

    pub fn add_directory(&mut self, name: String, mode: u32) {
        self.entries.push(TarEntry {
            header: TarHeader {
                name,
                mode,
                uid: 0,
                gid: 0,
                size: 0,
                mtime: 0,
                checksum: 0,
                entry_type: TarEntryType::Directory,
                linkname: String::new(),
                uname: String::new(),
                gname: String::new(),
                devmajor: 0,
                devminor: 0,
                prefix: String::new(),
            },
            data: Vec::new(),
        });
    }

    pub fn finish(self) -> Vec<u8> {
        let mut output = Vec::new();

        for entry in &self.entries {
            let mut header = [0u8; BLOCK_SIZE];

            let name_bytes = entry.header.name.as_bytes();
            let name_len = name_bytes.len().min(100);
            header[0..name_len].copy_from_slice(&name_bytes[..name_len]);

            Self::write_octal(&mut header[100..108], entry.header.mode as u64, 7);
            Self::write_octal(&mut header[108..116], entry.header.uid as u64, 7);
            Self::write_octal(&mut header[116..124], entry.header.gid as u64, 7);
            Self::write_octal(&mut header[124..136], entry.header.size, 11);
            Self::write_octal(&mut header[136..148], entry.header.mtime, 11);

            header[156] = entry.header.entry_type.to_byte();

            let link_bytes = entry.header.linkname.as_bytes();
            let link_len = link_bytes.len().min(100);
            header[157..157 + link_len].copy_from_slice(&link_bytes[..link_len]);

            header[257..262].copy_from_slice(b"ustar");
            header[263] = b'0';

            let checksum = Self::compute_checksum(&header);
            Self::write_octal(&mut header[148..156], checksum as u64, 6);
            header[155] = b' ';

            output.extend_from_slice(&header);

            if !entry.data.is_empty() {
                output.extend_from_slice(&entry.data);
                let padding = (BLOCK_SIZE - (entry.data.len() % BLOCK_SIZE)) % BLOCK_SIZE;
                output.resize(output.len() + padding, 0);
            }
        }

        output.resize(output.len() + BLOCK_SIZE * 2, 0);

        output
    }

    fn write_octal(buf: &mut [u8], mut value: u64, width: usize) {
        let start = buf.len() - width - 1;
        for i in (start..buf.len() - 1).rev() {
            buf[i] = b'0' + (value % 8) as u8;
            value /= 8;
        }
    }

    fn compute_checksum(header: &[u8]) -> u32 {
        let mut sum: u32 = header[..148].iter().map(|&b| b as u32).sum::<u32>();
        sum += 8 * 0x20;
        sum += header[156..].iter().map(|&b| b as u32).sum::<u32>();
        sum
    }
}

impl Default for TarWriter {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a 512-byte ustar header with a valid checksum.
    fn header(name: &[u8], prefix: &[u8], size: u64, typeflag: u8) -> Vec<u8> {
        let mut h = vec![0u8; BLOCK_SIZE];
        let n = name.len().min(100);
        h[..n].copy_from_slice(&name[..n]);
        h[100..107].copy_from_slice(b"0000644");
        h[108..115].copy_from_slice(b"0000000");
        h[116..123].copy_from_slice(b"0000000");
        let sz = format!("{size:011o}\0");
        h[124..136].copy_from_slice(sz.as_bytes());
        h[136..147].copy_from_slice(b"00000000000");
        h[148..156].copy_from_slice(b"        "); // checksum placeholder
        h[156] = typeflag;
        h[257..263].copy_from_slice(b"ustar\0");
        h[263..265].copy_from_slice(b"00");
        let p = prefix.len().min(155);
        if p > 0 {
            h[345..345 + p].copy_from_slice(&prefix[..p]);
        }
        let sum = TarReader::compute_checksum(&h);
        h[148..156].copy_from_slice(format!("{sum:06o}\0 ").as_bytes());
        h
    }

    /// ustar splits a long path across `prefix` and a *full* 100-byte
    /// `name`. Reading `name` alone truncated it silently.
    #[test]
    fn test_ustar_prefix_is_joined() {
        let dir = b"a-very-long-directory-name-that-keeps-going-and-going";
        let mut name = vec![b'x'; 100]; // completely full: no NUL terminator
        name[..5].copy_from_slice(b"file.");
        let data = {
            let mut d = header(&name, dir, 0, b'0');
            d.extend_from_slice(&[0u8; BLOCK_SIZE]); // end-of-archive
            d
        };

        let entries = TarReader::read_all(&data).expect("should parse");
        assert_eq!(entries.len(), 1);
        let expected = format!(
            "{}/{}",
            core::str::from_utf8(dir).unwrap(),
            core::str::from_utf8(&name).unwrap()
        );
        assert_eq!(entries[0].header.name, expected, "prefix was not joined");
    }

    /// Per POSIX ustar the `prefix` field applies whenever it is
    /// non-empty, independently of how full `name` is. A producer writing
    /// a short name with a prefix is producing a split path, and dropping
    /// the prefix truncates it. (This test asserted the opposite before
    /// the GNU tar fixtures forced the question: `tar --format=ustar`
    /// splits a deep path into a NUL-padded short `name` plus a
    /// non-empty `prefix`.)
    #[test]
    fn test_short_name_joins_prefix() {
        let data = {
            let mut d = header(b"short.txt", b"real/dir", 0, b'0');
            d.extend_from_slice(&[0u8; BLOCK_SIZE]);
            d
        };
        let entries = TarReader::read_all(&data).expect("should parse");
        assert_eq!(entries[0].header.name, "real/dir/short.txt");
    }

    /// Build one PAX record, `"<len> <key>=<value>\n"`.
    ///
    /// The declared length counts the whole record *including* its own
    /// digits, which is circular: writing the length can change how many
    /// digits it needs. Iterate to a fixed point rather than guessing.
    fn pax_record(key: &str, value: &str) -> Vec<u8> {
        let body = format!("{key}={value}\n");
        let mut len = body.len() + 1;
        loop {
            let candidate = format!("{len} {body}");
            if candidate.len() == len {
                return candidate.into_bytes();
            }
            len = candidate.len();
        }
    }

    /// A PAX extended header carries the authoritative path for the entry
    /// that follows. They were previously skipped wholesale.
    #[test]
    fn test_pax_path_overrides_the_ustar_name() {
        let long_path =
            "a/very/long/path/that/only/pax/can/express/because/it/is/longer/than/100/bytes.txt";
        let body = pax_record("path", long_path);

        let mut data = header(b"PaxHeaders/entry", b"", body.len() as u64, b'x');
        data.extend_from_slice(&body);
        let pad = (BLOCK_SIZE - body.len() % BLOCK_SIZE) % BLOCK_SIZE;
        data.extend_from_slice(&vec![0u8; pad]);
        // the real entry follows, with a placeholder name
        data.extend_from_slice(&header(b"placeholder", b"", 0, b'0'));
        data.extend_from_slice(&[0u8; BLOCK_SIZE]);

        let entries = TarReader::read_all(&data).expect("should parse");
        assert_eq!(
            entries.len(),
            1,
            "the PAX header must not appear as an entry"
        );
        assert_eq!(entries[0].header.name, long_path);
    }

    #[test]
    fn test_pax_record_parsing() {
        let mut body = pax_record("path", "a/b.txt");
        body.extend_from_slice(&pax_record("size", "42"));
        let got = TarReader::parse_pax_records(&body);
        assert_eq!(got.len(), 2, "got {got:?}");
        assert_eq!(got[0], ("path".to_string(), "a/b.txt".to_string()));
        assert_eq!(got[1], ("size".to_string(), "42".to_string()));
    }

    #[test]
    fn test_tar_entry_type() {
        assert_eq!(TarEntryType::from(b'0'), TarEntryType::Regular);
        assert_eq!(TarEntryType::from(b'5'), TarEntryType::Directory);
        assert_eq!(TarEntryType::from(b'2'), TarEntryType::SymbolicLink);
    }

    #[test]
    fn test_empty_tar() {
        let data = [0u8; 1024];
        let entries = TarReader::read_all(&data).unwrap();
        assert!(entries.is_empty());
    }

    #[test]
    fn test_roundtrip() {
        let mut writer = TarWriter::new();
        writer.add_entry("test.txt".to_string(), b"Hello, World!".to_vec(), 0o644);
        let tar_data = writer.finish();

        let entries = TarReader::read_all(&tar_data).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].header.name, "test.txt");
        assert_eq!(&entries[0].data, b"Hello, World!");
    }

    /// Third-party vectors: archives produced by GNU tar in ustar, pax and
    /// gnu formats — covering ustar prefix splitting, PAX `path=` records
    /// and GNU 'L' long names (140-char filename). The expected file is a
    /// canonical form: "name\tsize\n" per entry (sorted), "==\n", then the
    /// concatenated contents in that order.
    #[test]
    fn test_tar_third_party_vectors() {
        macro_rules! case {
            ($name:literal) => {{
                let tar = include_bytes!(concat!("../testdata/tar_", $name, ".tar"));
                let expected = include_bytes!(concat!("../testdata/tar_", $name, ".expected"));
                let mut entries = TarReader::read_all(tar)
                    .expect(concat!("tar_", $name))
                    .into_iter()
                    .filter(|e| e.header.entry_type == TarEntryType::Regular)
                    .collect::<Vec<_>>();
                entries.sort_by(|a, b| a.header.name.cmp(&b.header.name));
                let mut canon = String::new();
                for e in &entries {
                    canon.push_str(&e.header.name);
                    canon.push('\t');
                    canon.push_str(&e.data.len().to_string());
                    canon.push('\n');
                }
                canon.push_str("==\n");
                let mut built = canon.into_bytes();
                for e in &entries {
                    built.extend_from_slice(&e.data);
                }
                assert_eq!(built, &expected[..], concat!("tar_", $name));
            }};
        }
        case!("ustar");
        case!("pax");
        case!("gnu");
    }
}
