//! Canonical Huffman tables for the RAR5 compressed stream.
//!
//! Ported from libarchive's `create_decode_tables` / `decode_number`
//! (BSD-2-Clause, Grzegorz Antoniak). Codes are canonical: assigned by
//! increasing code length, and by symbol index within one length.

use crate::bitreader::BitReader;
use crate::{Error, Result};
use alloc::vec;
use alloc::vec::Vec;

pub struct DecodeTable {
    size: usize,
    quick_bits: u8,
    /// First code value of each length, left-aligned to 16 bits.
    decode_len: [u32; 16],
    /// Index of first symbol of each length in `decode_num`.
    decode_pos: [u16; 16],
    /// Symbols sorted by (code length, symbol index).
    decode_num: Vec<u16>,
    /// One-entry-per-prefix fast lookup.
    quick_len: Vec<u8>,
    quick_num: Vec<u16>,
}

impl DecodeTable {
    /// Build a decode table from per-symbol bit lengths (0 = unused).
    /// Mirrors `create_decode_tables`.
    pub fn build(bit_length: &[u8], size: usize) -> Result<DecodeTable> {
        if bit_length.len() < size || size == 0 {
            return Err(Error::Malformed("huffman table size mismatch"));
        }
        let quick_bits = if size == 306 { 10 } else { 7 };
        let mut lc = [0usize; 16];
        for &l in &bit_length[..size] {
            lc[(l & 15) as usize] += 1;
        }

        lc[0] = 0;
        let mut decode_len = [0u32; 16];
        let mut decode_pos = [0u16; 16];
        let mut upper_limit = 0usize;
        for i in 1..16 {
            upper_limit += lc[i];
            decode_len[i] = (upper_limit as u32) << (16 - i);
            decode_pos[i] = decode_pos[i - 1].wrapping_add(lc[i - 1] as u16);
            upper_limit <<= 1;
        }
        // Over-subscribed code (Kraft sum > 2^16) is invalid.
        if upper_limit > 65536 {
            return Err(Error::Malformed("over-subscribed huffman code"));
        }

        let mut decode_num = vec![0u16; size];
        let mut clone = decode_pos;
        for (i, &l) in bit_length[..size].iter().enumerate() {
            let clen = (l & 15) as usize;
            if clen > 0 {
                let p = clone[clen] as usize;
                if p < size {
                    decode_num[p] = i as u16;
                }
                clone[clen] = clone[clen].wrapping_add(1);
            }
        }

        // Quick lookup table.
        let quick_data_size = 1usize << quick_bits;
        let mut quick_len = vec![0u8; quick_data_size];
        let mut quick_num = vec![0u16; quick_data_size];
        let mut cur_len = 1usize;
        for code in 0..quick_data_size {
            let bit_field = (code as u32) << (16 - quick_bits);
            while cur_len < 16 && bit_field >= decode_len[cur_len] {
                cur_len += 1;
            }
            quick_len[code] = cur_len as u8;
            let dist = (bit_field - decode_len[cur_len - 1]) >> (16 - cur_len);
            let pos = decode_pos[cur_len & 15] as u32 + dist;
            if cur_len < 16 && (pos as usize) < size {
                quick_num[code] = decode_num[pos as usize];
            }
        }

        Ok(DecodeTable {
            size,
            quick_bits,
            decode_len,
            decode_pos,
            decode_num,
            quick_len,
            quick_num,
        })
    }

    /// Decode one symbol. Mirrors `decode_number`.
    pub fn decode(&self, br: &mut BitReader) -> Result<u16> {
        let bitfield = (br.peek16()? & 0xfffe) as u32;
        let qb = self.quick_bits as u32;

        if bitfield < self.decode_len[self.quick_bits as usize] {
            let code = (bitfield >> (16 - qb)) as usize;
            br.skip(self.quick_len[code] as u32);
            return Ok(self.quick_num[code]);
        }

        let mut bits = 15usize;
        for i in (self.quick_bits as usize + 1)..15 {
            if bitfield < self.decode_len[i] {
                bits = i;
                break;
            }
        }

        br.skip(bits as u32);
        let dist = (bit_field_sub(bitfield, self.decode_len[bits - 1])) >> (16 - bits);
        let mut pos = self.decode_pos[bits] as u32 + dist;
        if pos >= self.size as u32 {
            pos = 0;
        }
        Ok(self.decode_num[pos as usize])
    }
}

#[inline]
fn bit_field_sub(a: u32, b: u32) -> u32 {
    a.wrapping_sub(b)
}
