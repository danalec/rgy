//! PPMd variant-H decoder with the RAR carry-less range decoder, as used by
//! RAR 2.9/3.x/4.x "PPMd blocks" inside the LZ+Huffman packed stream.
//!
//! Ported from libarchive's `archive_ppmd7.c` (Igor Pavlov, public domain,
//! itself based on Dmitry Shkarin's PPMd var.H, public domain), the same
//! BSD-2-Clause tree the rest of the rar3 decoder was ported from. Only the
//! decode half is ported. All multi-byte arena fields are little-endian,
//! matching the C behaviour on little-endian hosts (and unrar/7-Zip).
//!
//! Memory model: the C code type-puns a `malloc`'d byte arena. This port
//! keeps that layout with `u32` offsets into a `Vec<u8>` (the C "not
//! PPMD_32BIT" mode): a 12-byte context is `NumStats: u16, SummFreq: u16,
//! Stats: u32, Suffix: u32`; a 6-byte state is `Symbol: u8, Freq: u8,
//! SuccessorLow: u16, SuccessorHigh: u16`; a free-list node reuses the first
//! 12 bytes as `Stamp: u16, NU: u16, Next: u32, Prev: u32`.
//!
//! Reads past the arena yield 0 and writes are dropped: valid streams never
//! reference outside the arena, and corrupt streams are caught by the
//! member-size / CRC checks upstream instead of panicking.

use alloc::vec;
use alloc::vec::Vec;

use crate::unpack3::BitReader3;
use crate::{Error, Result};

const PPMD_INT_BITS: u32 = 7;
const PPMD_PERIOD_BITS: u32 = 7;
const PPMD_BIN_SCALE: u32 = 1 << (PPMD_INT_BITS + PPMD_PERIOD_BITS);
const PPMD_NUM_INDEXES: usize = 38; // N1+N2+N3+N4 = 4+4+4+(128+3-4-8-12)/4

const MAX_FREQ: u32 = 124;
const UNIT_SIZE: u32 = 12;
const STATE_SIZE: u32 = 6;
const K_TOP_VALUE: u32 = 1 << 24;
const RAR_BOTTOM: u32 = 0x8000;

const K_INIT_BIN_ESC: [u16; 8] = [
    0x3CDD, 0x1F3F, 0x59BF, 0x48F3, 0x64A1, 0x5ABC, 0x6632, 0x6051,
];
const K_EXP_ESCAPE: [u8; 16] = [25, 14, 9, 7, 5, 5, 4, 4, 4, 3, 3, 3, 2, 2, 2, 2];

#[inline]
fn get_mean(summ: u32) -> u32 {
    (summ + (1 << (PPMD_PERIOD_BITS - 2))) >> PPMD_PERIOD_BITS
}

/// SEE-context (scaled escape estimation) laid out as flat fields.
#[derive(Clone, Copy, Default)]
struct See {
    summ: u16,
    shift: u8,
    count: u8,
}

pub struct Ppmd7 {
    min_context: u32,
    max_context: u32,
    found_state: u32,
    order_fall: u32,
    init_esc: u32,
    prev_success: u32,
    max_order: u32,
    hi_bits_flag: u32,
    run_length: i32,
    init_rl: i32,
    size: u32,
    glue_count: u32,
    align_offset: u32,
    lo_unit: u32,
    hi_unit: u32,
    text: u32,
    units_start: u32,
    base: Vec<u8>,
    indx2units: [u8; PPMD_NUM_INDEXES],
    units2indx: [u8; 128],
    free_list: [u32; PPMD_NUM_INDEXES],
    ns2indx: [u8; 256],
    ns2bsindx: [u8; 256],
    hb2flag: [u8; 256],
    dummy_see: See,
    see: Vec<See>,      // 25 * 16
    bin_summ: Vec<u16>, // 128 * 64
    // RAR range decoder state.
    rd_code: u32,
    rd_low: u32,
    rd_range: u32,
}

// ---------------------------------------------------------------- arena I/O

impl Ppmd7 {
    #[inline]
    fn r16(&self, off: u32) -> u16 {
        match self.base.get(off as usize..off as usize + 2) {
            Some(b) => u16::from_le_bytes([b[0], b[1]]),
            None => 0,
        }
    }

    #[inline]
    fn w16(&mut self, off: u32, v: u16) {
        if let Some(b) = self.base.get_mut(off as usize..off as usize + 2) {
            b.copy_from_slice(&v.to_le_bytes());
        }
    }

    #[inline]
    fn r32(&self, off: u32) -> u32 {
        match self.base.get(off as usize..off as usize + 4) {
            Some(b) => u32::from_le_bytes([b[0], b[1], b[2], b[3]]),
            None => 0,
        }
    }

    #[inline]
    fn w32(&mut self, off: u32, v: u32) {
        if let Some(b) = self.base.get_mut(off as usize..off as usize + 4) {
            b.copy_from_slice(&v.to_le_bytes());
        }
    }

    #[inline]
    fn byte(&self, off: u32) -> u8 {
        self.base.get(off as usize).copied().unwrap_or(0)
    }

    #[inline]
    fn set_byte(&mut self, off: u32, v: u8) {
        if let Some(b) = self.base.get_mut(off as usize) {
            *b = v;
        }
    }

    // State (6 bytes): Symbol u8, Freq u8, SuccessorLow u16, SuccessorHigh u16.
    #[inline]
    fn st_symbol(&self, s: u32) -> u8 {
        self.byte(s)
    }

    #[inline]
    fn st_freq(&self, s: u32) -> u8 {
        self.byte(s + 1)
    }

    #[inline]
    fn st_set_freq(&mut self, s: u32, f: u8) {
        self.set_byte(s + 1, f);
    }

    #[inline]
    fn st_succ(&self, s: u32) -> u32 {
        self.r16(s + 2) as u32 | ((self.r16(s + 4) as u32) << 16)
    }

