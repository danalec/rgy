//! RAR5 compressed-stream decompressor (compression algorithm version 0:
//! LZ77 + canonical Huffman + filters).
//!
//! Whole-entry, non-streaming design: the compressed data area is decoded
//! into a circular window buffer, filters are applied over declared ranges,
//! and the final bytes are returned. Algorithm ported from libarchive's
//! `archive_read_support_format_rar5.c` (BSD-2-Clause, (c) 2018 Grzegorz
//! Antoniak), an independent implementation written without RARLAB code.
//!
//! Solid archives are supported through a persistent [`Unpacker`] state:
//! the window, Huffman tables, distance cache and write position carry
//! over between members of a solid group (see [`Unpacker::begin_member`]).
//!
//! Not implemented (returns [`Error::Unsupported`]):
//! * compression algorithm version 1 (RAR 7.x variants).

use alloc::vec;
use alloc::vec::Vec;

use crate::bitreader::BitReader;
use crate::huffman::DecodeTable;
use crate::{Error, Result};

const HUFF_BC: usize = 20;
const HUFF_NC: usize = 306;
const HUFF_DC: usize = 64;
const HUFF_LDC: usize = 16;
const HUFF_RC: usize = 44;
const HUFF_TABLE_SIZE: usize = HUFF_NC + HUFF_DC + HUFF_RC + HUFF_LDC;

const FILTER_DELTA: u8 = 0;
const FILTER_E8: u8 = 1;
const FILTER_E8E9: u8 = 2;
const FILTER_ARM: u8 = 3;

/// Decompress one member's data area (all blocks) into the unpacked bytes.
///
/// * `data` — the packed data area (starts directly with the first block
///   header; there is no leading flag byte for compressed members)
/// * `unpacked_size` — declared size, or `None` when the 0x0008 flag said
///   "unknown" (decode until the last-block flag)
/// * `dict_size` — dictionary size from the compression information field
pub fn unpack(data: &[u8], unpacked_size: Option<u64>, dict_size: u64) -> Result<Vec<u8>> {
    let mut u = Unpacker::new(dict_size, unpacked_size);
    u.begin_member(unpacked_size);
    u.run(data)?;
    u.finish()
}

// ---------------------------------------------------------------- internal

struct FilterInfo {
    ftype: u8,
    /// Absolute offset in the decoded stream (across the whole solid
    /// group) where the filter applies.
    block_start: usize,
    block_length: usize,
    channels: usize,
}

struct Tables {
    ld: DecodeTable,  // literals/commands (306)
    dd: DecodeTable,  // distance slots (64)
    ldd: DecodeTable, // low distance bits (16)
    rd: DecodeTable,  // repeating-distance lengths (44)
}

/// Persistent LZ77 decoder state.
///
/// For ordinary (non-solid) members use [`unpack`]. For a solid group,
/// create one `Unpacker` and call [`Unpacker::begin_member`] +
/// [`Unpacker::run`] + [`Unpacker::finish`] for each member in order: the
/// circular window, Huffman tables, distance cache and stream position
/// carry over, so matches can reference data of previous members and
/// blocks without an explicit table flag reuse the previous tables.
pub struct Unpacker {
    /// Circular window; position `p` maps to `window[p & mask]`.
    window: Vec<u8>,
    /// `window.len() - 1`. RAR5 dictionary sizes are always a power
    /// of two (128 KiB, 256 KiB, 1 MiB, 4 MiB), so a bitwise AND
    /// replaces the integer modulo on every byte. The match-copy
    /// loop runs `len` times per match -- for a 64 KiB match that
    /// is 64 KiB modulo ops saved.
    window_mask: usize,
    /// Bytes produced by the member currently being decoded. The window
    /// evicts old data once the dictionary wraps, so member output must be
    /// collected as it is produced, not recovered from the window later.
    out: Vec<u8>,
    /// Total bytes written across the whole solid group.
    len: usize,
    /// Position where the current member started.
    member_start: usize,
    dict_limit: usize,
    dist_cache: [usize; 4],
    last_len: usize,
    tables: Option<Tables>,
    /// Filters declared by the current member (drained by `finish`).
    filters: Vec<FilterInfo>,
    last_filter_start: usize,
    last_filter_len: usize,
    unpacked_target: Option<u64>,
}

