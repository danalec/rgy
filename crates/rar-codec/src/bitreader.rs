//! MSB-first bit reader over one compressed block's data.
//!
//! Ported from the bit-reader semantics of libarchive's
//! `archive_read_support_format_rar5.c` (BSD-2-Clause, Grzegorz
//! Antoniak): bits are consumed from the top of a big-endian view of
//! the buffer; `peek16`/`peek32` look ahead without consuming;
//! `skip` advances.

use crate::{Error, Result};

pub struct BitReader<'a> {
    data: &'a [u8],
    /// Current byte position.
    pub pos: usize,
    /// Current bit position within the byte (0..8).
    pub bit: u8,
}

impl<'a> BitReader<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        BitReader {
            data,
            pos: 0,
            bit: 0,
        }
    }

    /// Absolute bit position.
    pub fn bit_pos(&self) -> usize {
        self.pos * 8 + self.bit as usize
    }

    /// Peek up to 16 bits at the current position (left-aligned in
    /// the u16, matching `read_bits_16` in the reference
    /// implementation). 8-byte load: the fast path through the
    /// compressed blocks calls `peek16` for every Huffman symbol,
    /// often millions per file, so the three bounds-checks per peek
    /// had to collapse to one.
    pub fn peek16(&self) -> Result<u16> {
        if self.pos + 3 > self.data.len() {
            return Err(Error::Truncated);
        }
        // Zero-fill past `data.len()` so bits beyond the buffer read as
        // zero; matches the original code's behaviour where a truncated
        // stream caused an error rather than a phantom bit.
        let mut buf = [0u8; 8];
        let take = (self.data.len() - self.pos).min(8);
        buf[..take].copy_from_slice(&self.data[self.pos..self.pos + take]);
        let v = u64::from_be_bytes(buf);
        // `v` carries `data[pos..]` in big-endian: byte 0 of the window
        // occupies bits 56..63 (bit 63 = byte 0's MSB, bit 56 = LSB).
        // After shifting right by `48 - bit`, the current bit lands at
        // position 15 of the result, with the next 15 bits below it.
        Ok((v >> (48 - self.bit as u64)) as u16)
    }

    /// Peek 32 bits at the current position (left-aligned, matching
    /// `read_bits_32`). Same single-load strategy as `peek16`.
    pub fn peek32(&self) -> Result<u32> {
        if self.pos + 5 > self.data.len() {
            return Err(Error::Truncated);
        }
        let mut buf = [0u8; 8];
        let take = (self.data.len() - self.pos).min(8);
        buf[..take].copy_from_slice(&self.data[self.pos..self.pos + take]);
        let v = u64::from_be_bytes(buf);
        Ok((v << self.bit as u64 >> 32) as u32)
    }

    /// Advance by `n` bits.
    pub fn skip(&mut self, n: u32) {
        let total = self.bit as u32 + n;
        self.pos += (total >> 3) as usize;
        self.bit = (total & 7) as u8;
    }

    /// Read `n` bits (1..=16), MSB-first.
    pub fn take(&mut self, n: u32) -> Result<u32> {
        if n == 0 || n > 16 {
            return Err(Error::Malformed("bad bit count"));
        }
        let v = self.peek16()?;
        let out = (v >> (16 - n)) as u32;
        self.skip(n);
        Ok(out)
    }
}
