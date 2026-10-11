//! EXIF reader, vendored from upstream `b01/io/image/exif` at commits 37acc97
//! and d4177e5 (5 files + makers/). upstream is the source of truth: the
//! container layer (JPEG/WebP/PNG/TIFF unwrapping), the three
//! value-extraction fixes and the per-entry renderer all landed there
//! first, with Pillow-written vectors covering all four containers.
//!
//! Copy-time changes: `alloc::` paths rewritten to `std::` (the source is
//! no_std + alloc), upstream `crate::b01::…` paths pointed at the local
//! module, fixture paths re-anchored to this crate's `testdata/`
//! (fixtures are byte-identical copies), and nothing else.
//!
//! Vendored copy: upstream keeps the whole tag vocabulary and MakerNote
//! tables as `pub` items with no internal users; inside this adapter
//! crate they are private to the module, so the unused-constant pass
//! fires on hundreds of names the tag lookups still serve by table.
#![allow(dead_code)]

use alloc::string::{String, ToString};
use alloc::vec::Vec;

mod ifd;
pub mod makers;
mod tags;
mod tiff;

pub use ifd::*;
pub use tags::*;
pub use tiff::*;

/// EXIF reader.
#[derive(Debug)]
pub struct ExifReader<'a> {
    data: &'a [u8],
    pos: usize,
    /// TIFF header start position in data.
    tiff_start: usize,
    /// TIFF byte order.
    byte_order: ByteOrder,
    /// IFD0 entries.
    pub ifd0: Vec<IfdEntry>,
    /// IFD1 entries (thumbnail).
    pub ifd1: Vec<IfdEntry>,
    /// Exif IFD entries.
    pub exif_ifd: Vec<IfdEntry>,
    /// GPS IFD entries.
    pub gps_ifd: Vec<IfdEntry>,
    /// Interop IFD entries.
    pub interop_ifd: Vec<IfdEntry>,
}

/// TIFF byte order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ByteOrder {
    #[default]
    LittleEndian,
    BigEndian,
}

impl<'a> ExifReader<'a> {
    /// Create a new EXIF reader.
    #[must_use]
    pub fn new(data: &'a [u8]) -> Self {
        Self {
            data,
            pos: 0,
            tiff_start: 0,
            byte_order: ByteOrder::LittleEndian,
            ifd0: Vec::new(),
            ifd1: Vec::new(),
            exif_ifd: Vec::new(),
            gps_ifd: Vec::new(),
            interop_ifd: Vec::new(),
        }
    }

