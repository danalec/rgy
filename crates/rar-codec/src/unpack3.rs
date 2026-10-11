//! RAR 2.9/3.x/4.x compressed-stream decompressor (LZ77 + Huffman tables
//! with the standard E8/E8E9, delta, RGB and audio VM filters).
//!
//! The packed data area of one file member is decoded into a circular LZSS
//! window; the decoder stops when the declared unpacked size is reached or
//! the stream reports end-of-file (symbol 256 + "new file" bits). Algorithm
//! ported from libarchive's `archive_read_support_format_rar.c` (BSD-2-Clause),
//! an independent implementation written without RARLAB code, with the
//! solid-member state semantics (Init/UnpInitData/InitFilters30) verified
//! against the UnRAR source for reference.
//!
//! Solid archives share one [`Unpacker3`] across the members of a group:
//! the LZ window, distance cache (`oldoffset`, `lastoffset`, `lastlength`),
//! Huffman tables and compiled filter programs persist, while the pending
//! filter stack, the bit reader and the per-member output reset for every
//! member. Filter blocks must lie entirely inside the member that declares
//! them (matches UnRAR: "Filters never share several solid files").
//!
//! Not implemented (returns [`Error::Unsupported`]):
//! * PPMd filters applied inside a PPMd block,
//! * arbitrary RAR VM program filters (the five standard filters
//!   fingerprinted by libarchive are supported).
//!
//! PPMd var-H blocks (the table-header flag bit) are decoded by the
//! companion [`crate::ppmd7`] module.

use alloc::vec;
use alloc::vec::Vec;

use crate::crc32::crc32;
use crate::{Error, Result};

const MAINCODE_SIZE: usize = 299;
const OFFSETCODE_SIZE: usize = 60;
const LOWOFFSETCODE_SIZE: usize = 17;
const LENGTHCODE_SIZE: usize = 28;
const HUFFMAN_TABLE_SIZE: usize =
    MAINCODE_SIZE + OFFSETCODE_SIZE + LOWOFFSETCODE_SIZE + LENGTHCODE_SIZE;
const MAX_SYMBOL_LENGTH: u8 = 15;
const MAX_SYMBOLS: usize = 20;
/// Largest window libarchive allocates (and the largest rar3 dictionary).
const DICTIONARY_MAX_SIZE: usize = 0x40000 * 16;
const PROGRAM_WORK_SIZE: u32 = 0x3C000;

const LENGTHBASES: [u32; LENGTHCODE_SIZE] = [
    0, 1, 2, 3, 4, 5, 6, 7, 8, 10, 12, 14, 16, 20, 24, 28, 32, 40, 48, 56, 64, 80, 96, 112, 128,
    160, 192, 224,
];
const LENGTHBITS: [u32; LENGTHCODE_SIZE] = [
    0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5,
];
const OFFSETBASES: [u32; OFFSETCODE_SIZE] = [
    0, 1, 2, 3, 4, 6, 8, 12, 16, 24, 32, 48, 64, 96, 128, 192, 256, 384, 512, 768, 1024, 1536,
    2048, 3072, 4096, 6144, 8192, 12288, 16384, 24576, 32768, 49152, 65536, 98304, 131072, 196608,
    262144, 327680, 393216, 458752, 524288, 589824, 655360, 720896, 786432, 851968, 917504, 983040,
    1048576, 1310720, 1572864, 1835008, 2097152, 2359296, 2621440, 2883584, 3145728, 3407872,
    3670016, 3932160,
];
const OFFSETBITS: [u32; OFFSETCODE_SIZE] = [
    0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13,
    13, 14, 14, 15, 15, 16, 16, 16, 16, 16, 16, 16, 16, 16, 16, 16, 16, 16, 16, 18, 18, 18, 18, 18,
    18, 18, 18, 18, 18, 18, 18,
];
const SHORTBASES: [u32; 8] = [0, 4, 8, 16, 32, 64, 128, 192];
const SHORTBITS: [u32; 8] = [2, 2, 3, 4, 5, 6, 6, 6];

// Standard filter fingerprints: CRC-32 of the bytecode | length << 32.
const FP_DELTA: u64 = 0x1D0E06077D;
const FP_E8: u64 = 0x35AD576887;
const FP_E8E9: u64 = 0x393CD7E57E;
const FP_RGB: u64 = 0x951C2C5DC8;
const FP_AUDIO: u64 = 0xD8BC85E701;

/// Decompress one RAR4 file member (methods 0x31..=0x35).
///
/// * `data` — the packed data area
/// * `unp_size` — declared unpacked size
pub fn unpack(data: &[u8], unp_size: u64) -> Result<Vec<u8>> {
    if unp_size == 0 {
        return Ok(Vec::new());
    }
    // Window: smallest power of two greater than the unpacked size,
    // capped at 4 MB (libarchive sizes the window from the unpacked
    // size; a match can never reference further back than the current
    // position, which is always smaller than the window).
    let window_size = if unp_size >= DICTIONARY_MAX_SIZE as u64 {
        DICTIONARY_MAX_SIZE
    } else {
        1usize << (64 - (unp_size | 1).leading_zeros())
    };
    let mut d = Unpacker3::new(window_size);
    d.run_member(data, unp_size)
}

// ---------------------------------------------------------------- bit reader

/// MSB-first bit reader over the packed data. Unconsumed bits live in the
/// low `avail` bits of `cache`; the next bit to read is at position
/// `avail - 1` (same layout as libarchive's `rar_br`).
pub(crate) struct BitReader3<'a> {
    data: &'a [u8],
    next: usize,
    cache: u64,
    avail: u32,
}

