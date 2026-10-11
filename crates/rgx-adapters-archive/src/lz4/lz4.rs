//! LZ4 compression.
//!
//! Implements the LZ4 block format and frame format (RFC 6713).
//! LZ4 is focused on speed — it offers compression/decompression
//! speeds of 400 MB/s+ on modern hardware.
//!
//! Supports two match-finding strategies:
//! - `GraphMode::Standard` — Original multiplicative hash chain match finder
//! - `GraphMode::Paley` — Paley graph-inspired hash with lazy matching,
//!   secondary verification, and FlipPredictor-style chain scoring

use super::error::{CompressError, CompressResult};
use super::graph_optimize::{
    count_witnesses, extend_with_witnesses, paley_hash4, paley_hash4_b_bits, paley_hash4_c_bits,
    paley_hash4_verify, ramsey_min_depth, DataRegion, GraphMode, MatchPredictor, OffsetPredictor,
    StridePredictor, WitnessConfig,
};
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use rgx_core::compress::cap::{Bounded, OutputCap};

/// LZ4 frame magic number (native endian).
pub const LZ4_MAGIC: u32 = 0x184D_2204;

/// LZ4 frame magic number (skippable).
pub const LZ4_SKIPPABLE_MAGIC: u32 = 0x184D_2A50;

/// LZ4 block max size (default).
pub const LZ4_BLOCK_MAX: usize = 4 * 1024 * 1024;

/// LZ4 block max size (64KB, for legacy compatibility).
pub const LZ4_BLOCK_MAX_64KB: usize = 64 * 1024;

/// Magic of the deprecated *legacy* container, as emitted by `lz4 -l`.
pub const LZ4_LEGACY_MAGIC: u32 = 0x184C_2102;

/// Largest compressed size a single legacy block can occupy. The legacy
/// container uses 8 MiB blocks, not the frame format's 64 KiB.
pub const LZ4_LEGACY_BLOCK_MAX_COMPRESSED: usize = 8 * 1024 * 1024 + (8 * 1024 * 1024) / 255 + 16;

/// Minimum match length.
const MIN_MATCH: usize = 4;
/// A match may not start within this many bytes of the block's end.
const MFLIMIT: usize = 12;
/// The block must end with at least this many literal bytes.
const LAST_LITERALS: usize = 5;

/// Last literal length maximum (15 bits).
const LAST_LITERAL_MAX: usize = (1 << 15) - 1;

/// LZ4 compression level (0-12+).
///
/// Unlike gzip levels, LZ4 levels control search depth.
/// Level 0 = fast, 9 = high compression, 12+ = max.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Lz4Level(pub u32);

impl Lz4Level {
    pub const FASTEST: Self = Self(0);
    pub const FAST: Self = Self(3);
    pub const DEFAULT: Self = Self(4);
    pub const HIGH: Self = Self(9);
    pub const MAX: Self = Self(16);
}

impl Default for Lz4Level {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// LZ4 block compression mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BlockMode {
    /// Blocks are linked (sliding window across blocks).
    #[default]
    Linked,
    /// Blocks are independent.
    Independent,
}

/// LZ4 content checksum mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ContentChecksum {
    /// No checksum.
    #[default]
    None,
    /// XXH-32 checksum.
    XXH32,
}

/// LZ4 frame header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Lz4FrameHeader {
    pub block_max_size: u32,
    pub block_mode: BlockMode,
    pub content_checksum: ContentChecksum,
    pub has_content_size: bool,
    pub content_size: Option<u64>,
    pub dict_id: Option<u32>,
}

/// LZ4 encoder settings.
#[derive(Debug, Clone)]
pub struct Lz4Encoder {
    level: Lz4Level,
    block_max_size: usize,
    block_mode: BlockMode,
    content_checksum: ContentChecksum,
    graph_mode: GraphMode,
}

impl Lz4Encoder {
    /// Create a new LZ4 encoder with default settings.
    pub fn new() -> Self {
        Self {
            level: Lz4Level::DEFAULT,
            block_max_size: LZ4_BLOCK_MAX,
            block_mode: BlockMode::Linked,
            content_checksum: ContentChecksum::None,
            graph_mode: GraphMode::default(),
        }
    }

    /// Create with a specific compression level.
    pub fn with_level(level: Lz4Level) -> Self {
        Self {
            level,
            ..Self::new()
        }
    }

    /// Set block independence mode.
    pub fn with_block_mode(mut self, mode: BlockMode) -> Self {
        self.block_mode = mode;
        self
    }

    /// Set content checksum.
    pub fn with_checksum(mut self, checksum: ContentChecksum) -> Self {
        self.content_checksum = checksum;
        self
    }

    /// Select match-finding strategy.
    pub fn with_graph_mode(mut self, mode: GraphMode) -> Self {
        self.graph_mode = mode;
        self
    }

    /// Compress data using LZ4 block format (raw, no frame header).
    pub fn encode_block(&self, input: &[u8]) -> Vec<u8> {
        match self.graph_mode {
            GraphMode::Standard => self.encode_block_standard(input),
            GraphMode::Paley => self.encode_block_graph(input),
        }
    }

    /// Standard LZ4 block compression with multiplicative hash chain.
    fn encode_block_standard(&self, input: &[u8]) -> Vec<u8> {
        let mut output = Vec::with_capacity(input.len() + input.len() / 255 + 16);
        let input_len = input.len();

        if input_len == 0 {
            return output;
        }

        let max_chain = match self.level.0 {
            0..=2 => 8,
            3..=5 => 32,
            6..=8 => 64,
            9..=12 => 256,
            _ => 1024,
        };

        // Hash chain match finder
        const HASH_BITS: usize = 16;
        const HASH_SIZE: usize = 1 << HASH_BITS;
        const CHAIN_SENTINEL: u32 = u32::MAX;
        let mut hash_table: Vec<u32> = vec![CHAIN_SENTINEL; HASH_SIZE];
        let mut chain: Vec<u32> = vec![CHAIN_SENTINEL; input_len];

        let mut anchor = 0usize;
        let mut pos = 0usize;

        if input_len < MIN_MATCH {
            emit_last_literals(&input[anchor..], &mut output);
            return output;
        }

        let match_limit = input_len.saturating_sub(LAST_LITERALS);
        let mf_limit = input_len.saturating_sub(MFLIMIT);

        // Main compression loop. Stops at MFLIMIT: a match may not start
        // in the last 12 bytes, and a match may not extend past the last
        // 5, so the block always ends on literals.
        while pos + MIN_MATCH <= mf_limit {
            let h = hash4_standard(&input[pos..]);

            // Save previous head before inserting current position
            let prev_head = hash_table[h];
            chain[pos] = prev_head;
            hash_table[h] = pos as u32;

            // Walk the hash chain starting from previous entries
            let mut best_len = 0usize;
            let mut best_ref = 0usize;
            let mut candidate = prev_head;
            let mut steps = 0;

            while candidate != CHAIN_SENTINEL && steps < max_chain {
                let cand = candidate as usize;
                let dist = pos - cand;
                if dist > LZ4_BLOCK_MAX {
                    break;
                }

                // Quick 4-byte check before extending
                if input[cand..cand + MIN_MATCH] == input[pos..pos + MIN_MATCH] {
                    let mut len = MIN_MATCH;
                    while pos + len < match_limit && input[cand + len] == input[pos + len] {
                        len += 1;
                    }
                    if len > best_len {
                        best_len = len;
                        best_ref = cand;
                        if len >= 65536 {
                            break;
                        }
                    }
                }

                candidate = chain[cand];
                steps += 1;
            }

            if best_len >= MIN_MATCH {
                let lit_len = pos - anchor;
                encode_sequence(
                    &input[anchor..pos],
                    lit_len,
                    pos - best_ref,
                    best_len - MIN_MATCH,
                    &mut output,
                );

                // Insert skipped positions into hash chain for better future matches
                let advance = best_len;
                for i in 1..advance {
                    let ip = pos + i;
                    if ip + MIN_MATCH <= input_len {
                        let ih = hash4_standard(&input[ip..]);
                        chain[ip] = hash_table[ih];
                        hash_table[ih] = ip as u32;
                    }
                }

                pos += advance;
                anchor = pos;
            } else {
                pos += 1;
            }
        }

        // Emit remaining literals
        emit_last_literals(&input[anchor..input_len], &mut output);

        output
    }

