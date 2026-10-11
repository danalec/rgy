//! Carries the encoder half (`RangeEncoder` and friends), which rgx
//! never calls: it decodes lzma and does not produce it. Kept for the
//! same reason the archive and 7z trees keep their write paths.
#![allow(dead_code)]
//! LZMA (Lempel-Ziv-Markov chain Algorithm) compression.
//!
//! Standards-compliant LZMA1/LZMA2 implementation: range encoder and
//! decoder with context modeling, bit-tree and reverse bit-tree codes,
//! and the LZMA2 chunk layer used by xz and 7z. Validated against
//! liblzma-generated test vectors (see `testdata/lzma_*.bin`).
//! LZMA/LZMA2 decoder, vendored from upstream `b02/compress/lzma.rs`.
//!
//! Provenance: upstream at the time of the copy, verified against 12
//! liblzma-generated vectors that travel with it. Needed by the vendored
//! zip container for method 14. The `graph_optimize` import serves the
//! encoder's match finder, which is not vendored: the decoder half is
//! what rgx uses.

use crate::compress::cap::Bounded;
use crate::compress::error::{CompressError, CompressResult};
use alloc::vec;
use alloc::vec::Vec;
// Vendored trim: GraphMode served the encoder match finder (not vendored).

/// LZMA properties
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LzmaProps {
    pub lc: u8,
    pub lp: u8,
    pub pb: u8,
}

impl Default for LzmaProps {
    fn default() -> Self {
        Self {
            lc: 3,
            lp: 0,
            pb: 2,
        }
    }
}

impl LzmaProps {
    pub fn from_byte(byte: u8) -> Result<Self, CompressError> {
        if byte >= 9 * 5 * 5 {
            return Err(CompressError::InvalidFormat("LZMA properties out of range"));
        }
        Ok(Self {
            lc: byte % 9,
            lp: (byte / 9) % 5,
            pb: byte / 45,
        })
    }

    pub fn to_byte(self) -> u8 {
        self.lc + self.lp * 9 + self.pb * 45
    }
}

pub const LZMA_DICT_MIN: u32 = 1 << 12;
pub const LZMA_DICT_MAX: u32 = 1 << 27;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LzmaLevel(pub u8);

impl LzmaLevel {
    pub const FASTEST: Self = Self(0);
    pub const FAST: Self = Self(1);
    pub const NORMAL: Self = Self(3);
    pub const MAX: Self = Self(6);
    pub const ULTRA: Self = Self(9);
}

impl Default for LzmaLevel {
    fn default() -> Self {
        Self::NORMAL
    }
}

const NUM_STATES: usize = 12;
const NUM_POS_BITS_MAX: usize = 4;
const NUM_LEN_TO_POS_STATES: usize = 4;
const END_POS_MODEL_INDEX: usize = 14;
const NUM_FULL_DISTANCES: usize = 1 << (END_POS_MODEL_INDEX / 2);
const NUM_POS_SLOTS: usize = 1 << 6;
const ALIGN_BITS: usize = 4;
const ALIGN_TABLE_SIZE: usize = 1 << ALIGN_BITS;
const MATCH_MIN_LEN: usize = 2;

/// Probability array shared by the reverse bit trees of distance slots
/// 4..13, indexed by `(distance_base - slot)`. Size 115 covers the last
/// tree (slot 13 starts at index 83 and needs 32 probabilities).
const NUM_POS_SPECIAL: usize = NUM_FULL_DISTANCES - END_POS_MODEL_INDEX + 1; // 115

const PROB_INIT: u16 = 1024;
const PROB_BITS: u32 = 11;
const BIT_MODEL_TOTAL: u32 = 2048;
const MOVE_BITS: u32 = 5;
const TOP_VALUE: u32 = 1 << 24;

/// Range encoder
struct RangeEncoder {
    range: u64,
    low: u64,
    cache: u8,
    cache_size: u64,
    output: Vec<u8>,
}

impl RangeEncoder {
    fn new() -> Self {
        Self {
            range: 0xFFFFFFFF,
            low: 0,
            cache: 0,
            cache_size: 1,
            output: Vec::new(),
        }
    }

    fn normalize(&mut self) {
        if self.range < TOP_VALUE as u64 {
            self.range <<= 8;
            self.shift_low();
        }
    }

    fn shift_low(&mut self) {
        if self.low < 0xFF000000 || (self.low >> 32) != 0 {
            let carry = (self.low >> 32) as u8;
            let mut temp = self.cache;
            let mut n = self.cache_size;
            while n > 0 {
                self.output.push(temp.wrapping_add(carry));
                temp = 0xFF;
                n -= 1;
            }
            self.cache_size = 0;
            self.cache = (self.low >> 24) as u8;
        }
        self.cache_size += 1;
        // Keep only the low 32 bits: bits 24..31 were just moved to the
        // cache and any carry (bit 32+) was folded into the output.
        self.low = (self.low << 8) & 0xFFFFFFFF;
    }

    fn encode_bit(&mut self, prob: &mut u16, bit: u32) {
        let bound = (self.range >> PROB_BITS) * (*prob as u64);

        if bit == 0 {
            self.range = bound;
            *prob += ((BIT_MODEL_TOTAL - *prob as u32) >> MOVE_BITS) as u16;
        } else {
            self.low += bound;
            self.range -= bound;
            *prob -= *prob >> MOVE_BITS;
        }
        self.normalize();
    }