    #[inline]
    fn st_set_succ(&mut self, s: u32, v: u32) {
        self.w16(s + 2, (v & 0xFFFF) as u16);
        self.w16(s + 4, (v >> 16) as u16);
    }

    /// Copy one full state (6 bytes).
    #[inline]
    fn copy_state(&mut self, dst: u32, src: u32) {
        for i in 0..6 {
            let v = self.byte(src + i);
            self.set_byte(dst + i, v);
        }
    }

    #[inline]
    fn swap_states(&mut self, a: u32, b: u32) {
        let mut tmp = [0u8; 6];
        for (i, t) in tmp.iter_mut().enumerate() {
            *t = self.byte(a + i as u32);
        }
        for i in 0..6 {
            let v = self.byte(b + i);
            self.set_byte(a + i, v);
        }
        for (i, t) in tmp.iter().enumerate() {
            self.set_byte(b + i as u32, *t);
        }
    }

    /// `MyMem12Cpy`: copy `num` 12-byte units.
    fn mem12_copy(&mut self, dst: u32, src: u32, num: u32) {
        for i in 0..num * 12 {
            let v = self.byte(src + i);
            self.set_byte(dst + i, v);
        }
    }

    // Context (12 bytes): NumStats u16, SummFreq u16, Stats u32, Suffix u32.
    #[inline]
    fn ctx_num_stats(&self, c: u32) -> u16 {
        self.r16(c)
    }

    #[inline]
    fn ctx_set_num_stats(&mut self, c: u32, v: u16) {
        self.w16(c, v);
    }

    #[inline]
    fn ctx_summ_freq(&self, c: u32) -> u16 {
        self.r16(c + 2)
    }

    #[inline]
    fn ctx_set_summ_freq(&mut self, c: u32, v: u16) {
        self.w16(c + 2, v);
    }

    #[inline]
    fn ctx_stats(&self, c: u32) -> u32 {
        self.r32(c + 4)
    }

    #[inline]
    fn ctx_set_stats(&mut self, c: u32, v: u32) {
        self.w32(c + 4, v);
    }

    #[inline]
    fn ctx_suffix(&self, c: u32) -> u32 {
        self.r32(c + 8)
    }

    #[inline]
    fn ctx_set_suffix(&mut self, c: u32, v: u32) {
        self.w32(c + 8, v);
    }

    /// OneState overlays the context's SummFreq field.
    #[inline]
    fn one_state(c: u32) -> u32 {
        c + 2
    }

    // ------------------------------------------------------------ unit index

    #[inline]
    fn u2b(nu: u32) -> u32 {
        nu * UNIT_SIZE
    }

    #[inline]
    fn i2u(&self, indx: u32) -> u32 {
        // The C code can index one past the last entry on adversarial
        // NumStats values; reads outside return 0 instead of UB.
        self.indx2units.get(indx as usize).copied().unwrap_or(0) as u32
    }

    #[inline]
    fn u2i(&self, nu: u32) -> u32 {
        self.units2indx.get((nu - 1) as usize).copied().unwrap_or(0) as u32
    }

    // ------------------------------------------------------------ allocation

    fn insert_node(&mut self, node: u32, indx: u32) {
        self.w32(node, self.free_list[indx as usize]);
        self.free_list[indx as usize] = node;
    }

    fn remove_node(&mut self, indx: u32) -> u32 {
        let node = self.free_list[indx as usize];
        self.free_list[indx as usize] = self.r32(node);
        node
    }

    fn split_block(&mut self, ptr: u32, old_indx: u32, new_indx: u32) {
        let nu = self.i2u(old_indx) - self.i2u(new_indx);
        let ptr = ptr + Self::u2b(self.i2u(new_indx));
        let mut i = self.u2i(nu);
        if self.i2u(i) != nu {
            i -= 1;
            let k = self.i2u(i);
            self.insert_node(ptr + Self::u2b(k), nu - k - 1);
        }
        self.insert_node(ptr, i);
    }

    fn glue_free_blocks(&mut self) {
        let head = self.align_offset + self.size;
        let mut n = head;
        self.glue_count = 255;

        // Create a doubly-linked list of free blocks.
        for i in 0..PPMD_NUM_INDEXES {
            let nu = self.i2u(i as u32);
            let mut next = self.free_list[i];
            self.free_list[i] = 0;
            while next != 0 {
                let node = next;
                self.w32(node + 4, n); // node->Next = n
                self.w32(n + 8, next); // NODE(n)->Prev = next
                n = next;
                next = self.r32(node); // old free-list link in Stamp/NU
                self.w16(node, 0); // Stamp = 0
                self.w16(node + 2, nu as u16); // NU
            }
        }
        self.w16(head, 1); // NODE(head)->Stamp = 1
        self.w32(head + 4, n); // NODE(head)->Next = n
        self.w32(n + 8, head); // NODE(n)->Prev = head
        if self.lo_unit != self.hi_unit {
            self.w16(self.lo_unit, 1); // Stamp = 1
        }

        // Glue adjacent free blocks.
        while n != head {
            let mut nu = self.r16(n + 2) as u32;
            loop {
                let node2 = n + nu * UNIT_SIZE;
                nu += self.r16(node2 + 2) as u32;
                if self.r16(node2) != 0 || nu >= 0x10000 {
                    break;
                }
                let prev = self.r32(node2 + 8);
                let next = self.r32(node2 + 4);
                self.w32(prev + 4, next);
                self.w32(next + 8, prev);
                self.w16(n + 2, nu as u16);
            }
            n = self.r32(n + 4);
        }

        // Fill the free lists back up.
        let mut n = self.r32(head + 4);
        while n != head {
            let mut node = n;
            let next = self.r32(n + 4);
            let mut nu = self.r16(node + 2) as u32;
            while nu > 128 {
                self.insert_node(node, (PPMD_NUM_INDEXES - 1) as u32);
                nu -= 128;
                node += 128 * UNIT_SIZE;
            }
            let mut i = self.u2i(nu);
            if self.i2u(i) != nu {
                i -= 1;
                let k = self.i2u(i);
                self.insert_node(node + Self::u2b(k), nu - k - 1);
            }
            self.insert_node(node, i);
            n = next;
        }
    }