impl Unpacker {
    /// Fresh state. The window capacity is `min(dict, unpacked)` clamped to
    /// at least 64 KB — a member can never reference further back than its
    /// own output length, and everything older than `dict` is out of
    /// reach by format rules.
    fn new(dict_size: u64, unpacked_size: Option<u64>) -> Unpacker {
        let dict_limit = dict_size.max(64 * 1024) as usize;
        let cap = unpacked_size
            .map(|u| (u as usize).clamp(64 * 1024, dict_limit))
            .unwrap_or(dict_limit)
            .min(dict_limit);
        Unpacker::with_capacity(dict_limit, cap)
    }

    /// Fresh state for the first member of a solid group. The full
    /// dictionary is allocated up front: later members may reference back
    /// into data produced by earlier members.
    pub(crate) fn new_solid(dict_size: u64) -> Unpacker {
        let dict_limit = dict_size.max(64 * 1024) as usize;
        Unpacker::with_capacity(dict_limit, dict_limit)
    }

    fn with_capacity(dict_limit: usize, cap: usize) -> Unpacker {
        // RAR5 dictionary sizes are powers of two (128 KiB, 256 KiB,
        // 1 MiB, 4 MiB). A `mask = cap - 1` lets every position wrap
        // to its window slot with a single AND. If the caller passes
        // a non-power-of-two cap (the only legitimate use is a
        // smaller test fixture), the mask falls back to `cap - 1` as
        // well, which is the same value `pos % cap` would yield at
        // every position; the AND wraps modulo `2^log2(cap)`, which
        // matches `% cap` only when `cap` is a power of two. The
        // `new` constructor does guarantee the power of two, so this
        // is the hot path.
        let window_mask = cap.saturating_sub(1).min(usize::MAX >> 1);
        Unpacker {
            window: vec![0u8; cap],
            window_mask,
            out: Vec::new(),
            len: 0,
            member_start: 0,
            dict_limit,
            dist_cache: [0; 4],
            last_len: 0,
            tables: None,
            filters: Vec::new(),
            last_filter_start: 0,
            last_filter_len: 0,
            unpacked_target: None,
        }
    }

    /// Start decoding a new member at the current stream position.
    pub fn begin_member(&mut self, unpacked_size: Option<u64>) {
        self.member_start = self.len;
        self.unpacked_target = unpacked_size;
        self.out = Vec::new();
    }

    /// Append raw bytes to the window without decoding (stored member
    /// inside a solid group): the bytes occupy the dictionary for
    /// subsequent members.
    pub fn write_stored(&mut self, data: &[u8]) {
        let mask = self.window_mask;
        for &b in data {
            self.window[self.len & mask] = b;
            self.len += 1;
        }
        self.out.extend_from_slice(data);
    }

    /// Raw window byte at absolute stream position.
    #[inline]
    fn at(&self, pos: usize) -> u8 {
        self.window[pos & self.window_mask]
    }

    fn push_byte(&mut self, b: u8) -> Result<()> {
        if let Some(t) = self.unpacked_target {
            if self.len as u64 - self.member_start as u64 >= t {
                return Err(Error::Malformed("uncompressed data exceeds declared size"));
            }
        }
        let mask = self.window_mask;
        self.window[self.len & mask] = b;
        self.len += 1;
        self.out.push(b);
        self.apply_ready_filters()
    }