impl<'a> BitReader3<'a> {
    pub(crate) fn new(data: &'a [u8]) -> Self {
        BitReader3 {
            data,
            next: 0,
            cache: 0,
            avail: 0,
        }
    }

    fn fill_to(&mut self, n: u32) -> Result<()> {
        while self.avail < n {
            let b = self.data.get(self.next).ok_or(Error::Truncated)?;
            self.cache = (self.cache << 8) | u64::from(*b);
            self.next += 1;
            self.avail += 8;
        }
        Ok(())
    }

    /// Read and consume `n` bits (1..=32).
    pub(crate) fn take(&mut self, n: u32) -> Result<u32> {
        if n == 0 {
            return Ok(0);
        }
        self.fill_to(n)?;
        self.avail -= n;
        Ok(((self.cache >> self.avail) & ((1u64 << n) - 1)) as u32)
    }

    /// Drop bits up to the next byte boundary of the original stream.
    fn align_byte(&mut self) {
        self.avail -= self.avail % 8;
    }
}

// ---------------------------------------------------------------- huffman

/// Binary prefix-code tree. Node 0 is the root; a fresh node is `[-1, -2]`,
/// a leaf has both branches equal to the symbol value.
struct HuffmanCode {
    tree: Vec<[i32; 2]>,
}

impl HuffmanCode {
    /// Build a tree from bit lengths, assigning canonical codes in symbol
    /// order (libarchive's `create_code` / `add_value`).
    fn create(lengths: &[u8], numsymbols: usize, maxlength: u8) -> Result<HuffmanCode> {
        let mut code = HuffmanCode {
            tree: vec![[-1, -2]],
        };
        let mut codebits: u32 = 0;
        let mut symbolsleft = numsymbols as i32;
        for i in 1..=maxlength {
            for (j, &l) in lengths.iter().enumerate().take(numsymbols) {
                if l != i {
                    continue;
                }
                code.add_value(j as u32, codebits, i)?;
                codebits += 1;
                symbolsleft -= 1;
                if symbolsleft <= 0 {
                    break;
                }
            }
            if symbolsleft <= 0 {
                break;
            }
            codebits <<= 1;
        }
        Ok(code)
    }

    fn add_value(&mut self, value: u32, codebits: u32, length: u8) -> Result<()> {
        let mut lastnode = 0usize;
        for bitpos in (0..length).rev() {
            let bit = ((codebits >> bitpos) & 1) as usize;
            // Leaf node check.
            if self.tree[lastnode][0] == self.tree[lastnode][1] {
                return Err(Error::Malformed("prefix code: prefix found"));
            }
            if self.tree[lastnode][bit] < 0 {
                let idx = self.tree.len() as i32;
                self.tree[lastnode][bit] = idx;
                self.tree.push([-1, -2]);
            }
            lastnode = self.tree[lastnode][bit] as usize;
        }
        if self.tree[lastnode] != [-1, -2] {
            return Err(Error::Malformed("prefix code: prefix found"));
        }
        self.tree[lastnode] = [value as i32, value as i32];
        Ok(())
    }

    /// Decode one symbol by walking the tree bit by bit.
    fn decode(&self, br: &mut BitReader3) -> Result<u32> {
        let mut node = 0usize;
        loop {
            let (b0, b1) = (self.tree[node][0], self.tree[node][1]);
            if b0 == b1 {
                if b0 < 0 {
                    // Empty tree (no symbols): any input is invalid.
                    return Err(Error::Malformed("invalid prefix code in bitstream"));
                }
                return Ok(b0 as u32);
            }
            let bit = br.take(1)? as usize;
            let next = self.tree[node][bit];
            if next < 0 {
                return Err(Error::Malformed("invalid prefix code in bitstream"));
            }
            node = next as usize;
        }
    }
}

// ---------------------------------------------------------------- filters

/// One compiled filter program (bytecode identified by fingerprint).
struct Program {
    fingerprint: u64,
    usagecount: u32,
    oldfilterlength: u32,
}

/// A filter instance waiting for its block to be decoded. `blockstartpos`
/// is a global window position (member base + declared relative offset).
struct Filter {
    prog: usize,
    initialregisters: [u32; 8],
    blockstartpos: usize,
    blocklength: u32,
}

struct FilterState {
    progs: Vec<Program>,
    stack: Vec<Filter>,
    lastfilternum: u32,
}

/// MSB-first bit reader over a filter's bytecode block (libarchive's
/// `memory_bit_reader`). Overreads return zero bits and set `at_eof`.
struct MemBitReader<'a> {
    bytes: &'a [u8],
    offset: usize,
    bits: u64,
    available: u32,
    at_eof: bool,
}

impl<'a> MemBitReader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        MemBitReader {
            bytes,
            offset: 0,
            bits: 0,
            available: 0,
            at_eof: false,
        }
    }

    fn fill(&mut self, n: u32) -> bool {
        while self.available < n && self.offset < self.bytes.len() {
            self.bits = (self.bits << 8) | u64::from(self.bytes[self.offset]);
            self.offset += 1;
            self.available += 8;
        }
        if n > self.available {
            self.at_eof = true;
            return false;
        }
        true
    }

    fn bits(&mut self, n: u32) -> u32 {
        if n > self.available && (self.at_eof || !self.fill(n)) {
            return 0;
        }
        self.available -= n;
        ((self.bits >> self.available) & ((1u64 << n) - 1)) as u32
    }

    /// Variable-length integer used by the filter protocol.
    fn next_number(&mut self) -> u32 {
        match self.bits(2) {
            0 => self.bits(4),
            1 => {
                let val = self.bits(8);
                if val >= 16 {
                    val
                } else {
                    0xFFFFFF00 | (val << 4) | self.bits(4)
                }
            }
            2 => self.bits(16),
            _ => self.bits(32),
        }
    }
}

