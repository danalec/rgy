//! RAR5 archive writer (store method only).
//!
//! Produces archives that follow RARLAB's published RAR5 layout and are
//! readable by the reference `unrar`/`WinRAR`. The proprietary compression
//! algorithms are never used: every entry is written with method 0x30
//! (store), i.e. raw bytes wrapped in the RAR5 container.

use alloc::vec::Vec;

use crate::crc32::crc32;
use crate::{vint, Error, Result};

const MAGIC5: &[u8; 8] = b"Rar!\x1a\x07\x01\x00";

/// Incremental RAR5 writer.
///
/// ```
/// let mut w = rar_codec::ArchiveBuilder::new();
/// w.add_stored("a.txt", b"alpha").unwrap();
/// w.add_stored("dir/b.bin", b"").unwrap(); // empty file is fine
/// let bytes = w.finish();
/// assert_eq!(&bytes[..8], b"Rar!\x1a\x07\x01\x00");
/// ```
#[derive(Debug)]
pub struct ArchiveBuilder {
    buf: Vec<u8>,
    count: usize,
}

impl Default for ArchiveBuilder {
    /// Same as [`ArchiveBuilder::new`]: magic marker plus main header.
    fn default() -> Self {
        Self::new()
    }
}

impl ArchiveBuilder {
    /// Start a new empty archive: writes the RAR5 magic marker and the
    /// mandatory main archive header (type 1, no extra area, not a volume).
    pub fn new() -> Self {
        let mut w = ArchiveBuilder {
            buf: MAGIC5.to_vec(),
            count: 0,
        };
        // Main header body: HTYPE = 1, HFLAGS = 0, archive flags vint = 0.
        w.push_header(&[1, 0, 0]);
        w
    }

    /// Number of entries added so far.
    pub fn len(&self) -> usize {
        self.count
    }

    pub fn is_empty(&self) -> bool {
        self.count == 0
    }

    /// Add a file entry, stored uncompressed.
    ///
    /// `name` is the member path (e.g. `dir/file.txt`); it must be non-empty,
    /// shorter than 2^16 bytes and free of NUL bytes.
    pub fn add_stored(&mut self, name: &str, data: &[u8]) -> Result<&mut Self> {
        self.add(name, data, false)
    }

    /// Add a directory entry (no payload).
    pub fn add_directory(&mut self, name: &str) -> Result<&mut Self> {
        let name = name.strip_suffix('/').unwrap_or(name);
        self.add(name, &[], true)
    }

    fn add(&mut self, name: &str, data: &[u8], dir: bool) -> Result<&mut Self> {
        if name.is_empty() {
            return Err(Error::Malformed("empty entry name"));
        }
        if name.len() > u16::MAX as usize {
            return Err(Error::Malformed("entry name too long"));
        }
        if name.as_bytes().contains(&0) {
            return Err(Error::Malformed("NUL in entry name"));
        }

        // Header body: everything between the HSIZE vint and the payload.
        // HFLAGS = 0x02 (data size field present); no extra area.
        let mut body = Vec::with_capacity(name.len() + 24);
        body.push(2); // HTYPE = file header
        if dir {
            // directories carry no data area
            vint::write(&mut body, 0); // HFLAGS
        } else {
            vint::write(&mut body, 0x02); // HFLAGS: data size follows
                                          // Data size is a *common* field: right after HFLAGS, before the
                                          // file-specific fields. Stored data is raw, no flag byte.
            vint::write(&mut body, data.len() as u64);
        }
        let mut file_flags = 0x04u64; // data CRC32 present
        if dir {
            file_flags |= 0x01;
        }
        vint::write(&mut body, file_flags);
        vint::write(&mut body, data.len() as u64); // unpacked size
        vint::write(&mut body, 0); // attributes
        body.extend_from_slice(&crc32(data).to_le_bytes()); // file CRC-32
                                                            // Compression information: version 0, not solid, method 0 (store),
                                                            // minimum dictionary size (bits 11-15 = 0 → 128 KB).
        vint::write(&mut body, 0);
        vint::write(&mut body, 1); // Host OS: Unix
        vint::write(&mut body, name.len() as u64);
        body.extend_from_slice(name.as_bytes());

        self.push_header(&body);

        if !dir {
            // Stored payload: raw bytes, no flag byte (RAR5 store = plain).
            self.buf.extend_from_slice(data);
        }
        self.count += 1;
        Ok(self)
    }

    /// Finish the archive: append the end-of-archive header and return the
    /// full byte stream.
    pub fn finish(mut self) -> Vec<u8> {
        // End header body: HTYPE = 5, HFLAGS = 0, no fields.
        self.push_header(&[5, 0]);
        self.buf
    }

    /// Write one header: CRC32 over (HSIZE vint ++ body), then those bytes.
    fn push_header(&mut self, body: &[u8]) {
        let mut sized = Vec::with_capacity(body.len() + 3);
        vint::write(&mut sized, body.len() as u64);
        sized.extend_from_slice(body);
        self.buf.extend_from_slice(&crc32(&sized).to_le_bytes());
        self.buf.extend_from_slice(&sized);
    }
}