    /// MSB-first bit tree (used for lengths, pos slots and literals).
    fn encode_bit_tree(&mut self, probs: &mut [u16], value: u32, bits: u32) {
        let mut m = 1u32;
        for i in (0..bits).rev() {
            let bit = (value >> i) & 1;
            self.encode_bit(&mut probs[m as usize], bit);
            m = (m << 1) | bit;
        }
    }

    fn encode_direct(&mut self, value: u32, bits: u32) {
        for i in (0..bits).rev() {
            self.range >>= 1;
            self.low += self.range & (((value >> i) & 1) as u64 * 0xFFFFFFFFFFFFFFFF);
            self.normalize();
        }
    }

    /// LSB-first (reverse) bit tree (used for align bits and the shared
    /// distance-slot trees of slots 4..13).
    fn encode_reverse(&mut self, probs: &mut [u16], value: u32, bits: u32) {
        let mut m = 1;
        for i in 0..bits {
            let bit = (value >> i) & 1;
            self.encode_bit(&mut probs[m as usize], bit);
            m = (m << 1) | bit;
        }
    }

    fn finish(mut self) -> Vec<u8> {
        for _ in 0..5 {
            self.shift_low();
        }
        self.output
    }
}

/// Range decoder (LZMA1 raw stream: leading 0x00 byte + 4-byte init code).
struct RangeDecoder<'a> {
    input: &'a [u8],
    pos: usize,
    range: u32,
    code: u32,
}

impl<'a> RangeDecoder<'a> {
    fn new(input: &'a [u8]) -> CompressResult<Self> {
        if input.len() < 5 {
            return Err(CompressError::UnexpectedEof);
        }
        let code = u32::from_be_bytes([input[1], input[2], input[3], input[4]]);
        Ok(Self {
            input,
            pos: 5,
            range: u32::MAX,
            code,
        })
    }

    #[inline]
    fn next_byte(&mut self) -> u32 {
        let b = if self.pos < self.input.len() {
            self.input[self.pos]
        } else {
            0
        };
        self.pos += 1;
        b as u32
    }

    #[inline]
    fn normalize(&mut self) {
        if self.range < TOP_VALUE {
            let b = self.next_byte();
            self.code = (self.code << 8) | b;
            self.range <<= 8;
        }
    }

    #[inline]
    fn decode_bit(&mut self, prob: &mut u16) -> u32 {
        let bound = (self.range >> PROB_BITS) * (*prob as u32);
        if self.code < bound {
            self.range = bound;
            *prob += ((BIT_MODEL_TOTAL - *prob as u32) >> MOVE_BITS) as u16;
            self.normalize();
            0
        } else {
            self.range -= bound;
            self.code -= bound;
            *prob -= *prob >> MOVE_BITS;
            self.normalize();
            1
        }
    }

    #[inline]
    fn decode_direct(&mut self, bits: u32) -> u32 {
        let mut result = 0u32;
        for _ in 0..bits {
            self.range >>= 1;
            self.code = self.code.wrapping_sub(self.range);
            let t = 0u32.wrapping_sub(self.code >> 31);
            self.code = self.code.wrapping_add(self.range & t);
            result = (result << 1).wrapping_add(t.wrapping_add(1));
            self.normalize();
        }
        result
    }

    fn decode_bit_tree(&mut self, probs: &mut [u16], bits: u32) -> u32 {
        let mut m = 1u32;
        for _ in 0..bits {
            let b = self.decode_bit(&mut probs[m as usize]);
            m = (m << 1) | b;
        }
        m - (1 << bits)
    }

    fn decode_bit_tree_reverse(&mut self, probs: &mut [u16], bits: u32) -> u32 {
        let mut m = 1u32;
        let mut result = 0u32;
        for i in 0..bits {
            let b = self.decode_bit(&mut probs[m as usize]);
            m = (m << 1) | b;
            result |= b << i;
        }
        result
    }
}

/// LZMA state machine
#[derive(Debug, Clone, Copy)]
struct LzmaState {
    state: usize,
    rep0: u32,
    rep1: u32,
    rep2: u32,
    rep3: u32,
}

impl LzmaState {
    fn new() -> Self {
        Self {
            state: 0,
            rep0: 0,
            rep1: 0,
            rep2: 0,
            rep3: 0,
        }
    }

    fn update_literal(&mut self) {
        if self.state < 4 {
            self.state = 0;
        } else if self.state < 10 {
            self.state -= 3;
        } else {
            self.state -= 6;
        }
    }

    fn update_match(&mut self) {
        self.state = if self.state < 7 { 7 } else { 10 };
    }

    fn update_rep(&mut self) {
        self.state = if self.state < 7 { 8 } else { 11 };
    }

    fn update_short_rep(&mut self) {
        self.state = if self.state < 7 { 9 } else { 11 };
    }

    fn is_char(&self) -> bool {
        self.state < 7
    }

    fn update_reps(&mut self, dist: u32) {
        self.rep3 = self.rep2;
        self.rep2 = self.rep1;
        self.rep1 = self.rep0;
        self.rep0 = dist;
    }
}

/// Length coder models (shared shape between encoder and decoder)
#[derive(Clone)]
struct LenModels {
    choice: u16,
    choice2: u16,
    low: [[u16; 8]; 16],
    mid: [[u16; 8]; 16],
    high: [u16; 256],
}