    /// Parse EXIF out of a whole image file: JPEG (APP1 segment scan),
    /// WebP (RIFF `EXIF` chunk), PNG (`eXIf` chunk) or a bare TIFF/GeoTIFF.
    /// The container is unwrapped first; the payload is then parsed as it
    /// presents itself (`Exif\0\0`-prefixed in JPEG/WebP-EXIF, bare TIFF
    /// in PNG `eXIf` and TIFF files).
    pub fn parse_container(data: &'a [u8]) -> Result<ExifReader<'a>, String> {
        let payload = find_exif_payload(data).ok_or("no EXIF payload in container")?;
        let mut reader = ExifReader::new(payload);
        if payload.len() >= 6 && &payload[0..6] == b"Exif\x00\x00" {
            reader.parse_jpeg()?;
        } else {
            reader.parse_tiff()?;
        }
        Ok(reader)
    }

    /// Parse EXIF data from JPEG APP1 segment (with "Exif\0\0" header).
    pub fn parse_jpeg(&mut self) -> Result<(), String> {
        // Check for "Exif\0\0" header
        if self.data.len() < 6 {
            return Err("Data too short for JPEG EXIF".into());
        }

        if &self.data[0..6] != b"Exif\x00\x00" {
            return Err("Missing Exif header".into());
        }

        self.pos = 6;
        self.tiff_start = 6; // TIFF header starts at byte 6
        self.parse_tiff()
    }

    /// Parse TIFF-format EXIF data (without Exif header).
    pub fn parse_tiff(&mut self) -> Result<(), String> {
        if self.data.len() < self.pos + 8 {
            return Err("Data too short for TIFF header".into());
        }

        // Record TIFF header position if not set
        if self.tiff_start == 0 {
            self.tiff_start = self.pos;
        }

        // Determine byte order
        let bo = &self.data[self.pos..self.pos + 2];
        if bo == b"II" {
            self.byte_order = ByteOrder::LittleEndian;
        } else if bo == b"MM" {
            self.byte_order = ByteOrder::BigEndian;
        } else {
            return Err("Invalid TIFF byte order".into());
        }
        self.pos += 2;

        // Check magic number (42)
        let magic = self.read_u16()?;
        if magic != 42 {
            return Err("Invalid TIFF magic number".into());
        }

        // Read IFD0 offset (relative to TIFF header start)
        let ifd0_offset = self.read_u32()? as usize;

        // Parse IFD0 (add TIFF header position to offset)
        let absolute_offset = self.tiff_start + ifd0_offset;
        let ifd0_entries = self.parse_ifd_entries(absolute_offset, IfdType::Ifd0)?;
        self.ifd0 = ifd0_entries;

        // Extract sub-IFDs
        self.extract_sub_ifds()?;

        Ok(())
    }

    /// Parse IFD entries at the given offset, returning the entries.
    fn parse_ifd_entries(
        &mut self,
        offset: usize,
        ifd_type: IfdType,
    ) -> Result<Vec<IfdEntry>, String> {
        if offset >= self.data.len() {
            return Ok(Vec::new());
        }

        self.pos = offset;

        let count = self.read_u16()? as usize;
        let mut entries = Vec::new();

        for _ in 0..count {
            if self.pos + 12 > self.data.len() {
                break;
            }

            let tag = self.read_u16()?;
            let format = self.read_u16()?;
            let entry_count = self.read_u32()?;
            let value_offset = self.pos;
            let raw_value = self.read_u32()?;
            // An inline value occupies `type_size x count` bytes at the
            // start of the value field, in the file's byte order. For a
            // big-endian file a 1- or 2-byte value sits in the *high*
            // bytes of the u32 we just read, so the numeric value is
            // that field shifted down (ISO 400 read back as 400<<16).
            let total = Self::type_size(format).saturating_mul(entry_count as usize);
            let value = if total > 0 && total < 4 && self.byte_order == ByteOrder::BigEndian {
                raw_value >> (8 * (4 - total))
            } else {
                raw_value
            };

            entries.push(IfdEntry {
                tag,
                format,
                count: entry_count,
                value_offset,
                value,
                ifd_type,
            });
        }

        // Read next IFD offset for IFD0/IFD1 chain
        if ifd_type == IfdType::Ifd0 && self.pos + 4 <= self.data.len() {
            let next_ifd = self.read_u32()? as usize;
            let absolute_next = self.tiff_start + next_ifd;
            if next_ifd > 0 && absolute_next < self.data.len() {
                let ifd1_entries = self.parse_ifd_entries(absolute_next, IfdType::Ifd1)?;
                self.ifd1 = ifd1_entries;
            }
        }

        Ok(entries)
    }

    /// Extract ExifIFD, GPSIFD, and InteropIFD from IFD0.
    fn extract_sub_ifds(&mut self) -> Result<(), String> {
        // Find ExifIFD pointer
        if let Some(entry) = self.ifd0.iter().find(|e| e.tag == 0x8769) {
            let offset = entry.value as usize;
            let absolute_offset = self.tiff_start + offset;
            if absolute_offset < self.data.len() {
                self.exif_ifd = self.parse_ifd_entries(absolute_offset, IfdType::Exif)?;
            }
        }

        // Find GPS IFD pointer
        if let Some(entry) = self.ifd0.iter().find(|e| e.tag == 0x8825) {
            let offset = entry.value as usize;
            let absolute_offset = self.tiff_start + offset;
            if absolute_offset < self.data.len() {
                self.gps_ifd = self.parse_ifd_entries(absolute_offset, IfdType::Gps)?;
            }
        }

        // Find InteropIFD pointer (inside ExifIFD)
        if let Some(entry) = self.exif_ifd.iter().find(|e| e.tag == 0xA005) {
            let offset = entry.value as usize;
            let absolute_offset = self.tiff_start + offset;
            if absolute_offset < self.data.len() {
                self.interop_ifd = self.parse_ifd_entries(absolute_offset, IfdType::Interop)?;
            }
        }

        Ok(())
    }

    /// Read a u16 value.
    fn read_u16(&mut self) -> Result<u16, String> {
        if self.pos + 2 > self.data.len() {
            return Err("Unexpected end of data".into());
        }

        let value = match self.byte_order {
            ByteOrder::LittleEndian => {
                u16::from_le_bytes([self.data[self.pos], self.data[self.pos + 1]])
            }
            ByteOrder::BigEndian => {
                u16::from_be_bytes([self.data[self.pos], self.data[self.pos + 1]])
            }
        };
        self.pos += 2;
        Ok(value)
    }

    /// Read a u32 value.
    fn read_u32(&mut self) -> Result<u32, String> {
        if self.pos + 4 > self.data.len() {
            return Err("Unexpected end of data".into());
        }

        let value = match self.byte_order {
            ByteOrder::LittleEndian => u32::from_le_bytes([
                self.data[self.pos],
                self.data[self.pos + 1],
                self.data[self.pos + 2],
                self.data[self.pos + 3],
            ]),
            ByteOrder::BigEndian => u32::from_be_bytes([
                self.data[self.pos],
                self.data[self.pos + 1],
                self.data[self.pos + 2],
                self.data[self.pos + 3],
            ]),
        };
        self.pos += 4;
        Ok(value)
    }

    /// Absolute buffer offset holding an entry's value.
    ///
    /// Values of four bytes or fewer live inline in the entry, and
    /// `value_offset` is already an absolute buffer index. Longer values
    /// store an offset that is **relative to the TIFF header**, so it has
    /// to have `tiff_start` added — which is 6 for a JPEG, 0 for a bare
    /// TIFF. Reading it as absolute makes every out-of-line string and
    /// every rational short by exactly the header size, which for a JPEG
    /// means GPS coordinates, exposure, f-number and focal length all
    /// read from the wrong place. IFD *pointer* offsets already went
    /// through `self.tiff_start`; the value readers did not.
    /// Bytes per element for a TIFF field type (types 1-13).
    fn type_size(format: u16) -> usize {
        match format {
            1 | 2 | 6 | 7 => 1,    // BYTE ASCII SBYTE UNDEFINED
            3 | 8 => 2,            // SHORT
            4 | 9 | 11 => 4,       // LONG SLONG FLOAT
            5 | 10 | 12 | 13 => 8, // RATIONAL SRATIONAL DOUBLE LONG8
            _ => 0,
        }
    }

    fn value_offset_of(&self, entry: &IfdEntry) -> Option<usize> {
        let total = Self::type_size(entry.format).checked_mul(entry.count as usize)?;
        if total <= 4 {
            Some(entry.value_offset)
        } else {
            self.tiff_start.checked_add(entry.value as usize)
        }
    }

    /// First entry with `tag`, searching the Exif SubIFD before IFD0.
    ///
    /// Cameras put ISO/exposure/date tags in the Exif SubIFD (0x8769);
    /// Pillow and some encoders write the same tags straight into IFD0.
    /// The typed getters take either, the way a whole-file tag walk
    /// would.
    fn find_entry(&self, tag: u16) -> Option<&IfdEntry> {
        self.exif_ifd
            .iter()
            .chain(self.ifd0.iter())
            .find(|e| e.tag == tag)
    }

    /// Read a string value from an IFD entry.
    fn read_string(&self, entry: &IfdEntry) -> Option<String> {
        let offset = self.value_offset_of(entry)?;

        let end = offset.checked_add(entry.count as usize)?;
        if end > self.data.len() {
            return None;
        }

        let bytes = &self.data[offset..end];

        // Find null terminator
        let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());

        Some(String::from_utf8_lossy(&bytes[..end]).into_owned())
    }