    fn alloc_units_rare(&mut self, indx: u32) -> Option<u32> {
        if self.glue_count == 0 {
            self.glue_free_blocks();
            if self.free_list[indx as usize] != 0 {
                return Some(self.remove_node(indx));
            }
        }
        let mut i = indx;
        loop {
            i += 1;
            if i == PPMD_NUM_INDEXES as u32 {
                let num_bytes = Self::u2b(self.i2u(indx));
                self.glue_count = self.glue_count.wrapping_sub(1);
                return if self.units_start - self.text > num_bytes {
                    self.units_start -= num_bytes;
                    Some(self.units_start)
                } else {
                    None
                };
            }
            if self.free_list[i as usize] != 0 {
                break;
            }
        }
        let ret = self.remove_node(i);
        self.split_block(ret, i, indx);
        Some(ret)
    }

    fn alloc_units(&mut self, indx: u32) -> Option<u32> {
        if self.free_list[indx as usize] != 0 {
            return Some(self.remove_node(indx));
        }
        let num_bytes = Self::u2b(self.i2u(indx));
        if num_bytes <= self.hi_unit - self.lo_unit {
            let ret = self.lo_unit;
            self.lo_unit += num_bytes;
            return Some(ret);
        }
        self.alloc_units_rare(indx)
    }

    fn shrink_units(&mut self, old_ptr: u32, old_nu: u32, new_nu: u32) -> u32 {
        let i0 = self.u2i(old_nu);
        let i1 = self.u2i(new_nu);
        if i0 == i1 {
            return old_ptr;
        }
        if self.free_list[i1 as usize] != 0 {
            let ptr = self.remove_node(i1);
            self.mem12_copy(ptr, old_ptr, new_nu);
            self.insert_node(old_ptr, i0);
            return ptr;
        }
        self.split_block(old_ptr, i0, i1);
        old_ptr
    }

    // ------------------------------------------------------------------ init

    /// Construct + allocate. `size` is the PPMd memory size in bytes.
    pub fn new(size: u32) -> Result<Ppmd7> {
        if size < UNIT_SIZE {
            return Err(Error::Malformed("ppmd memory size too small"));
        }
        let mut p = Ppmd7 {
            min_context: 0,
            max_context: 0,
            found_state: 0,
            order_fall: 0,
            init_esc: 0,
            prev_success: 0,
            max_order: 0,
            hi_bits_flag: 0,
            run_length: 0,
            init_rl: 0,
            size,
            glue_count: 0,
            align_offset: 4 - (size & 3),
            lo_unit: 0,
            hi_unit: 0,
            text: 0,
            units_start: 0,
            base: vec![0u8; (4 - (size & 3) + size + UNIT_SIZE) as usize],
            indx2units: [0; PPMD_NUM_INDEXES],
            units2indx: [0; 128],
            free_list: [0; PPMD_NUM_INDEXES],
            ns2indx: [0; 256],
            ns2bsindx: [0; 256],
            hb2flag: [0; 256],
            dummy_see: See::default(),
            see: vec![See::default(); 25 * 16],
            bin_summ: vec![0u16; 128 * 64],
            rd_code: 0,
            rd_low: 0,
            rd_range: 0,
        };
        // Unit index tables.
        let mut k = 0usize;
        for i in 0..PPMD_NUM_INDEXES {
            let step = if i >= 12 { 4 } else { (i >> 2) + 1 };
            for _ in 0..step {
                p.units2indx[k] = i as u8;
                k += 1;
            }
            p.indx2units[i] = k as u8;
        }
        p.ns2bsindx[0] = 0;
        p.ns2bsindx[1] = 2;
        for v in p.ns2bsindx.iter_mut().take(11).skip(2) {
            *v = 4;
        }
        for v in p.ns2bsindx.iter_mut().skip(11) {
            *v = 6;
        }
        for (i, v) in p.ns2indx.iter_mut().enumerate().take(3) {
            *v = i as u8;
        }
        let mut i = 3usize;
        let mut m = 3u8;
        let mut k = 1i32;
        while i < 256 {
            p.ns2indx[i] = m;
            i += 1;
            k -= 1;
            if k == 0 {
                m += 1;
                k = m as i32 - 2;
            }
        }
        for v in p.hb2flag.iter_mut().take(0x40) {
            *v = 0;
        }
        for v in p.hb2flag.iter_mut().skip(0x40) {
            *v = 8;
        }
        Ok(p)
    }

    fn restart_model(&mut self) {
        self.free_list = [0; PPMD_NUM_INDEXES];
        self.text = self.align_offset;
        self.hi_unit = self.text + self.size;
        self.lo_unit = self.hi_unit - (self.size / 8 / UNIT_SIZE) * 7 * UNIT_SIZE;
        self.units_start = self.lo_unit;
        self.glue_count = 0;

        self.order_fall = self.max_order;
        self.run_length = -(self.max_order.min(12) as i32) - 1;
        self.init_rl = self.run_length;
        self.prev_success = 0;

        self.hi_unit -= UNIT_SIZE;
        self.min_context = self.hi_unit;
        self.max_context = self.min_context;
        self.ctx_set_suffix(self.min_context, 0);
        self.ctx_set_num_stats(self.min_context, 256);
        self.ctx_set_summ_freq(self.min_context, 257);
        self.found_state = self.lo_unit;
        self.lo_unit += Self::u2b(128);
        self.ctx_set_stats(self.min_context, self.found_state);
        for i in 0..256u32 {
            let s = self.found_state + i * 6;
            self.set_byte(s, i as u8);
            self.set_byte(s + 1, 1);
            self.st_set_succ(s, 0);
        }

        for i in 0..128u32 {
            for k in 0..8u32 {
                let val = (PPMD_BIN_SCALE - K_INIT_BIN_ESC[k as usize] as u32 / (i + 2)) as u16;
                for m in (0..64u32).step_by(8) {
                    self.bin_summ[(i * 64 + k + m) as usize] = val;
                }
            }
        }

        for i in 0..25u32 {
            for k in 0..16u32 {
                let idx = (i * 16 + k) as usize;
                self.see[idx].shift = (PPMD_PERIOD_BITS - 4) as u8;
                self.see[idx].summ = ((5 * i + 10) << (PPMD_PERIOD_BITS - 4)) as u16;
                self.see[idx].count = 4;
            }
        }
    }

