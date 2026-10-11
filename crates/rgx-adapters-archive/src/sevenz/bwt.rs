//! Burrows-Wheeler Transform (BWT).
//!
//! Implements BWT for use in BZIP2 compression.

use alloc::vec::Vec;

/// Burrows-Wheeler Transform.
#[derive(Debug, Default)]
pub struct Bwt;

impl Bwt {
    /// Create a new BWT processor.
    pub fn new() -> Self {
        Self
    }

    /// Perform forward BWT.
    ///
    /// Returns (transformed_data, original_index).
    /// The original_index is the position of the original string in the sorted rotations.
    ///
    /// Uses prefix doubling to sort cyclic shifts in O(n log n), which stays
    /// fast even on highly repetitive inputs (the naive rotation comparison
    /// degenerates to O(n^2) there).
    pub fn transform(&mut self, data: &[u8]) -> (Vec<u8>, usize) {
        if data.is_empty() {
            return (Vec::new(), 0);
        }

        let n = data.len();
        let mut sa: Vec<usize> = (0..n).collect();
        let mut rank: Vec<u32> = data.iter().map(|&b| b as u32).collect();
        let mut tmp = vec![0u32; n];

        let mut k = 1usize;
        while k < n {
            // Sort cyclic shifts by their 2k-prefix: (rank[i], rank[i+k]).
            sa.sort_by_key(|&i| (rank[i], rank[(i + k) % n]));

            // Re-rank the 2k-prefixes.
            tmp[sa[0]] = 0;
            let mut r = 0u32;
            for j in 1..n {
                let (a, b) = (sa[j - 1], sa[j]);
                if rank[a] != rank[b] || rank[(a + k) % n] != rank[(b + k) % n] {
                    r += 1;
                }
                tmp[b] = r;
            }
            core::mem::swap(&mut rank, &mut tmp);
            if r as usize == n - 1 {
                break;
            }
            k *= 2;
        }

        // Find original string position and build output
        let mut original_index = 0;
        let mut output = Vec::with_capacity(n);

        for (i, &idx) in sa.iter().enumerate() {
            if idx == 0 {
                original_index = i;
            }
            output.push(data[(idx + n - 1) % n]);
        }

        (output, original_index)
    }

    /// Perform inverse BWT.
    ///
    /// # Arguments
    /// * `data` - Transformed data (last column of sorted rotations).
    /// * `original_index` - Position of original string in sorted rotations.
    pub fn inverse(&mut self, data: &[u8], original_index: usize) -> Vec<u8> {
        if data.is_empty() {
            return Vec::new();
        }

        let n = data.len();

        // Count occurrences of each byte
        let mut counts = [0usize; 256];
        for &b in data {
            counts[b as usize] += 1;
        }

        // Compute cumulative counts (starting positions for each byte in sorted first column)
        let mut cumsum = [0usize; 256];
        let mut sum = 0;
        for i in 0..256 {
            cumsum[i] = sum;
            sum += counts[i];
        }

        // Build transformation vector: T[i] maps from position i in last column
        // to the corresponding position in first column.
        // For each character in last column, find its position in first column.
        let mut transform = vec![0usize; n];
        let mut positions = [0usize; 256];

        for (i, &b) in data.iter().enumerate() {
            let b = b as usize;
            // Position in first column for this occurrence of b
            let first_col_pos = cumsum[b] + positions[b];
            // T[i] = position in first column
            transform[i] = first_col_pos;
            positions[b] += 1;
        }

        // Reconstruct original string by following the transform
        let mut output = Vec::with_capacity(n);
        let mut idx = original_index;

        for _ in 0..n {
            output.push(data[idx]);
            idx = transform[idx];
        }

        // Output is in reverse order, need to reverse it
        output.reverse();
        output
    }
}

/// Move-to-Front (MTF) encoding.
#[derive(Debug)]
pub struct MtfEncoder {
    /// Symbol table (initialized 0-255).
    table: [u8; 256],
}

impl Default for MtfEncoder {
    fn default() -> Self {
        Self::new()
    }
}

impl MtfEncoder {
    /// Create a new MTF encoder.
    pub fn new() -> Self {
        let mut table = [0u8; 256];
        for (i, slot) in table.iter_mut().enumerate() {
            *slot = i as u8;
        }
        Self { table }
    }

    /// Encode data using MTF.
    pub fn encode(&mut self, data: &[u8]) -> Vec<u8> {
        data.iter()
            .map(|&b| {
                // Find position of b in table
                let pos = self.table.iter().position(|&x| x == b).unwrap_or(0) as u8;

                // Move b to front
                for i in (0..pos as usize).rev() {
                    self.table[i + 1] = self.table[i];
                }
                self.table[0] = b;

                pos
            })
            .collect()
    }

    /// Decode MTF-encoded data.
    pub fn decode(&mut self, data: &[u8]) -> Vec<u8> {
        data.iter()
            .map(|&pos| {
                let b = self.table[pos as usize];

                // Move b to front
                for i in (0..pos as usize).rev() {
                    self.table[i + 1] = self.table[i];
                }
                self.table[0] = b;

                b
            })
            .collect()
    }

    /// Reset the symbol table.
    pub fn reset(&mut self) {
        for i in 0..256 {
            self.table[i] = i as u8;
        }
    }
}