    /// Graph-enhanced LZ4 block compression with multi-probe Paley hashing,
    /// verify hash, lazy matching, witness amplification, offset prediction,
    /// and FlipPredictor-style adaptive chain scoring.
    fn encode_block_graph(&self, input: &[u8]) -> Vec<u8> {
        let mut output = Vec::with_capacity(input.len() + input.len() / 255 + 16);
        let input_len = input.len();

        if input_len == 0 {
            return output;
        }

        let base_chain = match self.level.0 {
            0..=2 => 8,
            3..=5 => 32,
            6..=8 => 64,
            9..=12 => 256,
            _ => 1024,
        };

        // DataRegion-adaptive parameters
        let region = DataRegion::classify(input);
        let ramsey_depth = ramsey_min_depth(5);
        let max_chain = match region {
            DataRegion::LowEntropy => base_chain.min(ramsey_depth),
            DataRegion::MediumEntropy => base_chain.max(ramsey_depth),
            DataRegion::HighEntropy => (base_chain * 5) / 4,
        };
        let witness_config = match region {
            DataRegion::LowEntropy => WitnessConfig::qr5(),
            DataRegion::MediumEntropy => WitnessConfig::qr13(),
            DataRegion::HighEntropy => WitnessConfig::qr17(),
        };

        // Adaptive hash sizing: low-entropy data needs fewer buckets,
        // high-entropy data benefits from larger tables.
        let hash_bits: usize = match region {
            DataRegion::LowEntropy => 14,
            DataRegion::MediumEntropy => 16,
            DataRegion::HighEntropy => 18,
        };
        let hash_size = 1usize << hash_bits;
        const VERIFY_SIZE: usize = 1 << 16;
        const CHAIN_SENTINEL: u32 = u32::MAX;

        // Three independent hash tables for multi-probe
        let mut hash_a: Vec<u32> = vec![CHAIN_SENTINEL; hash_size];
        let mut hash_b: Vec<u32> = vec![CHAIN_SENTINEL; hash_size];
        let mut hash_c: Vec<u32> = vec![CHAIN_SENTINEL; hash_size];
        let mut verify_table: Vec<u16> = vec![0; VERIFY_SIZE];
        let mut chain_a: Vec<u32> = vec![CHAIN_SENTINEL; input_len];
        let mut chain_b: Vec<u32> = vec![CHAIN_SENTINEL; input_len];
        let mut chain_c: Vec<u32> = vec![CHAIN_SENTINEL; input_len];
        let mut predictor = MatchPredictor::new();
        let mut offset_predictor = OffsetPredictor::new();
        let mut stride_predictor = StridePredictor::new();

        let mut anchor = 0usize;
        let mut pos = 0usize;

        if input_len < MIN_MATCH {
            emit_last_literals(&input[anchor..], &mut output);
            return output;
        }

        let match_limit = input_len.saturating_sub(LAST_LITERALS);
        let mf_limit = input_len.saturating_sub(MFLIMIT);

        /// Walk a single hash chain with verify-hash early reject and
        /// predict_quality() early termination.
        // Hot inner loop of the HC match finder: the arguments are the
        // caller's locals, kept as plain parameters so codegen is unchanged.
        #[allow(clippy::too_many_arguments)]
        fn walk_chain(
            input: &[u8],
            input_len: usize,
            match_limit: usize,
            chain: &[u32],
            verify_table: &[u16],
            pos: usize,
            mut candidate: u32,
            max_steps: usize,
            my_verify: u16,
            predictor: &MatchPredictor,
            best_len: &mut usize,
            best_ref: &mut usize,
        ) {
            let mut steps = 0;
            let mut consecutive_skips = 0usize;
            while candidate != CHAIN_SENTINEL && steps < max_steps {
                let cand = candidate as usize;
                if cand >= pos {
                    candidate = chain[cand];
                    steps += 1;
                    continue;
                }
                let dist = pos - cand;
                if dist > LZ4_BLOCK_MAX {
                    return;
                }

                // Verify-hash early reject (only when not aliased)
                let cand_verify = verify_table[cand & (VERIFY_SIZE - 1)];
                if cand_verify != 0 && cand_verify != my_verify {
                    candidate = chain[cand];
                    continue;
                }

                // predict_quality() early termination
                if predictor.should_skip(dist, steps, *best_len) {
                    consecutive_skips += 1;
                    if consecutive_skips >= 4 {
                        return;
                    }
                    candidate = chain[cand];
                    steps += 1;
                    continue;
                }
                consecutive_skips = 0;

                if input[cand..cand + MIN_MATCH] == input[pos..pos + MIN_MATCH] {
                    let max_len = (match_limit - pos).min(65536);
                    let len = extend_with_witnesses(input, pos, cand, MIN_MATCH, max_len);
                    if len > *best_len {
                        *best_len = len;
                        *best_ref = cand;
                        if len >= 65536 {
                            return;
                        }
                    }
                }

                candidate = chain[cand];
                steps += 1;
            }
        }

        while pos + MIN_MATCH <= mf_limit {
            let my_verify = hash4_verify(&input[pos..]);
            verify_table[pos & (VERIFY_SIZE - 1)] = my_verify;

            // Insert into all 3 hash tables (dynamic hash_bits)
            let h_a = paley_hash4(&input[pos..]) as usize & ((1 << hash_bits) - 1);
            let prev_a = hash_a[h_a];
            chain_a[pos] = prev_a;
            hash_a[h_a] = pos as u32;

            let h_b = paley_hash4_b_bits(&input[pos..], hash_bits as u32) as usize;
            let prev_b = hash_b[h_b];
            chain_b[pos] = prev_b;
            hash_b[h_b] = pos as u32;

            let h_c = paley_hash4_c_bits(&input[pos..], hash_bits as u32) as usize;
            let prev_c = hash_c[h_c];
            chain_c[pos] = prev_c;
            hash_c[h_c] = pos as u32;

            // Walk all 3 chains (with predict_quality early termination)
            let mut best_len = 0usize;
            let mut best_ref = 0usize;

            // Van der Waerden stride prediction: try speculative stride match first
            if stride_predictor.is_active() {
                let max_len = (match_limit - pos).min(65536);
                if let Some((dist, len)) = stride_predictor.try_stride(input, pos, max_len) {
                    if len >= MIN_MATCH {
                        best_len = len;
                        best_ref = pos - dist;
                    }
                }
            }

            walk_chain(
                input,
                input_len,
                match_limit,
                &chain_a,
                &verify_table,
                pos,
                prev_a,
                max_chain,
                my_verify,
                &predictor,
                &mut best_len,
                &mut best_ref,
            );
            walk_chain(
                input,
                input_len,
                match_limit,
                &chain_b,
                &verify_table,
                pos,
                prev_b,
                max_chain / 2,
                my_verify,
                &predictor,
                &mut best_len,
                &mut best_ref,
            );
            walk_chain(
                input,
                input_len,
                match_limit,
                &chain_c,
                &verify_table,
                pos,
                prev_c,
                max_chain / 2,
                my_verify,
                &predictor,
                &mut best_len,
                &mut best_ref,
            );

            // Offset prediction: try direct lookups at predicted offsets
            let predicted = offset_predictor.top_offsets(4);
            for &offset in &predicted {
                let off = offset as usize;
                if off == 0 || off >= pos || off > LZ4_BLOCK_MAX {
                    continue;
                }
                let cand = pos - off;
                if input[cand..cand + MIN_MATCH] == input[pos..pos + MIN_MATCH] {
                    let max_len = (input_len - pos).min(65536);
                    let len = extend_with_witnesses(input, pos, cand, MIN_MATCH, max_len);
                    if len > best_len {
                        best_len = len;
                        best_ref = cand;
                    }
                }
            }

            // Witness amplification for short matches
            if best_len >= MIN_MATCH && best_len < witness_config.threshold {
                let ref_pos = pos - (pos - best_ref);
                let witnesses = count_witnesses(input, pos, ref_pos, best_len, &witness_config);
                if witnesses < witness_config.min_witnesses {
                    best_len = 0;
                    best_ref = 0;
                }
            }

            if best_len >= MIN_MATCH {
                predictor.record_match(pos - best_ref, best_len);
                offset_predictor.record((pos - best_ref) as u32, best_len);
                stride_predictor.record(pos, (pos - best_ref) as u32);
            }

            if best_len >= MIN_MATCH {
                let lit_len = pos - anchor;
                encode_sequence(
                    &input[anchor..pos],
                    lit_len,
                    pos - best_ref,
                    best_len - MIN_MATCH,
                    &mut output,
                );

                let advance = best_len;
                for i in 1..advance {
                    let ip = pos + i;
                    if ip + MIN_MATCH <= input_len {
                        let ih_a = paley_hash4(&input[ip..]) as usize & ((1 << hash_bits) - 1);
                        chain_a[ip] = hash_a[ih_a];
                        hash_a[ih_a] = ip as u32;

                        let ih_b = paley_hash4_b_bits(&input[ip..], hash_bits as u32) as usize;
                        chain_b[ip] = hash_b[ih_b];
                        hash_b[ih_b] = ip as u32;

                        let ih_c = paley_hash4_c_bits(&input[ip..], hash_bits as u32) as usize;
                        chain_c[ip] = hash_c[ih_c];
                        hash_c[ih_c] = ip as u32;

                        verify_table[ip & (VERIFY_SIZE - 1)] = hash4_verify(&input[ip..]);
                    }
                }

                pos += advance;
                anchor = pos;
            } else {
                pos += 1;
            }

            // Periodic predictor learning
            predictor.current_cycle += 1;
            if predictor.current_cycle.is_multiple_of(500) {
                predictor.learn();
            }
        }

        emit_last_literals(&input[anchor..input_len], &mut output);
        output
    }