// ---------------------------------------------------------------- decoder

struct Tables {
    maincode: HuffmanCode,
    offsetcode: HuffmanCode,
    lowoffsetcode: HuffmanCode,
    lengthcode: HuffmanCode,
}

/// Stateful RAR3/4 unpacker. One instance is shared by all members of a
/// solid group: window, distance cache, Huffman tables and compiled filter
/// programs persist across `run_member` calls, while the bit reader, the
/// pending filter stack and the output reset for every member (matching
/// UnRAR's UnpInitData/InitFilters30 with Solid=true).
pub struct Unpacker3 {
    /// Circular LZSS window (power of two).
    window: Vec<u8>,
    mask: usize,
    /// Decoded output of the member currently being processed; filter
    /// results replace their ranges in place.
    out: Vec<u8>,
    /// Global window position of the first byte of the current member.
    member_base: usize,
    /// Declared unpacked size of the current member.
    cur_unp_size: u64,
    tables: Option<Tables>,
    /// Whether the current table set may be reused by the next solid
    /// member (UnRAR's TablesRead3).
    tables_read: bool,
    /// Bit lengths persist across `parse_codes` calls when a table set is
    /// kept (within a member and across solid members).
    lengthtable: [u8; HUFFMAN_TABLE_SIZE],
    lastlength: u32,
    lastoffset: u32,
    oldoffset: [u32; 4],
    lastlowoffset: u32,
    numlowoffsetrepeats: u32,
    filters: FilterState,
    /// PPMd model of the current PPMd block (valid within one member; the
    /// reference implementations tear it down at every file header).
    ppmd: Option<crate::ppmd7::Ppmd7>,
    /// The packed stream is in a PPMd block (set by `parse_codes`).
    is_ppmd_block: bool,
    /// Escape symbol for the current PPMd block (2 unless the block says
    /// otherwise); also the literal emitted by the "default" escape code.
    ppmd_escape: u32,
    /// Set when the PPMd stream signalled end-of-data (code 2).
    ppmd_eod: bool,
}

impl Unpacker3 {
    /// Create an unpacker with a `window_size`-byte circular window
    /// (must be a power of two, at least 4).
    pub fn new(window_size: usize) -> Unpacker3 {
        debug_assert!(window_size.is_power_of_two());
        Unpacker3 {
            window: vec![0u8; window_size],
            mask: window_size - 1,
            out: Vec::new(),
            member_base: 0,
            cur_unp_size: 0,
            tables: None,
            tables_read: false,
            lengthtable: [0; HUFFMAN_TABLE_SIZE],
            lastlength: 0,
            lastoffset: 0,
            oldoffset: [0; 4],
            lastlowoffset: 0,
            numlowoffsetrepeats: 0,
            filters: FilterState {
                progs: Vec::new(),
                stack: Vec::new(),
                lastfilternum: 0,
            },
            ppmd: None,
            is_ppmd_block: false,
            ppmd_escape: 2,
            ppmd_eod: false,
        }
    }

    /// Decode one member's packed data (`data`) of declared unpacked size
    /// `unp_size`, returning the member's decoded bytes.
    ///
    /// `self.out` accumulates the whole group so far (matches may reference
    /// any earlier member through the shared window); the returned bytes are
    /// this member's range of it.
    pub fn run_member(&mut self, data: &[u8], unp_size: u64) -> Result<Vec<u8>> {
        self.member_base = self.out.len();
        self.cur_unp_size = unp_size;
        // Pending filters never share solid members (UnRAR's InitFilters30
        // clears the program stack, but keeps compiled programs and their
        // block lengths).
        self.filters.stack.clear();
        // The PPMd model never survives a file header either (libarchive
        // frees it while parsing the header; unrar re-inits per file).
        self.ppmd = None;
        self.is_ppmd_block = false;
        self.ppmd_eod = false;

        if unp_size == 0 {
            return Ok(Vec::new());
        }

        let mut br = BitReader3::new(data);
        // First member of a group (or a non-solid member) always carries
        // tables; later solid members reuse them unless the previous
        // member ended with a "new table" announcement.
        if !self.tables_read {
            self.parse_codes(&mut br)?;
        }
        self.decode_loop(&mut br)?;
        self.finish_member()
    }

    /// Store a member without compression (method 0x30): bytes go straight
    /// into the window so later solid members can reference them.
    pub fn write_stored(&mut self, data: &[u8]) -> Result<()> {
        self.member_base = self.out.len();
        self.cur_unp_size = data.len() as u64;
        let base = self.member_base;
        for (i, &b) in data.iter().enumerate() {
            self.window[(base + i) & self.mask] = b;
        }
        self.out.extend_from_slice(data);
        Ok(())
    }

    /// Current global window position (next byte written).
    fn wpos(&self) -> usize {
        self.out.len()
    }

    fn member_written(&self) -> u64 {
        (self.out.len() - self.member_base) as u64
    }

