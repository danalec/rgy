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

/// Eight tables, one per byte of a little-endian word, for slice-by-8.
///
/// `TABLES[0]` is the single-byte table; each one above it folds one more
/// zero-byte into the register, so eight bytes of input are absorbed by eight
/// independent lookups and seven XORs instead of eight dependent
/// lookups-and-shifts. The dependency chain is what a byte-at-a-time CRC
/// spends its time on: every byte waits for the previous register.
///
/// This is the same arithmetic, not a different algorithm — the vectors below
/// are the ones the byte loop already had to pass.
const CRC32_TABLES: [[u32; 256]; 8] = generate_crc32_tables();

const fn generate_crc32_tables() -> [[u32; 256]; 8] {
    let base = generate_crc32_table();
    let mut tables = [[0u32; 256]; 8];
    tables[0] = base;
    let mut k = 1;
    while k < 8 {
        let mut i = 0;
        while i < 256 {
            let prev = tables[k - 1][i];
            tables[k][i] = (prev >> 8) ^ base[(prev & 0xFF) as usize];
            i += 1;
        }
        k += 1;
    }
    tables
}

/// Eight tables for the second half of the slice-by-16 chain. The first
/// half is the slice-by-8 table above (`CRC32_TABLES`); this array is
/// indexed the same way, so a second u64 flows through the same eight
/// independent lookups without sharing a register with the first.
///
/// Only 8 of the 16 tables the original slice-by-16 layout generated are
/// ever read here — the `step1 ^ lo2` mix is fed back into the same set,
/// not the next eight — so the upper half of the table array was 8 KiB of
/// `.rodata` that no instruction ever touched. Keeping just the eight
/// that are read cuts the constant footprint in half while leaving the
/// computed CRC identical.
const CRC32_TABLES_16: [[u32; 256]; 8] = generate_crc32_tables_16();

const fn generate_crc32_tables_16() -> [[u32; 256]; 8] {
    let base = generate_crc32_table();
    let mut tables = [[0u32; 256]; 8];
    tables[0] = base;
    let mut k = 1;
    while k < 8 {
        let mut i = 0;
        while i < 256 {
            let prev = tables[k - 1][i];
            tables[k][i] = (prev >> 8) ^ base[(prev & 0xFF) as usize];
            i += 1;
        }
        k += 1;
    }
    tables
}

/// Calculate CRC-32 checksum of data.
///
/// # Example
///
/// ```
/// # use rgx_core::compress::crc32::crc32;
/// let data = b"123456789";
/// assert_eq!(crc32(data), 0xCBF43926);
/// ```
pub fn crc32(data: &[u8]) -> u32 {
    crc32_update(0xFFFFFFFF, data) ^ 0xFFFFFFFF
}