    /// RAR range-decoder init (4 code bytes, then Bottom = 0x8000). Used both
    /// for a fresh model and when a later PPMd block re-inits only the range
    /// decoder while keeping the model.
    pub fn init_range_rar(&mut self, br: &mut BitReader3) -> Result<()> {
        self.rd_low = 0;
        self.rd_range = 0xFFFFFFFF;
        let mut code = 0u32;
        for _ in 0..4 {
            code = (code << 8) | br.take(8)?;
        }
        if code == 0xFFFFFFFF {
            return Err(Error::Malformed("ppmd range decoder init"));
        }
        self.rd_code = code;
        Ok(())
    }

    /// `Ppmd7_Init`: restart the model at `max_order` and set the escape
    /// initial value.
    pub fn init_model(&mut self, max_order: u32, init_esc: u32) {
        self.max_order = max_order;
        self.restart_model();
        self.dummy_see.shift = PPMD_PERIOD_BITS as u8;
        self.dummy_see.summ = 0;
        self.dummy_see.count = 64;
        self.init_esc = init_esc;
    }

    pub fn set_init_esc(&mut self, esc: u32) {
        self.init_esc = esc;
    }

    // ------------------------------------------------------------ range coder

    fn get_threshold(&mut self, _br: &mut BitReader3, total: u32) -> Result<u32> {
        self.rd_range /= total;
        if self.rd_range == 0 {
            return Err(Error::Malformed("ppmd zero range"));
        }
        Ok(self.rd_code.wrapping_sub(self.rd_low) / self.rd_range)
    }

    fn normalize(&mut self, br: &mut BitReader3) -> Result<()> {
        loop {
            if (self.rd_low ^ (self.rd_low.wrapping_add(self.rd_range))) >= K_TOP_VALUE {
                if self.rd_range >= RAR_BOTTOM {
                    break;
                }
                self.rd_range = self.rd_low.wrapping_neg() & (RAR_BOTTOM - 1);
            }
            self.rd_code = (self.rd_code << 8) | br.take(8)?;
            self.rd_range <<= 8;
            self.rd_low <<= 8;
        }
        Ok(())
    }

    fn decode(&mut self, br: &mut BitReader3, start: u32, size: u32) -> Result<()> {
        self.rd_low = self.rd_low.wrapping_add(start.wrapping_mul(self.rd_range));
        self.rd_range = self.rd_range.wrapping_mul(size);
        self.normalize(br)
    }

    fn decode_bit(&mut self, br: &mut BitReader3, size0: u32) -> Result<u32> {
        let value = self.get_threshold(br, PPMD_BIN_SCALE)?;
        if value < size0 {
            self.decode(br, 0, size0)?;
            Ok(0)
        } else {
            self.decode(br, size0, PPMD_BIN_SCALE - size0)?;
            Ok(1)
        }
    }

    // ----------------------------------------------------------- model update

    fn create_successors(&mut self, skip: bool) -> Option<u32> {
        let mut ps = [0u32; 64];
        let mut num_ps = 0usize;
        let mut c = self.min_context;
        let up_branch = self.st_succ(self.found_state);
        let found_symbol = self.st_symbol(self.found_state);

        if !skip {
            ps[num_ps] = self.found_state;
            num_ps += 1;
        }

        while self.ctx_suffix(c) != 0 {
            c = self.ctx_suffix(c);
            let s = if self.ctx_num_stats(c) != 1 {
                let mut s0 = self.ctx_stats(c);
                let mut guard = 0;
                while self.st_symbol(s0) != found_symbol {
                    s0 += 6;
                    guard += 1;
                    if guard > 4096 {
                        return None;
                    }
                }
                s0
            } else {
                Self::one_state(c)
            };
            let successor = self.st_succ(s);
            if successor != up_branch {
                c = successor;
                if num_ps == 0 {
                    return Some(c);
                }
                break;
            }
            if num_ps >= ps.len() {
                return None;
            }
            ps[num_ps] = s;
            num_ps += 1;
        }

        let up_symbol = self.byte(up_branch);
        let up_freq = if self.ctx_num_stats(c) == 1 {
            self.st_freq(Self::one_state(c))
        } else {
            let mut s = self.ctx_stats(c);
            let mut guard = 0;
            while self.st_symbol(s) != up_symbol {
                s += 6;
                guard += 1;
                if guard > 4096 {
                    return None;
                }
            }
            // C promotes the Byte to int before subtracting, so freq == 0
            // wraps to 0xFFFFFFFF (unsigned). Reproduce with wrapping math.
            let cf = (self.st_freq(s) as u32).wrapping_sub(1);
            let s0 = (self.ctx_summ_freq(c) as u32)
                .wrapping_sub(self.ctx_num_stats(c) as u32)
                .wrapping_sub(cf);
            if s0 == 0 {
                return None;
            }
            let two_cf = cf.wrapping_mul(2);
            let three_s0 = s0.wrapping_mul(3);
            let denom = s0.wrapping_mul(2);
            if denom == 0 {
                return None;
            }
            1 + if two_cf <= s0 {
                u8::from(cf.wrapping_mul(5) > s0)
            } else {
                (two_cf.wrapping_add(three_s0).wrapping_sub(1) / denom) as u8
            }
        };
        let up_succ = up_branch.wrapping_add(1);

        while num_ps != 0 {
            // AllocContext.
            let c1 = if self.hi_unit != self.lo_unit {
                self.hi_unit -= UNIT_SIZE;
                self.hi_unit
            } else if self.free_list[0] != 0 {
                self.remove_node(0)
            } else {
                self.alloc_units_rare(0)?
            };
            self.ctx_set_num_stats(c1, 1);
            // *ONE_STATE(c1) = upState
            self.set_byte(c1 + 2, up_symbol);
            self.set_byte(c1 + 3, up_freq);
            self.w16(c1 + 4, (up_succ & 0xFFFF) as u16);
            self.w16(c1 + 6, (up_succ >> 16) as u16);
            self.ctx_set_suffix(c1, c);
            num_ps -= 1;
            self.st_set_succ(ps[num_ps], c1);
            c = c1;
        }
        Some(c)
    }

