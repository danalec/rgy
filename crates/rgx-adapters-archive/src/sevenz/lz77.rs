//! LZ77 sliding window compression.
//!
//! Implements the LZ77 algorithm used in DEFLATE, gzip, and other compressors.

use alloc::vec::Vec;

use super::graph_mode::{paley_hash3_bits, GraphMode};

/// Default window size for DEFLATE (32KB).
pub const DEFAULT_WINDOW_SIZE: usize = 32 * 1024;

/// Minimum match length.
pub const MIN_MATCH: usize = 3;

/// Maximum match length for DEFLATE.
pub const MAX_MATCH: usize = 258;

/// Maximum distance for DEFLATE.
pub const MAX_DISTANCE: usize = 32 * 1024;

/// LZ77 match result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Match {
    /// Distance back from current position.
    pub distance: u16,
    /// Length of the match.
    pub length: u16,
}

/// LZ77 compressor.
#[derive(Debug)]
pub struct Lz77Encoder {
    /// Sliding window buffer.
    window: Vec<u8>,
    /// Current position in window.
    pos: usize,
    /// Window size.
    window_size: usize,
    /// Hash table for fast match finding.
    hash_table: Vec<usize>,
    /// Hash chain for collisions.
    hash_chain: Vec<usize>,
    /// Current hash chain head.
    chain_head: usize,
    /// Compression level (1-9).
    level: u8,
    /// Maximum chain depth for match searching.
    max_chain: usize,
    /// Maximum lazy match depth.
    max_lazy: usize,
    /// Match-finding strategy.
    graph_mode: GraphMode,
}

impl Lz77Encoder {
    /// Create a new LZ77 encoder with default settings.
    pub fn new() -> Self {
        Self::with_level(6)
    }

    /// Create an LZ77 encoder with a specific compression level (1-9).
    pub fn with_level(level: u8) -> Self {
        let window_size = DEFAULT_WINDOW_SIZE;
        let hash_size = 1 << 15; // 32K hash entries

        let (max_chain, max_lazy) = match level {
            1 => (4, 4),
            2 => (6, 6),
            3 => (32, 8),
            4 => (16, 12),
            5 => (32, 16),
            6 => (128, 32),
            7 => (256, 64),
            8 => (512, 128),
            _ => (1024, 258), // level 9
        };

        Self {
            window: Vec::new(),
            pos: 0,
            window_size,
            hash_table: vec![0; hash_size],
            hash_chain: vec![0; window_size],
            chain_head: 0,
            level,
            max_chain,
            max_lazy,
            graph_mode: GraphMode::default(),
        }
    }

    /// Select match-finding strategy.
    pub fn with_graph_mode(mut self, mode: GraphMode) -> Self {
        self.graph_mode = mode;
        self
    }

    /// Compress data using LZ77.
    ///
    /// Returns a vector of literals and match references.
    pub fn compress(&mut self, data: &[u8]) -> Vec<Lz77Token> {
        let mut tokens = Vec::with_capacity(data.len() / 4);
        self.window.clear();
        self.window.extend_from_slice(data);
        self.pos = 0;

        while self.pos < data.len() {
            if let Some(m) = self.find_match() {
                // Try lazy matching
                let mut best_match = m;
                let mut best_pos = self.pos;

                if self.level >= 4 {
                    for lazy in 1..=self.max_lazy.min(258 - m.length as usize) {
                        if self.pos + lazy + MIN_MATCH > data.len() {
                            break;
                        }
                        self.pos += lazy;
                        if let Some(lazy_match) = self.find_match() {
                            if lazy_match.length > best_match.length {
                                // Output lazy literals
                                for i in 0..lazy {
                                    tokens.push(Lz77Token::Literal(data[best_pos + i]));
                                }
                                best_match = lazy_match;
                                best_pos = self.pos;
                                break;
                            }
                        }
                        self.pos -= lazy;
                    }
                }

                self.pos = best_pos;
                tokens.push(Lz77Token::Match(Match {
                    distance: best_match.distance,
                    length: best_match.length,
                }));

                // Update hash table for matched bytes
                for i in 0..best_match.length as usize {
                    if self.pos + i + MIN_MATCH <= data.len() {
                        self.update_hash(self.pos + i);
                    }
                }
                self.pos += best_match.length as usize;
            } else {
                tokens.push(Lz77Token::Literal(data[self.pos]));
                self.update_hash(self.pos);
                self.pos += 1;
            }
        }

        tokens
    }

    /// Find the best match at the current position.
    fn find_match(&self) -> Option<Match> {
        if self.pos + MIN_MATCH > self.window.len() {
            return None;
        }

        let hash = self.compute_hash(self.pos);
        let mut best_match: Option<Match> = None;
        let mut chain_len = 0;
        let mut prev = self.hash_table[hash];

        while prev != 0 && chain_len < self.max_chain {
            let dist = self.pos.saturating_sub(prev);
            if dist > MAX_DISTANCE || dist == 0 {
                break;
            }

            let len = self.match_length(prev, self.pos);
            if len >= MIN_MATCH {
                if let Some(ref best) = best_match {
                    if len as u16 > best.length {
                        best_match = Some(Match {
                            distance: dist as u16,
                            length: len as u16,
                        });
                    }
                } else {
                    best_match = Some(Match {
                        distance: dist as u16,
                        length: len as u16,
                    });
                }
            }

            prev = self.hash_chain[prev % self.window_size];
            chain_len += 1;
        }

        best_match
    }