    fn decode_loop(&mut self, br: &mut BitReader3) -> Result<()> {
        loop {
            if self.member_written() >= self.cur_unp_size {
                break;
            }
            if self.is_ppmd_block {
                self.ppmd_step(br)?;
                if self.ppmd_eod {
                    // The model is dead: the next member must carry a fresh
                    // table/PPMd definition.
                    self.tables_read = false;
                    break;
                }
                self.apply_ready_filters()?;
                continue;
            }
            let symbol = match self.decode_symbol(br) {
                Ok(s) => s,
                // A member may simply run out of bits once its declared
                // size is reached (UnRAR stops on input exhaustion too).
                Err(Error::Truncated) if self.member_written() == self.cur_unp_size => break,
                Err(e) => return Err(e),
            };
            match symbol {
                0..=255 => self.emit_literal(symbol as u8)?,
                256 => {
                    // End-of-block marker: "1" = new tables follow in this
                    // member; "00" = end of file, keep tables; "01" = end of
                    // file and the next member starts with fresh tables.
                    let new_table = br.take(1)? == 1;
                    if new_table {
                        self.parse_codes(br)?;
                    } else {
                        let next_has_tables = br.take(1)? == 1;
                        self.tables_read = !next_has_tables;
                        break;
                    }
                }
                257 => self.read_filter(br)?,
                258 => {
                    if self.lastlength != 0 {
                        self.emit_match(self.lastoffset, self.lastlength)?;
                    }
                }
                259..=262 => {
                    let offsindex = (symbol - 259) as usize;
                    let offs = self.oldoffset[offsindex];
                    let lensymbol = {
                        let t = self.tables.as_ref().ok_or(Error::Malformed("no tables"))?;
                        t.lengthcode.decode(br)?
                    } as usize;
                    if lensymbol >= LENGTHCODE_SIZE {
                        return Err(Error::Malformed("bad length symbol"));
                    }
                    let mut len = LENGTHBASES[lensymbol] + 2;
                    len += br.take(LENGTHBITS[lensymbol])?;
                    for i in (1..=offsindex).rev() {
                        self.oldoffset[i] = self.oldoffset[i - 1];
                    }
                    self.oldoffset[0] = offs;
                    self.lastoffset = offs;
                    self.lastlength = len;
                    self.emit_match(offs, len)?;
                }
                263..=270 => {
                    let s = (symbol - 263) as usize;
                    let mut offs = SHORTBASES[s] + 1;
                    offs += br.take(SHORTBITS[s])?;
                    for i in (1..4).rev() {
                        self.oldoffset[i] = self.oldoffset[i - 1];
                    }
                    self.oldoffset[0] = offs;
                    self.lastoffset = offs;
                    self.lastlength = 2;
                    self.emit_match(offs, 2)?;
                }
                _ => {
                    // 271..=298: new match.
                    let s = (symbol - 271) as usize;
                    if s >= LENGTHCODE_SIZE {
                        return Err(Error::Malformed("bad match symbol"));
                    }
                    let mut len = LENGTHBASES[s] + 3;
                    len += br.take(LENGTHBITS[s])?;
                    let offssymbol = {
                        let t = self.tables.as_ref().ok_or(Error::Malformed("no tables"))?;
                        t.offsetcode.decode(br)?
                    } as usize;
                    if offssymbol >= OFFSETCODE_SIZE {
                        return Err(Error::Malformed("bad offset symbol"));
                    }
                    let mut offs = OFFSETBASES[offssymbol] + 1;
                    if OFFSETBITS[offssymbol] > 0 {
                        if offssymbol > 9 {
                            if OFFSETBITS[offssymbol] > 4 {
                                offs += br.take(OFFSETBITS[offssymbol] - 4)? << 4;
                            }
                            if self.numlowoffsetrepeats > 0 {
                                self.numlowoffsetrepeats -= 1;
                                offs += self.lastlowoffset;
                            } else {
                                let lowsymbol = {
                                    let t = self
                                        .tables
                                        .as_ref()
                                        .ok_or(Error::Malformed("no tables"))?;
                                    t.lowoffsetcode.decode(br)?
                                };
                                if lowsymbol == 16 {
                                    self.numlowoffsetrepeats = 15;
                                    offs += self.lastlowoffset;
                                } else {
                                    offs += lowsymbol;
                                    self.lastlowoffset = lowsymbol;
                                }
                            }
                        } else {
                            offs += br.take(OFFSETBITS[offssymbol])?;
                        }
                    }
                    if offs >= 0x40000 {
                        len += 1;
                    }
                    if offs >= 0x2000 {
                        len += 1;
                    }
                    for i in (1..4).rev() {
                        self.oldoffset[i] = self.oldoffset[i - 1];
                    }
                    self.oldoffset[0] = offs;
                    self.lastoffset = offs;
                    self.lastlength = len;
                    self.emit_match(offs, len)?;
                }
            }
            self.apply_ready_filters()?;
        }
        Ok(())
    }