    /// Compress data using LZ4 frame format.
    pub fn encode(&self, input: &[u8]) -> Vec<u8> {
        let mut output = Vec::new();

        // Frame magic
        output.extend_from_slice(&LZ4_MAGIC.to_le_bytes());

        // Frame descriptor
        // FLG byte: version(2) | B.Indep(1) | B.Checksum(1) | C.Size(1) | C.Checksum(1) | Reserved(1) | DictID(1)
        // Version = 01 (bits 6-7)
        // Block Independence = 1 (bit 5)
        // Block Checksum = 0 (bit 4)
        // Content Size = 1 (bit 3)
        // Content Checksum = 0 (bit 2)
        let flg: u8 = 0x40 | 0x20 | 0x08; // version=01, B.Indep=1, ContentSize=1
        let bd: u8 = 0x40 | 0x20; // BlockMaxSize=4 (64KB)

        // Optional fields precede HC, and HC covers all of them.
        let size = input.len() as u64;
        let descriptor = frame_descriptor(flg, bd, Some(size), None);
        output.extend_from_slice(&descriptor);
        output.push(frame_header_checksum(&descriptor));

        // Block max size for this encoder
        let block_max = LZ4_BLOCK_MAX_64KB;
        let mut offset = 0usize;

        // A block must end on a literal-only sequence with at least 5
        // trailing literals; `encode_block` can end on a match, which
        // `lz4 -t` rejects.
        while offset < input.len() {
            let end = (offset + block_max).min(input.len());
            let block = &input[offset..end];
            let compressed = self.encode_block(block);

            let block_size = compressed.len();

            // Block header. Vendored correction, same defect as the
            // decoder's: the upstream copy shifted the size left by one
            // and used the freed bit as the last-block flag, which is a
            // layout no conformant reader accepts -- `lz4frame.c` writes
            // the size into bits 0..30 and lets the end mark terminate
            // the frame, which is what the reference output in
            // `testdata/` shows (a 70000-byte final block carries header
            // 0x80011170, bit 0 clear). rgx never encodes; this is
            // corrected so the round-trip tests below are not asserting
            // a private format.
            let block_header = block_size as u32;
            output.extend_from_slice(&block_header.to_le_bytes());
            output.extend_from_slice(&compressed);

            offset = end;
        }

        // End mark
        output.extend_from_slice(&0u32.to_le_bytes());

        // Content checksum (XXH-32 of original data, simplified)
        if self.content_checksum == ContentChecksum::XXH32 {
            let checksum = xxh32(input);
            output.extend_from_slice(&checksum.to_le_bytes());
        }

        output
    }
}

impl Default for Lz4Encoder {
    fn default() -> Self {
        Self::new()
    }
}

/// LZ4 decoder.
pub struct Lz4Decoder {
    block_max_size: usize,
}

impl Lz4Decoder {
    /// Create a new LZ4 decoder with default settings.
    pub fn new() -> Self {
        Self {
            block_max_size: LZ4_BLOCK_MAX,
        }
    }

    /// Decompress LZ4 block format (raw, no frame header).
    pub fn decode_block(&self, input: &[u8]) -> CompressResult<Vec<u8>> {
        let mut output = Vec::with_capacity(input.len() * 2);
        self.decode_block_into(input, &mut output, &mut OutputCap::unlimited())?;
        Ok(output)
    }

    /// Append one LZ4 block to `output`, which doubles as the match
    /// dictionary.
    ///
    /// Vendored note: the upstream copy decoded each block into a fresh
    /// `Vec`, so a match could only ever reach inside its own block. That
    /// is fine for independent blocks and wrong for dependent ones: with
    /// `-BD` the reference encoder links blocks and a match in block N
    /// resolves against bytes block N-1 produced, which the fresh-`Vec`
    /// read rejected as "match offset beyond output buffer". Decoding
    /// straight into the frame's running output is what the block format
    /// means by a match offset, and it is what makes `-BD` frames
    /// readable.
    fn decode_block_into(
        &self,
        input: &[u8],
        output: &mut Vec<u8>,
        cap: &mut OutputCap,
    ) -> CompressResult<()> {
        let mut pos = 0;
        let input_len = input.len();

        while pos < input_len {
            if cap.capped() {
                return Ok(());
            }
            // Read token
            let token = input[pos];
            pos += 1;

            // Literal length
            let lit_len = if (token >> 4) == 15 {
                let mut len = 15usize;
                while pos < input_len {
                    let extra = input[pos] as usize;
                    pos += 1;
                    len += extra;
                    if extra != 255 {
                        break;
                    }
                }
                len
            } else {
                (token >> 4) as usize
            };

            // Copy literals
            if pos + lit_len > input_len {
                return Err(CompressError::UnexpectedEof);
            }
            if !cap.take(lit_len as u64) {
                return Ok(());
            }
            output.extend_from_slice(&input[pos..pos + lit_len]);
            pos += lit_len;

            // Check if we're at end (last sequence has no match part)
            if pos >= input_len {
                break;
            }

            // Match offset (2 bytes, little-endian)
            if pos + 2 > input_len {
                return Err(CompressError::UnexpectedEof);
            }
            let offset = u16::from_le_bytes([input[pos], input[pos + 1]]) as usize;
            pos += 2;

            if offset == 0 {
                return Err(CompressError::CorruptedData("zero match offset"));
            }

            // Match length
            let match_len = if (token & 0x0F) == 15 {
                let mut len = 15usize;
                while pos < input_len {
                    let extra = input[pos] as usize;
                    pos += 1;
                    len += extra;
                    if extra != 255 {
                        break;
                    }
                }
                len + MIN_MATCH
            } else {
                ((token & 0x0F) as usize) + MIN_MATCH
            };

            // Copy match (may overlap)
            if offset > output.len() {
                return Err(CompressError::CorruptedData(
                    "match offset beyond output buffer",
                ));
            }
            if !cap.take(match_len as u64) {
                return Ok(());
            }

            let match_start = output.len() - offset;
            output.reserve(match_len);
            if offset >= match_len {
                // Source and destination cannot overlap, so this is one move.
                output.extend_from_within(match_start..match_start + match_len);
            } else {
                // Overlapping: the match repeats the `offset` bytes before it.
                // Emit it `offset` at a time, which keeps each chunk's source
                // behind the write cursor -- a `memmove` over the whole range
                // would instead shift the pattern and produce different bytes.
                let mut done = 0usize;
                while done < match_len {
                    let n = (match_len - done).min(offset);
                    output.extend_from_within(match_start + done..match_start + done + n);
                    done += n;
                }
            }
        }

        Ok(())
    }

    /// Decompress LZ4 frame format.
    pub fn decode(&self, input: &[u8]) -> CompressResult<Vec<u8>> {
        self.decode_bounded(input, rgx_core::compress::cap::UNLIMITED)
            .map(|b| b.bytes)
    }