    /// Compute hash for a position.
    fn compute_hash(&self, pos: usize) -> usize {
        if pos + 2 >= self.window.len() {
            return 0;
        }

        match self.graph_mode {
            GraphMode::Standard => {
                let h = ((self.window[pos] as usize) << 16)
                    ^ ((self.window[pos + 1] as usize) << 8)
                    ^ (self.window[pos + 2] as usize);
                h % self.hash_table.len()
            }
            GraphMode::Paley => {
                paley_hash3_bits(&self.window[pos..pos + 3], 15) as usize % self.hash_table.len()
            }
        }
    }

    /// Update hash table for a position.
    fn update_hash(&mut self, pos: usize) {
        if pos + MIN_MATCH > self.window.len() {
            return;
        }

        let hash = self.compute_hash(pos);
        self.chain_head = (self.chain_head + 1) % self.window_size;
        self.hash_chain[self.chain_head] = self.hash_table[hash];
        self.hash_table[hash] = pos;
    }

    /// Calculate match length between two positions.
    fn match_length(&self, pos1: usize, pos2: usize) -> usize {
        let mut len = 0;
        let max_len = (self.window.len() - pos2).min(MAX_MATCH);

        while len < max_len && self.window[pos1 + len] == self.window[pos2 + len] {
            len += 1;
        }

        len
    }
}

impl Default for Lz77Encoder {
    fn default() -> Self {
        Self::new()
    }
}

/// LZ77 token (literal or match).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lz77Token {
    /// Literal byte.
    Literal(u8),
    /// Match reference.
    Match(Match),
}

/// LZ77 decompressor.
#[derive(Debug, Default)]
pub struct Lz77Decoder {
    /// Output buffer (acts as sliding window).
    window: Vec<u8>,
}

impl Lz77Decoder {
    /// Create a new LZ77 decoder.
    pub fn new() -> Self {
        Self::default()
    }

    /// Decompress LZ77 tokens.
    pub fn decompress(&mut self, tokens: &[Lz77Token]) -> Vec<u8> {
        self.window.clear();

        for token in tokens {
            match token {
                Lz77Token::Literal(byte) => {
                    self.window.push(*byte);
                }
                Lz77Token::Match(m) => {
                    let dist = m.distance as usize;
                    let len = m.length as usize;
                    let start = self.window.len().saturating_sub(dist);

                    if start >= self.window.len() {
                        // Invalid distance
                        continue;
                    }

                    for i in 0..len {
                        let src_pos = start + i;
                        if src_pos < self.window.len() {
                            self.window.push(self.window[src_pos]);
                        }
                    }
                }
            }
        }

        self.window.clone()
    }

    /// Copy from sliding window (used by DEFLATE decoder).
    pub fn copy_match(&mut self, distance: usize, length: usize) {
        let start = self.window.len().saturating_sub(distance);

        for i in 0..length {
            let src_pos = start + i;
            if src_pos < self.window.len() {
                self.window.push(self.window[src_pos]);
            }
        }
    }

    /// Add a literal byte.
    pub fn add_literal(&mut self, byte: u8) {
        self.window.push(byte);
    }

    /// Get the output.
    pub fn output(&self) -> &[u8] {
        &self.window
    }

    /// Reset the decoder.
    pub fn reset(&mut self) {
        self.window.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_lz77_literal_only() {
        let mut encoder = Lz77Encoder::with_level(1);
        let data = b"abcdefgh";
        let tokens = encoder.compress(data);

        for token in &tokens {
            assert!(matches!(token, Lz77Token::Literal(_)));
        }
    }

    #[test]
    fn test_lz77_repeated_data() {
        let mut encoder = Lz77Encoder::new();
        let data = b"abcabcabcabc";
        let tokens = encoder.compress(data);

        // Should have at least one match
        assert!(tokens.iter().any(|t| matches!(t, Lz77Token::Match(_))));
    }

    #[test]
    fn test_lz77_roundtrip() {
        let mut encoder = Lz77Encoder::new();
        let mut decoder = Lz77Decoder::new();

        let data = b"Hello, World! Hello, World! Hello, World!";
        let tokens = encoder.compress(data);
        let decompressed = decoder.decompress(&tokens);

        assert_eq!(data.as_slice(), decompressed.as_slice());
    }

    #[test]
    fn test_lz77_decoder_copy_match() {
        let mut decoder = Lz77Decoder::new();

        // Add some literals
        for b in b"abc" {
            decoder.add_literal(*b);
        }

        // Copy match (distance 3, length 3) should repeat "abc"
        decoder.copy_match(3, 3);

        assert_eq!(decoder.output(), b"abcabc");
    }
}