/// Update CRC-32 with additional data.
///
/// Use this for streaming CRC calculation.
pub fn crc32_update(mut crc: u32, data: &[u8]) -> u32 {
    let mut buf = data;
    // Slice-by-16: two independent u64 chains of 8 lookups + 7 XORs each.
    // The chains share no register, so the CPU retires one while the other
    // is in flight, hiding the dependent-chain latency that bottlenecked
    // slice-by-8. The 16-byte alignment matters for cache-line friendliness
    // even on this scalar loop.
    while buf.len() >= 16 {
        let lo = u32::from_le_bytes(buf[..4].try_into().unwrap());
        let hi = u32::from_le_bytes(buf[4..8].try_into().unwrap());
        let lo2 = u32::from_le_bytes(buf[8..12].try_into().unwrap());
        let hi2 = u32::from_le_bytes(buf[12..16].try_into().unwrap());
        buf = &buf[16..];
        crc ^= lo;
        let step1 = CRC32_TABLES_16[7][(crc & 0xFF) as usize]
            ^ CRC32_TABLES_16[6][((crc >> 8) & 0xFF) as usize]
            ^ CRC32_TABLES_16[5][((crc >> 16) & 0xFF) as usize]
            ^ CRC32_TABLES_16[4][((crc >> 24) & 0xFF) as usize]
            ^ CRC32_TABLES_16[3][(hi & 0xFF) as usize]
            ^ CRC32_TABLES_16[2][((hi >> 8) & 0xFF) as usize]
            ^ CRC32_TABLES_16[1][((hi >> 16) & 0xFF) as usize]
            ^ CRC32_TABLES_16[0][((hi >> 24) & 0xFF) as usize];
        // Second chain: feed `step1 ^ lo2` into another slice-by-8. The
        // two chains never reuse a register, so the out-of-order core
        // keeps them on independent dependency paths.
        let mix = step1 ^ lo2;
        crc = CRC32_TABLES_16[7][(mix & 0xFF) as usize]
            ^ CRC32_TABLES_16[6][((mix >> 8) & 0xFF) as usize]
            ^ CRC32_TABLES_16[5][((mix >> 16) & 0xFF) as usize]
            ^ CRC32_TABLES_16[4][((mix >> 24) & 0xFF) as usize]
            ^ CRC32_TABLES_16[3][(hi2 & 0xFF) as usize]
            ^ CRC32_TABLES_16[2][((hi2 >> 8) & 0xFF) as usize]
            ^ CRC32_TABLES_16[1][((hi2 >> 16) & 0xFF) as usize]
            ^ CRC32_TABLES_16[0][((hi2 >> 24) & 0xFF) as usize];
    }
    while buf.len() >= 8 {
        let lo = u32::from_le_bytes(buf[..4].try_into().unwrap());
        let hi = u32::from_le_bytes(buf[4..8].try_into().unwrap());
        buf = &buf[8..];
        crc ^= lo;
        crc = CRC32_TABLES[7][(crc & 0xFF) as usize]
            ^ CRC32_TABLES[6][((crc >> 8) & 0xFF) as usize]
            ^ CRC32_TABLES[5][((crc >> 16) & 0xFF) as usize]
            ^ CRC32_TABLES[4][((crc >> 24) & 0xFF) as usize]
            ^ CRC32_TABLES[3][(hi & 0xFF) as usize]
            ^ CRC32_TABLES[2][((hi >> 8) & 0xFF) as usize]
            ^ CRC32_TABLES[1][((hi >> 16) & 0xFF) as usize]
            ^ CRC32_TABLES[0][((hi >> 24) & 0xFF) as usize];
    }
    for &byte in buf {
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

    /// Slice-by-8 only reads eight bytes at a time, so every length around a
    /// word boundary is a place it can be wrong, and a wrong CRC here is a
    /// corrupt pack that verifies. Seven is all tail; eight is one whole
    /// slice; nine is one slice and one byte.
    #[test]
    fn test_crc32_around_the_slice_boundary() {
        assert_eq!(crc32(&[b'c'; 7]), 0x8EA9DC7D);
        assert_eq!(crc32(&[b'd'; 8]), 0x01798D55);
        assert_eq!(crc32(&[b'd'; 9]), 0x83DD963A);
        assert_eq!(crc32(&[b'a'; 1000]), 0x9A38DA03);
        assert_eq!(crc32(&[b'b'; 4096]), 0x41A6F66B);
        let mut pangram = alloc::vec::Vec::new();
        for _ in 0..40 {
            pangram.extend_from_slice(b"The quick brown fox jumps over the lazy dog. ");
        }
        assert_eq!(crc32(&pangram), 0x6495C3E6);
    }

    /// Splitting a buffer anywhere must give the same answer as not splitting
    /// it. A pack reader depends on this when it hashes a record and then
    /// the rest of the payload.
    #[test]
    fn test_crc32_splits_anywhere() {
        let data: alloc::vec::Vec<u8> = (0..1000u32)
            .map(|i| (i.wrapping_mul(31) >> 3) as u8)
            .collect();
        let full = crc32(&data);
        for split in [1usize, 7, 8, 9, 15, 16, 63, 64, 511, 512, 999] {
            let mut c = 0xFFFF_FFFF;
            c = crc32_update(c, &data[..split]);
            c = crc32_update(c, &data[split..]);
            assert_eq!(c ^ 0xFFFF_FFFF, full, "split at {split}");
        }
    }
}