    fn update_model(&mut self) {
        if self.st_freq(self.found_state) < (MAX_FREQ / 4) as u8
            && self.ctx_suffix(self.min_context) != 0
        {
            let c = self.ctx_suffix(self.min_context);
            if self.ctx_num_stats(c) == 1 {
                let s = Self::one_state(c);
                if self.st_freq(s) < 32 {
                    let f = self.st_freq(s);
                    self.st_set_freq(s, f + 1);
                }
            } else {
                let found_symbol = self.st_symbol(self.found_state);
                let mut s = self.ctx_stats(c);
                let mut guard = 0;
                if self.st_symbol(s) != found_symbol {
                    loop {
                        s += 6;
                        guard += 1;
                        if guard > 4096 {
                            self.restart_model();
                            return;
                        }
                        if self.st_symbol(s) == found_symbol {
                            break;
                        }
                    }
                    if self.st_freq(s) >= self.st_freq(s - 6) {
                        self.swap_states(s, s - 6);
                        s -= 6;
                    }
                }
                if self.st_freq(s) < (MAX_FREQ - 9) as u8 {
                    let f = self.st_freq(s);
                    self.st_set_freq(s, f + 2);
                    let sf = self.ctx_summ_freq(c);
                    self.ctx_set_summ_freq(c, sf.wrapping_add(2));
                }
            }
        }

        if self.order_fall == 0 {
            match self.create_successors(true) {
                Some(mc) => {
                    self.min_context = mc;
                    self.max_context = mc;
                }
                None => {
                    self.restart_model();
                    return;
                }
            }
            self.st_set_succ(self.found_state, self.min_context);
            return;
        }

        let symbol = self.st_symbol(self.found_state);
        self.set_byte(self.text, symbol);
        self.text += 1;
        let mut successor = self.text;
        if self.text >= self.units_start {
            self.restart_model();
            return;
        }

        let mut f_successor = self.st_succ(self.found_state);
        if f_successor != 0 {
            if f_successor <= successor {
                match self.create_successors(false) {
                    Some(cs) => f_successor = cs,
                    None => {
                        self.restart_model();
                        return;
                    }
                }
            }
            self.order_fall -= 1;
            if self.order_fall == 0 {
                successor = f_successor;
                if self.max_context != self.min_context {
                    self.text -= 1;
                }
            }
        } else {
            self.st_set_succ(self.found_state, successor);
            f_successor = self.min_context;
        }

        let ns = self.ctx_num_stats(self.min_context) as u32;
        let s0 = (self.ctx_summ_freq(self.min_context) as u32)
            .wrapping_sub(ns)
            .wrapping_sub(self.st_freq(self.found_state) as u32 - 1);

        let mut c = self.max_context;
        loop {
            if c == self.min_context {
                break;
            }
            let ns1 = self.ctx_num_stats(c) as u32;
            if ns1 != 1 {
                if (ns1 & 1) == 0 {
                    let old_nu = ns1 >> 1;
                    let i = self.u2i(old_nu);
                    if i != self.u2i(old_nu + 1) {
                        let ptr = match self.alloc_units(i + 1) {
                            Some(p) => p,
                            None => {
                                self.restart_model();
                                return;
                            }
                        };
                        let old_ptr = self.ctx_stats(c);
                        self.mem12_copy(ptr, old_ptr, old_nu);
                        self.insert_node(old_ptr, i);
                        self.ctx_set_stats(c, ptr);
                    }
                }
                let sf = self.ctx_summ_freq(c) as u32;
                let new_sf =
                    sf + u32::from(2 * ns1 < ns) + 2 * u32::from(4 * ns1 <= ns && sf <= 8 * ns1);
                self.ctx_set_summ_freq(c, new_sf as u16);
            } else {
                let s = match self.alloc_units(0) {
                    Some(s) => s,
                    None => {
                        self.restart_model();
                        return;
                    }
                };
                self.copy_state(s, Self::one_state(c));
                self.ctx_set_stats(c, s);
                let f = self.st_freq(s);
                let nf = if f < (MAX_FREQ / 4 - 1) as u8 {
                    f << 1
                } else {
                    (MAX_FREQ - 4) as u8
                };
                self.st_set_freq(s, nf);
                let sf = nf as u32 + self.init_esc + u32::from(ns > 3);
                self.ctx_set_summ_freq(c, sf as u16);
            }
            let mut cf =
                2 * self.st_freq(self.found_state) as u32 * (self.ctx_summ_freq(c) as u32 + 6);
            let sf = s0 + self.ctx_summ_freq(c) as u32;
            if cf < 6 * sf {
                cf = 1 + u32::from(cf > sf) + u32::from(cf >= 4 * sf);
                let v = self.ctx_summ_freq(c);
                self.ctx_set_summ_freq(c, v.wrapping_add(3));
            } else {
                cf = 4
                    + u32::from(cf >= 9 * sf)
                    + u32::from(cf >= 12 * sf)
                    + u32::from(cf >= 15 * sf);
                let v = self.ctx_summ_freq(c);
                self.ctx_set_summ_freq(c, v.wrapping_add(cf as u16));
            }
            let s = self.ctx_stats(c) + ns1 * STATE_SIZE;
            self.st_set_succ(s, successor);
            self.set_byte(s, symbol);
            self.set_byte(s + 1, cf as u8);
            self.ctx_set_num_stats(c, (ns1 + 1) as u16);

            c = self.ctx_suffix(c);
        }
        self.max_context = f_successor;
        self.min_context = f_successor;
    }