    /// One iteration of the PPMd block decode (libarchive's read_data_compressed
    /// `is_ppmd_block` branch). Escape codes: 0 = new table set follows,
    /// 2 = end of PPMd data, 3 = filters (unsupported), 4 = LZ match with
    /// 3-byte offset + 1-byte length, 5 = RLE match (offset 1), other =
    /// the escape byte itself is a literal.
    fn ppmd_step(&mut self, br: &mut BitReader3) -> Result<()> {
        let sym = {
            let m = self
                .ppmd
                .as_mut()
                .ok_or(Error::Malformed("PPMd block without model"))?;
            m.decode_symbol(br)?
        };
        if sym < 0 {
            return Err(Error::Malformed("invalid PPMd symbol"));
        }
        if sym as u32 != self.ppmd_escape {
            self.emit_literal(sym as u8)?;
            return Ok(());
        }
        let code = {
            let m = self
                .ppmd
                .as_mut()
                .ok_or(Error::Malformed("PPMd block without model"))?;
            m.decode_symbol(br)?
        };
        if code < 0 {
            return Err(Error::Malformed("invalid PPMd escape"));
        }
        match code {
            0 => {
                // New table set (either a fresh PPMd block or a switch back
                // to LZ+Huffman tables) follows in this member.
                self.parse_codes(br)?;
            }
            2 => {
                self.ppmd_eod = true;
            }
            3 => return Err(Error::Unsupported("PPMd filters")),
            4 => {
                let mut offset = 0u32;
                for i in (0..3).rev() {
                    let b = {
                        let m = self
                            .ppmd
                            .as_mut()
                            .ok_or(Error::Malformed("PPMd block without model"))?;
                        m.decode_symbol(br)?
                    };
                    if b < 0 {
                        return Err(Error::Malformed("invalid PPMd match offset"));
                    }
                    offset |= (b as u32) << (i * 8);
                }
                let length = {
                    let m = self
                        .ppmd
                        .as_mut()
                        .ok_or(Error::Malformed("PPMd block without model"))?;
                    m.decode_symbol(br)?
                };
                if length < 0 {
                    return Err(Error::Malformed("invalid PPMd match length"));
                }
                self.emit_match(offset + 2, length as u32 + 32)?;
            }
            5 => {
                let length = {
                    let m = self
                        .ppmd
                        .as_mut()
                        .ok_or(Error::Malformed("PPMd block without model"))?;
                    m.decode_symbol(br)?
                };
                if length < 0 {
                    return Err(Error::Malformed("invalid PPMd match length"));
                }
                self.emit_match(1, length as u32 + 4)?;
            }
            _ => {
                self.emit_literal(self.ppmd_escape as u8)?;
            }
        }
        Ok(())
    }

    fn decode_symbol(&self, br: &mut BitReader3) -> Result<u32> {
        let t = self.tables.as_ref().ok_or(Error::Malformed("no tables"))?;
        t.maincode.decode(br)
    }

    fn finish_member(&mut self) -> Result<Vec<u8>> {
        // Any filter whose block completes exactly at the end of the member.
        self.apply_ready_filters()?;
        if let Some(f) = self.filters.stack.first() {
            let end = f
                .blockstartpos
                .checked_add(f.blocklength as usize)
                .ok_or(Error::Malformed("filter block overflow"))?;
            let member_end = self.member_base + self.cur_unp_size as usize;
            if f.blockstartpos < self.member_base || end > member_end {
                return Err(Error::Malformed("filter block spans solid members"));
            }
            return Err(Error::Malformed("filter block never completed"));
        }
        if self.member_written() != self.cur_unp_size {
            return Err(Error::Malformed("member size mismatch"));
        }
        self.tables_read = true;
        Ok(self.out[self.member_base..].to_vec())
    }

    fn emit_literal(&mut self, b: u8) -> Result<()> {
        if self.member_written() >= self.cur_unp_size + 260 {
            // Defensive: the stream is producing far beyond the declared
            // size, i.e. it is corrupt (CRC will also fail upstream).
            return Err(Error::Malformed("output exceeds declared size"));
        }
        let pos = self.wpos();
        self.window[pos & self.mask] = b;
        self.out.push(b);
        Ok(())
    }

    fn emit_match(&mut self, offset: u32, length: u32) -> Result<()> {
        if offset == 0 {
            return Err(Error::Malformed("zero match distance"));
        }
        // Hoist the bounds check to once per match. The previous
        // shape paid `member_written() + 1` checks per byte inside
        // the match; a 64 KiB match is 64 KiB redundant tests. The
        // 260-byte slack matches `emit_literal`'s allowed overrun:
        // a 0-byte literal can legitimately emit past `cur_unp_size`
        // by a small constant, and a match is a sequence of literals.
        let written = self.member_written();
        if written + length as u64 > self.cur_unp_size + 260 {
            return Err(Error::Malformed("output exceeds declared size"));
        }
        // Pre-reserve and copy match bytes directly. The pre-reserve
        // turns a 64 KiB push-loop into a 64 KiB memcpy after the
        // first allocation; without it, every byte pays a capacity
        // re-check.
        self.out.reserve(length as usize);
        let mask = self.mask;
        for _ in 0..length {
            // Wrapping mirrors the reference: corrupt distances read stale
            // window bytes instead of aborting (the CRC catches them).
            let (src, dst) = {
                let wpos = self.wpos();
                (wpos.wrapping_sub(offset as usize) & mask, wpos & mask)
            };
            let b = self.window[src];
            self.window[dst] = b;
            self.out.push(b);
        }
        Ok(())
    }

