//! Huffman coding implementation.
//!
//! Provides Huffman tree construction, encoding, and decoding for use in
//! DEFLATE, BZIP2, and Brotli compression algorithms.

use alloc::boxed::Box;
use alloc::collections::BinaryHeap;
use alloc::vec::Vec;
use core::cmp::Ordering;

/// Maximum code length for DEFLATE (15 bits).
pub const MAX_DEFLATE_CODE_LEN: usize = 15;

/// Maximum code length for BZIP2 (20 bits).
pub const MAX_BZIP2_CODE_LEN: usize = 20;

/// Huffman tree node.
#[derive(Debug, Clone)]
pub struct HuffmanNode {
    /// Symbol value (None for internal nodes).
    pub symbol: Option<u16>,
    /// Frequency/count.
    pub freq: u64,
    /// Left child (bit 0).
    pub left: Option<Box<HuffmanNode>>,
    /// Right child (bit 1).
    pub right: Option<Box<HuffmanNode>>,
}

impl HuffmanNode {
    /// Create a leaf node with a symbol.
    pub fn leaf(symbol: u16, freq: u64) -> Self {
        Self {
            symbol: Some(symbol),
            freq,
            left: None,
            right: None,
        }
    }

    /// Create an internal node.
    pub fn internal(left: Box<HuffmanNode>, right: Box<HuffmanNode>) -> Self {
        Self {
            symbol: None,
            freq: left.freq + right.freq,
            left: Some(left),
            right: Some(right),
        }
    }

    /// Check if this is a leaf node.
    pub fn is_leaf(&self) -> bool {
        self.symbol.is_some()
    }
}

impl PartialEq for HuffmanNode {
    fn eq(&self, other: &Self) -> bool {
        self.freq == other.freq
    }
}

impl Eq for HuffmanNode {}

