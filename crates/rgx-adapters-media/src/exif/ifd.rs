//! IFD (Image File Directory) handling.
//!
//! Provides utilities for parsing and interpreting IFD entries.

use super::tags::IfdEntry;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

/// Calculate the size of a TIFF format type.
#[must_use]
pub fn format_size(format: u16) -> usize {
    match format {
        1 => 1,  // BYTE
        2 => 1,  // ASCII
        3 => 2,  // SHORT
        4 => 4,  // LONG
        5 => 8,  // RATIONAL
        6 => 1,  // SBYTE
        7 => 1,  // UNDEFINED
        8 => 2,  // SSHORT
        9 => 4,  // SLONG
        10 => 8, // SRATIONAL
        11 => 4, // FLOAT
        12 => 8, // DOUBLE
        13 => 4, // IFD
        _ => 0,
    }
}

/// Check if value is inline (fits in 4 bytes).
#[must_use]
pub fn is_inline(entry: &IfdEntry) -> bool {
    let size = format_size(entry.format) * entry.count as usize;
    size <= 4
}

/// Parse IFD entry value offset.
#[must_use]
pub fn value_offset(entry: &IfdEntry, base_offset: usize) -> usize {
    if is_inline(entry) {
        entry.value_offset
    } else {
        base_offset + entry.value as usize
    }
}

/// Format name for TIFF type.
#[must_use]
pub fn format_name(format: u16) -> &'static str {
    match format {
        1 => "BYTE",
        2 => "ASCII",
        3 => "SHORT",
        4 => "LONG",
        5 => "RATIONAL",
        6 => "SBYTE",
        7 => "UNDEFINED",
        8 => "SSHORT",
        9 => "SLONG",
        10 => "SRATIONAL",
        11 => "FLOAT",
        12 => "DOUBLE",
        13 => "IFD",
        _ => "Unknown",
    }
}

/// IFD value interpretation.
#[derive(Debug, Clone)]
pub enum IfdValue {
    Byte(u8),
    Short(u16),
    Long(u32),
    Rational { numerator: u32, denominator: u32 },
    SByte(i8),
    SShort(i16),
    SLong(i32),
    SRational { numerator: i32, denominator: i32 },
    Float(f32),
    Double(f64),
    String(String),
    Bytes(Vec<u8>),
    Shorts(Vec<u16>),
    Longs(Vec<u32>),
    Rationals(Vec<(u32, u32)>),
    Unknown,
}

impl IfdValue {
    /// Convert to string representation.
    #[must_use]
    pub fn to_display_string(&self) -> String {
        match self {
            Self::Byte(v) => v.to_string(),
            Self::Short(v) => v.to_string(),
            Self::Long(v) => v.to_string(),
            Self::Rational {
                numerator,
                denominator,
            } => {
                if *denominator == 0 {
                    String::from("0/0")
                } else {
                    let val = *numerator as f64 / *denominator as f64;
                    format!("{:.4}", val)
                }
            }
            Self::SByte(v) => v.to_string(),
            Self::SShort(v) => v.to_string(),
            Self::SLong(v) => v.to_string(),
            Self::SRational {
                numerator,
                denominator,
            } => {
                if *denominator == 0 {
                    String::from("0/0")
                } else {
                    let val = *numerator as f64 / *denominator as f64;
                    format!("{:.4}", val)
                }
            }
            Self::Float(v) => format!("{:.6}", v),
            Self::Double(v) => format!("{:.12}", v),
            Self::String(s) => s.clone(),
            Self::Bytes(b) => format!("[{} bytes]", b.len()),
            Self::Shorts(s) => format!("[{} shorts]", s.len()),
            Self::Longs(l) => format!("[{} longs]", l.len()),
            Self::Rationals(r) => format!("[{} rationals]", r.len()),
            Self::Unknown => String::from("?"),
        }
    }

    /// Get as f64 if applicable.
    #[must_use]
    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Self::Byte(v) => Some(*v as f64),
            Self::Short(v) => Some(*v as f64),
            Self::Long(v) => Some(*v as f64),
            Self::Rational {
                numerator,
                denominator,
            } => {
                if *denominator == 0 {
                    None
                } else {
                    Some(*numerator as f64 / *denominator as f64)
                }
            }
            Self::SByte(v) => Some(*v as f64),
            Self::SShort(v) => Some(*v as f64),
            Self::SLong(v) => Some(*v as f64),
            Self::SRational {
                numerator,
                denominator,
            } => {
                if *denominator == 0 {
                    None
                } else {
                    Some(*numerator as f64 / *denominator as f64)
                }
            }
            Self::Float(v) => Some(*v as f64),
            Self::Double(v) => Some(*v),
            _ => None,
        }
    }

    /// Get as u32 if applicable.
    #[must_use]
    pub fn as_u32(&self) -> Option<u32> {
        match self {
            Self::Byte(v) => Some(*v as u32),
            Self::Short(v) => Some(*v as u32),
            Self::Long(v) => Some(*v),
            _ => None,
        }
    }

    /// Get as string if applicable.
    #[must_use]
    pub fn as_string(&self) -> Option<&str> {
        match self {
            Self::String(s) => Some(s.as_str()),
            _ => None,
        }
    }
}

/// Orientation transformation.
#[derive(Debug, Clone, Copy, Default)]
pub struct OrientationTransform {
    /// Flip horizontally.
    pub flip_h: bool,
    /// Flip vertically.
    pub flip_v: bool,
    /// Rotate degrees (0, 90, 180, 270).
    pub rotate: u16,
}

/// Get orientation transformation from EXIF orientation value.
#[must_use]
pub fn orientation_transform(orientation: u16) -> OrientationTransform {
    match orientation {
        1 => OrientationTransform::default(),
        2 => OrientationTransform {
            flip_h: true,
            flip_v: false,
            rotate: 0,
        },
        3 => OrientationTransform {
            flip_h: false,
            flip_v: true,
            rotate: 180,
        },
        4 => OrientationTransform {
            flip_h: true,
            flip_v: true,
            rotate: 0,
        },
        5 => OrientationTransform {
            flip_h: false,
            flip_v: false,
            rotate: 90,
        },
        6 => OrientationTransform {
            flip_h: false,
            flip_v: true,
            rotate: 90,
        },
        7 => OrientationTransform {
            flip_h: true,
            flip_v: false,
            rotate: 270,
        },
        8 => OrientationTransform {
            flip_h: false,
            flip_v: false,
            rotate: 270,
        },
        _ => OrientationTransform::default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_size() {
        assert_eq!(format_size(1), 1);
        assert_eq!(format_size(3), 2);
        assert_eq!(format_size(4), 4);
        assert_eq!(format_size(5), 8);
        assert_eq!(format_size(12), 8);
    }

    #[test]
    fn test_orientation_transform() {
        let t = orientation_transform(1);
        assert!(!t.flip_h && !t.flip_v && t.rotate == 0);

        let t = orientation_transform(6);
        assert!(!t.flip_h && t.flip_v && t.rotate == 90);

        let t = orientation_transform(3);
        assert!(!t.flip_h && t.flip_v && t.rotate == 180);
    }
}