    /// Render one entry's value as text: ASCII verbatim, inline
    /// numbers as numbers, rationals as decimals, anything else as its
    /// raw u32. The whole-file tag walk (`for entry in reader.ifd0…`)
    /// pairs this with [`tags::tag_name`] to produce a searchable
    /// listing without knowing each tag's type up front.
    pub fn entry_value(&self, entry: &IfdEntry) -> String {
        match entry.format {
            1 | 2 | 6 | 7 => self
                .read_string(entry)
                .unwrap_or_else(|| format!("<{} bytes>", entry.count)),
            3 | 4 | 8 | 9 => entry.value.to_string(),
            5 | 10 => match self.read_rational(entry) {
                Some(v) => {
                    if (v.fract()).abs() < f64::EPSILON {
                        format!("{}", v as i64)
                    } else {
                        format!("{v}")
                    }
                }
                None => format!("<rational {}>", entry.value),
            },
            11 => f32::from_bits(entry.value).to_string(),
            12 => self
                .value_offset_of(entry)
                .filter(|o| o + 8 <= self.data.len())
                .map(|o| {
                    f64::from_bits(u64::from_le_bytes(
                        self.data[o..o + 8].try_into().unwrap_or([0; 8]),
                    ))
                    .to_string()
                })
                .unwrap_or_else(|| "<double>".to_string()),
            _ => entry.value.to_string(),
        }
    }