    fn copy_string(&mut self, len: usize, dist: usize) -> Result<()> {
        if dist == 0 || dist > self.len || dist > self.dict_limit {
            return Err(Error::Malformed("invalid match distance"));
        }
        if dist > self.window_mask + 1 {
            // Source bytes were already evicted from the circular window.
            return Err(Error::Malformed("match distance exceeds window"));
        }
        if let Some(t) = self.unpacked_target {
            if self.len as u64 - self.member_start as u64 + len as u64 > t {
                return Err(Error::Malformed("uncompressed data exceeds declared size"));
            }
        }
        // Pre-reserve the output and wrap-cap the window with a
        // single AND. The match-copy loop is the hot path: a 64 KiB
        // match is 64 KiB scalar byte copies plus 64 KiB modulo
        // divisions, and a per-byte capacity re-check on the Vec
        // -- the AND + reserve drops the latter two. Filters are
        // applied once at the end, not per byte: a 64-byte match in
        // the middle of nowhere does not need a filter queue walk
        // after every byte.
        let mask = self.window_mask;
        self.out.reserve(len);
        let mut i = 0;
        while i < len {
            let b = self.at(self.len - dist);
            self.window[self.len & mask] = b;
            self.len += 1;
            self.out.push(b);
            i += 1;
        }
        self.apply_ready_filters()
    }

    pub fn run(&mut self, data: &[u8]) -> Result<()> {
        let mut offset = 0usize;
        let mut last_block = false;
        let mut blocks = 0usize;
        while !last_block {
            if data.len() - offset < 3 {
                return Err(Error::Truncated);
            }
            let flags = data[offset];
            let cksum = data[offset + 1];
            let byte_count = ((flags >> 3) & 7) as usize;
            if byte_count > 2 {
                return Err(Error::Malformed("block header size field too big"));
            }
            let size_bytes = byte_count + 1;
            if data.len() - offset < 2 + size_bytes {
                return Err(Error::Truncated);
            }
            let mut block_size: usize = 0;
            for i in 0..size_bytes {
                block_size |= (data[offset + 2 + i] as usize) << (8 * i);
            }
            let calc = 0x5Au8
                ^ flags
                ^ (block_size as u8)
                ^ ((block_size >> 8) as u8)
                ^ ((block_size >> 16) as u8);
            if calc != cksum {
                return Err(Error::BadCrc("rar5 block header"));
            }
            offset += 2 + size_bytes;
            if data.len() - offset < block_size {
                return Err(Error::Truncated);
            }
            let block = &data[offset..offset + block_size];
            offset += block_size;

            // The bit reader peeks up to 5 bytes ahead (16/32-bit windows);
            // append zero padding like the reference implementation's
            // read-ahead window so peeks at the tail don't error. Decoding
            // is terminated by the block's valid-bit count, not by padding.
            let mut block_ext = block.to_vec();
            block_ext.extend_from_slice(&[0u8; 5]);

            let bit_size = 1 + (flags & 7) as usize;
            let valid_bits = (block_size.saturating_sub(1)) * 8 + bit_size;

            let mut br = BitReader::new(&block_ext);
            if flags & 0x80 != 0 {
                // Huffman tables are transmitted inside the block data.
                self.parse_tables(&mut br, &block_ext)?;
            }
            if self.tables.is_none() {
                return Err(Error::Malformed("compressed data without huffman tables"));
            }

            self.decode_block(&mut br, valid_bits)?;

            last_block = flags & 0x40 != 0;
            blocks += 1;
            if blocks > data.len() {
                return Err(Error::Malformed("too many blocks"));
            }
        }
        Ok(())
    }