impl LenModels {
    fn new() -> Self {
        Self {
            choice: PROB_INIT,
            choice2: PROB_INIT,
            low: [[PROB_INIT; 8]; 16],
            mid: [[PROB_INIT; 8]; 16],
            high: [PROB_INIT; 256],
        }
    }

    fn encode(&mut self, rc: &mut RangeEncoder, pos_state: usize, len: usize) {
        if len < 8 {
            rc.encode_bit(&mut self.choice, 0);
            rc.encode_bit_tree(&mut self.low[pos_state][..], len as u32, 3);
        } else if len < 16 {
            rc.encode_bit(&mut self.choice, 1);
            rc.encode_bit(&mut self.choice2, 0);
            rc.encode_bit_tree(&mut self.mid[pos_state][..], (len - 8) as u32, 3);
        } else {
            rc.encode_bit(&mut self.choice, 1);
            rc.encode_bit(&mut self.choice2, 1);
            rc.encode_bit_tree(&mut self.high[..], (len - 16) as u32, 8);
        }
    }

    fn decode(&mut self, rc: &mut RangeDecoder, pos_state: usize) -> CompressResult<usize> {
        if rc.decode_bit(&mut self.choice) == 0 {
            Ok(rc.decode_bit_tree(&mut self.low[pos_state][..], 3) as usize)
        } else if rc.decode_bit(&mut self.choice2) == 0 {
            Ok(8 + rc.decode_bit_tree(&mut self.mid[pos_state][..], 3) as usize)
        } else {
            Ok(16 + rc.decode_bit_tree(&mut self.high[..], 8) as usize)
        }
    }
}

/// Full probability model set + state, shared by LZMA1 and LZMA2 decode.
struct LzmaCore {
    st: LzmaState,
    is_match: [[u16; NUM_POS_BITS_MAX + 1]; NUM_STATES],
    is_rep: [u16; NUM_STATES],
    is_rep0: [u16; NUM_STATES],
    is_rep1: [u16; NUM_STATES],
    is_rep2: [u16; NUM_STATES],
    is_rep0_long: [[u16; NUM_POS_BITS_MAX + 1]; NUM_STATES],
    rep_len: LenModels,
    match_len: LenModels,
    literal: Vec<[u16; 0x300]>,
    pos_slot: [[u16; NUM_POS_SLOTS]; NUM_LEN_TO_POS_STATES],
    pos_special: [u16; NUM_POS_SPECIAL],
    align: [u16; ALIGN_TABLE_SIZE],
}

impl LzmaCore {
    fn new(props: LzmaProps) -> Self {
        Self {
            st: LzmaState::new(),
            is_match: [[PROB_INIT; NUM_POS_BITS_MAX + 1]; NUM_STATES],
            is_rep: [PROB_INIT; NUM_STATES],
            is_rep0: [PROB_INIT; NUM_STATES],
            is_rep1: [PROB_INIT; NUM_STATES],
            is_rep2: [PROB_INIT; NUM_STATES],
            is_rep0_long: [[PROB_INIT; NUM_POS_BITS_MAX + 1]; NUM_STATES],
            rep_len: LenModels::new(),
            match_len: LenModels::new(),
            literal: vec![[PROB_INIT; 0x300]; 1 << (props.lc + props.lp)],
            pos_slot: [[PROB_INIT; NUM_POS_SLOTS]; NUM_LEN_TO_POS_STATES],
            pos_special: [PROB_INIT; NUM_POS_SPECIAL],
            align: [PROB_INIT; ALIGN_TABLE_SIZE],
        }
    }

    /// Reset state machine, reps and all probabilities (LZMA2 state reset).
    fn reset_state(&mut self) {
        let lit_len = self.literal.len();
        self.st = LzmaState::new();
        self.is_match = [[PROB_INIT; NUM_POS_BITS_MAX + 1]; NUM_STATES];
        self.is_rep = [PROB_INIT; NUM_STATES];
        self.is_rep0 = [PROB_INIT; NUM_STATES];
        self.is_rep1 = [PROB_INIT; NUM_STATES];
        self.is_rep2 = [PROB_INIT; NUM_STATES];
        self.is_rep0_long = [[PROB_INIT; NUM_POS_BITS_MAX + 1]; NUM_STATES];
        self.rep_len = LenModels::new();
        self.match_len = LenModels::new();
        self.literal = vec![[PROB_INIT; 0x300]; lit_len];
        self.pos_slot = [[PROB_INIT; NUM_POS_SLOTS]; NUM_LEN_TO_POS_STATES];
        self.pos_special = [PROB_INIT; NUM_POS_SPECIAL];
        self.align = [PROB_INIT; ALIGN_TABLE_SIZE];
    }

    fn pos_mask(props: &LzmaProps) -> usize {
        (1 << props.pb) - 1
    }

    fn lit_state(props: &LzmaProps, out: &[u8], pos_abs: usize, dict_base: usize) -> usize {
        let lc = props.lc as usize;
        let lp = props.lp as usize;
        let pos_rel = pos_abs.saturating_sub(dict_base);
        ((pos_rel & ((1 << lp) - 1)) << lc)
            + (if pos_abs > dict_base {
                out[pos_abs - 1] as usize >> (8 - lc)
            } else {
                0
            })
    }