    /// Get entry by tag from IFD0.
    #[must_use]
    pub fn get(&self, tag: u16) -> Option<&IfdEntry> {
        self.ifd0
            .iter()
            .find(|e| e.tag == tag)
            .or_else(|| self.exif_ifd.iter().find(|e| e.tag == tag))
    }

    /// Get string value by tag.
    #[must_use]
    pub fn get_string(&self, tag: u16) -> Option<String> {
        let entry = self.get(tag)?;
        self.read_string(entry)
    }

    /// Get DateTimeOriginal.
    #[must_use]
    pub fn datetime_original(&self) -> Option<String> {
        self.get_string(0x9003) // DateTimeOriginal
    }

    /// Get DateTime.
    #[must_use]
    pub fn datetime(&self) -> Option<String> {
        self.get_string(0x0132) // DateTime
    }

    /// Get make (manufacturer).
    #[must_use]
    pub fn make(&self) -> Option<String> {
        self.get_string(0x010F)
    }

    /// Get model.
    #[must_use]
    pub fn model(&self) -> Option<String> {
        self.get_string(0x0110)
    }

    /// Get GPS coordinates as (latitude, longitude).
    #[must_use]
    pub fn get_gps(&self) -> Option<(f64, f64)> {
        let lat_entry = self.gps_ifd.iter().find(|e| e.tag == 0x0002)?; // GPSLatitude
        let lat_ref = self.gps_ifd.iter().find(|e| e.tag == 0x0001); // GPSLatitudeRef
        let lon_entry = self.gps_ifd.iter().find(|e| e.tag == 0x0004)?; // GPSLongitude
        let lon_ref = self.gps_ifd.iter().find(|e| e.tag == 0x0003); // GPSLongitudeRef

        let lat = self.read_rational(lat_entry)?;
        let lon = self.read_rational(lon_entry)?;

        let lat = if let Some(ref_entry) = lat_ref {
            let ref_char = self.read_string(ref_entry);
            if ref_char.map(|s| s.starts_with('S')).unwrap_or(false) {
                -lat
            } else {
                lat
            }
        } else {
            lat
        };

        let lon = if let Some(ref_entry) = lon_ref {
            let ref_char = self.read_string(ref_entry);
            if ref_char.map(|s| s.starts_with('W')).unwrap_or(false) {
                -lon
            } else {
                lon
            }
        } else {
            lon
        };

        Some((lat, lon))
    }

    /// Read a rational value (degrees/minutes/seconds -> decimal).
    fn read_rational(&self, entry: &IfdEntry) -> Option<f64> {
        let offset = self.value_offset_of(entry)?;
        let end = offset.checked_add(24)?;
        if end > self.data.len() {
            return None;
        }

        // Three rationals: degrees, minutes, seconds
        let deg = self.read_rational_at(offset)?;
        let min = self.read_rational_at(offset + 8)?;
        let sec = self.read_rational_at(offset + 16)?;

        Some(deg + min / 60.0 + sec / 3600.0)
    }

    /// Read a single rational at offset.
    fn read_rational_at(&self, offset: usize) -> Option<f64> {
        if offset + 8 > self.data.len() {
            return None;
        }

        let (num, denom) = match self.byte_order {
            ByteOrder::LittleEndian => {
                let num = u32::from_le_bytes([
                    self.data[offset],
                    self.data[offset + 1],
                    self.data[offset + 2],
                    self.data[offset + 3],
                ]);
                let denom = u32::from_le_bytes([
                    self.data[offset + 4],
                    self.data[offset + 5],
                    self.data[offset + 6],
                    self.data[offset + 7],
                ]);
                (num, denom)
            }
            ByteOrder::BigEndian => {
                let num = u32::from_be_bytes([
                    self.data[offset],
                    self.data[offset + 1],
                    self.data[offset + 2],
                    self.data[offset + 3],
                ]);
                let denom = u32::from_be_bytes([
                    self.data[offset + 4],
                    self.data[offset + 5],
                    self.data[offset + 6],
                    self.data[offset + 7],
                ]);
                (num, denom)
            }
        };

        if denom == 0 {
            return None;
        }

        Some(num as f64 / denom as f64)
    }