    /// Read a new set of Huffman tables (libarchive's `parse_codes`).
    fn parse_codes(&mut self, br: &mut BitReader3) -> Result<()> {
        br.align_byte();

        // PPMd block flag.
        if br.take(1)? != 0 {
            let flags = br.take(7)?;
            // Read order is fixed by the format: dictionary byte (if 0x20)
            // comes before the escape byte (if 0x40).
            let dict_size = if flags & 0x20 != 0 {
                (br.take(8)? + 1) << 20
            } else {
                0
            };
            if flags & 0x40 != 0 {
                self.ppmd_escape = br.take(8)?;
            } else {
                self.ppmd_escape = 2;
            }
            if flags & 0x20 != 0 {
                let mut maxorder = (flags & 0x1F) + 1;
                if maxorder > 16 {
                    maxorder = 16 + (maxorder - 16) * 3;
                }
                if maxorder == 1 {
                    return Err(Error::Malformed("bad PPMd max order"));
                }
                let esc = self.ppmd_escape;
                let mut model = crate::ppmd7::Ppmd7::new(dict_size)?;
                model.init_range_rar(br)?;
                model.init_model(maxorder, esc);
                self.ppmd = Some(model);
            } else {
                match self.ppmd.as_mut() {
                    Some(m) => {
                        let esc = self.ppmd_escape;
                        m.set_init_esc(esc);
                        m.init_range_rar(br)?;
                    }
                    None => {
                        return Err(Error::Malformed(
                            "PPMd block without a preceding model reset",
                        ))
                    }
                }
            }
            self.is_ppmd_block = true;
            return Ok(());
        }

        self.is_ppmd_block = false;

        // Low-distance repeat state belongs to the current table set.
        self.lastlowoffset = 0;
        self.numlowoffsetrepeats = 0;

        // Keep-existing-table flag: without it the lengths start from zero.
        if br.take(1)? == 0 {
            self.lengthtable = [0; HUFFMAN_TABLE_SIZE];
        }

        let mut bitlengths = [0u8; MAX_SYMBOLS];
        let mut i = 0usize;
        while i < MAX_SYMBOLS {
            let v = br.take(4)? as u8;
            i += 1;
            if v == 0xF {
                let zerocount = br.take(4)?;
                if zerocount != 0 {
                    i -= 1;
                    for _ in 0..zerocount + 2 {
                        if i >= MAX_SYMBOLS {
                            break;
                        }
                        bitlengths[i] = 0;
                        i += 1;
                    }
                } else {
                    bitlengths[i - 1] = v;
                }
            } else {
                bitlengths[i - 1] = v;
            }
        }
        let precode = HuffmanCode::create(&bitlengths, MAX_SYMBOLS, MAX_SYMBOL_LENGTH)?;

        let mut idx = 0usize;
        while idx < HUFFMAN_TABLE_SIZE {
            let val = precode.decode(br)?;
            if val < 16 {
                // Table lengths accumulate modulo 16 across table sets.
                self.lengthtable[idx] = (self.lengthtable[idx] + val as u8) & 0xF;
                idx += 1;
            } else if val < 18 {
                if idx == 0 {
                    return Err(Error::Malformed("repeat before any table entry"));
                }
                let n = if val == 16 {
                    br.take(3)? + 3
                } else {
                    br.take(7)? + 11
                };
                for _ in 0..n {
                    if idx >= HUFFMAN_TABLE_SIZE {
                        break;
                    }
                    self.lengthtable[idx] = self.lengthtable[idx - 1];
                    idx += 1;
                }
            } else {
                let n = if val == 18 {
                    br.take(3)? + 3
                } else {
                    br.take(7)? + 11
                };
                for _ in 0..n {
                    if idx >= HUFFMAN_TABLE_SIZE {
                        break;
                    }
                    self.lengthtable[idx] = 0;
                    idx += 1;
                }
            }
        }

        let maincode = HuffmanCode::create(
            &self.lengthtable[0..MAINCODE_SIZE],
            MAINCODE_SIZE,
            MAX_SYMBOL_LENGTH,
        )?;
        let offsetcode = HuffmanCode::create(
            &self.lengthtable[MAINCODE_SIZE..MAINCODE_SIZE + OFFSETCODE_SIZE],
            OFFSETCODE_SIZE,
            MAX_SYMBOL_LENGTH,
        )?;
        let lowoffsetcode = HuffmanCode::create(
            &self.lengthtable[MAINCODE_SIZE + OFFSETCODE_SIZE
                ..MAINCODE_SIZE + OFFSETCODE_SIZE + LOWOFFSETCODE_SIZE],
            LOWOFFSETCODE_SIZE,
            MAX_SYMBOL_LENGTH,
        )?;
        let lengthcode = HuffmanCode::create(
            &self.lengthtable
                [MAINCODE_SIZE + OFFSETCODE_SIZE + LOWOFFSETCODE_SIZE..HUFFMAN_TABLE_SIZE],
            LENGTHCODE_SIZE,
            MAX_SYMBOL_LENGTH,
        )?;
        self.tables = Some(Tables {
            maincode,
            offsetcode,
            lowoffsetcode,
            lengthcode,
        });
        Ok(())
    }

    /// Read one filter definition from the main stream (symbol 257).
    fn read_filter(&mut self, br: &mut BitReader3) -> Result<()> {
        let flags = br.take(8)? as u8;
        let mut length = u32::from(flags & 0x07) + 1;
        if length == 7 {
            length = br.take(8)? + 7;
        } else if length == 8 {
            length = (br.take(8)? << 8) | br.take(8)?;
        }
        let mut code = Vec::with_capacity(length as usize);
        for _ in 0..length {
            code.push(br.take(8)? as u8);
        }
        self.parse_filter(&code, flags)
    }

