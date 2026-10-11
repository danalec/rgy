//! TIFF structure utilities.
//!
//! Provides byte-order-aware reading utilities for TIFF/EXIF data.

use super::ByteOrder;
use alloc::string::String;
use alloc::vec::Vec;

/// TIFF header structure.
#[derive(Debug, Clone)]
pub struct TiffHeader {
    /// Byte order marker.
    pub byte_order: ByteOrder,
    /// Magic number (42).
    pub magic: u16,
    /// Offset to first IFD.
    pub ifd0_offset: u32,
}

impl Default for TiffHeader {
    fn default() -> Self {
        Self {
            byte_order: ByteOrder::LittleEndian,
            magic: 42,
            ifd0_offset: 8,
        }
    }
}

/// Parse TIFF header from bytes.
pub fn parse_header(data: &[u8]) -> Result<TiffHeader, String> {
    if data.len() < 8 {
        return Err("Data too short for TIFF header".into());
    }

    let byte_order = if &data[0..2] == b"II" {
        ByteOrder::LittleEndian
    } else if &data[0..2] == b"MM" {
        ByteOrder::BigEndian
    } else {
        return Err("Invalid TIFF byte order".into());
    };

    let magic = read_u16(data, 2, byte_order)?;
    if magic != 42 {
        return Err("Invalid TIFF magic number".into());
    }

    let ifd0_offset = read_u32(data, 4, byte_order)?;

    Ok(TiffHeader {
        byte_order,
        magic,
        ifd0_offset,
    })
}

/// Read u16 at offset with byte order.
pub fn read_u16(data: &[u8], offset: usize, byte_order: ByteOrder) -> Result<u16, String> {
    if offset + 2 > data.len() {
        return Err("Unexpected end of data".into());
    }

    let value = match byte_order {
        ByteOrder::LittleEndian => u16::from_le_bytes([data[offset], data[offset + 1]]),
        ByteOrder::BigEndian => u16::from_be_bytes([data[offset], data[offset + 1]]),
    };
    Ok(value)
}

/// Read u32 at offset with byte order.
pub fn read_u32(data: &[u8], offset: usize, byte_order: ByteOrder) -> Result<u32, String> {
    if offset + 4 > data.len() {
        return Err("Unexpected end of data".into());
    }

    let value = match byte_order {
        ByteOrder::LittleEndian => u32::from_le_bytes([
            data[offset],
            data[offset + 1],
            data[offset + 2],
            data[offset + 3],
        ]),
        ByteOrder::BigEndian => u32::from_be_bytes([
            data[offset],
            data[offset + 1],
            data[offset + 2],
            data[offset + 3],
        ]),
    };
    Ok(value)
}

/// Read i16 at offset with byte order.
pub fn read_i16(data: &[u8], offset: usize, byte_order: ByteOrder) -> Result<i16, String> {
    if offset + 2 > data.len() {
        return Err("Unexpected end of data".into());
    }

    let value = match byte_order {
        ByteOrder::LittleEndian => i16::from_le_bytes([data[offset], data[offset + 1]]),
        ByteOrder::BigEndian => i16::from_be_bytes([data[offset], data[offset + 1]]),
    };
    Ok(value)
}

/// Read i32 at offset with byte order.
pub fn read_i32(data: &[u8], offset: usize, byte_order: ByteOrder) -> Result<i32, String> {
    if offset + 4 > data.len() {
        return Err("Unexpected end of data".into());
    }

    let value = match byte_order {
        ByteOrder::LittleEndian => i32::from_le_bytes([
            data[offset],
            data[offset + 1],
            data[offset + 2],
            data[offset + 3],
        ]),
        ByteOrder::BigEndian => i32::from_be_bytes([
            data[offset],
            data[offset + 1],
            data[offset + 2],
            data[offset + 3],
        ]),
    };
    Ok(value)
}

/// Read rational (u32/u32) at offset.
pub fn read_rational(
    data: &[u8],
    offset: usize,
    byte_order: ByteOrder,
) -> Result<(u32, u32), String> {
    if offset + 8 > data.len() {
        return Err("Unexpected end of data".into());
    }

    let numerator = read_u32(data, offset, byte_order)?;
    let denominator = read_u32(data, offset + 4, byte_order)?;
    Ok((numerator, denominator))
}

/// Read signed rational (i32/i32) at offset.
pub fn read_srational(
    data: &[u8],
    offset: usize,
    byte_order: ByteOrder,
) -> Result<(i32, i32), String> {
    if offset + 8 > data.len() {
        return Err("Unexpected end of data".into());
    }

    let numerator = read_i32(data, offset, byte_order)?;
    let denominator = read_i32(data, offset + 4, byte_order)?;
    Ok((numerator, denominator))
}