    /// Get ISO speed.
    #[must_use]
    pub fn iso(&self) -> Option<u16> {
        let entry = self.find_entry(0x8827)?;
        Some(entry.value as u16)
    }

    /// Get focal length in mm.
    #[must_use]
    pub fn focal_length(&self) -> Option<f64> {
        let entry = self.find_entry(0x920A)?;
        self.read_single_rational(entry)
    }

    /// Get f-number (aperture).
    #[must_use]
    pub fn f_number(&self) -> Option<f64> {
        let entry = self.find_entry(0x829D)?;
        self.read_single_rational(entry)
    }

    /// Get exposure time (shutter speed).
    #[must_use]
    pub fn exposure_time(&self) -> Option<f64> {
        let entry = self.find_entry(0x829A)?;
        self.read_single_rational(entry)
    }

    /// Read a single rational from an entry.
    fn read_single_rational(&self, entry: &IfdEntry) -> Option<f64> {
        if entry.count != 1 || entry.format != 5 {
            return None;
        }
        let offset = self.value_offset_of(entry)?;

        self.read_rational_at(offset)
    }

    /// Get image width.
    #[must_use]
    pub fn image_width(&self) -> Option<u32> {
        self.ifd0
            .iter()
            .find(|e| e.tag == 0x0100)
            .map(|e| e.value)
            .or_else(|| {
                self.exif_ifd
                    .iter()
                    .find(|e| e.tag == 0xA002)
                    .map(|e| e.value)
            })
    }

    /// Get image height.
    #[must_use]
    pub fn image_height(&self) -> Option<u32> {
        self.ifd0
            .iter()
            .find(|e| e.tag == 0x0101)
            .map(|e| e.value)
            .or_else(|| {
                self.exif_ifd
                    .iter()
                    .find(|e| e.tag == 0xA003)
                    .map(|e| e.value)
            })
    }

    /// Get orientation.
    #[must_use]
    pub fn orientation(&self) -> Option<u16> {
        self.ifd0
            .iter()
            .find(|e| e.tag == 0x0112)
            .map(|e| e.value as u16)
    }

    /// Get X resolution (DPI).
    #[must_use]
    pub fn x_resolution(&self) -> Option<f64> {
        let entry = self.ifd0.iter().find(|e| e.tag == 0x011A)?;
        self.read_single_rational(entry)
    }

    /// Get Y resolution (DPI).
    #[must_use]
    pub fn y_resolution(&self) -> Option<f64> {
        let entry = self.ifd0.iter().find(|e| e.tag == 0x011B)?;
        self.read_single_rational(entry)
    }
}

/// Locate the Exif payload inside an image container.
///
/// JPEG: walk the marker segments (the SOI must be first) and return the
/// APP1 segment whose payload starts with `Exif\0\0`. WebP: the RIFF
/// `EXIF` chunk. PNG: the `eXIf` chunk (bare TIFF, no Exif header). A
/// file that is itself a TIFF/GeoTIFF is returned as-is.
pub fn find_exif_payload(data: &[u8]) -> Option<&[u8]> {
    if data.len() >= 4 && data[0] == 0xFF && data[1] == 0xD8 {
        return jpeg_exif(data);
    }
    if data.len() >= 12 && &data[0..4] == b"RIFF" && &data[8..12] == b"WEBP" {
        return riff_exif(data);
    }
    if data.len() >= 8 && data[0] == 0x89 && &data[1..4] == b"PNG" {
        return png_exif(data);
    }
    if data.len() >= 4 && (&data[0..4] == b"II*\x00" || &data[0..4] == b"MM\x00*") {
        return Some(data);
    }
    None
}

