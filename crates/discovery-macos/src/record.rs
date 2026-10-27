// SPDX-License-Identifier: Apache-2.0

//! The records `getattrlistbulk` writes, read without trusting them.
//!
//! The kernel writes the records and says how many there are; the reader
//! has to believe the count and the lengths, and a kernel that disagrees
//! with itself is not a thing a file walk should panic on. Every read here
//! is checked, and a record that does not fit ends the listing.
//!
//! The functions are plain functions over bytes, with no platform in
//! them, so they are tested on every platform rather than only on a Mac
//! (the walk itself is `cfg(target_os = "macos")` -- see `walk.rs`).

use alloc::vec::Vec;

/// The 32-bit word at `at`, if the buffer holds it.
pub fn word32(b: &[u8], at: usize) -> Option<u32> {
    let w = b.get(at..at + 4)?;
    Some(u32::from_ne_bytes(w.try_into().unwrap()))
}

/// The 64-bit word at `at`, if the buffer holds it.
pub fn word64(b: &[u8], at: usize) -> Option<u64> {
    let w = b.get(at..at + 8)?;
    Some(u64::from_ne_bytes(w.try_into().unwrap()))
}

/// The smallest a record can be: the length and the common attributes.
pub const MIN_RECORD: usize = 8;

/// The byte ranges of the records in `buf`.
///
/// `count` is the number the kernel said it wrote. A record whose length
/// runs past the end of the buffer ends the listing: the walk carries on
/// with the records it can trust, and the rest of the directory is read
/// on the next pass.
pub fn records(buf: &[u8], count: usize) -> Vec<(usize, usize)> {
    let mut out = Vec::with_capacity(count);
    let mut p = 0usize;
    for _ in 0..count {
        let Some(len) = word32(buf, p).map(|v| v as usize) else {
            break;
        };
        let Some(end) = p.checked_add(len) else {
            break;
        };
        if len < MIN_RECORD || end > buf.len() {
            break;
        }
        out.push((p, end));
        p = end;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A buffer of `n` records, each `len` bytes long.
    ///
    /// The length is the size of the whole record, the four bytes of the
    /// length included -- the way `getattrlistbulk` writes it.
    fn buf(records: &[usize]) -> Vec<u8> {
        let mut b = Vec::new();
        for &len in records {
            let start = b.len();
            b.extend_from_slice(&len.to_ne_bytes());
            b.resize(start + len, 0);
        }
        b
    }

    /// The records of a well-formed listing are all found.
    #[test]
    fn the_records_of_a_listing_are_found() {
        let b = buf(&[16, 16, 16]);
        let r = records(&b, 3);
        assert_eq!(r, vec![(0, 16), (16, 32), (32, 48)]);
    }

    /// A record whose length runs past the end of the buffer ends the
    /// listing: the walk reads what it can trust.
    ///
    /// The length is a value the kernel wrote, and reading past the end
    /// of the buffer is a panic on a value that came from the kernel.
    #[test]
    fn a_record_that_does_not_fit_ends_the_listing() {
        // The second record claims 64 bytes; the buffer holds 16.
        let mut b = buf(&[16]);
        b.extend_from_slice(&64u32.to_ne_bytes());
        b.resize(20, 0);
        let r = records(&b, 2);
        assert_eq!(
            r,
            vec![(0, 16)],
            "the first record is read; the second does not fit"
        );
    }

    /// A record shorter than the fields it claims to hold is not a record.
    #[test]
    fn a_record_that_is_too_short_ends_the_listing() {
        let mut b = buf(&[16]);
        b.extend_from_slice(&4u32.to_ne_bytes());
        b.resize(20, 0);
        let r = records(&b, 2);
        assert_eq!(r, vec![(0, 16)], "the second record is too short to be one");
    }

    /// A length that would overflow the offset does not wrap around.
    #[test]
    fn a_length_that_would_overflow_ends_the_listing() {
        let mut b = buf(&[16]);
        b.extend_from_slice(&u32::MAX.to_ne_bytes());
        b.resize(20, 0);
        let r = records(&b, 2);
        assert_eq!(r, vec![(0, 16)], "the length does not wrap into the buffer");
    }

    /// More records than the buffer holds is not a reason to read past it.
    #[test]
    fn more_records_than_the_buffer_holds_ends_the_listing() {
        let b = buf(&[16]);
        let r = records(&b, 100);
        assert_eq!(r, vec![(0, 16)]);
    }

    /// The words are read when they are there, and not read when they are
    /// not.
    #[test]
    fn a_word_is_read_only_when_the_buffer_holds_it() {
        let b = [1u8, 2, 3, 4, 5, 6, 7, 8];
        assert_eq!(word32(&b, 0), Some(u32::from_ne_bytes([1, 2, 3, 4])));
        assert_eq!(
            word64(&b, 0),
            Some(u64::from_ne_bytes([1, 2, 3, 4, 5, 6, 7, 8]))
        );
        assert_eq!(word32(&b, 5), None, "the word would run past the end");
        assert_eq!(word64(&b, 1), None);
        assert_eq!(word32(&b, 100), None);
    }
}
