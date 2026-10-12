// SPDX-License-Identifier: Apache-2.0

//! CRC-32 (IEEE 802.3) over byte slices.
//!
//! Independent implementation from the published CRC definition: the
//! reflected polynomial 0xEDB88320, an initial register of all ones,
//! and a final complement. The lookup table is generated at compile
//! time from the polynomial itself — it is the definition, not a copied
//! table. Per-member checksums in the 7z container are small buffers,
//! so the byte-at-a-time fold is the whole story here.

/// The reflected IEEE 802.3 polynomial.
const POLY: u32 = 0xEDB8_8320;

/// `TABLE[i]` is the CRC of the single byte `i`.
const TABLE: [u32; 256] = build_table();

const fn build_table() -> [u32; 256] {
    let mut table = [0u32; 256];
    let mut i = 0;
    while i < 256 {
        let mut crc = i as u32;
        let mut bit = 0;
        while bit < 8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ POLY
            } else {
                crc >> 1
            };
            bit += 1;
        }
        table[i] = crc;
        i += 1;
    }
    table
}

/// CRC-32 checksum of `data`.
#[allow(missing_docs)]
pub fn crc32(data: &[u8]) -> u32 {
    crc32_update(0xFFFF_FFFF, data) ^ 0xFFFF_FFFF
}

/// Fold `data` into a running CRC register. `crc32` is
/// `crc32_update(0xFFFF_FFFF, data) ^ 0xFFFF_FFFF`; keep the same
/// convention when chaining calls.
pub fn crc32_update(mut crc: u32, data: &[u8]) -> u32 {
    for &byte in data {
        let index = ((crc ^ u32::from(byte)) & 0xFF) as usize;
        crc = (crc >> 8) ^ TABLE[index];
    }
    crc
}

/// Streaming form: feed chunks, then take the finished checksum.
#[derive(Debug, Clone, Default)]
pub struct Crc32Calculator {
    crc: u32,
}

impl Crc32Calculator {
    /// A calculator at the start of a stream.
    pub fn new() -> Self {
        Self { crc: 0xFFFF_FFFF }
    }

    /// Absorb the next chunk.
    pub fn update(&mut self, data: &[u8]) {
        self.crc = crc32_update(self.crc, data);
    }

    /// The checksum of everything absorbed so far.
    pub fn finish(self) -> u32 {
        self.crc ^ 0xFFFF_FFFF
    }

    /// Back to the start of a stream.
    pub fn reset(&mut self) {
        self.crc = 0xFFFF_FFFF;
    }

    /// The checksum so far, without consuming the calculator.
    pub fn current(&self) -> u32 {
        self.crc ^ 0xFFFF_FFFF
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
        // The standard check value: CRC-32 of "123456789".
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

        let mut partial = 0xFFFF_FFFF;
        partial = crc32_update(partial, b"Hello, ");
        partial = crc32_update(partial, b"World!");

        assert_eq!(partial ^ 0xFFFF_FFFF, full);
    }

    #[test]
    fn test_crc32_various_inputs() {
        assert_eq!(crc32(b"a"), 0xE8B7BE43);
        assert_eq!(crc32(b"ab"), 0x9E83486D);
        assert_eq!(crc32(b"abc"), 0x352441C2);
        assert_eq!(crc32(b"abcd"), 0xED82CD11);
    }
}