impl PartialOrd for HuffmanNode {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for HuffmanNode {
    fn cmp(&self, other: &Self) -> Ordering {
        // Reverse order for min-heap behavior
        other.freq.cmp(&self.freq)
    }
}

/// Build a Huffman tree from symbol frequencies.
///
/// Returns the root node of the tree.
pub fn build_huffman_tree(freqs: &[u64]) -> Option<Box<HuffmanNode>> {
    let mut heap: BinaryHeap<HuffmanNode> = BinaryHeap::new();

    for (symbol, &freq) in freqs.iter().enumerate() {
        if freq > 0 {
            heap.push(HuffmanNode::leaf(symbol as u16, freq));
        }
    }

    if heap.is_empty() {
        return None;
    }

    while heap.len() > 1 {
        let left = heap.pop()?;
        let right = heap.pop()?;
        heap.push(HuffmanNode::internal(Box::new(left), Box::new(right)));
    }

    heap.pop().map(Box::new)
}

/// Code length table entry.
#[derive(Debug, Clone, Copy, Default)]
pub struct CodeLength {
    /// Number of bits for this code.
    pub bits: u8,
    /// The code value.
    pub code: u16,
}

/// Build canonical Huffman codes from code lengths.
///
/// Uses the algorithm from RFC 1951 section 3.2.2.
pub fn build_canonical_codes(lengths: &[u8], max_bits: usize) -> Vec<CodeLength> {
    let n = lengths.len();
    let mut codes = vec![CodeLength::default(); n];

    // Count codes of each length
    let mut bl_count = vec![0u16; max_bits + 1];
    for &len in lengths {
        if len > 0 && (len as usize) <= max_bits {
            bl_count[len as usize] += 1;
        }
    }

    // Find the numerical value of the smallest code for each code length
    let mut next_code = vec![0u16; max_bits + 1];
    let mut code: u16 = 0;
    for bits in 1..=max_bits {
        code = (code + bl_count[bits - 1]) << 1;
        next_code[bits] = code;
    }

    // Assign numerical values to all codes
    for (symbol, &len) in lengths.iter().enumerate() {
        if len > 0 && (len as usize) <= max_bits {
            codes[symbol].bits = len;
            codes[symbol].code = next_code[len as usize];
            next_code[len as usize] += 1;
        }
    }

    codes
}

/// Huffman decoder lookup table.
///
/// Uses a table-driven approach for fast decoding.
#[derive(Debug, Clone)]
pub struct HuffmanDecoder {
    /// Lookup table entries.
    table: Vec<DecoderEntry>,
    /// Number of bits for primary lookup.
    table_bits: u8,
    /// Code lengths for slow-path decoding.
    lengths: Vec<u8>,
    /// Canonical codes for slow-path decoding.
    codes: Vec<CodeLength>,
}

/// Decoder table entry.
#[derive(Debug, Clone, Copy)]
struct DecoderEntry {
    /// Symbol value (if leaf).
    symbol: u16,
    /// Number of bits for this entry (0 if not a leaf).
    bits: u8,
    /// Whether this is a valid entry.
    valid: bool,
}

/// Reverse the bottom `len` bits of `code`.
///
/// Used to convert between MSB-first canonical Huffman codes and LSB-first
/// stream representation used in DEFLATE.
///
/// # Examples
///
/// Prose, not a runnable example: this module is crate-private, so a doc-test
/// cannot reach it. The behaviour is asserted by the identical public function
/// in `rgx-core::compress::huffman`, whose doc-test runs.
///
/// ```
/// assert_eq!(rgx_core::compress::huffman::reverse_bits(0b10010001, 8), 0b10001001);
/// ```
pub fn reverse_bits(code: u16, len: u8) -> u16 {
    let mut result = 0u16;
    let mut val = code;
    for _ in 0..len {
        result = (result << 1) | (val & 1);
        val >>= 1;
    }
    result
}

impl HuffmanDecoder {
    /// Build a decoder from code lengths.
    ///
    /// Uses bit-reversed lookup table following the DEFLATE convention:
    /// - Canonical Huffman codes are prefix-free when read MSB-first
    /// - DEFLATE streams pack bits LSB-first
    /// - To make the table fill pattern `(i << len) | code` work correctly,
    ///   we reverse each code's bits so the prefix-free property holds for
    ///   the bottom bits (LSB)
    /// - The encoder must also write reversed codes via `write_bits(reverse_bits(code, len), len)`
    pub fn from_lengths(lengths: &[u8], max_bits: u8) -> Self {
        let table_bits = max_bits.min(12); // Limit table size
        let table_size = 1 << table_bits;
        let mut table = vec![
            DecoderEntry {
                symbol: 0,
                bits: 0,
                valid: false,
            };
            table_size
        ];

        let codes = build_canonical_codes(lengths, max_bits as usize);

        for (symbol, code_len) in codes.iter().enumerate() {
            if code_len.bits == 0 {
                continue;
            }

            let code = code_len.code;
            let len = code_len.bits;

            // Reverse the canonical code bits for LSB-first table indexing.
            // This ensures the prefix-free property holds for the bottom bits.
            let reversed = reverse_bits(code, len);

            if len <= table_bits {
                // Fill all table entries where the bottom `len` bits equal `reversed`.
                let count = 1usize << (table_bits - len);
                for i in 0..count {
                    let idx = (i << len) | (reversed as usize);
                    if idx < table_size {
                        table[idx] = DecoderEntry {
                            symbol: symbol as u16,
                            bits: len,
                            valid: true,
                        };
                    }
                }
            }
        }

        Self {
            table,
            table_bits,
            lengths: lengths.to_vec(),
            codes,
        }
    }

    /// Decode a symbol from raw LSB-first stream bits.
    ///
    /// `bits` should be the value returned by `peek_bits()`. The encoder must
    /// have written reversed canonical codes (via `write_bits(reverse_bits(code, len), len)`).
    ///
    /// Returns `(symbol, bits_consumed)` or `None` if no valid code matches.
    pub fn decode(&self, bits: u32, available_bits: u8) -> Option<(u16, u8)> {
        // Fast path: table lookup
        let lookup_bits = self.table_bits.min(available_bits);
        let mask = (1u32 << lookup_bits) - 1;
        let idx = (bits & mask) as usize;

        if idx < self.table.len() {
            let entry = &self.table[idx];
            if entry.valid && entry.bits <= available_bits {
                return Some((entry.symbol, entry.bits));
            }
        }

        // Slow path: for codes longer than table_bits, linear scan.
        // Compare reversed stream bits against canonical codes.
        let max_check = available_bits.min(15);
        for len in (self.table_bits + 1)..=max_check {
            let mask = (1u32 << len) - 1;
            let stream_val = (bits & mask) as u16;
            // stream_val is the reversed code that was written to the stream.
            // Un-reverse to get the canonical code for comparison.
            let canonical = reverse_bits(stream_val, len);

            for (symbol, code_info) in self.codes.iter().enumerate() {
                if code_info.bits == len && code_info.code == canonical {
                    return Some((symbol as u16, len));
                }
            }
        }

        None
    }