    /// Decode symbols until `unpack` bytes are produced.
    /// `dict_base` is the index in `out` where the current dictionary
    /// window starts (LZMA2 dictionary resets).
    ///
    /// Returns `true` if the end-of-stream marker was hit.
    /// Decode one chunk. `unpack` is the target the caller is aiming at and
    /// `hard_cap` a second, lower ceiling that is a stopping point rather
    /// than an error: a match that would cross it ends the chunk cleanly, so
    /// a bomb stops where the cap is instead of failing as corrupt.
    fn decode_chunk(
        &mut self,
        rc: &mut RangeDecoder,
        props: &LzmaProps,
        out: &mut Vec<u8>,
        unpack: usize,
        dict_base: usize,
        hard_cap: usize,
    ) -> CompressResult<bool> {
        let pos_mask = Self::pos_mask(props);

        let target = out.len().saturating_add(unpack).min(hard_cap);
        while out.len() < target {
            // LZMA2's position counter resets with the dictionary, so the
            // state index is the offset since `dict_base`, not the absolute
            // output length.
            let pos_state = (out.len() - dict_base) & pos_mask;

            if rc.decode_bit(&mut self.is_match[self.st.state][pos_state]) == 0 {
                // Literal
                let probs = &mut self.literal[Self::lit_state(props, out, out.len(), dict_base)];
                let mut symbol = 1usize;
                if self.st.is_char() {
                    while symbol < 0x100 {
                        let b = rc.decode_bit(&mut probs[symbol]) as usize;
                        symbol = (symbol << 1) | b;
                    }
                } else {
                    let dist = self.st.rep0 as usize;
                    let mb_pos =
                        out.len()
                            .checked_sub(dist + 1)
                            .ok_or(CompressError::InvalidFormat(
                                "LZMA rep distance out of range",
                            ))?;
                    if mb_pos < dict_base {
                        return Err(CompressError::InvalidFormat(
                            "LZMA rep distance before dictionary start",
                        ));
                    }
                    let mut match_byte = out[mb_pos] as usize;
                    let mut offset: usize = 0x100;
                    while symbol < 0x100 {
                        match_byte <<= 1;
                        let match_bit = match_byte & offset;
                        let b = rc.decode_bit(&mut probs[offset + match_bit + symbol]) as usize;
                        symbol = (symbol << 1) | b;
                        offset &= !(match_bit ^ (b * 0x100));
                    }
                }
                out.push((symbol - 0x100) as u8);
                self.st.update_literal();
                continue;
            }

            // Match or rep
            let len;
            if rc.decode_bit(&mut self.is_rep[self.st.state]) != 0 {
                // Rep match
                if rc.decode_bit(&mut self.is_rep0[self.st.state]) == 0 {
                    if rc.decode_bit(&mut self.is_rep0_long[self.st.state][pos_state]) == 0 {
                        // Short rep: copy 1 byte
                        self.st.update_short_rep();
                        let dist = self.st.rep0 as usize;
                        let src = out
                            .len()
                            .checked_sub(dist + 1)
                            .ok_or(CompressError::InvalidFormat("LZMA rep0 out of range"))?;
                        if src < dict_base {
                            return Err(CompressError::InvalidFormat(
                                "LZMA rep0 before dictionary start",
                            ));
                        }
                        let b = out[src];
                        out.push(b);
                        continue;
                    }
                } else {
                    let dist;
                    if rc.decode_bit(&mut self.is_rep1[self.st.state]) == 0 {
                        dist = self.st.rep1;
                    } else if rc.decode_bit(&mut self.is_rep2[self.st.state]) == 0 {
                        dist = self.st.rep2;
                        self.st.rep2 = self.st.rep1;
                    } else {
                        dist = self.st.rep3;
                        self.st.rep3 = self.st.rep2;
                        self.st.rep2 = self.st.rep1;
                    }
                    self.st.rep1 = self.st.rep0;
                    self.st.rep0 = dist;
                }
                len = self.rep_len.decode(rc, pos_state)? + MATCH_MIN_LEN;
                self.st.update_rep();
            } else {
                // Simple match
                self.st.rep3 = self.st.rep2;
                self.st.rep2 = self.st.rep1;
                self.st.rep1 = self.st.rep0;
                len = self.match_len.decode(rc, pos_state)? + MATCH_MIN_LEN;
                self.st.update_match();

                let len_state = (len - MATCH_MIN_LEN).min(NUM_LEN_TO_POS_STATES - 1);
                let slot = rc.decode_bit_tree(&mut self.pos_slot[len_state][..], 6) as usize;

                let mut dist;
                if slot < 4 {
                    dist = slot as u32;
                } else {
                    let num_direct = (slot >> 1) - 1;
                    dist = (2 | (slot & 1) as u32) << num_direct;
                    if slot < END_POS_MODEL_INDEX {
                        let start = (dist - slot as u32) as usize;
                        dist += rc.decode_bit_tree_reverse(
                            &mut self.pos_special[start..],
                            num_direct as u32,
                        );
                    } else {
                        dist += rc.decode_direct((num_direct - ALIGN_BITS) as u32) << ALIGN_BITS;
                        dist += rc.decode_bit_tree_reverse(&mut self.align[..], ALIGN_BITS as u32);
                    }
                }

                if dist == u32::MAX {
                    // End-of-stream marker
                    return Ok(true);
                }
                self.st.rep0 = dist;
            }

            // Copy match. LZMA distances are zero-based: the source is
            // `dist + 1` bytes back.
            let dist = self.st.rep0 as usize;
            if dist + 1 > out.len() - dict_base {
                return Err(CompressError::InvalidFormat(
                    "LZMA match distance out of range",
                ));
            }
            if out.len() + len > target {
                if target != hard_cap {
                    return Err(CompressError::InvalidFormat("LZMA chunk size mismatch"));
                }
                // The cap is what bounds this chunk, not the container.
                // Emit the part of the match that fits and stop: refusing
                // it whole would hand the caller a decode that had made no
                // progress at all, and LZMA1 has no chunk boundary to stop
                // at, so there would be nothing to stop on either.
                let room = target - out.len();
                let start = out.len() - dist - 1;
                for i in 0..room {
                    let b = out[start + i];
                    out.push(b);
                }
                return Ok(false);
            }
            let start = out.len() - dist - 1;
            for i in 0..len {
                let b = out[start + i];
                out.push(b);
            }
        }
        Ok(false)
    }
}
// Vendored trim: the encoder half (LzmaEncoder, the range encoder and the
// Paley-graph match finder it drives) is not vendored — rgx decodes LZMA
// for zip method 14 and nothing else.
pub struct LzmaDecoder;