    fn rescale(&mut self) {
        let stats = self.ctx_stats(self.min_context);
        let mut s = self.found_state;
        {
            // Save the found state BEFORE shifting: C does `tmp = *s` first.
            let tmp_sym = self.byte(s);
            let tmp_freq = self.byte(s + 1);
            let tmp_lo = self.r16(s + 2);
            let tmp_hi = self.r16(s + 4);
            let mut t = s;
            let mut guard = 0;
            while t != stats {
                self.copy_state(t, t.wrapping_sub(6));
                t = t.wrapping_sub(6);
                guard += 1;
                if guard > 4096 {
                    return;
                }
            }
            // Move the found state to the front.
            self.set_byte(stats, tmp_sym);
            self.set_byte(stats + 1, tmp_freq);
            self.w16(stats + 2, tmp_lo);
            self.w16(stats + 4, tmp_hi);
        }
        s = stats;

        let mut esc_freq =
            (self.ctx_summ_freq(self.min_context) as u32).wrapping_sub(self.st_freq(s) as u32);
        let f = self.st_freq(s) as u32 + 4;
        let adder = u32::from(self.order_fall != 0);
        let f = (f + adder) >> 1;
        self.set_byte(s + 1, f as u8);
        let mut sum_freq = f;

        let mut i = self.ctx_num_stats(self.min_context) as u32 - 1;
        loop {
            s = s.wrapping_add(6);
            esc_freq = esc_freq.wrapping_sub(self.st_freq(s) as u32);
            let f = (self.st_freq(s) as u32 + adder) >> 1;
            self.set_byte(s + 1, f as u8);
            sum_freq += f;
            if self.st_freq(s) > self.st_freq(s.wrapping_sub(6)) {
                let tmp_sym = self.byte(s);
                let tmp_freq = self.byte(s + 1);
                let tmp_lo = self.r16(s + 2);
                let tmp_hi = self.r16(s + 4);
                let mut s1 = s;
                loop {
                    self.copy_state(s1, s1.wrapping_sub(6));
                    s1 = s1.wrapping_sub(6);
                    if s1 == stats || tmp_freq <= self.st_freq(s1.wrapping_sub(6)) {
                        break;
                    }
                }
                self.set_byte(s1, tmp_sym);
                self.set_byte(s1 + 1, tmp_freq);
                self.w16(s1 + 2, tmp_lo);
                self.w16(s1 + 4, tmp_hi);
            }
            i -= 1;
            if i == 0 {
                break;
            }
        }

        if self.st_freq(s) == 0 {
            let num_stats_orig = self.ctx_num_stats(self.min_context) as u32;
            let mut i2 = 0u32;
            loop {
                i2 += 1;
                s = s.wrapping_sub(6);
                if self.st_freq(s) != 0 || i2 > 4096 {
                    break;
                }
            }
            esc_freq += i2;
            let num_stats = num_stats_orig.saturating_sub(i2);
            self.ctx_set_num_stats(self.min_context, num_stats as u16);
            if num_stats == 1 {
                let tmp_sym = self.byte(stats);
                let mut freq = self.st_freq(stats) as u32;
                let tmp_lo = self.r16(stats + 2);
                let tmp_hi = self.r16(stats + 4);
                loop {
                    freq -= freq >> 1;
                    esc_freq >>= 1;
                    if esc_freq <= 1 {
                        break;
                    }
                }
                self.insert_node(stats, self.u2i((num_stats_orig + 1) >> 1));
                let os = Self::one_state(self.min_context);
                self.set_byte(os, tmp_sym);
                self.set_byte(os + 1, freq as u8);
                self.w16(os + 2, tmp_lo);
                self.w16(os + 4, tmp_hi);
                self.found_state = os;
                return;
            }
            let n0 = (num_stats_orig + 1) >> 1;
            let n1 = (num_stats + 1) >> 1;
            if n0 != n1 {
                let shrunk = self.shrink_units(stats, n0, n1);
                self.ctx_set_stats(self.min_context, shrunk);
            }
        }
        let sf = sum_freq.wrapping_add(esc_freq).wrapping_sub(esc_freq >> 1) as u16;
        self.ctx_set_summ_freq(self.min_context, sf);
        self.found_state = self.ctx_stats(self.min_context);
    }

    /// Returns (see index, escape frequency); see index is None for DummySee.
    fn make_esc_freq(&mut self, num_masked: u32) -> (Option<usize>, u32) {
        let num_stats = self.ctx_num_stats(self.min_context) as u32;
        let non_masked = num_stats - num_masked;
        if num_stats != 256 {
            let suffix_num_stats = self.ctx_num_stats(self.ctx_suffix(self.min_context)) as u32;
            let row = self.ns2indx[(non_masked - 1) as usize] as usize;
            // C compares against the *wrapping* unsigned difference.
            let col = (non_masked < suffix_num_stats.wrapping_sub(num_stats)) as usize
                + 2 * (self.ctx_summ_freq(self.min_context) < 11 * num_stats as u16) as usize
                + 4 * (num_masked > non_masked) as usize
                + self.hi_bits_flag as usize;
            let idx = row * 16 + col;
            let see = self.see[idx];
            let r = (see.summ >> see.shift) as u32;
            self.see[idx].summ = see.summ.wrapping_sub(r as u16);
            (Some(idx), r + u32::from(r == 0))
        } else {
            (None, 1)
        }
    }