    /// Get the table size in bits.
    pub fn table_bits(&self) -> u8 {
        self.table_bits
    }
}

/// Bit writer for Huffman encoding.
#[derive(Debug, Clone, Default)]
pub struct BitWriter {
    buffer: Vec<u8>,
    current_byte: u8,
    bit_pos: u8,
}

impl BitWriter {
    /// Create a new bit writer.
    pub fn new() -> Self {
        Self {
            buffer: Vec::new(),
            current_byte: 0,
            bit_pos: 0,
        }
    }

    /// Write bits (LSB first).
    pub fn write_bits(&mut self, value: u32, num_bits: u8) {
        for i in 0..num_bits {
            let bit = ((value >> i) & 1) as u8;
            self.current_byte |= bit << self.bit_pos;
            self.bit_pos += 1;

            if self.bit_pos == 8 {
                self.buffer.push(self.current_byte);
                self.current_byte = 0;
                self.bit_pos = 0;
            }
        }
    }

    /// Write bits (MSB first, for BZIP2).
    pub fn write_bits_msb(&mut self, value: u64, num_bits: u8) {
        for i in (0..num_bits).rev() {
            let bit = ((value >> i) & 1) as u8;
            self.current_byte |= bit << self.bit_pos;
            self.bit_pos += 1;

            if self.bit_pos == 8 {
                self.buffer.push(self.current_byte);
                self.current_byte = 0;
                self.bit_pos = 0;
            }
        }
    }

    /// Finish writing and return the buffer.
    pub fn finish(mut self) -> Vec<u8> {
        if self.bit_pos > 0 {
            self.buffer.push(self.current_byte);
        }
        self.buffer
    }

    /// Get current bit position.
    pub fn bit_position(&self) -> usize {
        self.buffer.len() * 8 + self.bit_pos as usize
    }
}

/// Bit reader for Huffman decoding.
#[derive(Debug, Clone)]
pub struct BitReader<'a> {
    data: &'a [u8],
    /// Current byte position.
    pub byte_pos: usize,
    /// Current bit position within byte.
    pub bit_pos: u8,
}

impl<'a> BitReader<'a> {
    /// Create a new bit reader.
    pub fn new(data: &'a [u8]) -> Self {
        Self {
            data,
            byte_pos: 0,
            bit_pos: 0,
        }
    }

    /// Read bits (LSB first).
    pub fn read_bits(&mut self, num_bits: u8) -> Option<u32> {
        let mut result: u32 = 0;

        for i in 0..num_bits {
            if self.byte_pos >= self.data.len() {
                return None;
            }

            let bit = (self.data[self.byte_pos] >> self.bit_pos) & 1;
            result |= (bit as u32) << i;

            self.bit_pos += 1;
            if self.bit_pos == 8 {
                self.byte_pos += 1;
                self.bit_pos = 0;
            }
        }

        Some(result)
    }

    /// Peek at bits without consuming them (LSB first).
    pub fn peek_bits(&self, num_bits: u8) -> Option<u32> {
        let mut result: u32 = 0;
        let mut byte_pos = self.byte_pos;
        let mut bit_pos = self.bit_pos;

        for i in 0..num_bits {
            if byte_pos >= self.data.len() {
                return None;
            }

            let bit = (self.data[byte_pos] >> bit_pos) & 1;
            result |= (bit as u32) << i;

            bit_pos += 1;
            if bit_pos == 8 {
                byte_pos += 1;
                bit_pos = 0;
            }
        }

        Some(result)
    }