impl LzmaDecoder {
    pub fn new() -> Self {
        Self
    }

    /// Decode a legacy `.lzma` (FORMAT_ALONE) stream: 1-byte props, 4-byte
    /// little-endian dictionary size, 8-byte little-endian uncompressed size
    /// (0xFFFF..FFFF = unknown).
    pub fn decode(&self, input: &[u8]) -> CompressResult<Vec<u8>> {
        self.decode_bounded(input, super::cap::UNLIMITED)
            .map(|b| b.bytes)
    }

    /// Decode a legacy `.lzma` stream without expanding it past `limit`
    /// bytes. FORMAT_ALONE's header carries the uncompressed size in bytes
    /// 5..13, so a stream that declares more than the limit is refused
    /// before a single byte is decoded. That field is all-ones when the
    /// encoder did not know the size, and then the ceiling has to be the
    /// running one.
    pub fn decode_bounded(&self, input: &[u8], limit: u64) -> CompressResult<Bounded> {
        if input.len() < 13 {
            return Err(CompressError::UnexpectedEof);
        }

        let props = LzmaProps::from_byte(input[0])?;
        let dict_size = u32::from_le_bytes([input[1], input[2], input[3], input[4]]);
        let size_raw = u64::from_le_bytes([
            input[5], input[6], input[7], input[8], input[9], input[10], input[11], input[12],
        ]);
        let unpack_size = if size_raw == u64::MAX {
            None
        } else {
            Some(size_raw)
        };

        // Empty payload is valid when the uncompressed size is zero.
        if unpack_size == Some(0) && input.len() == 13 {
            return Ok(Bounded::complete(Vec::new(), Some(0), limit));
        }
        if let Some(n) = unpack_size {
            if n > limit {
                return Ok(Bounded::stopped(Vec::new(), Some(n), limit));
            }
        }

        self.decode_raw_bounded(&input[13..], props, dict_size, unpack_size, limit)
    }

    /// Decode a raw LZMA1 stream (no header), as stored inside 7z archives.
    pub fn decode_raw(
        &self,
        input: &[u8],
        props: LzmaProps,
        dict_size: u32,
        unpack_size: Option<u64>,
    ) -> CompressResult<Vec<u8>> {
        self.decode_raw_bounded(input, props, dict_size, unpack_size, super::cap::UNLIMITED)
            .map(|b| b.bytes)
    }

    /// [`Self::decode_raw`] under a ceiling on the output.
    pub fn decode_raw_bounded(
        &self,
        input: &[u8],
        props: LzmaProps,
        dict_size: u32,
        unpack_size: Option<u64>,
        limit: u64,
    ) -> CompressResult<Bounded> {
        let mut rc = RangeDecoder::new(input)?;
        let mut core = LzmaCore::new(props);
        let mut out: Vec<u8> = Vec::with_capacity(unpack_size.unwrap_or(0).min(1 << 26) as usize);
        let mut cap = super::cap::OutputCap::new(limit);
        let mut counted = 0u64;

        let unpack = match unpack_size {
            Some(n) => n as usize,
            None => usize::MAX,
        };
        loop {
            let remaining = unpack.saturating_sub(out.len());
            if remaining == 0 {
                break;
            }
            // A *minimum* per call, never a maximum: `decode_chunk`
            // refuses a match crossing its target, and LZMA1 has no
            // chunk structure, so capping the target made every match
            // straddling 16 MiB a hard error. See upstream 7db078d.
            // The target depends on whether the size is known, and
            // getting it wrong fails silently in both directions.
            //
            // Unknown size: the only terminator is the end-of-stream
            // marker, and there is nothing else to aim at. `decode_chunk`
            // refuses a match crossing its target and LZMA1 has no chunk
            // structure, so any finite target makes every match straddling
            // it a hard error. `remaining` is already usize::MAX here.
            //
            // Known size: the stream usually carries no end marker at all,
            // because the container already said how many bytes to expect.
            // Aiming past that overshoots the payload and runs off the end
            // of the input -- "unexpected end of input" on a file every
            // other tool reads. This is what 7-Zip emits for its LZMA1
            // coder, and the 16 MiB floor broke every such archive.
            let chunk = match unpack_size {
                Some(_) => remaining,
                None => usize::MAX,
            };
            let ended = core.decode_chunk(
                &mut rc,
                &props,
                &mut out,
                chunk,
                0,
                cap.remaining().min(usize::MAX as u64) as usize,
            )?;
            let emitted = out.len() as u64;
            if emitted > counted {
                cap.take(emitted - counted);
                counted = emitted;
            }
            if ended || out.len() >= unpack {
                break;
            }
            // `take` only refuses past the limit, so reaching it exactly is
            // the cap too: without this the chunk loop would spin on a
            // target it has already hit. Checked after the stream's own end
            // so a file that is exactly the cap long is not called short.
            if cap.capped() || emitted >= limit {
                return Ok(Bounded::stopped(out, unpack_size, limit));
            }
            if chunk == 0 {
                break;
            }
        }
        if let Some(n) = unpack_size {
            if out.len() != n as usize {
                return Err(CompressError::UnexpectedEof);
            }
        }
        let _ = dict_size;
        Ok(Bounded::complete(out, unpack_size, limit))
    }

