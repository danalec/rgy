//! RAR5 variable-length integers: 7 payload bits per byte, high bit = more
//! bytes follow. At most 10 bytes (70 bits; the 10th byte may only use its
//! lowest bit for a u64).

use crate::{Error, Result};
use alloc::vec::Vec;

/// Read a vint from `buf`, advancing `pos`. `None` on truncation/overflow.
pub(crate) fn read(buf: &[u8], pos: &mut usize) -> Option<u64> {
    let mut v: u64 = 0;
    let mut i = 0;
    loop {
        let &b = buf.get(*pos)?;
        *pos += 1;
        if i == 9 && b > 1 {
            // 10th byte: only 1 payload bit remains in a u64
            return None;
        }
        v |= ((b & 0x7F) as u64) << (i * 7);
        if b & 0x80 == 0 {
            return Some(v);
        }
        i += 1;
        if i >= 10 {
            return None;
        }
    }
}

/// Append `v` as a vint to `out`.
pub(crate) fn write(out: &mut Vec<u8>, mut v: u64) {
    loop {
        let mut b = (v & 0x7F) as u8;
        v >>= 7;
        if v != 0 {
            b |= 0x80;
        }
        out.push(b);
        if v == 0 {
            break;
        }
    }
}

/// Read a vint or fail with a malformed-header error.
pub(crate) fn read_r(buf: &[u8], pos: &mut usize) -> Result<u64> {
    read(buf, pos).ok_or(Error::Malformed("truncated vint"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec::Vec;

    #[test]
    fn roundtrip_values() {
        for v in [0u64, 1, 0x7F, 0x80, 0x3FFF, 0x4000, 123_456_789, u64::MAX] {
            let mut buf = Vec::new();
            write(&mut buf, v);
            let mut p = 0;
            assert_eq!(read(&buf, &mut p), Some(v));
            assert_eq!(p, buf.len());
        }
    }

    #[test]
    fn rejects_overlong() {
        let buf = [0xFFu8; 10];
        let mut p = 0;
        assert_eq!(read(&buf, &mut p), None);
    }
}
