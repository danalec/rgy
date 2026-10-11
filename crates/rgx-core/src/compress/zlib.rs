//! ZLIB compression format (RFC 1950).
//!
//! Wrapper around DEFLATE with ZLIB header/trailer (ADLER32 checksum).

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt;

use super::cap::Bounded;
use super::deflate::{CompressionLevel, DeflateDecoder, DeflateEncoder};
use super::error::CompressError;

fn adler32(data: &[u8]) -> u32 {
    // 5552 bytes is the largest chunk over which `a` and `b` cannot overflow
    // u32, so the `% 65521` modulus only needs to run at the end of each
    // chunk (and once over the tail) — not inside the per-byte loop.
    let mut a = 1u32;
    let mut b = 0u32;

    let (chunks, remainder) = data.as_chunks::<5552>();
    for chunk in chunks {
        for byte in chunk {
            a = a.wrapping_add(*byte as u32);
            b = b.wrapping_add(a);
        }
        a %= 65521;
        b %= 65521;
    }
    for byte in remainder {
        a = a.wrapping_add(*byte as u32);
        b = b.wrapping_add(a);
    }
    a %= 65521;
    b %= 65521;

    (b << 16) | a
}

#[derive(Debug, Clone)]
pub struct ZlibError {
    pub message: String,
}

impl fmt::Display for ZlibError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ZLIB error: {}", self.message)
    }
}

impl From<CompressError> for ZlibError {
    fn from(e: CompressError) -> Self {
        ZlibError {
            message: format!("DEFLATE error: {:?}", e),
        }
    }
}

pub type ZlibResult<T> = Result<T, ZlibError>;

// Vendored std adaptation — see compress/gzip.rs.
#[cfg(feature = "std")]
impl std::error::Error for ZlibError {}

pub struct ZlibEncoder {
    level: CompressionLevel,
}

impl ZlibEncoder {
    pub fn new() -> Self {
        Self::with_level(CompressionLevel::Default)
    }

    pub fn with_level(level: CompressionLevel) -> Self {
        Self { level }
    }

    pub fn encode(&self, data: &[u8]) -> Vec<u8> {
        let mut output = Vec::new();

        let cmf: u8 = 0x78;
        let flevel: u8 = match self.level {
            CompressionLevel::None => 0,
            CompressionLevel::Fast => 1,
            CompressionLevel::Default => 2,
            CompressionLevel::Best => 3,
        };
        let flg_partial = (flevel << 6) as u16;
        let fcheck = (31 - ((cmf as u16 * 256 + flg_partial) % 31)) % 31;
        let flg: u8 = (flevel << 6) | (fcheck as u8);
        output.push(cmf);
        output.push(flg);

        let mut encoder = DeflateEncoder::with_level(self.level as u8);
        let compressed = encoder.encode(data);
        output.extend_from_slice(&compressed);

        let adler = adler32(data);
        output.extend_from_slice(&adler.to_be_bytes());

        output
    }
}

impl Default for ZlibEncoder {
    fn default() -> Self {
        Self::new()
    }
}

pub struct ZlibDecoder;

impl ZlibDecoder {
    pub fn new() -> Self {
        Self
    }

    pub fn decode(&self, data: &[u8]) -> ZlibResult<Vec<u8>> {
        self.decode_bounded(data, super::cap::UNLIMITED)
            .map(|b| b.bytes)
    }

    /// Decode without expanding past `limit` bytes.
    ///
    /// RFC 1950 declares no uncompressed size, so there is nothing to refuse
    /// early from and the ceiling is the running one, in the deflate pass.
    /// A stream the cap stops comes back short and flagged, and its ADLER32
    /// is not checked: the checksum covers bytes the decode never saw.
    pub fn decode_bounded_with_consumed(
        &self,
        data: &[u8],
        limit: u64,
    ) -> ZlibResult<(Bounded, usize)> {
        if data.len() < 6 {
            return Err(ZlibError {
                message: "Data too short for ZLIB".into(),
            });
        }

        let cmf = data[0];
        let flg = data[1];

        if cmf & 0x0F != 8 {
            return Err(ZlibError {
                message: format!("Unsupported compression method: {}", cmf & 0x0F),
            });
        }

        if !(cmf as u16 * 256 + flg as u16).is_multiple_of(31) {
            return Err(ZlibError {
                message: "Invalid ZLIB header checksum".into(),
            });
        }

        let deflate_data = &data[2..];

        let mut decoder = DeflateDecoder::new();
        let (decompressed, consumed_deflate) = decoder
            .decode_bounded_with_consumed(deflate_data, limit)
            .map_err(ZlibError::from)?;

        let total_consumed = 2 + consumed_deflate + 4;
        if total_consumed > data.len() {
            return Err(ZlibError {
                message: "Unexpected EOF in ZLIB stream".into(),
            });
        }

        if decompressed.capped {
            return Ok((decompressed, total_consumed));
        }

        let trailer_offset = 2 + consumed_deflate;
        let expected_adler = u32::from_be_bytes([
            data[trailer_offset],
            data[trailer_offset + 1],
            data[trailer_offset + 2],
            data[trailer_offset + 3],
        ]);

        let actual_adler = adler32(&decompressed.bytes);
        if actual_adler != expected_adler {
            return Err(ZlibError {
                message: format!(
                    "ADLER32 mismatch: expected {}, got {}",
                    expected_adler, actual_adler
                ),
            });
        }

        Ok((decompressed, total_consumed))
    }