    /// Decompress LZ4 frame format, refusing to expand it past `limit`
    /// bytes. FLG bit 3 puts the decompressed size in the Frame Descriptor,
    /// so a frame that declares more than the limit is refused from the
    /// declaration, before a block is decoded. The flag is optional — the
    /// reference encoder only sets it when asked — and a frame without it
    /// is bounded by the running cap in the block loop instead.
    pub fn decode_bounded(&self, input: &[u8], limit: u64) -> CompressResult<Bounded> {
        if input.len() < 4 {
            return Err(CompressError::UnexpectedEof);
        }

        let magic = u32::from_le_bytes([input[0], input[1], input[2], input[3]]);
        if magic != LZ4_MAGIC {
            return Err(CompressError::InvalidFormat("invalid LZ4 magic number"));
        }

        let mut pos = 4usize;

        // Frame descriptor
        if pos >= input.len() {
            return Err(CompressError::UnexpectedEof);
        }

        let flg = input[pos];
        pos += 1;
        if pos >= input.len() {
            return Err(CompressError::UnexpectedEof);
        }
        let _bd = input[pos];
        pos += 1;

        // Optional fields precede the header checksum.
        let has_content_size = (flg & 0x08) != 0;
        let content_size = if has_content_size {
            if pos + 8 > input.len() {
                return Err(CompressError::UnexpectedEof);
            }
            let mut buf = [0u8; 8];
            buf.copy_from_slice(&input[pos..pos + 8]);
            pos += 8;
            Some(u64::from_le_bytes(buf))
        } else {
            None
        };

        if let Some(n) = content_size {
            if n > limit {
                return Ok(Bounded::stopped(Vec::new(), Some(n), limit));
            }
        }
        let mut cap = OutputCap::new(limit);

        let has_dict_id = (flg & 0x01) != 0;
        let dict_id = if has_dict_id {
            if pos + 4 > input.len() {
                return Err(CompressError::UnexpectedEof);
            }
            let mut buf = [0u8; 4];
            buf.copy_from_slice(&input[pos..pos + 4]);
            pos += 4;
            Some(u32::from_le_bytes(buf))
        } else {
            None
        };

        // Header checksum: one byte, after the optional fields.
        if pos >= input.len() {
            return Err(CompressError::UnexpectedEof);
        }
        let header_checksum = input[pos];
        pos += 1;

        // Per-block checksum: 4 bytes follow every block's data (FLG bit 4).
        // The reference encoder emits this with -c; the decoder skips it.
        let has_block_checksum = (flg & 0x10) != 0;

        let mut output = Vec::new();

        // Decode blocks
        loop {
            if pos + 4 > input.len() {
                // The reference encoder writes a frame with no end mark for
                // empty content (the vector from lz4.frame is 11 bytes: the
                // tail is the 0-3 zero bytes of the mark). Trailing all-zero
                // bytes at EOF mean the frame ended; anything else is a
                // genuinely truncated stream.
                if input[pos..].iter().all(|&b| b == 0) {
                    break;
                }
                return Err(CompressError::UnexpectedEof);
            }
            let block_header =
                u32::from_le_bytes([input[pos], input[pos + 1], input[pos + 2], input[pos + 3]]);
            pos += 4;

            // Block size field. Vendored correction: the upstream copy read this
            // as `(header >> 1) & 0x7FFF_FFFF`, but the reference encoder
            // writes `size | (uncompressed << 31) | (last << 0)` -- the
            // size sits in bits 0..30 and the "last block" flag is bit 0
            // of that same field, so the two overlap and the size is
            // never shifted. Shifting it here halved every real block
            // length, which is why no reference-produced .lz4 frame
            // walked. See the note in this tree's mod.rs.
            let block_size = (block_header & 0x7FFF_FFFF) as usize;
            let block_uncompressed = (block_header & (1 << 31)) != 0;

            // End mark: block_size == 0 signals end of stream
            if block_size == 0 {
                break;
            }

            if pos + block_size > input.len() {
                return Err(CompressError::UnexpectedEof);
            }

            if block_uncompressed {
                // Uncompressed block
                if !cap.take(block_size as u64) {
                    return Ok(Bounded::stopped(output, content_size, limit));
                }
                output.extend_from_slice(&input[pos..pos + block_size]);
            } else {
                // Compressed block. Decoded straight into the frame's
                // running output rather than a per-block buffer: with
                // dependent blocks a match here can resolve against the
                // previous block's bytes.
                self.decode_block_into(&input[pos..pos + block_size], &mut output, &mut cap)?;
                if cap.capped() {
                    return Ok(Bounded::stopped(output, content_size, limit));
                }
            }

            pos += block_size;
            if has_block_checksum {
                if pos + 4 > input.len() {
                    return Err(CompressError::UnexpectedEof);
                }
                pos += 4;
            }

            // Vendored correction: the upstream copy also stopped on the "last
            // block" bit. That bit is bit 0 of the size field, so it
            // reads as set for every odd-sized block and would truncate
            // any multi-block frame. The end mark above is the
            // terminator.
        }

        // Content checksum
        let has_checksum = (flg & 0x04) != 0;
        if has_checksum && pos + 4 > input.len() {
            return Err(CompressError::UnexpectedEof);
        }
        // Skip checksum verification

        Ok(Bounded::complete(output, content_size, limit))
    }

    /// Read the deprecated *legacy* container that `lz4 -l` emits.
    ///
    /// New code, not a vendored port: the legacy format is a bare sequence
    /// of length-prefixed blocks with no frame header, no block checksums and
    /// no end-mark semantics, so it shares only the block compressor with
    /// the frame reader above.
    ///
    /// Format details are as verified against the reference `lz4io.c` and
    /// confirmed against `lz4 -l` fixtures, not assumed:
    ///
    /// - magic `0x184C2102`, little-endian
    /// - then repeated: a 4-byte little-endian size, then that many bytes
    ///   of block-compressed data. The size counts the **payload only** —
    ///   it does not include its own 4 bytes (`lz4io.c:794-799` writes
    ///   `cSize`, and the reader at `:1789` `fread`s `blockSize` straight
    ///   after the header).
    /// - a size of `0` ends the stream, as does a size past
    ///   `LZ4_LEGACY_BLOCK_MAX_COMPRESSED`, which is the reference reader's
    ///   "this is the next stream's magic" case (`lz4io.c:1777-1786`).
    ///   The reference writer never emits either.
    /// - blocks are 8 MiB, not the frame format's 64 KiB (`lz4io.c:91`).
    ///
    /// Blocks decode into one running buffer rather than a fresh `Vec` each
    /// time. The `lz4 -l` writer compresses each block independently so its
    /// output does not need that, but a legacy container written by an older
    /// tool can link blocks, and then a match in block N resolves against
    /// bytes block N-1 produced.
    pub fn decode_legacy(&self, input: &[u8]) -> CompressResult<Vec<u8>> {
        self.decode_legacy_bounded(input, rgx_core::compress::cap::UNLIMITED)
            .map(|b| b.bytes)
    }

    /// [`Self::decode_legacy`] under a ceiling on the output. The legacy
    /// container has no header to declare a size in, so the ceiling is the
    /// running one, inside the block loop.
    pub fn decode_legacy_bounded(&self, input: &[u8], limit: u64) -> CompressResult<Bounded> {
        if input.len() < 4 {
            return Err(CompressError::UnexpectedEof);
        }

        let magic = u32::from_le_bytes([input[0], input[1], input[2], input[3]]);
        if magic != LZ4_LEGACY_MAGIC {
            return Err(CompressError::InvalidFormat(
                "not an LZ4 legacy container (magic 0x184C2102)",
            ));
        }

        let mut pos = 4usize;
        let mut output = Vec::new();
        let mut cap = OutputCap::new(limit);

        while pos < input.len() {
            if pos + 4 > input.len() {
                return Err(CompressError::UnexpectedEof);
            }
            let block_size =
                u32::from_le_bytes([input[pos], input[pos + 1], input[pos + 2], input[pos + 3]])
                    as usize;
            pos += 4;

            // End mark, and the reference reader's "this is the next
            // stream's magic, not a block" case.
            if block_size == 0 {
                break;
            }
            if block_size > LZ4_LEGACY_BLOCK_MAX_COMPRESSED {
                break;
            }

            if pos + block_size > input.len() {
                return Err(CompressError::UnexpectedEof);
            }

            self.decode_block_into(&input[pos..pos + block_size], &mut output, &mut cap)?;
            if cap.capped() {
                return Ok(Bounded::stopped(output, None, limit));
            }
            pos += block_size;
        }

        Ok(Bounded::complete(output, None, limit))
    }

    /// Decompress either container, choosing by the 4-byte magic.
    ///
    /// This is the entry point to reach for when the container is not known
    /// up front -- `.lz4` files in the wild are a mix of the two, and
    /// rejecting one of them is the regression `decode_legacy` exists to
    /// undo.
    pub fn decode_auto(&self, input: &[u8]) -> CompressResult<Vec<u8>> {
        self.decode_auto_bounded(input, rgx_core::compress::cap::UNLIMITED)
            .map(|b| b.bytes)
    }

    /// [`Self::decode_auto`] under a ceiling on the output.
    pub fn decode_auto_bounded(&self, input: &[u8], limit: u64) -> CompressResult<Bounded> {
        if input.len() >= 4 {
            let magic = u32::from_le_bytes([input[0], input[1], input[2], input[3]]);
            if magic == LZ4_LEGACY_MAGIC {
                return self.decode_legacy_bounded(input, limit);
            }
        }
        self.decode_bounded(input, limit)
    }

    /// Report whether `input` starts with the legacy container magic.
    pub fn is_legacy(input: &[u8]) -> bool {
        input.len() >= 4
            && u32::from_le_bytes([input[0], input[1], input[2], input[3]]) == LZ4_LEGACY_MAGIC
    }
}

impl Default for Lz4Decoder {
    fn default() -> Self {
        Self::new()
    }
}

/// Standard LZ4 multiplicative hash (16-bit).
fn hash4_standard(data: &[u8]) -> usize {
    let v = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);
    (v.wrapping_mul(0x9E37_A7BD) >> (32 - 16)) as usize
}

/// Paley graph-inspired hash (16-bit) for better avalanche properties.
fn hash4_paley(data: &[u8]) -> usize {
    paley_hash4(data) as usize & ((1 << 16) - 1)
}

/// Paley B-probe hash (16-bit) for multi-probe hashing.
fn hash4_paley_b(data: &[u8]) -> usize {
    paley_hash4_b_bits(data, 16) as usize
}