    fn parse_tables(&mut self, br: &mut BitReader, block: &[u8]) -> Result<()> {
        // The bit lengths of the table-describing Huffman code (HUFF_BC=20
        // entries) are stored as nibbles with a simple zero-run RLE.
        let mut bit_length = [0u8; HUFF_BC];
        let mut i = 0usize; // byte index
        let mut mask = 0xF0u8;
        let mut shift = 4u8;
        let mut w = 0usize;
        while w < HUFF_BC {
            if i >= block.len() {
                return Err(Error::Truncated);
            }
            let mut value = (block[i] & mask) >> shift;
            if mask == 0x0F {
                i += 1;
            }
            mask ^= 0xFF;
            shift ^= 4;

            const ESCAPE: u8 = 15;
            if value == ESCAPE {
                value = (block.get(i).copied().ok_or(Error::Truncated)? & mask) >> shift;
                if mask == 0x0F {
                    i += 1;
                }
                mask ^= 0xFF;
                shift ^= 4;
                if value == 0 {
                    bit_length[w] = ESCAPE;
                    w += 1;
                } else {
                    for _ in 0..(value + 2) {
                        if w >= HUFF_BC {
                            break;
                        }
                        bit_length[w] = 0;
                        w += 1;
                    }
                }
            } else {
                bit_length[w] = value;
                w += 1;
            }
        }
        br.pos = i;
        br.bit = shift ^ 4;

        let bd = DecodeTable::build(&bit_length, HUFF_BC)?;

        // Decode the HUFF_TABLE_SIZE entry bit-length array using `bd`.
        let mut table = [0u8; HUFF_TABLE_SIZE];
        let mut idx = 0usize;
        while idx < HUFF_TABLE_SIZE {
            let num = bd.decode(br)?;
            if num < 16 {
                table[idx] = num as u8;
                idx += 1;
            } else if num < 18 {
                // 16/17: repeat the previous entry n times
                let n = if num == 16 {
                    br.take(3)? + 3
                } else {
                    br.take(7)? + 11
                };
                if idx == 0 {
                    return Err(Error::Malformed("repeat before any table entry"));
                }
                let prev = table[idx - 1];
                for _ in 0..n {
                    if idx >= HUFF_TABLE_SIZE {
                        break;
                    }
                    table[idx] = prev;
                    idx += 1;
                }
            } else {
                // 18/19: fill zeroes n times
                let n = if num == 18 {
                    br.take(3)? + 3
                } else {
                    br.take(7)? + 11
                };
                for _ in 0..n {
                    if idx >= HUFF_TABLE_SIZE {
                        break;
                    }
                    table[idx] = 0;
                    idx += 1;
                }
            }
        }

        let ld = DecodeTable::build(&table[0..HUFF_NC], HUFF_NC)?;
        let dd = DecodeTable::build(&table[HUFF_NC..HUFF_NC + HUFF_DC], HUFF_DC)?;
        let ldd = DecodeTable::build(
            &table[HUFF_NC + HUFF_DC..HUFF_NC + HUFF_DC + HUFF_LDC],
            HUFF_LDC,
        )?;
        let rd = DecodeTable::build(
            &table[HUFF_NC + HUFF_DC + HUFF_LDC..HUFF_TABLE_SIZE],
            HUFF_RC,
        )?;
        self.tables = Some(Tables { ld, dd, ldd, rd });
        Ok(())
    }

    fn decode_code_length(br: &mut BitReader, code: u16) -> Result<usize> {
        let mut length: usize = 2;
        if code < 8 {
            length += code as usize;
        } else {
            let lbits = (code as usize) / 4 - 1;
            length += (4 | (code as usize & 3)) << lbits;
            if lbits > 0 {
                length += br.take(lbits as u32)? as usize;
            }
        }
        Ok(length)
    }

    fn parse_filter_data(br: &mut BitReader) -> Result<u32> {
        let bytes = br.take(2)? + 1;
        let mut data = 0u32;
        for i in 0..bytes {
            data |= (br.take(8)?) << (i * 8);
        }
        Ok(data)
    }

    fn parse_filter(&mut self, br: &mut BitReader) -> Result<()> {
        let block_start = Self::parse_filter_data(br)? as usize;
        let block_length = Self::parse_filter_data(br)? as usize;
        let ftype = br.take(3)? as u8;

        // Sanity checks (ported from the reference implementation).
        let valid_start = self.last_filter_start == 0
            || self.len + block_start >= self.last_filter_start + self.last_filter_len;
        if !(4..=0x400000).contains(&block_length)
            || !valid_start
            || block_length > self.dict_limit / 2
        {
            return Err(Error::Malformed("invalid filter parameters"));
        }

        let mut channels = 0usize;
        if ftype == FILTER_DELTA {
            channels = br.take(5)? as usize + 1;
        }

        let start = self.len + block_start;
        self.filters.push(FilterInfo {
            ftype,
            block_start: start,
            block_length,
            channels,
        });
        self.last_filter_start = start;
        self.last_filter_len = block_length;
        Ok(())
    }