    fn next_context(&mut self) {
        let c = self.st_succ(self.found_state);
        if self.order_fall == 0 && c > self.text {
            self.min_context = c;
            self.max_context = c;
        } else {
            self.update_model();
        }
    }

    fn update1(&mut self) {
        let mut s = self.found_state;
        let f = self.st_freq(s) as u32 + 4;
        self.set_byte(s + 1, f as u8);
        let sf = self.ctx_summ_freq(self.min_context);
        self.ctx_set_summ_freq(self.min_context, sf.wrapping_add(4));
        if self.st_freq(s) > self.st_freq(s.wrapping_sub(6)) {
            self.swap_states(s, s.wrapping_sub(6));
            s = s.wrapping_sub(6);
            self.found_state = s;
            if self.st_freq(s) as u32 > MAX_FREQ {
                self.rescale();
            }
        }
        self.next_context();
    }

    fn update1_0(&mut self) {
        self.prev_success = u32::from(
            2 * self.st_freq(self.found_state) as u32 > self.ctx_summ_freq(self.min_context) as u32,
        );
        self.run_length += self.prev_success as i32;
        let sf = self.ctx_summ_freq(self.min_context);
        self.ctx_set_summ_freq(self.min_context, sf.wrapping_add(4));
        let f = self.st_freq(self.found_state) as u32 + 4;
        self.set_byte(self.found_state + 1, f as u8);
        if f > MAX_FREQ {
            self.rescale();
        }
        self.next_context();
    }

    fn update_bin(&mut self) {
        let f = self.st_freq(self.found_state);
        if f < 128 {
            self.set_byte(self.found_state + 1, f + 1);
        }
        self.prev_success = 1;
        self.run_length += 1;
        self.next_context();
    }

    fn update2(&mut self) {
        let sf = self.ctx_summ_freq(self.min_context);
        self.ctx_set_summ_freq(self.min_context, sf.wrapping_add(4));
        let f = self.st_freq(self.found_state) as u32 + 4;
        self.set_byte(self.found_state + 1, f as u8);
        if f > MAX_FREQ {
            self.rescale();
        }
        self.run_length = self.init_rl;
        self.update_model();
    }

    fn see_update(&mut self, idx: usize) {
        let see = &mut self.see[idx];
        if see.shift < PPMD_PERIOD_BITS as u8 {
            see.count = see.count.wrapping_sub(1);
            if see.count == 0 {
                see.summ <<= 1;
                see.count = 3u8.wrapping_shl(see.shift as u32);
                see.shift += 1;
            }
        }
    }

    // ------------------------------------------------------------- symbol API

    /// Decode one symbol; returns -1/-2 on corrupt stream (mirrors the C
    /// sentinel returns, which libarchive treats as hard errors).
    pub fn decode_symbol(&mut self, br: &mut BitReader3) -> Result<i32> {
        let mut char_mask = [0xFFu8; 256];
        if self.ctx_num_stats(self.min_context) != 1 {
            let mut s = self.ctx_stats(self.min_context);
            let count = self.get_threshold(br, self.ctx_summ_freq(self.min_context) as u32)?;
            let mut hi_cnt = self.st_freq(s) as u32;
            if count < hi_cnt {
                self.decode(br, 0, hi_cnt)?;
                self.found_state = s;
                let symbol = self.st_symbol(s);
                self.update1_0();
                return Ok(symbol as i32);
            }
            self.prev_success = 0;
            let mut i = self.ctx_num_stats(self.min_context) as u32 - 1;
            loop {
                s = s.wrapping_add(6);
                hi_cnt += self.st_freq(s) as u32;
                if hi_cnt > count {
                    self.decode(br, hi_cnt - self.st_freq(s) as u32, self.st_freq(s) as u32)?;
                    self.found_state = s;
                    let symbol = self.st_symbol(s);
                    self.update1();
                    return Ok(symbol as i32);
                }
                i -= 1;
                if i == 0 {
                    break;
                }
            }
            if count >= self.ctx_summ_freq(self.min_context) as u32 {
                return Ok(-2);
            }
            self.hi_bits_flag = self.hb2flag[self.st_symbol(self.found_state) as usize] as u32;
            let sf = self.ctx_summ_freq(self.min_context) as u32;
            self.decode(br, hi_cnt, sf.wrapping_sub(hi_cnt))?;
            char_mask.fill(0xFF);
            char_mask[self.st_symbol(s) as usize] = 0;
            let mut i = self.ctx_num_stats(self.min_context) as u32 - 1;
            loop {
                s = s.wrapping_sub(6);
                char_mask[self.st_symbol(s) as usize] = 0;
                i -= 1;
                if i == 0 {
                    break;
                }
            }
        } else {
            let idx = self.bin_summ_index();
            let prob = self.bin_summ[idx];
            if self.decode_bit(br, prob as u32)? == 0 {
                self.bin_summ[idx] =
                    (prob as u32 + (1 << PPMD_INT_BITS) - get_mean(prob as u32)) as u16;
                let os = Self::one_state(self.min_context);
                self.found_state = os;
                let symbol = self.st_symbol(os);
                self.update_bin();
                return Ok(symbol as i32);
            }
            self.bin_summ[idx] = (prob as u32 - get_mean(prob as u32)) as u16;
            self.init_esc = K_EXP_ESCAPE[(self.bin_summ[idx] as usize) >> 10] as u32;
            char_mask.fill(0xFF);
            char_mask[self.st_symbol(Self::one_state(self.min_context)) as usize] = 0;
            self.prev_success = 0;
        }

        // Escape: climb the context suffix chain.
        let mut ps = [0u32; 256];
        loop {
            let num_masked = self.ctx_num_stats(self.min_context) as u32;
            loop {
                self.order_fall += 1;
                let suffix = self.ctx_suffix(self.min_context);
                if suffix == 0 {
                    return Ok(-1);
                }
                self.min_context = suffix;
                if self.ctx_num_stats(self.min_context) as u32 != num_masked {
                    break;
                }
            }

            let mut hi_cnt = 0u32;
            let mut s = self.ctx_stats(self.min_context);
            let mut i = 0usize;
            let num = self.ctx_num_stats(self.min_context) as usize - num_masked as usize;
            let mut guard = 0usize;
            loop {
                if char_mask[self.st_symbol(s) as usize] != 0 {
                    hi_cnt += self.st_freq(s) as u32;
                    ps[i] = s;
                    i += 1;
                }
                s = s.wrapping_add(6);
                guard += 1;
                if i == num || guard > 4096 {
                    break;
                }
            }
            if i != num {
                return Err(Error::Malformed("ppmd escape accounting"));
            }

            let (see_idx, mut freq_sum) = self.make_esc_freq(num_masked);
            freq_sum += hi_cnt;
            let count = self.get_threshold(br, freq_sum)?;

            if count < hi_cnt {
                let mut h = 0u32;
                let mut pi = 0usize;
                loop {
                    h += self.st_freq(ps[pi]) as u32;
                    if h > count {
                        break;
                    }
                    pi += 1;
                    if pi >= ps.len() {
                        return Err(Error::Malformed("ppmd escape pick"));
                    }
                }
                let s = ps[pi];
                self.decode(br, h - self.st_freq(s) as u32, self.st_freq(s) as u32)?;
                if let Some(si) = see_idx {
                    self.see_update(si);
                }
                self.found_state = s;
                let symbol = self.st_symbol(s);
                self.update2();
                return Ok(symbol as i32);
            }
            if count >= freq_sum {
                return Ok(-2);
            }
            self.decode(br, hi_cnt, freq_sum.wrapping_sub(hi_cnt))?;
            if let Some(si) = see_idx {
                let summ = self.see[si].summ;
                self.see[si].summ = summ.wrapping_add(freq_sum as u16);
            }
            loop {
                i -= 1;
                char_mask[self.st_symbol(ps[i]) as usize] = 0;
                if i == 0 {
                    break;
                }
            }
        }
    }