/// Paley C-probe hash (16-bit) for multi-probe hashing.
fn hash4_paley_c(data: &[u8]) -> usize {
    paley_hash4_c_bits(data, 16) as usize
}

/// Secondary verification hash for reducing false positives.
fn hash4_verify(data: &[u8]) -> u16 {
    paley_hash4_verify(data)
}

fn emit_last_literals(literals: &[u8], output: &mut Vec<u8>) {
    let lit_len = literals.len();
    if lit_len == 0 {
        return;
    }

    // Token: match_len=0 (so lower nibble=0)
    let mut token = 0u8;

    if lit_len >= 15 {
        token |= 0xF0;
        let mut remaining = lit_len - 15;
        output.push(token);
        while remaining >= 255 {
            output.push(255);
            remaining -= 255;
        }
        output.push(remaining as u8);
    } else {
        token |= (lit_len as u8) << 4;
        output.push(token);
    }

    output.extend_from_slice(literals);
}

fn encode_sequence(
    literals: &[u8],
    lit_len: usize,
    offset: usize,
    match_len: usize,
    output: &mut Vec<u8>,
) {
    // match_len is the excess (actual_match_len - MIN_MATCH)
    // Token nibble stores the excess, capped at 15
    let lit_nibble = if lit_len >= 15 { 15u8 } else { lit_len as u8 };
    let match_nibble = if match_len >= 15 {
        15u8
    } else {
        match_len as u8
    };
    let token = (lit_nibble << 4) | match_nibble;
    output.push(token);

    // Extended literal length
    if lit_len >= 15 {
        let mut remaining = lit_len - 15;
        while remaining >= 255 {
            output.push(255);
            remaining -= 255;
        }
        output.push(remaining as u8);
    }

    // Literals
    output.extend_from_slice(literals);

    // Match offset (2 bytes, little-endian)
    output.push(offset as u8);
    output.push((offset >> 8) as u8);

    // Extended match length (when excess >= 15, i.e., nibble = 15)
    if match_len >= 15 {
        let mut remaining = match_len - 15;
        while remaining >= 255 {
            output.push(255);
            remaining -= 255;
        }
        output.push(remaining as u8);
    }
}

/// XXH32, seed 0.
fn xxh32(data: &[u8]) -> u32 {
    const PRIME1: u32 = 2_654_435_761;
    const PRIME2: u32 = 2_246_822_519;
    const PRIME3: u32 = 3_266_489_917;
    const PRIME4: u32 = 668_265_263;
    const PRIME5: u32 = 374_761_393;

    #[inline]
    fn round(seed: u32, lane: u32) -> u32 {
        (seed.wrapping_add(lane.wrapping_mul(PRIME2)).rotate_left(13)).wrapping_mul(PRIME1)
    }

    let n = data.len();
    let mut i = 0usize;
    let mut h;
    if n >= 16 {
        let mut v1 = PRIME1.wrapping_add(PRIME2);
        let mut v2 = PRIME2;
        let mut v3 = 0u32;
        let mut v4 = 0u32.wrapping_sub(PRIME1);
        while i + 16 <= n {
            let lane =
                |o: usize| u32::from_le_bytes([data[o], data[o + 1], data[o + 2], data[o + 3]]);
            v1 = round(v1, lane(i));
            v2 = round(v2, lane(i + 4));
            v3 = round(v3, lane(i + 8));
            v4 = round(v4, lane(i + 12));
            i += 16;
        }
        h = v1
            .rotate_left(1)
            .wrapping_add(v2.rotate_left(7))
            .wrapping_add(v3.rotate_left(12))
            .wrapping_add(v4.rotate_left(18));
        h = h.wrapping_add(n as u32);
    } else {
        h = PRIME5.wrapping_add(n as u32);
    }

    while i + 4 <= n {
        let lane = u32::from_le_bytes([data[i], data[i + 1], data[i + 2], data[i + 3]]);
        h = h.wrapping_add(lane.wrapping_mul(PRIME3));
        h = h.rotate_left(17).wrapping_mul(PRIME4);
        i += 4;
    }
    while i < n {
        h = h.wrapping_add((data[i] as u32).wrapping_mul(PRIME5));
        h = h.rotate_left(11).wrapping_mul(PRIME1);
        i += 1;
    }

    h ^= h >> 15;
    h = h.wrapping_mul(PRIME2);
    h ^= h >> 13;
    h = h.wrapping_mul(PRIME3);
    h ^= h >> 16;
    h
}

/// FLG, BD and the optional fields; the header checksum covers exactly these.
fn frame_descriptor(flg: u8, bd: u8, content_size: Option<u64>, dict_id: Option<u32>) -> Vec<u8> {
    let mut d = Vec::with_capacity(14);
    d.push(flg);
    d.push(bd);
    if let Some(size) = content_size {
        d.extend_from_slice(&size.to_le_bytes());
    }
    if let Some(id) = dict_id {
        d.extend_from_slice(&id.to_le_bytes());
    }
    d
}