    /// Decode a raw LZMA2 chunk stream (as used by xz and 7z).
    pub fn decode_lzma2(&self, input: &[u8], dict_size: u32) -> CompressResult<Vec<u8>> {
        self.decode_lzma2_bounded(input, dict_size, super::cap::UNLIMITED)
            .map(|b| b.bytes)
    }

    /// [`Self::decode_lzma2`] under a ceiling on the output. Every LZMA2
    /// chunk declares its own uncompressed size in the control byte and the
    /// two size fields, so the cap is checked against that declaration
    /// before the chunk is decoded, and again against what it produced.
    pub fn decode_lzma2_bounded(
        &self,
        mut input: &[u8],
        dict_size: u32,
        limit: u64,
    ) -> CompressResult<Bounded> {
        let mut out: Vec<u8> = Vec::new();
        let mut cap = super::cap::OutputCap::new(limit);
        let mut props: Option<LzmaProps> = None;
        let mut core: Option<LzmaCore> = None;
        let mut need_dict_reset = true;
        // After an uncompressed chunk the LZMA state is invalid until a
        // chunk resets it (control >= 0xA0).
        let mut state_valid = false;
        // liblzma's `need_props` (lzma2_decoder.c): a dictionary reset
        // requires properties, and only a chunk that carries them clears
        // the requirement. A control byte >= 0xC0 does both at once, so
        // the only reset that leaves the flag standing is 0x01 (reset
        // with a single uncompressed byte and no properties). The next
        // LZMA chunk then has to set new properties, because the state
        // that goes with a new dictionary is the state that goes with new
        // properties. Without the check a 0xA0 chunk straight after a 0x01
        // reset is accepted here and rejected everywhere else.
        let mut need_props = false;
        let mut dict_base = 0usize;

        loop {
            let Some(&control) = input.first() else {
                return Err(CompressError::UnexpectedEof);
            };
            input = &input[1..];

            if control == 0x00 {
                break;
            }

            if control >= 0xE0 || control == 0x01 {
                // Dictionary reset
                need_dict_reset = false;
                need_props = true;
                dict_base = out.len();
            } else if need_dict_reset {
                return Err(CompressError::InvalidFormat(
                    "LZMA2 stream missing dictionary reset",
                ));
            }

            if control >= 0x80 {
                // LZMA chunk: [control][uncompressed size (5 bits in the
                // control byte + 2 BE bytes)][packed size u16 BE], then the
                // payload, then the new properties byte when control >= 0xC0.
                if input.len() < 4 {
                    return Err(CompressError::UnexpectedEof);
                }
                let unpack = ((((control & 0x1F) as usize) << 16)
                    | u16::from_be_bytes([input[0], input[1]]) as usize)
                    + 1;
                let packed = u16::from_be_bytes([input[2], input[3]]) as usize + 1;
                input = &input[4..];

                // The chunk states how many bytes it will produce, so a
                // chunk that cannot fit is refused from the statement
                // rather than decoded to find out.
                if !cap.fits(unpack as u64) {
                    return Ok(Bounded::stopped(out, None, limit));
                }

                if need_props && control < 0xC0 {
                    return Err(CompressError::InvalidFormat(
                        "LZMA2 chunk after a dictionary reset must set new properties",
                    ));
                }
                if control >= 0xC0 {
                    // New properties byte follows the size fields
                    let Some(&b) = input.first() else {
                        return Err(CompressError::UnexpectedEof);
                    };
                    input = &input[1..];
                    let p = LzmaProps::from_byte(b)?;
                    props = Some(p);
                    core = Some(LzmaCore::new(p));
                    state_valid = true;
                    need_props = false;
                } else if !state_valid {
                    // State must be reset after uncompressed chunks.
                    if control < 0xA0 {
                        return Err(CompressError::InvalidFormat(
                            "LZMA2 chunk missing state reset",
                        ));
                    }
                    let p = props.ok_or(CompressError::InvalidFormat(
                        "LZMA2 chunk without properties",
                    ))?;
                    core = Some(LzmaCore::new(p));
                    state_valid = true;
                } else if control >= 0xA0 {
                    // State reset (keeps props)
                    if let Some(c) = core.as_mut() {
                        c.reset_state();
                    }
                }
                let c = core.as_mut().unwrap();

                if input.len() < packed {
                    return Err(CompressError::UnexpectedEof);
                }
                let chunk = &input[..packed];
                input = &input[packed..];

                let mut rc = RangeDecoder::new(chunk)?;
                let before = out.len() as u64;
                c.decode_chunk(
                    &mut rc,
                    &props.unwrap(),
                    &mut out,
                    unpack,
                    dict_base,
                    cap.remaining().min(usize::MAX as u64) as usize,
                )?;
                let emitted = out.len() as u64;
                if emitted > before && !cap.take(emitted - before) {
                    return Ok(Bounded::stopped(out, None, limit));
                }
            } else if control <= 0x02 {
                // Uncompressed chunk (0x01 also resets the dictionary)
                if input.len() < 2 {
                    return Err(CompressError::UnexpectedEof);
                }
                let size = u16::from_be_bytes([input[0], input[1]]) as usize + 1;
                input = &input[2..];
                if input.len() < size {
                    return Err(CompressError::UnexpectedEof);
                }
                if !cap.take(size as u64) {
                    return Ok(Bounded::stopped(out, None, limit));
                }
                out.extend_from_slice(&input[..size]);
                input = &input[size..];
                state_valid = false;
            } else {
                return Err(CompressError::InvalidFormat("invalid LZMA2 control byte"));
            }
        }

        let _ = dict_size;
        Ok(Bounded::complete(out, None, limit))
    }
}