    fn bin_summ_index(&mut self) -> usize {
        let one = Self::one_state(self.min_context);
        let idx = (self.st_freq(one) as usize).saturating_sub(1).min(127);
        let suffix_num_stats = self.ctx_num_stats(self.ctx_suffix(self.min_context));
        let col = self.prev_success as usize
            + self.ns2bsindx[(suffix_num_stats.max(1) - 1) as usize] as usize
            + {
                self.hi_bits_flag = self.hb2flag[self.st_symbol(self.found_state) as usize] as u32;
                self.hi_bits_flag as usize
            }
            + 2 * self.hb2flag[self.st_symbol(one) as usize] as usize
            + ((self.run_length >> 26) & 0x20) as usize;
        idx * 64 + col.min(63)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::unpack3::BitReader3;
    use alloc::vec::Vec;

    /// `Ppmd7::new` refuses a heap smaller than one unit -- the model cannot
    /// be built in it -- and accepts exactly one unit.
    #[test]
    fn new_rejects_a_heap_smaller_than_one_unit() {
        assert!(Ppmd7::new(UNIT_SIZE - 1).is_err());
        assert!(Ppmd7::new(0).is_err());
        assert!(Ppmd7::new(UNIT_SIZE).is_ok());
    }

    /// The RAR range decoder refuses an all-ones code word. `code ==
    /// 0xFFFFFFFF` is the C decoder's sentinel for a stream that cannot be
    /// initialised, and decoding one would divide by a zero range later.
    #[test]
    fn range_init_refuses_an_all_ones_code() {
        let mut p = Ppmd7::new(1 << 16).unwrap();
        let mut br = BitReader3::new(&[0xFF, 0xFF, 0xFF, 0xFF, 0x00]);
        assert!(p.init_range_rar(&mut br).is_err());
    }

    #[test]
    fn range_init_accepts_a_non_saturating_code() {
        let mut p = Ppmd7::new(1 << 16).unwrap();
        let mut br = BitReader3::new(&[0x00, 0x01, 0x02, 0x03]);
        assert!(p.init_range_rar(&mut br).is_ok());
    }

    /// A short buffer is a hard error, not a panic. `init_range_rar` reads
    /// four code bytes and must not walk past the end.
    #[test]
    fn range_init_truncates_cleanly() {
        let mut p = Ppmd7::new(1 << 16).unwrap();
        let mut br = BitReader3::new(&[0x01, 0x02]);
        assert!(p.init_range_rar(&mut br).is_err());
    }

    /// No PPMd-encoded RAR can be crafted on this machine (`rar-codec`
    /// encodes store-only), so the decoder has no known-good vector here.
    /// What it must still have is a floor: a full init-plus-decode cycle over
    /// arbitrary bytes returns an error and never panics, at every model
    /// order and over every length. A panic in this module is a crash in a
    /// search tool reading an untrusted archive.
    #[test]
    fn decode_never_panics_on_arbitrary_bytes() {
        let mut state = 0x9E37_79B9_7F4A_7C15u64;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        for max_order in [2u32, 4, 8, 16] {
            for len in [0usize, 1, 3, 4, 5, 16, 64, 256] {
                let bytes: Vec<u8> = (0..len).map(|_| (next() & 0xFF) as u8).collect();
                let mut p = match Ppmd7::new(1 << 12) {
                    Ok(p) => p,
                    Err(_) => continue,
                };
                p.init_model(max_order, 2);
                let mut br = BitReader3::new(&bytes);
                if p.init_range_rar(&mut br).is_err() {
                    continue;
                }
                for _ in 0..64 {
                    // Err is the documented answer for a corrupt stream; the C
                    // decoder returns a -1/-2 sentinel and libarchive treats
                    // either as a hard error. A panic is the only failure this
                    // test rejects.
                    match p.decode_symbol(&mut br) {
                        Ok(_) => {}
                        Err(_) => break,
                    }
                }
            }
        }
    }
}