/// Run-Length Encoding (RLE) for BZIP2.
///
/// BZIP2 uses RLE after MTF to encode runs of zeros.
#[derive(Debug, Default)]
pub struct Rle;

impl Rle {
    /// Encode runs of zeros.
    ///
    /// Format: runs of 1-4 zeros become 1-4, runs of 5+ zeros become multiple codes.
    /// Non-zero bytes are offset by 1 (so values 1-254 -> 2-255, value 255 needs special handling).
    /// We use a simpler scheme: zero runs use values 0-254, non-zero bytes use escape + value.
    pub fn encode(data: &[u8]) -> Vec<u8> {
        let mut output = Vec::with_capacity(data.len());
        let mut i = 0;

        while i < data.len() {
            if data[i] == 0 {
                // Count run of zeros
                let mut count = 0usize;
                while i < data.len() && data[i] == 0 && count < 255 {
                    count += 1;
                    i += 1;
                }
                // Output count (1-255 zeros encoded as 0-254)
                output.push((count - 1) as u8);
            } else {
                // Output escape (255) followed by the byte value
                // This distinguishes non-zero bytes from zero run counts
                output.push(255);
                output.push(data[i]);
                i += 1;
            }
        }

        output
    }

    /// Decode runs of zeros.
    pub fn decode(data: &[u8]) -> Vec<u8> {
        let mut output = Vec::with_capacity(data.len());
        let mut i = 0;

        while i < data.len() {
            if data[i] == 255 {
                // Escaped non-zero byte
                i += 1;
                if i < data.len() {
                    output.push(data[i]);
                    i += 1;
                }
            } else {
                // Zero run: value 0-254 means 1-255 zeros
                let count = (data[i] as usize) + 1;
                output.extend(core::iter::repeat_n(0, count));
                i += 1;
            }
        }

        output
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_bwt_simple() {
        let mut bwt = Bwt::new();
        let data = b"banana";
        let (transformed, idx) = bwt.transform(data);

        let inverse = bwt.inverse(&transformed, idx);
        assert_eq!(data.as_slice(), inverse.as_slice());
    }

    #[test]
    fn test_bwt_empty() {
        let mut bwt = Bwt::new();
        let (transformed, idx) = bwt.transform(b"");
        assert!(transformed.is_empty());
        assert_eq!(idx, 0);
    }

    #[test]
    fn test_bwt_single() {
        let mut bwt = Bwt::new();
        let (transformed, idx) = bwt.transform(b"a");
        assert_eq!(transformed, b"a");
        assert_eq!(idx, 0);

        let inverse = bwt.inverse(&transformed, idx);
        assert_eq!(inverse, b"a");
    }

    #[test]
    fn test_bwt_repeated() {
        let mut bwt = Bwt::new();
        let data = b"aaaa";
        let (transformed, idx) = bwt.transform(data);

        let inverse = bwt.inverse(&transformed, idx);
        assert_eq!(data.as_slice(), inverse.as_slice());
    }

    #[test]
    fn test_mtf_roundtrip() {
        let mut encoder = MtfEncoder::new();
        let data = b"banana";
        let encoded = encoder.encode(data);

        encoder.reset();
        let decoded = encoder.decode(&encoded);

        assert_eq!(data.as_slice(), decoded.as_slice());
    }

    #[test]
    fn test_mtf_zeros() {
        let mut encoder = MtfEncoder::new();
        let data = b"\x00\x00\x00abc";
        let encoded = encoder.encode(data);

        // First zero is at position 0, subsequent zeros should produce small values
        assert_eq!(encoded[0], 0);

        encoder.reset();
        let decoded = encoder.decode(&encoded);
        assert_eq!(data.as_slice(), decoded.as_slice());
    }

    #[test]
    fn test_rle_encode() {
        let data = [0, 0, 0, 5, 6];
        let encoded = Rle::encode(&data);
        // 3 zeros -> 2, then escape + 5, escape + 6
        assert_eq!(encoded, [2, 255, 5, 255, 6]);
    }

    #[test]
    fn test_rle_decode() {
        let encoded = [2, 255, 5, 255, 6];
        let decoded = Rle::decode(&encoded);
        assert_eq!(decoded, [0, 0, 0, 5, 6]);
    }

    #[test]
    fn test_rle_roundtrip() {
        let data = [0, 0, 0, 5, 6, 0, 0, 7];
        let encoded = Rle::encode(&data);
        let decoded = Rle::decode(&encoded);
        assert_eq!(data.as_slice(), decoded.as_slice());
    }

    #[test]
    fn test_bwt_mtf_rle_pipeline() {
        // Test the full BZIP2 preprocessing pipeline
        let mut bwt = Bwt::new();
        let mut mtf = MtfEncoder::new();

        let data = b"banana banana banana";
        let (bwt_out, idx) = bwt.transform(data);
        let mtf_out = mtf.encode(&bwt_out);
        let rle_out = Rle::encode(&mtf_out);

        // Reverse
        let rle_in = Rle::decode(&rle_out);
        mtf.reset();
        let mtf_in = mtf.decode(&rle_in);
        let final_out = bwt.inverse(&mtf_in, idx);

        assert_eq!(data.as_slice(), final_out.as_slice());
    }
}