    /// Skip (consume) the specified number of bits.
    pub fn skip_bits(&mut self, num_bits: u8) {
        for _ in 0..num_bits {
            self.bit_pos += 1;
            if self.bit_pos == 8 {
                self.byte_pos += 1;
                self.bit_pos = 0;
            }
        }
    }

    /// Read bits (MSB first, for BZIP2).
    pub fn read_bits_msb(&mut self, num_bits: u8) -> Option<u32> {
        let mut result: u32 = 0;

        for i in (0..num_bits).rev() {
            if self.byte_pos >= self.data.len() {
                return None;
            }

            let bit = (self.data[self.byte_pos] >> self.bit_pos) & 1;
            result |= (bit as u32) << i;

            self.bit_pos += 1;
            if self.bit_pos == 8 {
                self.byte_pos += 1;
                self.bit_pos = 0;
            }
        }

        Some(result)
    }

    /// Read bits as u64 (MSB first, for BZIP2 large values).
    pub fn read_bits_msb_64(&mut self, num_bits: u8) -> Option<u64> {
        let mut result: u64 = 0;

        for i in (0..num_bits).rev() {
            if self.byte_pos >= self.data.len() {
                return None;
            }

            let bit = (self.data[self.byte_pos] >> self.bit_pos) & 1;
            result |= (bit as u64) << i;

            self.bit_pos += 1;
            if self.bit_pos == 8 {
                self.byte_pos += 1;
                self.bit_pos = 0;
            }
        }

        Some(result)
    }

    /// Align to byte boundary.
    pub fn align(&mut self) {
        if self.bit_pos > 0 {
            self.byte_pos += 1;
            self.bit_pos = 0;
        }
    }

    /// Check if more bits are available.
    pub fn has_more(&self) -> bool {
        self.byte_pos < self.data.len()
    }

    /// Get remaining bytes.
    pub fn remaining_bytes(&self) -> usize {
        self.data.len().saturating_sub(self.byte_pos)
    }