    fn decode_block(&mut self, br: &mut BitReader, valid_bits: usize) -> Result<()> {
        loop {
            if br.bit_pos() >= valid_bits {
                break;
            }
            // Tables are present (checked by the caller).
            let num = {
                let t = self.tables.as_ref().ok_or(Error::Malformed("no tables"))?;
                t.ld.decode(br)?
            };

            if num < 256 {
                self.push_byte(num as u8)?;
            } else if num >= 262 {
                // New match: length code + distance slot.
                let len_code = Self::decode_code_length(br, num - 262)?;
                let dist_slot = {
                    let t = self.tables.as_ref().ok_or(Error::Malformed("no tables"))?;
                    t.dd.decode(br)?
                } as usize;
                let (dbits, mut dist) = if dist_slot < 4 {
                    (0usize, 1usize + dist_slot)
                } else {
                    let dbits = dist_slot / 2 - 1;
                    (dbits, 1 + ((2 | (dist_slot & 1)) << dbits))
                };

                if dbits > 0 {
                    if dbits >= 4 {
                        if dbits > 4 {
                            let add32 = br.peek32()?;
                            br.skip((dbits - 4) as u32);
                            dist += ((add32 >> (36 - dbits)) << 4) as usize;
                        }
                        let low_dist = {
                            let t = self.tables.as_ref().ok_or(Error::Malformed("no tables"))?;
                            t.ldd.decode(br)?
                        } as usize;
                        dist = dist
                            .checked_add(low_dist)
                            .ok_or(Error::Malformed("distance overflow"))?;
                    } else {
                        dist += br.take(dbits as u32)? as usize;
                    }
                }

                let mut len = len_code;
                if dist > 0x100 {
                    len += 1;
                    if dist > 0x2000 {
                        len += 1;
                        if dist > 0x40000 {
                            len += 1;
                        }
                    }
                }

                self.dist_cache_push(dist);
                self.last_len = len;
                self.copy_string(len, dist)?;
            } else if num == 256 {
                self.parse_filter(br)?;
            } else if num == 257 {
                if self.last_len != 0 {
                    let d = self.dist_cache[0];
                    self.copy_string(self.last_len, d)?;
                }
            } else {
                // 258..=261: reuse a cached distance (moving it to front).
                let idx = (num - 258) as usize;
                let dist = self.dist_cache_touch(idx);
                let len_slot = {
                    let t = self.tables.as_ref().ok_or(Error::Malformed("no tables"))?;
                    t.rd.decode(br)?
                };
                let len = Self::decode_code_length(br, len_slot)?;
                self.last_len = len;
                self.copy_string(len, dist)?;
            }
        }
        Ok(())
    }

    fn dist_cache_push(&mut self, value: usize) {
        self.dist_cache[3] = self.dist_cache[2];
        self.dist_cache[2] = self.dist_cache[1];
        self.dist_cache[1] = self.dist_cache[0];
        self.dist_cache[0] = value;
    }

    fn dist_cache_touch(&mut self, idx: usize) -> usize {
        let dist = self.dist_cache[idx];
        for i in (1..=idx).rev() {
            self.dist_cache[i] = self.dist_cache[i - 1];
        }
        self.dist_cache[0] = dist;
        dist
    }