    fn parse_filter(&mut self, bytes: &[u8], flags: u8) -> Result<()> {
        let mut br = MemBitReader::new(bytes);
        let numprogs = self.filters.progs.len() as u32;

        let num = if flags & 0x80 != 0 {
            let mut num = br.next_number();
            if num == 0 {
                self.filters.stack.clear();
                self.filters.progs.clear();
            } else {
                num -= 1;
            }
            if num > numprogs {
                return Err(Error::Malformed("invalid filter number"));
            }
            self.filters.lastfilternum = num;
            num
        } else {
            self.filters.lastfilternum
        };

        // Existing program at `num`, or None meaning a new program follows.
        let (prog_idx, usagecount) = match self.filters.progs.get_mut(num as usize) {
            Some(p) => {
                p.usagecount += 1;
                (Some(num as usize), p.usagecount)
            }
            None => (None, 0),
        };

        let mut blockstartpos = br.next_number() as usize;
        blockstartpos = blockstartpos.wrapping_add(self.wpos());
        if flags & 0x40 != 0 {
            blockstartpos = blockstartpos.wrapping_add(258);
        }
        let blocklength = if flags & 0x20 != 0 {
            br.next_number()
        } else {
            prog_idx
                .map(|i| self.filters.progs[i].oldfilterlength)
                .unwrap_or(0)
        };
        if blocklength as usize > self.window.len() {
            return Err(Error::Malformed("filter block too large"));
        }

        let mut registers = [0u32; 8];
        registers[3] = PROGRAM_WORK_SIZE;
        registers[4] = blocklength;
        registers[5] = usagecount;
        registers[7] = 0x40000;

        if flags & 0x10 != 0 {
            let mask = br.bits(7);
            // Indices, not a slice: register 6 is a guard, not an argument.
            #[allow(clippy::needless_range_loop)]
            for i in 0..7 {
                if mask & (1 << i) != 0 {
                    registers[i] = br.next_number();
                }
            }
        }

        let prog_idx = match prog_idx {
            Some(i) => i,
            None => {
                // A new program: bytecode follows, identified by its
                // CRC-32/length fingerprint.
                let len = br.next_number();
                if len == 0 || len > 0x10000 {
                    return Err(Error::Malformed("invalid filter program"));
                }
                let mut bytecode = Vec::with_capacity(len as usize);
                for _ in 0..len {
                    bytecode.push(br.bits(8) as u8);
                }
                let prog = compile_program(&bytecode)?;
                self.filters.progs.push(prog);
                self.filters.progs.len() - 1
            }
        };
        self.filters.progs[prog_idx].oldfilterlength = blocklength;

        if flags & 0x08 != 0 {
            let globaldatalen = br.next_number();
            if globaldatalen > 0x1FC0 {
                return Err(Error::Malformed("filter global data too large"));
            }
            for _ in 0..globaldatalen {
                br.bits(8);
            }
        }

        if br.at_eof {
            return Err(Error::Malformed("truncated filter data"));
        }

        self.filters.stack.push(Filter {
            prog: prog_idx,
            initialregisters: registers,
            blockstartpos,
            blocklength,
        });
        Ok(())
    }

    /// Apply every filter whose block has been fully decoded, in stream
    /// order (regions are checked to not overlap, like the reference).
    fn apply_ready_filters(&mut self) -> Result<()> {
        while let Some(f) = self.filters.stack.first() {
            let end = match f.blockstartpos.checked_add(f.blocklength as usize) {
                Some(e) => e,
                None => return Err(Error::Malformed("filter block overflow")),
            };
            if end > self.wpos() {
                break;
            }
            if f.blockstartpos < self.member_base {
                return Err(Error::Malformed("filter block spans solid members"));
            }
            let f = self.filters.stack.remove(0);
            let start_local = f.blockstartpos - self.member_base;
            let end_local = start_local + f.blocklength as usize;
            // `out` holds the whole group: member-local indices must be
            // offset by the member base.
            let gstart = self.member_base + start_local;
            let gend = self.member_base + end_local;
            let region = &self.out[gstart..gend];
            // The E8/E8E9 standard filters use R[6] = bytes written so far
            // of the CURRENT member, so the position handed to the filter
            // is member-relative (UnRAR's WrittenFileSize resets per member).
            let filtered = execute_filter(&f, &self.filters.progs[f.prog], region, start_local)?;
            self.out[gstart..gend].copy_from_slice(&filtered);
            if let Some(next) = self.filters.stack.first() {
                if next.blockstartpos < end_local + self.member_base {
                    return Err(Error::Malformed("overlapping filter blocks"));
                }
            }
        }
        Ok(())
    }
}

fn compile_program(bytecode: &[u8]) -> Result<Program> {
    let mut xor = 0u8;
    for &b in bytecode.iter().skip(1) {
        xor ^= b;
    }
    if bytecode.is_empty() || xor != bytecode[0] {
        return Err(Error::Malformed("corrupt filter program"));
    }
    let fingerprint = u64::from(crc32(bytecode)) | ((bytecode.len() as u64) << 32);
    let mut br = MemBitReader::new(bytecode);
    br.offset = 1;
    if br.bits(1) != 0 {
        let staticdatalen = br.next_number();
        if staticdatalen >= 0x40000 {
            return Err(Error::Malformed("filter static data too large"));
        }
        for _ in 0..staticdatalen + 1 {
            br.bits(8);
        }
    }
    Ok(Program {
        fingerprint,
        usagecount: 0,
        oldfilterlength: 0,
    })
}

fn execute_filter(f: &Filter, prog: &Program, region: &[u8], pos: usize) -> Result<Vec<u8>> {
    match prog.fingerprint {
        FP_DELTA => filter_delta(region, f.initialregisters[0] as usize),
        FP_E8 => filter_e8(region, pos, false),
        FP_E8E9 => filter_e8(region, pos, true),
        FP_RGB => filter_rgb(region, f.initialregisters[0], f.initialregisters[1]),
        FP_AUDIO => filter_audio(region, f.initialregisters[0] as usize),
        _ => Err(Error::Unsupported("RAR VM program filter")),
    }
}

fn filter_delta(region: &[u8], numchannels: usize) -> Result<Vec<u8>> {
    let length = region.len();
    if length > PROGRAM_WORK_SIZE as usize / 2 || numchannels == 0 || numchannels > 128 {
        return Err(Error::Malformed("invalid delta filter parameters"));
    }
    let mut out = vec![0u8; length];
    let mut src_pos = 0usize;
    for i in 0..numchannels {
        let mut last: u8 = 0;
        let mut dest = i;
        while dest < length {
            last = last.wrapping_sub(region[src_pos]);
            out[dest] = last;
            src_pos += 1;
            dest += numchannels;
        }
    }
    Ok(out)
}