    pub fn decode_bounded(&self, data: &[u8], limit: u64) -> ZlibResult<Bounded> {
        if data.len() < 6 {
            return Err(ZlibError {
                message: "Data too short for ZLIB".into(),
            });
        }

        let cmf = data[0];
        let flg = data[1];

        if cmf & 0x0F != 8 {
            return Err(ZlibError {
                message: format!("Unsupported compression method: {}", cmf & 0x0F),
            });
        }

        if !(cmf as u16 * 256 + flg as u16).is_multiple_of(31) {
            return Err(ZlibError {
                message: "Invalid ZLIB header checksum".into(),
            });
        }

        let deflate_data = &data[2..data.len() - 4];

        let mut decoder = DeflateDecoder::new();
        let decompressed = decoder
            .decode_bounded(deflate_data, limit)
            .map_err(ZlibError::from)?;

        if decompressed.capped {
            return Ok(decompressed);
        }

        let trailer_offset = data.len() - 4;
        let expected_adler = u32::from_be_bytes([
            data[trailer_offset],
            data[trailer_offset + 1],
            data[trailer_offset + 2],
            data[trailer_offset + 3],
        ]);

        let actual_adler = adler32(&decompressed.bytes);
        if actual_adler != expected_adler {
            return Err(ZlibError {
                message: format!(
                    "ADLER32 mismatch: expected {}, got {}",
                    expected_adler, actual_adler
                ),
            });
        }

        Ok(decompressed)
    }
}

impl Default for ZlibDecoder {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_zlib_empty() {
        let encoder = ZlibEncoder::new();
        let compressed = encoder.encode(b"");
        let decoder = ZlibDecoder::new();
        let decompressed = decoder.decode(&compressed).unwrap();
        assert!(decompressed.is_empty());
    }

    #[test]
    fn test_zlib_simple() {
        let data = b"Hello, World!";
        let encoder = ZlibEncoder::new();
        let compressed = encoder.encode(data);
        let decoder = ZlibDecoder::new();
        let decompressed = decoder.decode(&compressed).unwrap();
        assert_eq!(decompressed, data.as_slice());
    }

    #[test]
    fn test_zlib_levels() {
        let data = b"This is a test string for compression with repeated patterns.";

        for level in [
            CompressionLevel::Fast,
            CompressionLevel::Default,
            CompressionLevel::Best,
        ] {
            let encoder = ZlibEncoder::with_level(level);
            let compressed = encoder.encode(data);
            let decoder = ZlibDecoder::new();
            let decompressed = decoder.decode(&compressed).unwrap();
            assert_eq!(decompressed, data.as_slice());
        }
    }

    #[test]
    fn test_adler32() {
        assert_eq!(adler32(b""), 1);
        assert_eq!(adler32(b"a"), 0x00620062);
        assert_eq!(adler32(b"abc"), 0x024D0127);
        assert_eq!(adler32(b"Hello, World!"), 0x1F9E046A);
    }

    /// Third-party vectors: zlib streams produced by CPython's zlib
    /// (default level) over empty/text/repetitive/binary payloads.
    #[test]
    fn test_zlib_third_party_vectors() {
        macro_rules! case {
            ($name:literal) => {{
                let compressed = include_bytes!(concat!("../../testdata/zlib_", $name, ".bin"));
                let expected = include_bytes!(concat!("../../testdata/zlib_", $name, ".expected"));
                let out = ZlibDecoder::new()
                    .decode(compressed)
                    .expect(concat!("zlib_", $name));
                assert_eq!(out, &expected[..], concat!("zlib_", $name));
            }};
        }
        case!("empty");
        case!("text");
        case!("repeat");
        case!("binary");
        case!("mixed");
    }

    /// RFC 1950 has no uncompressed size in its header, so the ceiling is
    /// the running one. A stream the cap stops is not an error: the prefix
    /// comes back flagged, and its ADLER32 is not checked, because the
    /// checksum covers bytes the decode never saw.
    #[test]
    fn decode_bounded_stops_at_the_cap() {
        let compressed = include_bytes!("../../testdata/zlib_repeat.bin");
        let expected = include_bytes!("../../testdata/zlib_repeat.expected");
        let b = ZlibDecoder::new()
            .decode_bounded(compressed, 1000)
            .expect("bounded");
        assert!(b.capped);
        assert_eq!(b.declared, None);
        assert!(b.bytes.len() <= 1000, "emitted {} bytes", b.bytes.len());
        assert_eq!(b.bytes, &expected[..b.bytes.len()], "not the real prefix");

        let whole = ZlibDecoder::new()
            .decode_bounded(compressed, expected.len() as u64)
            .expect("exact");
        assert!(!whole.capped);
        assert_eq!(whole.bytes, expected);
    }
}
