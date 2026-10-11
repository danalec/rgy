//! CRC-32 checksum implementation.
//!
//! Implements the CRC-32 algorithm (IEEE 802.3 polynomial) used in ZIP, gzip, PNG, etc.

/// CRC-32 lookup table (IEEE 802.3 polynomial: 0xEDB88320).
const CRC32_TABLE: [u32; 256] = generate_crc32_table();

const fn generate_crc32_table() -> [u32; 256] {
    let mut table = [0u32; 256];
    let mut i = 0;
    while i < 256 {
        let mut crc = i as u32;
        let mut j = 0;
        while j < 8 {
            if crc & 1 != 0 {
                crc = (crc >> 1) ^ 0xEDB88320;
            } else {
                crc >>= 1;
            }
            j += 1;
        }
        table[i] = crc;
        i += 1;
    }
    table
}

/// Calculate CRC-32 checksum of data.
///
/// The worked example lives in [`tests::test_crc32_known`]; this module is
/// crate-private, so a doc-test cannot reach `crc32` at all.
#[allow(missing_docs)]
pub fn crc32(data: &[u8]) -> u32 {
    crc32_update(0xFFFFFFFF, data) ^ 0xFFFFFFFF
}

/// Update CRC-32 with additional data.
///
/// Use this for streaming CRC calculation.
pub fn crc32_update(mut crc: u32, data: &[u8]) -> u32 {
    for &byte in data {
        let index = ((crc ^ byte as u32) & 0xFF) as usize;
        crc = (crc >> 8) ^ CRC32_TABLE[index];
    }
    crc
}

/// CRC-32 calculator for streaming operations.
#[derive(Debug, Clone, Default)]
pub struct Crc32Calculator {
    crc: u32,
}

impl Crc32Calculator {
    /// Create a new CRC-32 calculator.
    pub fn new() -> Self {
        Self { crc: 0xFFFFFFFF }
    }

    /// Update with additional data.
    pub fn update(&mut self, data: &[u8]) {
        self.crc = crc32_update(self.crc, data);
    }

    /// Get the final CRC-32 value.
    pub fn finish(self) -> u32 {
        self.crc ^ 0xFFFFFFFF
    }

    /// Reset the calculator.
    pub fn reset(&mut self) {
        self.crc = 0xFFFFFFFF;
    }

    /// Get current CRC value without finalizing.
    pub fn current(&self) -> u32 {
        self.crc ^ 0xFFFFFFFF
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_crc32_empty() {
        assert_eq!(crc32(b""), 0);
    }

    #[test]
    fn test_crc32_known() {
        // Test vector from PNG spec
        assert_eq!(crc32(b"123456789"), 0xCBF43926);
    }

    #[test]
    fn test_crc32_streaming() {
        let mut calc = Crc32Calculator::new();
        calc.update(b"123");
        calc.update(b"456");
        calc.update(b"789");
        assert_eq!(calc.finish(), 0xCBF43926);
    }

    #[test]
    fn test_crc32_update_consistency() {
        let full = crc32(b"Hello, World!");

        let mut partial = 0xFFFFFFFF;
        partial = crc32_update(partial, b"Hello, ");
        partial = crc32_update(partial, b"World!");

        assert_eq!(partial ^ 0xFFFFFFFF, full);
    }

    #[test]
    fn test_crc32_various_inputs() {
        assert_eq!(crc32(b"a"), 0xE8B7BE43);
        assert_eq!(crc32(b"ab"), 0x9E83486D);
        assert_eq!(crc32(b"abc"), 0x352441C2);
        assert_eq!(crc32(b"abcd"), 0xED82CD11);
    }
}