impl Default for LzmaDecoder {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_props_roundtrip() {
        let props = LzmaProps::default();
        assert_eq!(props, LzmaProps::from_byte(props.to_byte()).unwrap());
    }

    /// Vectors produced by CPython's lzma module (liblzma), FORMAT_ALONE.
    #[test]
    fn test_liblzma_vectors_alone() {
        let decoder = LzmaDecoder::new();
        let cases: &[(&str, &[u8], &[u8])] = &[
            (
                "empty",
                include_bytes!("../../testdata/lzma_alone_empty.bin"),
                include_bytes!("../../testdata/lzma_alone_empty.expected"),
            ),
            (
                "rep_a",
                include_bytes!("../../testdata/lzma_alone_rep_a.bin"),
                include_bytes!("../../testdata/lzma_alone_rep_a.expected"),
            ),
            (
                "text",
                include_bytes!("../../testdata/lzma_alone_text.bin"),
                include_bytes!("../../testdata/lzma_alone_text.expected"),
            ),
            (
                "random",
                include_bytes!("../../testdata/lzma_alone_random.bin"),
                include_bytes!("../../testdata/lzma_alone_random.expected"),
            ),
            (
                "longrange",
                include_bytes!("../../testdata/lzma_alone_longrange.bin"),
                include_bytes!("../../testdata/lzma_alone_longrange.expected"),
            ),
            (
                "shortdist",
                include_bytes!("../../testdata/lzma_alone_shortdist.bin"),
                include_bytes!("../../testdata/lzma_alone_shortdist.expected"),
            ),
        ];
        for (name, compressed, expected) in cases {
            let out = decoder
                .decode(compressed)
                .unwrap_or_else(|e| panic!("{name}: {e}"));
            assert_eq!(&out, expected, "liblzma ALONE vector: {name}");
        }
    }

    /// Vectors produced by CPython's lzma module (liblzma), raw LZMA2.
    ///
    /// Each `.expected` is the plaintext of the matching `.bin` (an earlier
    /// revision pointed these at the ALONE twins, whose payloads differ —
    /// that mismatch, not the decoder, is what the old ignore covered).
    #[test]
    fn test_liblzma_vectors_lzma2() {
        let decoder = LzmaDecoder::new();
        let cases: &[(&str, &[u8], &[u8])] = &[
            (
                "empty",
                include_bytes!("../../testdata/lzma2_empty.bin"),
                include_bytes!("../../testdata/lzma2_empty.expected"),
            ),
            (
                "rep_a",
                include_bytes!("../../testdata/lzma2_rep_a.bin"),
                include_bytes!("../../testdata/lzma2_rep_a.expected"),
            ),
            (
                "text",
                include_bytes!("../../testdata/lzma2_text.bin"),
                include_bytes!("../../testdata/lzma2_text.expected"),
            ),
            (
                "random",
                include_bytes!("../../testdata/lzma2_random.bin"),
                include_bytes!("../../testdata/lzma2_random.expected"),
            ),
            (
                "longrange",
                include_bytes!("../../testdata/lzma2_longrange.bin"),
                include_bytes!("../../testdata/lzma2_longrange.expected"),
            ),
            (
                "shortdist",
                include_bytes!("../../testdata/lzma2_shortdist.bin"),
                include_bytes!("../../testdata/lzma2_shortdist.expected"),
            ),
        ];
        for (name, compressed, expected) in cases {
            let out = decoder
                .decode_lzma2(compressed, 1 << 26)
                .unwrap_or_else(|e| panic!("{name}: {e}"));
            assert_eq!(&out, expected, "liblzma LZMA2 vector: {name}");
        }
    }