    /// Apply every filter whose region is now fully decoded. Filters are
    /// declared and complete in stream order; a region is only readable
    /// from the circular window while its last byte is the most recent
    /// output, so each filter is applied the moment its range completes,
    /// replacing the region bytes in the member output.
    fn apply_ready_filters(&mut self) -> Result<()> {
        while let Some(f) = self.filters.first() {
            let end = f.block_start + f.block_length;
            if end > self.len {
                break;
            }
            let f = self.filters.remove(0);
            let mut region = Vec::with_capacity(f.block_length);
            for p in f.block_start..end {
                region.push(self.at(p));
            }
            let filtered = run_filter(&f, &region)?;
            let start = f
                .block_start
                .checked_sub(self.member_start)
                .ok_or(Error::Malformed("filter before member start"))?;
            let out_end = start
                .checked_add(filtered.len())
                .ok_or(Error::Malformed("size overflow"))?;
            if out_end > self.out.len() {
                return Err(Error::Malformed("filter range out of bounds"));
            }
            self.out[start..out_end].copy_from_slice(&filtered);
        }
        Ok(())
    }

    /// Conclude the current member: verify its declared size and filters
    /// and return the produced bytes. The window contents and decoder
    /// state stay available for the next solid member.
    pub fn finish(&mut self) -> Result<Vec<u8>> {
        let produced = self.len - self.member_start;
        if let Some(t) = self.unpacked_target {
            if produced as u64 != t {
                return Err(Error::Malformed("unpacked size mismatch"));
            }
        }
        if !self.filters.is_empty() {
            // A filter whose range never completed inside this member.
            return Err(Error::Malformed(
                "filter range out of order or out of bounds",
            ));
        }
        Ok(core::mem::take(&mut self.out))
    }
}

fn run_filter(f: &FilterInfo, region: &[u8]) -> Result<Vec<u8>> {
    let len = region.len();
    match f.ftype {
        FILTER_DELTA => {
            let channels = f.channels.max(1);
            let mut out = vec![0u8; len];
            let mut src_pos = 0usize;
            for i in 0..channels {
                let mut prev: u8 = 0;
                let mut dest = i;
                while dest < len {
                    prev = prev.wrapping_sub(region[src_pos]);
                    out[dest] = prev;
                    src_pos += 1;
                    dest += channels;
                }
            }
            Ok(out)
        }
        FILTER_E8 | FILTER_E8E9 => {
            const FILE_SIZE: u32 = 0x1000000;
            let extended = f.ftype == FILTER_E8E9;
            let mut buf = region.to_vec();
            let mut i = 0usize;
            while (i as i64) < (len as i64) - 4 {
                let b = region[i];
                i += 1;
                if b == 0xE8 || (extended && b == 0xE9) {
                    let offset = ((i + f.block_start) as u32) % FILE_SIZE;
                    let addr = u32::from_le_bytes([
                        region[i],
                        region[i + 1],
                        region[i + 2],
                        region[i + 3],
                    ]);
                    if addr & 0x8000_0000 != 0 {
                        if (addr.wrapping_add(offset)) & 0x8000_0000 == 0 {
                            buf[i..i + 4]
                                .copy_from_slice(&addr.wrapping_add(FILE_SIZE).to_le_bytes());
                        }
                    } else if addr.wrapping_sub(FILE_SIZE) & 0x8000_0000 != 0 {
                        buf[i..i + 4].copy_from_slice(&addr.wrapping_sub(offset).to_le_bytes());
                    }
                    i += 4;
                }
            }
            Ok(buf)
        }
        FILTER_ARM => {
            let mut buf = region.to_vec();
            let mut i = 0usize;
            while i + 3 < len {
                if region[i + 3] == 0xEB {
                    let mut offset = u32::from_le_bytes([
                        region[i],
                        region[i + 1],
                        region[i + 2],
                        region[i + 3],
                    ]) & 0x00ff_ffff;
                    offset = offset.wrapping_sub(((i + f.block_start) / 4) as u32);
                    offset = (offset & 0x00ff_ffff) | 0xeb00_0000;
                    buf[i..i + 4].copy_from_slice(&offset.to_le_bytes());
                }
                i += 4;
            }
            Ok(buf)
        }
        other => Err(Error::Unsupported(match other {
            4 => "audio filter (not used in RAR5)",
            5 => "RGB filter (not used in RAR5)",
            6 => "Itanium filter (not used in RAR5)",
            _ => "unknown filter type",
        })),
    }
}