/// Read f32 at offset.
pub fn read_f32(data: &[u8], offset: usize, byte_order: ByteOrder) -> Result<f32, String> {
    if offset + 4 > data.len() {
        return Err("Unexpected end of data".into());
    }

    let bytes = [
        data[offset],
        data[offset + 1],
        data[offset + 2],
        data[offset + 3],
    ];
    let value = match byte_order {
        ByteOrder::LittleEndian => f32::from_le_bytes(bytes),
        ByteOrder::BigEndian => f32::from_be_bytes(bytes),
    };
    Ok(value)
}

/// Read f64 at offset.
pub fn read_f64(data: &[u8], offset: usize, byte_order: ByteOrder) -> Result<f64, String> {
    if offset + 8 > data.len() {
        return Err("Unexpected end of data".into());
    }

    let bytes = [
        data[offset],
        data[offset + 1],
        data[offset + 2],
        data[offset + 3],
        data[offset + 4],
        data[offset + 5],
        data[offset + 6],
        data[offset + 7],
    ];
    let value = match byte_order {
        ByteOrder::LittleEndian => f64::from_le_bytes(bytes),
        ByteOrder::BigEndian => f64::from_be_bytes(bytes),
    };
    Ok(value)
}

/// Read ASCII string at offset.
#[must_use]
pub fn read_string(data: &[u8], offset: usize, length: usize) -> String {
    if offset + length > data.len() {
        return String::new();
    }

    let bytes = &data[offset..offset + length];
    let end = bytes.iter().position(|&b| b == 0).unwrap_or(length);
    let actual_end = end.min(bytes.len());
    String::from_utf8_lossy(&bytes[..actual_end]).into_owned()
}

/// Read array of u16 values.
pub fn read_u16_array(
    data: &[u8],
    offset: usize,
    count: usize,
    byte_order: ByteOrder,
) -> Result<Vec<u16>, String> {
    if offset + count * 2 > data.len() {
        return Err("Unexpected end of data".into());
    }

    let mut values = Vec::with_capacity(count);
    for i in 0..count {
        values.push(read_u16(data, offset + i * 2, byte_order)?);
    }
    Ok(values)
}

/// Read array of u32 values.
pub fn read_u32_array(
    data: &[u8],
    offset: usize,
    count: usize,
    byte_order: ByteOrder,
) -> Result<Vec<u32>, String> {
    if offset + count * 4 > data.len() {
        return Err("Unexpected end of data".into());
    }

    let mut values = Vec::with_capacity(count);
    for i in 0..count {
        values.push(read_u32(data, offset + i * 4, byte_order)?);
    }
    Ok(values)
}

/// Read array of rationals.
pub fn read_rational_array(
    data: &[u8],
    offset: usize,
    count: usize,
    byte_order: ByteOrder,
) -> Result<Vec<(u32, u32)>, String> {
    if offset + count * 8 > data.len() {
        return Err("Unexpected end of data".into());
    }

    let mut values = Vec::with_capacity(count);
    for i in 0..count {
        values.push(read_rational(data, offset + i * 8, byte_order)?);
    }
    Ok(values)
}

/// Calculate APEX value to photographic value.
/// APEX aperture: Av = 2 * log2(N) where N is f-number
#[must_use]
pub fn apex_to_f_number(apex_value: f64) -> f64 {
    let x = apex_value / 2.0;
    2.0f64.powf(x)
}

/// Convert APEX shutter speed to exposure time.
/// APEX shutter: Tv = -log2(t) where t is exposure time
#[must_use]
pub fn apex_to_exposure_time(apex_value: f64) -> f64 {
    2.0f64.powf(-apex_value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_read_u16_le() {
        let data = [0x01, 0x00];
        let val = read_u16(&data, 0, ByteOrder::LittleEndian).unwrap();
        assert_eq!(val, 1);
    }

    #[test]
    fn test_read_u16_be() {
        let data = [0x00, 0x01];
        let val = read_u16(&data, 0, ByteOrder::BigEndian).unwrap();
        assert_eq!(val, 1);
    }

    #[test]
    fn test_read_rational() {
        let data = [0x01, 0x00, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00];
        let (num, denom) = read_rational(&data, 0, ByteOrder::LittleEndian).unwrap();
        assert_eq!(num, 1);
        assert_eq!(denom, 2);
    }

    #[test]
    fn test_apex_conversion() {
        let f = apex_to_f_number(8.0);
        assert!((f - 16.0).abs() < 0.01);

        let t = apex_to_exposure_time(6.0);
        assert!((t - 1.0 / 64.0).abs() < 0.0001);
    }
}