    /// A stream that really does span several LZMA2 chunks.
    ///
    /// liblzma caps a chunk's uncompressed size at 2 MiB, so a stream this
    /// size is split, and every chunk after the first continues the
    /// probability model, the match state and the dictionary. The first
    /// chunk also carries part of its size in the control byte's high five
    /// bits, which is the field a two-byte read drops — that is what this
    /// vector pins.
    ///
    /// Only the compressed stream is stored: the plaintext is a fixed
    /// sequence of five phrases chosen by a linear congruential
    /// generator, and the test rebuilds it rather than committing three
    /// megabytes of fixture.
    #[test]
    fn lzma2_chunked_stream_vector() {
        const PHRASES: [&[u8]; 5] = [
            b"the quick brown fox jumps over the lazy dog. ",
            b"pack my box with five dozen liquor jugs. ",
            b"how vexingly quick daft zebras jump! ",
            b"sphinx of black quartz, judge my vow. ",
            b"the five boxing wizards jump quickly. ",
        ];
        let mut expected: Vec<u8> = Vec::with_capacity(3_000_000);
        let mut x: u32 = 12345;
        while expected.len() < 3_000_000 {
            x = x.wrapping_mul(1103515245).wrapping_add(12345);
            expected.extend_from_slice(PHRASES[(x >> 16) as usize % PHRASES.len()]);
        }
        expected.truncate(3_000_000);

        let out = LzmaDecoder::new()
            .decode_lzma2(include_bytes!("../../testdata/lzma2_chunked.bin"), 1 << 26)
            .expect("liblzma chunked LZMA2 stream");
        assert_eq!(out.len(), expected.len());
        assert_eq!(&out[..], &expected[..]);
    }

    /// An ALONE stream larger than 16 MiB, which is where the decoder
    /// used to fail.
    ///
    /// LZMA1 has no chunk structure, so a match crossing 16 MiB is legal
    /// and was an error anyway: `decode_raw` capped each call's target
    /// at 16 MiB and `decode_chunk` refuses a match crossing its target.
    /// Only the compressed stream is stored; the plaintext is rebuilt
    /// from the same generator the chunked vector uses.
    #[test]
    fn alone_stream_over_sixteen_mib() {
        const PHRASES: [&[u8]; 5] = [
            b"the quick brown fox jumps over the lazy dog. ",
            b"pack my box with five dozen liquor jugs. ",
            b"how vexingly quick daft zebras jump! ",
            b"sphinx of black quartz, judge my vow. ",
            b"the five boxing wizards jump quickly. ",
        ];
        let mut expected: Vec<u8> = Vec::with_capacity(17_000_000);
        let mut x: u32 = 12345;
        while expected.len() < 17_000_000 {
            x = x.wrapping_mul(1103515245).wrapping_add(12345);
            expected.extend_from_slice(PHRASES[(x >> 16) as usize % PHRASES.len()]);
        }
        expected.truncate(17_000_000);

        let out = LzmaDecoder::new()
            .decode(include_bytes!("../../testdata/lzma_alone_over16m.lzma"))
            .expect("ALONE stream over 16 MiB");
        assert_eq!(out.len(), expected.len());
        assert_eq!(&out[..], &expected[..]);
    }

    /// FORMAT_ALONE states the uncompressed size in bytes 5..13, so a
    /// stream that declares more than the cap is refused before a single
    /// byte of it is decoded -- which is the whole point of the field.
    ///
    /// liblzma writes all-ones there when the size is not known ahead of
    /// the stream (every vector in `lzma_alone_*` is one), so the field is
    /// written in here over a real liblzma payload: the claim under test is
    /// that the decoder reads and obeys the declaration, not that it can
    /// encode one.
    #[test]
    fn decode_bounded_refuses_from_the_declared_size() {
        let mut compressed = include_bytes!("../../testdata/lzma_alone_rep_a.bin").to_vec();
        let expected = include_bytes!("../../testdata/lzma_alone_rep_a.expected");
        compressed[5..13].copy_from_slice(&(expected.len() as u64).to_le_bytes());

        let b = LzmaDecoder::new()
            .decode_bounded(&compressed, 1000)
            .expect("bounded");
        assert!(b.capped);
        assert_eq!(b.declared, Some(expected.len() as u64));
        assert!(
            b.bytes.is_empty(),
            "refused early means nothing was decoded"
        );

        let whole = LzmaDecoder::new()
            .decode_bounded(&compressed, expected.len() as u64)
            .expect("exact");
        assert!(!whole.capped);
        assert_eq!(whole.bytes, expected);
    }

    /// A stream whose size is not known has no declaration to refuse from,
    /// so the ceiling is the running one. The bytes it did produce are the
    /// real ones, up to the cap.
    #[test]
    fn decode_bounded_stops_a_stream_that_declares_no_size() {
        let compressed = include_bytes!("../../testdata/lzma_alone_rep_a.bin");
        let expected = include_bytes!("../../testdata/lzma_alone_rep_a.expected");
        assert_eq!(
            u64::from_le_bytes(compressed[5..13].try_into().unwrap()),
            u64::MAX,
            "this vector is only the unknown-size case if liblzma left it unset"
        );
        let b = LzmaDecoder::new()
            .decode_bounded(compressed, 1000)
            .expect("bounded");
        assert!(
            b.capped,
            "a {} byte stream under a 1000 byte cap",
            expected.len()
        );
        assert_eq!(b.declared, None, "nothing was declared");
        assert_eq!(b.bytes, &expected[..1000], "not the real prefix");
    }
}