/// JPEG marker-segment walk to the first `Exif\0\0` APP1 payload.
fn jpeg_exif(data: &[u8]) -> Option<&[u8]> {
    let mut pos = 2usize; // past SOI
    while pos + 4 <= data.len() {
        if data[pos] != 0xFF {
            pos += 1; // resync over fill bytes
            continue;
        }
        let marker = data[pos + 1];
        match marker {
            0xFF => {
                pos += 1; // fill byte
                continue;
            }
            // markers without a length field
            0x01 | 0xD0..=0xD9 => {
                pos += 2;
                continue;
            }
            _ => {}
        }
        if pos + 4 > data.len() {
            return None;
        }
        let len = u16::from_be_bytes([data[pos + 2], data[pos + 3]]) as usize;
        if len < 2 {
            return None;
        }
        let seg = data.get(pos + 4..pos + 2 + len)?;
        if marker == 0xE1 && seg.len() >= 6 && &seg[0..6] == b"Exif\0\0" {
            return Some(seg);
        }
        if marker == 0xDA {
            return None; // start of scan: no more metadata segments
        }
        pos += 2 + len;
    }
    None
}

/// WebP: RIFF chunk walk to the `EXIF` chunk payload.
fn riff_exif(data: &[u8]) -> Option<&[u8]> {
    let mut pos = 12usize; // "RIFF" + size + "WEBP"
    while pos + 8 <= data.len() {
        let fourcc = &data[pos..pos + 4];
        let size = u32::from_le_bytes([data[pos + 4], data[pos + 5], data[pos + 6], data[pos + 7]])
            as usize;
        let body = data.get(pos + 8..pos + 8 + size)?;
        if fourcc == b"EXIF" {
            return Some(body);
        }
        pos += 8 + size + (size & 1); // chunks are word-aligned
    }
    None
}