    /// Get remaining bits (including the partially-consumed current byte).
    pub fn remaining_bits(&self) -> usize {
        self.remaining_bytes() * 8 - self.bit_pos as usize
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_canonical_codes() {
        // Example from RFC 1951 section 3.2.2
        // lengths = [3, 3, 3, 3, 3, 2, 4, 4]
        // Expected codes per RFC:
        // Symbol 0-4: length 3, codes 010, 011, 100, 101, 110
        // Symbol 5: length 2, code 00
        // Symbol 6-7: length 4, codes 1110, 1111
        let lengths = [3, 3, 3, 3, 3, 2, 4, 4];
        let codes = build_canonical_codes(&lengths, 15);

        // Verify symbol 0 (length 3)
        assert_eq!(codes[0].bits, 3);
        assert_eq!(codes[0].code, 0b010); // 2

        // Verify symbol 1 (length 3)
        assert_eq!(codes[1].bits, 3);
        assert_eq!(codes[1].code, 0b011); // 3

        // Verify symbol 5 (length 2, shortest code)
        assert_eq!(codes[5].bits, 2);
        assert_eq!(codes[5].code, 0b00); // 0

        // Verify symbol 6 (length 4)
        assert_eq!(codes[6].bits, 4);
        assert_eq!(codes[6].code, 0b1110); // 14
    }

    #[test]
    fn test_bit_writer_reader() {
        let mut writer = BitWriter::new();
        writer.write_bits(0b101, 3);
        writer.write_bits(0b11011, 5);
        let data = writer.finish();

        let mut reader = BitReader::new(&data);
        assert_eq!(reader.read_bits(3), Some(0b101));
        assert_eq!(reader.read_bits(5), Some(0b11011));
    }

    #[test]
    fn test_bit_writer_reader_msb() {
        let mut writer = BitWriter::new();
        writer.write_bits_msb(0b101, 3);
        writer.write_bits_msb(0b11011, 5);
        let data = writer.finish();

        let mut reader = BitReader::new(&data);
        assert_eq!(reader.read_bits_msb(3), Some(0b101));
        assert_eq!(reader.read_bits_msb(5), Some(0b11011));
    }

    #[test]
    fn test_huffman_decoder() {
        let lengths = [2, 2, 2, 2, 0, 0, 0, 0];
        let codes = build_canonical_codes(&lengths, 2);
        let decoder = HuffmanDecoder::from_lengths(&lengths, 2);

        // Verify canonical codes are 0,1,2,3
        assert_eq!(codes[0].code, 0);
        assert_eq!(codes[1].code, 1);
        assert_eq!(codes[2].code, 2);
        assert_eq!(codes[3].code, 3);

        // With bit-reversal, decode expects reversed codes as input.
        // Roundtrip: write reversed code, read back, decode.
        for sym in 0u16..4 {
            let code = codes[sym as usize].code;
            let bits = codes[sym as usize].bits;
            let rev = reverse_bits(code, bits);

            let mut writer = BitWriter::new();
            writer.write_bits(rev as u32, bits);
            let data = writer.finish();

            let reader = BitReader::new(&data);
            let stream_bits = reader.peek_bits(bits).unwrap();
            assert_eq!(stream_bits, rev as u32);

            let (decoded, consumed) = decoder
                .decode(stream_bits, bits)
                .unwrap_or_else(|| panic!("Failed to decode symbol {}", sym));
            assert_eq!(decoded, sym, "Symbol {} roundtrip failed", sym);
            assert_eq!(consumed, bits);
        }
    }
}

/// Build optimal length-limited Huffman codes using the package-merge algorithm.
///
/// Given a frequency table and maximum code length, produces code lengths
/// that minimize `sum(freq[i] * len[i])` subject to `len[i] <= max_bits`.
///
/// This is the algorithm used by real DEFLATE/ZSTD implementations for
/// optimal length-limited codes. It provably produces the minimum-cost
/// set of code lengths within the constraint.
///
/// # Algorithm
///
/// The package-merge algorithm works in `max_bits - 1` rounds:
/// 1. Start with leaf items (one per symbol, weighted by frequency)
/// 2. Each round: "package" pairs of smallest items, "merge" with original list
/// 3. Select the 2*(n-1) smallest items from the final list
/// 4. Count how many times each symbol appears = its code length
///
/// Reference: Larmore & Hirschberg, "A Fast Algorithm for Optimal
/// Length-Limited Huffman Codes" (1990)
pub fn package_merge_lengths(freqs: &[u64], max_bits: u8) -> Vec<u8> {
    let n = freqs.len();
    if n == 0 {
        return Vec::new();
    }

    // Filter active symbols
    let mut symbols: Vec<usize> = (0..n).filter(|&i| freqs[i] > 0).collect();
    let num_active = symbols.len();

    if num_active == 0 {
        return vec![0u8; n];
    }
    if num_active == 1 {
        let mut lengths = vec![0u8; n];
        lengths[symbols[0]] = 1;
        return lengths;
    }
    if num_active == 2 {
        let mut lengths = vec![0u8; n];
        lengths[symbols[0]] = 1;
        lengths[symbols[1]] = 1;
        return lengths;
    }

    let max_bits = max_bits.max(2) as usize;

    /// A packaged item: either a leaf (single symbol) or a package (pair).
    #[derive(Clone)]
    struct Item {
        weight: u64,
        symbols: Vec<usize>, // which symbols this item represents
    }

    // Initial list: one item per active symbol, sorted by frequency
    let mut list: Vec<Item> = symbols
        .iter()
        .map(|&s| Item {
            weight: freqs[s],
            symbols: alloc::vec![s],
        })
        .collect();
    list.sort_by_key(|item| item.weight);

    // Run max_bits - 1 rounds of package-merge
    for _round in 1..max_bits {
        // Package: pair consecutive items
        let mut packages: Vec<Item> = Vec::new();
        let mut i = 0;
        while i + 1 < list.len() {
            let combined_weight = list[i].weight + list[i + 1].weight;
            let mut combined_symbols = list[i].symbols.clone();
            combined_symbols.extend_from_slice(&list[i + 1].symbols);
            packages.push(Item {
                weight: combined_weight,
                symbols: combined_symbols,
            });
            i += 2;
        }

        // Merge: combine packages with original symbols, sorted by weight
        let original: Vec<Item> = symbols
            .iter()
            .map(|&s| Item {
                weight: freqs[s],
                symbols: alloc::vec![s],
            })
            .collect();

        // Merge sorted lists
        let mut merged = Vec::with_capacity(original.len() + packages.len());
        let mut oi = 0;
        let mut pi = 0;
        while oi < original.len() && pi < packages.len() {
            if original[oi].weight <= packages[pi].weight {
                merged.push(original[oi].clone());
                oi += 1;
            } else {
                merged.push(packages[pi].clone());
                pi += 1;
            }
        }
        while oi < original.len() {
            merged.push(original[oi].clone());
            oi += 1;
        }
        while pi < packages.len() {
            merged.push(packages[pi].clone());
            pi += 1;
        }

        // Keep only 2*(num_active - 1) smallest items
        let keep = 2 * (num_active - 1);
        merged.truncate(keep);
        list = merged;
    }

    // Final selection: take 2*(num_active - 1) items from the last list
    let keep = 2 * (num_active - 1);
    list.truncate(keep);

    // Count occurrences of each symbol = code length
    let mut lengths = vec![0u8; n];
    for item in &list {
        for &sym in &item.symbols {
            lengths[sym] += 1;
        }
    }

    // Verify Kraft inequality: sum(2^(max_bits - len)) <= 2^max_bits
    let mut kraft = 0i64;
    for &l in &lengths {
        if l > 0 {
            kraft += 1i64 << (max_bits - l as usize);
        }
    }
    if kraft > (1i64 << max_bits) {
        // Fallback: clamp and redistribute
        for l in lengths.iter_mut() {
            if *l > max_bits as u8 {
                *l = max_bits as u8;
            }
        }
    }

    lengths
}

#[cfg(test)]
mod package_merge_tests {
    use super::*;