/// The second byte of the descriptor's XXH32. The descriptor as a whole:
/// FLG and BD alone agrees with 16 of 18 reference frames and fails on the
/// two `--content-size` ones.
fn frame_header_checksum(descriptor: &[u8]) -> u8 {
    (xxh32(descriptor) >> 8) as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_lz4_magic() {
        assert_eq!(LZ4_MAGIC, 0x184D_2204);
    }

    #[test]
    fn test_lz4_level_defaults() {
        assert_eq!(Lz4Level::DEFAULT.0, 4);
        assert_eq!(Lz4Level::FASTEST.0, 0);
        assert_eq!(Lz4Level::MAX.0, 16);
    }

    #[test]
    fn test_encode_decode_block_empty() {
        let enc = Lz4Encoder::new();
        let dec = Lz4Decoder::new();
        let compressed = enc.encode_block(b"");
        let decompressed = dec.decode_block(&compressed).unwrap();
        assert!(decompressed.is_empty());
    }

    #[test]
    fn test_encode_decode_block_short() {
        let enc = Lz4Encoder::new();
        let dec = Lz4Decoder::new();
        let input = b"Hi";
        let compressed = enc.encode_block(input);
        let decompressed = dec.decode_block(&compressed).unwrap();
        assert_eq!(&decompressed, input);
    }

    #[test]
    fn test_encode_decode_block_short2() {
        let enc = Lz4Encoder::new();
        let dec = Lz4Decoder::new();
        let input = b"abc";
        let compressed = enc.encode_block(input);
        let decompressed = dec.decode_block(&compressed).unwrap();
        assert_eq!(&decompressed, input);
    }

    #[test]
    fn test_encode_decode_block_repeated() {
        let enc = Lz4Encoder::new();
        let dec = Lz4Decoder::new();
        let input = b"aaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let compressed = enc.encode_block(input);
        let decompressed = dec.decode_block(&compressed).unwrap();
        assert_eq!(&decompressed, input);
    }

    #[test]
    fn test_encode_decode_block_with_match() {
        let enc = Lz4Encoder::new();
        let dec = Lz4Decoder::new();
        let input = b"Hello, World! Hello, World!";
        let compressed = enc.encode_block(input);
        let decompressed = dec.decode_block(&compressed).unwrap();
        assert_eq!(&decompressed, input);
    }

    #[test]
    fn test_encode_decode_block_binary() {
        let enc = Lz4Encoder::new();
        let dec = Lz4Decoder::new();
        let input: Vec<u8> = (0..=255).cycle().take(1000).collect();
        let compressed = enc.encode_block(&input);
        let decompressed = dec.decode_block(&compressed).unwrap();
        assert_eq!(decompressed, input);
    }

    #[test]
    fn test_encode_decode_block_large() {
        let enc = Lz4Encoder::new();
        let dec = Lz4Decoder::new();
        let input: Vec<u8> = (0u8..=255).cycle().take(100_000).collect();
        let compressed = enc.encode_block(&input);
        let decompressed = dec.decode_block(&compressed).unwrap();
        assert_eq!(decompressed, input);
    }

    #[test]
    fn test_encode_decode_block_long_match() {
        let enc = Lz4Encoder::new();
        let dec = Lz4Decoder::new();
        let mut input = Vec::new();
        input.extend_from_slice(b"ABCDEF");
        input.extend_from_slice(&[0u8; 300]);
        input.extend_from_slice(b"ABCDEF");
        input.extend_from_slice(&[0u8; 300]);
        let compressed = enc.encode_block(&input);
        let decompressed = dec.decode_block(&compressed).unwrap();
        assert_eq!(decompressed, input);
    }

    #[test]
    fn test_encode_decode_frame() {
        let enc = Lz4Encoder::new();
        let dec = Lz4Decoder::new();
        let input = b"Hello, LZ4 frame compression!";

        // First test block encode/decode directly
        let block_compressed = enc.encode_block(input);
        let block_decompressed = dec
            .decode_block(&block_compressed)
            .expect("Block decode failed");
        assert_eq!(&block_decompressed, input, "Block roundtrip failed");

        // Then test frame encode/decode
        let compressed = enc.encode(input);
        let decompressed = dec.decode(&compressed).expect("Frame decode failed");
        assert_eq!(&decompressed, input);
    }

    #[test]
    fn test_decode_invalid_magic() {
        let dec = Lz4Decoder::new();
        assert!(dec.decode(&[0, 0, 0, 0]).is_err());
    }

    #[test]
    fn test_decode_too_short() {
        let dec = Lz4Decoder::new();
        assert!(dec.decode(&[0x04, 0x22, 0x4D, 0x18]).is_err());
    }

    #[test]
    fn test_decode_block_corrupted_offset() {
        let dec = Lz4Decoder::new();
        // Token with literals then invalid offset
        let corrupted = vec![0x10, b'A', 0x00, 0x00];
        let result = dec.decode_block(&corrupted);
        assert!(result.is_err());
    }

    #[test]
    fn test_decode_block_zero_offset() {
        let dec = Lz4Decoder::new();
        // Token: 0 lit, 0 match_len -> offset=0 (invalid)
        let corrupted = vec![0x00, 0x00, 0x00];
        let result = dec.decode_block(&corrupted);
        assert!(result.is_err());
    }

    #[test]
    fn test_compression_ratio_repeated() {
        let enc = Lz4Encoder::new();
        let input: Vec<u8> = b"ABCABCABCABCABCABCABCABCABC".to_vec();
        let compressed = enc.encode_block(&input);
        assert!(compressed.len() < input.len());
    }

    #[test]
    #[cfg(feature = "std")]
    fn test_lz4_paley_roundtrip_bib() {
        // Vendored note: the upstream copy pointed this at its own Calgary
        // corpus copy, which rgx does not have. It is a matched-pair
        // round trip, so it says nothing about interop -- the
        // reference-CLI vectors below are the ones that do. Repointed at
        // a committed payload purely to keep the large-input round trip
        // running.
        let bib: &[u8] = include_bytes!("../../testdata/lz4_big.expected");
        let enc = Lz4Encoder::new().with_graph_mode(GraphMode::Paley);
        let dec = Lz4Decoder::new();

        // Test block
        let block_compressed = enc.encode_block(bib);
        let block_decompressed = dec
            .decode_block(&block_compressed)
            .expect("Block decode failed");
        assert_eq!(&block_decompressed[..], bib, "Block roundtrip mismatch");

        // Test frame
        let frame_compressed = enc.encode(bib);
        let frame_decompressed = dec.decode(&frame_compressed).expect("Frame decode failed");
        assert_eq!(&frame_decompressed[..], bib, "Frame roundtrip mismatch");
    }

    #[test]
    fn test_xxh32_deterministic() {
        let h1 = xxh32(b"Hello");
        let h2 = xxh32(b"Hello");
        assert_eq!(h1, h2);
    }

    #[test]
    fn test_xxh32_different_inputs() {
        let h1 = xxh32(b"Hello");
        let h2 = xxh32(b"World");
        assert_ne!(h1, h2);
    }

    #[test]
    fn test_encoder_levels() {
        for level in [
            Lz4Level::FASTEST,
            Lz4Level::FAST,
            Lz4Level::DEFAULT,
            Lz4Level::HIGH,
        ] {
            let enc = Lz4Encoder::with_level(level);
            let dec = Lz4Decoder::new();
            let input: Vec<u8> = (0u8..=255).cycle().take(500).collect();
            let compressed = enc.encode_block(&input);
            let decompressed = dec.decode_block(&compressed).unwrap();
            assert_eq!(decompressed, input, "Failed for level {:?}", level);
        }
    }

    #[test]
    fn test_encoder_builder() {
        let enc = Lz4Encoder::new()
            .with_block_mode(BlockMode::Independent)
            .with_checksum(ContentChecksum::XXH32);
        let input = b"test data";
        let compressed = enc.encode(input);
        assert!(compressed.len() > 4);
    }

    /// Third-party vectors: frames produced by the **reference `lz4` CLI**
    /// (lz4 v1.10.0, Yann Collet), committed under `testdata/` so the
    /// test runs without the tool installed; see `testdata/lz4_README.md`
    /// for the exact command per vector.
    ///
    /// These are the vectors that matter, and they are why the frame walk
    /// is not the matched pair above. Between them they cover every frame
    /// feature the reference CLI can emit:
    /// - end mark only (empty), single block, many blocks (300 KB)
    /// - compressed *and* uncompressed blocks (incompressible payload)
    /// - content size present / absent (FLG bit 3)
    /// - content checksum present / absent (`--no-frame-crc`, FLG bit 2)
    /// - per-block checksums (`-BX`, FLG bit 4)
    /// - 64 KB and 4 MB block sizes (`-B4`, `-B7`)
    /// - linked/dependent blocks (`-BD`)
    /// - levels 1 and 9
    ///
    /// The upstream copy read the block size as `(header >> 1) & 0x7FFF_FFFF`
    /// and failed every one of these except the empty frame. The shifted
    /// read is the defect this file's frame walk corrects; see the note
    /// in the tree's `mod.rs`.
    #[test]
    fn lz4_reference_cli_vectors() {
        // (vector, payload) -- the payload is shared between the variants
        // of one input, so it is stored once.
        macro_rules! case {
            ($name:literal, $payload:literal) => {{
                let compressed = include_bytes!(concat!("../../testdata/lz4_", $name, ".lz4"));
                let expected =
                    include_bytes!(concat!("../../testdata/lz4_", $payload, ".expected"));
                let out = Lz4Decoder::new()
                    .decode(compressed)
                    .expect(concat!("lz4_", $name));
                assert_eq!(out, &expected[..], concat!("lz4_", $name));
            }};
        }
        // end mark with no block at all
        case!("empty_plain", "empty");
        // a single stored (uncompressed) block
        case!("tiny_plain", "tiny");
        // a single compressed block + content checksum
        case!("text_plain", "text");
        case!("text_l9", "text");
        // header variants
        case!("text_csize", "text");
        case!("text_nocrc", "text");
        // per-block checksums
        case!("text_bx", "text");
        case!("text_bx_csize", "text");
        // block sizes and block linkage
        case!("text_b4", "text");
        case!("text_b7", "text");
        case!("text_bd", "text");
        case!("text_bd_bx", "text");
        // highly repetitive: long matches
        case!("repeat_plain", "repeat");
        // incompressible: the reference stores this one uncompressed
        case!("binary_plain", "binary");
        case!("binary_b4", "binary");
        // 250 KB at 64 KB blocks: four compressed blocks in one frame
        case!("big_plain", "big");
        case!("big_bx", "big");
        case!("big_bd", "big");
    }

    #[test]
    fn header_checksum_matches_every_reference_frame() {
        macro_rules! frame {
            ($name:literal) => {
                include_bytes!(concat!("../../testdata/lz4_", $name, ".lz4"))
            };
        }
        let frames: &[(&str, &[u8])] = &[
            ("lz4_tiny_plain", frame!("tiny_plain")),
            ("lz4_empty_plain", frame!("empty_plain")),
            ("lz4_text_plain", frame!("text_plain")),
            ("lz4_text_l9", frame!("text_l9")),
            ("lz4_text_nocrc", frame!("text_nocrc")),
            ("lz4_text_csize", frame!("text_csize")),
            ("lz4_text_bx", frame!("text_bx")),
            ("lz4_text_bx_csize", frame!("text_bx_csize")),
            ("lz4_text_b4", frame!("text_b4")),
            ("lz4_text_b7", frame!("text_b7")),
            ("lz4_text_bd", frame!("text_bd")),
            ("lz4_text_bd_bx", frame!("text_bd_bx")),
            ("lz4_repeat_plain", frame!("repeat_plain")),
            ("lz4_binary_plain", frame!("binary_plain")),
            ("lz4_binary_b4", frame!("binary_b4")),
            ("lz4_big_plain", frame!("big_plain")),
            ("lz4_big_bx", frame!("big_bx")),
            ("lz4_big_bd", frame!("big_bd")),
        ];

        for (name, frame) in frames {
            assert_eq!(&frame[..4], &LZ4_MAGIC.to_le_bytes(), "{name}: not a frame");
            let flg = frame[4];
            let bd = frame[5];
            let mut at = 6usize;
            let content_size = if flg & 0x08 != 0 {
                let mut buf = [0u8; 8];
                buf.copy_from_slice(&frame[at..at + 8]);
                at += 8;
                Some(u64::from_le_bytes(buf))
            } else {
                None
            };
            let dict_id = if flg & 0x01 != 0 {
                let mut buf = [0u8; 4];
                buf.copy_from_slice(&frame[at..at + 4]);
                at += 4;
                Some(u32::from_le_bytes(buf))
            } else {
                None
            };
            let stored = frame[at];
            let descriptor = frame_descriptor(flg, bd, content_size, dict_id);
            assert_eq!(
                frame_header_checksum(&descriptor),
                stored,
                "{name}: header checksum over {descriptor:02x?} should be {stored:#04x}"
            );
        }
    }

    #[test]
    fn xxh32_matches_the_reference_content_checksums() {
        macro_rules! vector {
            ($name:literal, $payload:literal, $frame:literal) => {{
                let expected =
                    include_bytes!(concat!("../../testdata/lz4_", $payload, ".expected"));
                let frame = include_bytes!(concat!("../../testdata/lz4_", $frame, ".lz4"));
                assert_ne!(
                    frame[4] & 0x04,
                    0,
                    concat!($frame, ": this frame carries no content checksum")
                );
                let mut tail = [0u8; 4];
                tail.copy_from_slice(&frame[frame.len() - 4..]);
                ($name, &expected[..], u32::from_le_bytes(tail))
            }};
        }
        let vectors = [
            vector!("empty", "empty", "empty_plain"),
            vector!("tiny", "tiny", "tiny_plain"),
            vector!("repeat", "repeat", "repeat_plain"),
            vector!("text", "text", "text_plain"),
            vector!("binary", "binary", "binary_plain"),
            vector!("big", "big", "big_plain"),
        ];
        for (name, expected, stored) in vectors {
            assert_eq!(
                xxh32(expected),
                stored,
                "{name}: XXH32 over {} bytes must be {stored:#010x}",
                expected.len()
            );
        }
    }

    #[test]
    fn encoded_frames_are_laid_out_for_a_conformant_reader() {
        let input: Vec<u8> = (0u8..=255).cycle().take(4096).collect();
        let frame = Lz4Encoder::new().encode(&input);

        assert_eq!(&frame[..4], &LZ4_MAGIC.to_le_bytes());
        let flg = frame[4];
        let bd = frame[5];
        assert_ne!(flg & 0x08, 0, "the encoder declares a content size");
        assert_eq!(flg & 0x01, 0, "and no dictionary id");

        let size_at = 6usize;
        let mut size_buf = [0u8; 8];
        size_buf.copy_from_slice(&frame[size_at..size_at + 8]);
        assert_eq!(
            u64::from_le_bytes(size_buf),
            input.len() as u64,
            "the content size must sit immediately after FLG and BD"
        );

        // HC follows the optional fields, and covers them.
        let hc_at = size_at + 8;
        let descriptor = frame_descriptor(flg, bd, Some(input.len() as u64), None);
        assert_eq!(
            frame_header_checksum(&descriptor),
            frame[hc_at],
            "the header checksum follows the content size and covers it"
        );

        assert_ne!(
            frame_header_checksum(&descriptor),
            frame_header_checksum(&[flg, bd]),
            "the descriptor must cover more than FLG and BD"
        );
    }

    /// Every block this encoder writes obeys the end-of-block rules.
    ///
    /// A round trip cannot check this: rgx's decoder does not enforce the
    /// rules, so a malformed block still round-trips. `lz4 -t` does, and
    /// rejects anything whose final sequence is a match or whose last five
    /// bytes are not literals. So this walks each encoded block the way a
    /// conformant reader does and asserts the two invariants, across both
    /// match finders and a range of sizes that straddles MFLIMIT.
    #[test]
    fn encoded_blocks_end_on_literals() {
        fn check(block: &[u8], len: usize, what: &str) {
            // (literals, followed-by-a-match) for each sequence in the block.
            let mut seqs: Vec<(usize, bool)> = Vec::new();
            let mut at = 0usize;
            while at < block.len() {
                let token = block[at];
                at += 1;
                let mut lit = (token >> 4) as usize;
                if lit == 15 {
                    loop {
                        let b = block[at];
                        at += 1;
                        lit += b as usize;
                        if b != 255 {
                            break;
                        }
                    }
                }
                at += lit;
                let has_match = at < block.len();
                if has_match {
                    at += 2; // offset
                    let mut mlen = (token & 0xF) as usize;
                    if mlen == 15 {
                        loop {
                            let b = block[at];
                            at += 1;
                            mlen += b as usize;
                            if b != 255 {
                                break;
                            }
                        }
                    }
                }
                seqs.push((lit, has_match));
            }
            assert_eq!(at, block.len(), "{what}: block walk overran");

            let last = seqs.last().copied().unwrap_or((0, false));
            assert!(
                !last.1,
                "{what}: block of {len} bytes ends on a match, not literals"
            );
            // The 5-literal rule only binds once a match has been emitted; a
            // block that is nothing but literals is legal whatever its length.
            if seqs.iter().any(|(_, m)| *m) {
                assert!(
                    last.0 >= LAST_LITERALS,
                    "{what}: block of {len} bytes ends with only {} literal bytes",
                    last.0
                );
            }
        }

        for len in [
            0usize, 1, 2, 3, 4, 5, 6, 11, 12, 13, 14, 16, 17, 31, 32, 33, 63, 64, 65, 100, 255,
            256, 1000, 4096,
        ] {
            let mut x: u32 = 99 + len as u32;
            let incompressible: Vec<u8> = (0..len)
                .map(|_| {
                    x = x.wrapping_mul(1664525).wrapping_add(1013904223);
                    (x >> 24) as u8
                })
                .collect();
            let repetitive: Vec<u8> = (0..len).map(|i| b"abcde"[i % 5]).collect();
            let mixed: Vec<u8> = (0..len)
                .map(|i| {
                    if i % 7 < 3 {
                        b"xy"[(i / 7) % 2]
                    } else {
                        (i % 251) as u8
                    }
                })
                .collect();

            for data in [&incompressible, &repetitive, &mixed] {
                for mode in [GraphMode::Standard, GraphMode::Paley] {
                    let enc = Lz4Encoder::new().with_graph_mode(mode);
                    let frame = enc.encode(data);
                    // Skip magic + descriptor + content size + checksum.
                    let block_at = 15usize;
                    let hdr = u32::from_le_bytes([
                        frame[block_at],
                        frame[block_at + 1],
                        frame[block_at + 2],
                        frame[block_at + 3],
                    ]);
                    let size = (hdr & 0x7FFF_FFFF) as usize;
                    let block = &frame[block_at + 4..block_at + 4 + size];
                    check(block, len, &format!("len {len} mode {mode:?}"));

                    // And it must still decode to the input.
                    assert_eq!(
                        Lz4Decoder::new().decode(&frame).unwrap(),
                        *data,
                        "len {len} mode {mode:?}"
                    );
                }
            }
        }
    }

    /// A frame that states its decompressed size (FLG bit 3) is refused
    /// from the Frame Descriptor, before a block is walked.
    #[test]
    fn decode_bounded_refuses_from_the_frame_descriptor() {
        let frame = include_bytes!("../../testdata/lz4_text_csize.lz4");
        let expected = include_bytes!("../../testdata/lz4_text.expected");
        let b = Lz4Decoder::new()
            .decode_auto_bounded(frame, 1000)
            .expect("bounded");
        assert!(b.capped);
        assert_eq!(b.declared, Some(expected.len() as u64));
        assert!(
            b.bytes.is_empty(),
            "refused early means no block was decoded"
        );

        let whole = Lz4Decoder::new()
            .decode_auto_bounded(frame, expected.len() as u64)
            .expect("exact");
        assert!(!whole.capped);
        assert_eq!(whole.bytes, expected);
    }

    /// The reference encoder only writes that flag when asked, so a frame
    /// without it is bounded by the running cap, mid-block.
    #[test]
    fn decode_bounded_stops_a_frame_that_declares_no_size() {
        let frame = include_bytes!("../../testdata/lz4_text_plain.lz4");
        let expected = include_bytes!("../../testdata/lz4_text.expected");
        let b = Lz4Decoder::new()
            .decode_auto_bounded(frame, 1000)
            .expect("bounded");
        assert!(b.capped);
        assert_eq!(b.declared, None, "this frame states no content size");
        assert!(b.bytes.len() <= 1000, "emitted {} bytes", b.bytes.len());
        assert_eq!(b.bytes, &expected[..b.bytes.len()], "not the real prefix");
    }

    /// The legacy container has no header at all, so it has nothing to
    /// refuse from and is bounded by the running cap alone.
    #[test]
    fn decode_bounded_stops_a_legacy_container() {
        let frame = include_bytes!("../../testdata/lz4legacy_text.lz4");
        let expected = include_bytes!("../../testdata/lz4legacy_text.expected");
        let b = Lz4Decoder::new()
            .decode_auto_bounded(frame, 1000)
            .expect("bounded");
        assert!(b.capped);
        assert_eq!(b.declared, None);
        assert_eq!(b.bytes, &expected[..b.bytes.len()], "not the real prefix");
        assert!(
            b.bytes.len() <= 1000 && !b.bytes.is_empty(),
            "{} bytes",
            b.bytes.len()
        );
    }

    /// The match copy has two branches — one `memmove` when the source and
    /// destination cannot overlap, a chunked repeat when they can — and the
    /// second is where a match longer than its own offset lives. Every
    /// reference vector in `testdata/` happens to have short matches, so
    /// nothing in the corpus reaches the overlapping branch except
    /// `lz4legacy_multiblock`, which has no committed plaintext to check
    /// against. This checks the branch against the format definition
    /// itself: a direct transcription of RFC 8870 §3.1.1.3 ("the last
    /// `matchlength` bytes are copied from `output[-offset]`, a source
    /// that may overlap the bytes being written") over a block stream
    /// built to hit every combination of `offset` and `matchlength`,
    /// including the ones a real encoder never chooses.
    mod overlapping_match_copy {
        use super::*;

        /// RFC 8870 §3.1.1.3, transcribed literally: one byte at a time,
        /// reading from the bytes the stream has already produced.
        fn reference(literals: &[u8], out: &mut Vec<u8>, offset: usize, len: usize) {
            out.extend_from_slice(literals);
            for _ in 0..len {
                let byte = out[out.len() - offset];
                out.push(byte);
            }
        }

        /// A length that does not fit the token nibble is written as a
        /// chain of 255s terminated by the remainder (RFC 8870 §3.1.1.3),
        /// not as one byte -- which is the whole of a `len` above 274.
        fn extra(mut n: usize) -> Vec<u8> {
            let mut v = Vec::new();
            while n >= 255 {
                v.push(255u8);
                n -= 255;
            }
            v.push(n as u8);
            v
        }

        /// One *sequence*: token, `[lit extra]`, literals, offset,
        /// `[match extra]`. The last sequence of a block carries no match
        /// part, so the stream below ends with [`literals_only`].
        fn sequence(literals: &[u8], offset: u16, len: usize) -> Vec<u8> {
            let (nib_l, ex_l) = if literals.len() < 15 {
                (literals.len() as u8, Vec::new())
            } else {
                (0x0Fu8, extra(literals.len() - 15))
            };
            let (nib_m, ex_m) = if len - 4 < 15 {
                ((len - 4) as u8, Vec::new())
            } else {
                (0x0Fu8, extra(len - 4 - 15))
            };
            let mut b = Vec::new();
            b.push((nib_l << 4) | nib_m);
            b.extend_from_slice(&ex_l);
            b.extend_from_slice(literals);
            b.extend_from_slice(&offset.to_le_bytes());
            b.extend_from_slice(&ex_m);
            b
        }

        /// The literals-only last sequence, which is how a block ends.
        fn literals_only(literals: &[u8]) -> Vec<u8> {
            let (nib, ex) = if literals.len() < 15 {
                (literals.len() as u8, Vec::new())
            } else {
                (0x0Fu8, extra(literals.len() - 15))
            };
            let mut b = vec![nib << 4];
            b.extend_from_slice(&ex);
            b.extend_from_slice(literals);
            b
        }

        #[test]
        fn both_branches_agree_with_the_format_definition() {
            let d = Lz4Decoder::new();
            // Every offset from 1 (the run case, where a match repeats a
            // single byte far longer than itself) up past the match length
            // (the non-overlapping case), crossed with lengths that are
            // shorter than, exactly equal to, and many times the offset --
            // and past 274, where the token nibble stops holding the length
            // and the 255-chain starts.
            let mut checked = 0usize;
            let mut overlapping = 0usize;
            for seed in 0..4u32 {
                for offset in [
                    1usize, 2, 3, 4, 5, 7, 8, 15, 16, 31, 32, 63, 64, 255, 256, 1000, 4096,
                ] {
                    for len in [
                        4usize, 5, 6, 7, 8, 15, 16, 17, 18, 19, 20, 31, 32, 33, 63, 64, 100, 255,
                        256, 269, 270, 274, 275, 300, 1000, 4096, 20000, 70000,
                    ] {
                        // The literal run has to be at least `offset` bytes,
                        // or the match reaches before the start of the block.
                        let mut lit = Vec::with_capacity(offset + 8);
                        for i in 0..offset + 8 {
                            lit.push((seed as u8).wrapping_mul(31).wrapping_add(i as u8));
                        }
                        let tail = [0xEEu8, 0xDD];
                        let mut want = Vec::new();
                        reference(&lit, &mut want, offset, len);
                        want.extend_from_slice(&tail);
                        let mut stream = sequence(&lit, offset as u16, len);
                        stream.extend_from_slice(&literals_only(&tail));
                        let got = d
                            .decode_block(&stream)
                            .unwrap_or_else(|e| panic!("offset {offset} len {len}: {e:?}"));
                        assert_eq!(
                            got.len(),
                            want.len(),
                            "offset {offset}, match length {len}: wrong output length"
                        );
                        if got != want {
                            let first = got.iter().zip(&want).position(|(a, b)| a != b).unwrap();
                            panic!(
                                "offset {offset}, match length {len}: byte {first} is {:?}, want {:?} \
                                 -- the copy is not what RFC 8870 §3.1.1.3 says",
                                got[first], want[first]
                            );
                        }
                        checked += 1;
                        if len > offset {
                            overlapping += 1;
                        }
                    }
                }
            }
            assert!(checked > 1500, "only {checked} combinations");
            assert!(overlapping > 800, "only {overlapping} of them overlap");
        }

        /// The whole-block reading, not just one sequence: a stream of
        /// several matches at differing offsets, so the dictionary the
        /// later matches read from is itself the product of an earlier
        /// chunked copy.
        #[test]
        fn a_sequence_of_overlapping_matches_is_byte_identical() {
            let d = Lz4Decoder::new();
            let seed: Vec<u8> = (0..64u8).collect();
            let tail = [0x5Au8, 0xA5, 0x01, 0x02];
            let plan: Vec<(usize, usize)> = std::iter::once((8, 4000usize))
                .chain((0..40u32).map(|r| (1 + r as usize % 17, 1 + (r as usize * 137 % 9000) + 4)))
                .collect();
            let mut stream = Vec::new();
            for &(off, len) in &plan {
                stream.extend_from_slice(&sequence(&seed, off as u16, len));
            }
            stream.extend_from_slice(&literals_only(&tail));
            let got = d.decode_block(&stream).expect("stream");

            let mut want = Vec::new();
            for &(off, len) in &plan {
                reference(&seed, &mut want, off, len);
            }
            want.extend_from_slice(&tail);
            assert_eq!(
                got.len(),
                want.len(),
                "{} vs {} bytes",
                got.len(),
                want.len()
            );
            assert!(
                got == want,
                "{} bytes differ",
                got.iter().zip(&want).filter(|(a, b)| a != b).count()
            );
        }

        /// `lz4legacy_multiblock` is 40 MiB out of 178 KB, 81% of every
        /// byte the whole fixture corpus expands to, and it decodes on the
        /// overlapping branch. It ships without a committed plaintext --
        /// 40 MiB does not belong in a tree -- so the oracle here is the
        /// reference `lz4` CLI v1.10.0 (Yann Collet), which is where
        /// `testdata/lz4_README.md` gets every other vector from. The
        /// expected values are its output for this file, so a decoder that
        /// stopped early, ran on, or took the wrong branch cannot pass.
        #[test]
        fn the_largest_legacy_fixture_matches_the_reference_cli() {
            let frame = include_bytes!("../../testdata/lz4legacy_multiblock.lz4");
            let got = Lz4Decoder::new()
                .decode_auto(frame)
                .expect("legacy multiblock");
            assert_eq!(
                got.len(),
                41_943_040,
                "the reference lz4 1.10.0 CLI decompresses this fixture to 41943040 bytes"
            );
            // SHA-256 of the reference CLI's output, over slices chosen to
            // span the first block, a middle block and the last, so a copy
            // that drifts anywhere shows up. Whole-file hashing 40 MiB on
            // every test run is not worth the seconds it would cost.
            let mid = got.len() / 2;
            for (slice, want) in [
                (
                    &got[..65536],
                    "05721d3cb20e54029faf8f9bac0a2bb11d506723ce967626917d581ffdf3c3c2",
                ),
                (
                    &got[mid..mid + 4096],
                    "32daa20cb5f966c93119df1ffbc21f8747c67939d2bc9917008ae3957227d4ee",
                ),
                (
                    &got[got.len() - 65536..],
                    "109af08213aa96b3de3b4c2b792f5b46b04b154220c3d2c1ac830a371d434138",
                ),
            ] {
                assert_eq!(
                    sha256_hex(slice),
                    want,
                    "a slice of the decode does not match the reference lz4 CLI"
                );
            }
        }

        /// Small enough to keep in a test, and it goes through the crate's
        /// own sha256 rather than a dependency.
        fn sha256_hex(bytes: &[u8]) -> String {
            use rgx_core::sha256;
            let mut h = sha256::Sha256::new();
            h.update(bytes);
            h.finalize().iter().map(|b| format!("{b:02x}")).collect()
        }
    }
}