fn filter_e8(region: &[u8], pos: usize, e9also: bool) -> Result<Vec<u8>> {
    let length = region.len();
    if length > PROGRAM_WORK_SIZE as usize || length <= 4 {
        return Err(Error::Malformed("invalid e8 filter parameters"));
    }
    const FILE_SIZE: u32 = 0x1000000;
    let mut buf = region.to_vec();
    let mut i = 0usize;
    while i + 5 <= length {
        if buf[i] == 0xE8 || (e9also && buf[i] == 0xE9) {
            let currpos = (pos as u32).wrapping_add(i as u32).wrapping_add(1);
            let addr = i32::from_le_bytes([buf[i + 1], buf[i + 2], buf[i + 3], buf[i + 4]]);
            if addr < 0 && currpos >= (addr as u32).wrapping_neg() {
                buf[i + 1..i + 5]
                    .copy_from_slice(&addr.wrapping_add(FILE_SIZE as i32).to_le_bytes());
            } else if addr >= 0 && (addr as u32) < FILE_SIZE {
                buf[i + 1..i + 5]
                    .copy_from_slice(&(addr as u32).wrapping_sub(currpos).to_le_bytes());
            }
            i += 4;
        }
        i += 1;
    }
    Ok(buf)
}

fn filter_rgb(region: &[u8], stride: u32, byteoffset: u32) -> Result<Vec<u8>> {
    let blocklength = region.len();
    if blocklength > PROGRAM_WORK_SIZE as usize / 2
        || stride as usize > blocklength
        || blocklength < 3
        || byteoffset > 2
    {
        return Err(Error::Malformed("invalid rgb filter parameters"));
    }
    let stride = stride as usize;
    // Decode reads the block linearly (planar channel order) and writes the
    // output interleaved, into a separate half of the VM memory.
    let mut dst = vec![0u8; blocklength];
    let mut src_pos = 0usize;
    for i in 0..3usize {
        let mut byte: u8 = 0;
        let mut j = i;
        while j < blocklength {
            if j >= stride {
                let prev = j - stride;
                let up_left = dst[prev] as i32;
                let up = dst[prev + 3] as i32;
                let delta1 = (up - up_left).abs();
                let delta2 = (byte as i32 - up_left).abs();
                let delta3 = (up - up_left + byte as i32 - up_left).abs();
                if delta1 > delta2 || delta1 > delta3 {
                    byte = if delta2 <= delta3 {
                        dst[prev + 3]
                    } else {
                        dst[prev]
                    };
                }
            }
            byte = byte.wrapping_sub(region[src_pos]);
            src_pos += 1;
            dst[j] = byte;
            j += 3;
        }
    }
    let byteoffset = byteoffset as usize;
    let mut i = byteoffset;
    while i + 2 < blocklength {
        dst[i] = dst[i].wrapping_add(dst[i + 1]);
        dst[i + 2] = dst[i + 2].wrapping_add(dst[i + 1]);
        i += 3;
    }
    Ok(dst)
}

fn filter_audio(region: &[u8], numchannels: usize) -> Result<Vec<u8>> {
    let length = region.len();
    if length > PROGRAM_WORK_SIZE as usize / 2 || numchannels == 0 || numchannels > 128 {
        return Err(Error::Malformed("invalid audio filter parameters"));
    }
    let mut out = vec![0u8; length];
    let mut src_pos = 0usize;
    for i in 0..numchannels {
        let mut weight = [0i32; 5];
        let mut delta = [0i16; 4];
        let mut lastdelta: i16 = 0;
        let mut error = [0i32; 11];
        let mut count: u32 = 0;
        let mut lastbyte: u8 = 0;
        let mut j = i;
        while j < length {
            let d = region[src_pos] as i8 as i32;
            src_pos += 1;
            delta[2] = delta[1];
            delta[1] = lastdelta.wrapping_sub(delta[0]);
            delta[0] = lastdelta;
            let pred = (8 * lastbyte as i32
                + weight[0] * delta[0] as i32
                + weight[1] * delta[1] as i32
                + weight[2] * delta[2] as i32) as u32;
            // C computes the sum as Int32, casts to UInt32 and shifts
            // logically; an arithmetic shift differs for negative sums.
            let predbyte = ((pred >> 3) & 0xFF) as i32;
            let byte = (predbyte - d) & 0xFF;
            let prederror = d * 8;
            error[0] += prederror.abs();
            error[1] += (prederror - delta[0] as i32).abs();
            error[2] += (prederror + delta[0] as i32).abs();
            error[3] += (prederror - delta[1] as i32).abs();
            error[4] += (prederror + delta[1] as i32).abs();
            error[5] += (prederror - delta[2] as i32).abs();
            error[6] += (prederror + delta[2] as i32).abs();
            // prevDelta = (signed char)(decoded - prevByte): low byte, sign-extended.
            lastdelta = ((byte - lastbyte as i32) as i8) as i16;
            lastbyte = byte as u8;
            out[j] = lastbyte;
            if count & 0x1F == 0 {
                let mut idx = 0usize;
                for k in 1..7 {
                    if error[k] < error[idx] {
                        idx = k;
                    }
                }
                error = [0; 11];
                match idx {
                    1 if weight[0] >= -16 => weight[0] -= 1,
                    2 if weight[0] < 16 => weight[0] += 1,
                    3 if weight[1] >= -16 => weight[1] -= 1,
                    4 if weight[1] < 16 => weight[1] += 1,
                    5 if weight[2] >= -16 => weight[2] -= 1,
                    6 if weight[2] < 16 => weight[2] += 1,
                    _ => {}
                }
            }
            count = count.wrapping_add(1);
            j += numchannels;
        }
    }
    Ok(out)
}