    #[test]
    fn test_package_merge_basic() {
        let freqs = [10u64, 5, 3, 2];
        let lengths = package_merge_lengths(&freqs, 3);

        // All symbols should have code lengths <= 3
        for &l in &lengths {
            assert!(l <= 3, "Code length {} exceeds max 3", l);
        }

        // Verify Kraft inequality
        let kraft: i64 = lengths
            .iter()
            .filter(|&&l| l > 0)
            .map(|&l| 1i64 << (3 - l as usize))
            .sum();
        assert!(kraft <= 8, "Kraft inequality violated: {}", kraft);
    }

    #[test]
    fn test_package_merge_two_symbols() {
        let freqs = [100u64, 50];
        let lengths = package_merge_lengths(&freqs, 15);
        assert_eq!(lengths[0], 1);
        assert_eq!(lengths[1], 1);
    }

    #[test]
    fn test_package_merge_empty() {
        let lengths = package_merge_lengths(&[], 15);
        assert!(lengths.is_empty());
    }

    #[test]
    fn test_package_merge_single() {
        let freqs = [0u64, 0, 42, 0];
        let lengths = package_merge_lengths(&freqs, 11);
        assert_eq!(lengths[2], 1);
        assert_eq!(lengths[0], 0);
    }

    #[test]
    fn test_package_merge_better_than_clamping() {
        // Frequency distribution that benefits from package-merge vs clamping
        let freqs = [100u64, 50, 25, 12, 6, 3, 2, 1, 1];
        let lengths_pm = package_merge_lengths(&freqs, 4);

        // Verify all lengths within limit
        for &l in &lengths_pm {
            assert!(l <= 4 && l > 0, "Invalid length {}", l);
        }

        // Package-merge should produce valid Kraft inequality
        let kraft: i64 = lengths_pm
            .iter()
            .filter(|&&l| l > 0)
            .map(|&l| 1i64 << (4 - l as usize))
            .sum();
        assert!(kraft <= 16, "Kraft violated: {}", kraft);

        // Verify total cost is reasonable
        let total_cost: u64 = freqs
            .iter()
            .zip(lengths_pm.iter())
            .map(|(&f, &l)| f * l as u64)
            .sum();
        // Worst case: all 4 bits = 800
        assert!(total_cost < 800, "Cost {} too high", total_cost);
    }
}