/// PNG: chunk walk to the `eXIf` chunk (bare TIFF payload).
fn png_exif(data: &[u8]) -> Option<&[u8]> {
    let mut pos = 8usize; // signature + IHDR length/type
    while pos + 8 <= data.len() {
        let size =
            u32::from_be_bytes([data[pos], data[pos + 1], data[pos + 2], data[pos + 3]]) as usize;
        let fourcc = &data[pos + 4..pos + 8];
        let body = data.get(pos + 8..pos + 8 + size)?;
        if fourcc == b"eXIf" {
            return Some(body);
        }
        pos += 8 + size + 4; // + CRC
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_exif_minimal() {
        let mut data = vec![b'E', b'x', b'i', b'f', 0, 0];

        // TIFF header
        data.extend_from_slice(b"II"); // Little endian
        data.extend_from_slice(&42u16.to_le_bytes());
        data.extend_from_slice(&8u32.to_le_bytes()); // IFD0 offset

        // IFD0 at offset 8 (relative to TIFF header at byte 6)
        data.extend_from_slice(&1u16.to_le_bytes()); // 1 entry

        // Entry: Make (0x010F) - inline value (4 bytes max)
        data.extend_from_slice(&0x010Fu16.to_le_bytes()); // Tag
        data.extend_from_slice(&2u16.to_le_bytes()); // ASCII format
        data.extend_from_slice(&4u32.to_le_bytes()); // Count (4 chars fits inline)
        data.extend_from_slice(b"Tst\0"); // Value (4 bytes inline)

        // Next IFD offset
        data.extend_from_slice(&0u32.to_le_bytes());

        let mut reader = ExifReader::new(&data);
        reader.parse_jpeg().unwrap();

        assert_eq!(reader.make(), Some("Tst".to_string()));
    }

    /// Regression: an out-of-line value in a JPEG-wrapped EXIF.
    ///
    /// `test_exif_minimal` above uses a 4-byte Make, which fits *inline*
    /// in the entry, so it took the `value_offset` branch and passed
    /// regardless. Anything longer than four bytes is stored elsewhere as
    /// an offset **relative to the TIFF header** — which is byte 6 in a
    /// JPEG. Reading that as an absolute buffer index lands 6 bytes early,
    /// so this returned a mangled string until `value_offset_of` added
    /// `tiff_start` back.
    #[test]
    fn test_exif_out_of_line_value_in_jpeg() {
        const NAME: &[u8] = b"TestMake\0"; // 9 bytes: out of line
        let mut data: Vec<u8> = b"Exif\x00\x00".to_vec();
        // Everything below is relative to the TIFF header at byte 6.
        data.extend_from_slice(b"II");
        data.extend_from_slice(&42u16.to_le_bytes());
        data.extend_from_slice(&8u32.to_le_bytes()); // IFD0 at relative 8
        data.extend_from_slice(&1u16.to_le_bytes()); // one entry
        data.extend_from_slice(&0x010Fu16.to_le_bytes()); // Make
        data.extend_from_slice(&2u16.to_le_bytes()); // ASCII
        data.extend_from_slice(&(NAME.len() as u32).to_le_bytes()); // count
                                                                    // value/offset field: the offset, TIFF-header relative
        data.extend_from_slice(&30u32.to_le_bytes());
        data.extend_from_slice(&0u32.to_le_bytes()); // next IFD
        while data.len() < 6 + 30 {
            data.push(0); // pad up to the data area
        }
        data.extend_from_slice(NAME);

        let mut reader = ExifReader::new(&data);
        reader.parse_jpeg().expect("should parse");

        assert_eq!(
            reader.make(),
            Some("TestMake".to_string()),
            "out-of-line string read from the wrong offset"
        );
    }

    #[test]
    fn test_gps_parsing() {
        let mut reader = ExifReader::new(&[]);
        reader.byte_order = ByteOrder::LittleEndian;

        // Setup GPS IFD with coordinates
        let mut gps_data = vec![0u8; 100];

        // GPSLatitude (3 rationals at offset 24)
        let lat_offset = 24;
        gps_data.extend_from_slice(&(37u32.to_le_bytes())); // 37 deg
        gps_data.extend_from_slice(&(1u32.to_le_bytes()));
        gps_data.extend_from_slice(&(46u32.to_le_bytes())); // 46 min
        gps_data.extend_from_slice(&(1u32.to_le_bytes()));
        gps_data.extend_from_slice(&(30u32.to_le_bytes())); // 30 sec
        gps_data.extend_from_slice(&(1u32.to_le_bytes()));

        reader.gps_ifd.push(IfdEntry {
            tag: 0x0002,
            format: 5,
            count: 3,
            value_offset: 0,
            value: lat_offset as u32,
            ifd_type: IfdType::Gps,
        });

        reader.gps_ifd.push(IfdEntry {
            tag: 0x0001,
            format: 2,
            count: 2,
            value_offset: 0,
            value: b'N' as u32,
            ifd_type: IfdType::Gps,
        });

        reader.data = &gps_data;

        // This would need proper data setup
    }

    /// Third-party vectors: EXIF written by Pillow's standard TIFF/EXIF
    /// writer (Make/Model/DateTime, exposure, f-number, ISO, focal
    /// length, dimensions and a full GPS IFD) in four containers.
    #[test]
    fn test_exif_third_party_containers() {
        macro_rules! case {
            ($name:literal) => {{
                let data = include_bytes!(concat!("../../testdata/exif_sample.", $name));
                let r =
                    ExifReader::parse_container(data).unwrap_or_else(|e| panic!("{}: {e}", $name));
                assert_eq!(r.make().as_deref(), Some("KADANE"), "{}", $name);
                assert_eq!(r.model().as_deref(), Some("RGX-CAM"), "{}", $name);
                assert_eq!(
                    r.datetime_original().as_deref(),
                    Some("2026:09:28 12:00:00"),
                    "{}",
                    $name
                );
                assert_eq!(r.iso(), Some(400), "{}", $name);
                assert!((r.f_number().unwrap() - 2.8).abs() < 0.01, "{}", $name);
                assert!((r.focal_length().unwrap() - 25.0).abs() < 0.01, "{}", $name);
                assert!(
                    (r.exposure_time().unwrap() - 0.008).abs() < 0.0005,
                    "{}",
                    $name
                );
                let (lat, lon) = r.get_gps().unwrap_or_else(|| panic!("gps {}", $name));
                assert!((lat - 48.8582).abs() < 0.001, "lat {lat} {}", $name);
                assert!((lon - 2.2929).abs() < 0.001, "lon {lon} {}", $name);
            }};
        }
        case!("jpg");
        case!("webp");
        case!("png");
        case!("tiff");
    }

    #[test]
    fn test_find_exif_payload_rejects_non_images() {
        assert!(find_exif_payload(b"not an image at all").is_none());
        assert!(find_exif_payload(&[0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x02]).is_none());
    }

    #[test]
    fn test_byte_order() {
        assert_eq!(ByteOrder::default(), ByteOrder::LittleEndian);
    }
}
