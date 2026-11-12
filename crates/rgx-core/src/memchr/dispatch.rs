// SPDX-License-Identifier: Apache-2.0

//! The dispatch behind [`crate::memchr`].
//!
//! Everything in here asks the same question the rest of the crate asks --
//! *may this CPU run the AVX2 instructions?* -- and the answer comes from
//! [`crate::detect`], which is a CPUID/XGETBV probe written out by hand and
//! cached. There is no `feature = "std"` anywhere in this file, so the build
//! with `std` and the build without it reach the same kernel and produce
//! comparable numbers.
//!
//! **Both kernels are this crate's own.** The vector one is written out with
//! `core::arch` intrinsics -- the same shape the upstream crate's own
//! searchers have: a 32-byte block, one comparison per needle, the
//! comparisons ORed into one mask, and the mask's bits are the candidates.
//! The portable one is a word-at-a-time scan: a `usize` word at a time, with
//! the tail byte by byte. Neither of them needs a crate from outside the
//! tree, and neither of them needs `std`.
//!
//! The two answer the same question the same way, and the tests hold both of
//! them to a naive byte-at-a-time scan, because an oracle that shares a line
//! with the code it judges proves nothing.

use core::fmt;
use core::iter::Rev;

/// Search for the first occurrence of `needle` in `haystack`.
#[inline]
pub fn memchr(needle: u8, haystack: &[u8]) -> Option<usize> {
    #[cfg(target_arch = "x86_64")]
    {
        if crate::detect::avx2() {
            // SAFETY: the probe read the CPU's own feature bits and the
            // operating system's register state, and it said the AVX2
            // instructions may run here.
            return unsafe { x86_64::one_find(needle, haystack) };
        }
    }
    portable::one_find(needle, haystack)
}

/// Search for the last occurrence of `needle` in `haystack`.
#[inline]
pub fn memrchr(needle: u8, haystack: &[u8]) -> Option<usize> {
    #[cfg(target_arch = "x86_64")]
    {
        if crate::detect::avx2() {
            // SAFETY: the probe read the CPU's own feature bits and the
            // operating system's register state, and it said the AVX2
            // instructions may run here.
            return unsafe { x86_64::one_rfind(needle, haystack) };
        }
    }
    portable::one_rfind(needle, haystack)
}

/// Search for the first occurrence of either `needle1` or `needle2`.
#[inline]
pub fn memchr2(needle1: u8, needle2: u8, haystack: &[u8]) -> Option<usize> {
    #[cfg(target_arch = "x86_64")]
    {
        if crate::detect::avx2() {
            // SAFETY: the probe read the CPU's own feature bits and the
            // operating system's register state, and it said the AVX2
            // instructions may run here.
            return unsafe { x86_64::two_find(needle1, needle2, haystack) };
        }
    }
    portable::two_find(needle1, needle2, haystack)
}

/// Search for the last occurrence of either `needle1` or `needle2`.
#[inline]
pub fn memrchr2(needle1: u8, needle2: u8, haystack: &[u8]) -> Option<usize> {
    #[cfg(target_arch = "x86_64")]
    {
        if crate::detect::avx2() {
            // SAFETY: the probe read the CPU's own feature bits and the
            // operating system's register state, and it said the AVX2
            // instructions may run here.
            return unsafe { x86_64::two_rfind(needle1, needle2, haystack) };
        }
    }
    portable::two_rfind(needle1, needle2, haystack)
}

/// Search for the first occurrence of any of the three needles.
#[inline]
pub fn memchr3(needle1: u8, needle2: u8, needle3: u8, haystack: &[u8]) -> Option<usize> {
    #[cfg(target_arch = "x86_64")]
    {
        if crate::detect::avx2() {
            // SAFETY: the probe read the CPU's own feature bits and the
            // operating system's register state, and it said the AVX2
            // instructions may run here.
            return unsafe { x86_64::three_find(needle1, needle2, needle3, haystack) };
        }
    }
    portable::three_find(needle1, needle2, needle3, haystack)
}

/// Search for the last occurrence of any of the three needles.
#[inline]
pub fn memrchr3(needle1: u8, needle2: u8, needle3: u8, haystack: &[u8]) -> Option<usize> {
    #[cfg(target_arch = "x86_64")]
    {
        if crate::detect::avx2() {
            // SAFETY: the probe read the CPU's own feature bits and the
            // operating system's register state, and it said the AVX2
            // instructions may run here.
            return unsafe { x86_64::three_rfind(needle1, needle2, needle3, haystack) };
        }
    }
    portable::three_rfind(needle1, needle2, needle3, haystack)
}

/// Count the occurrences of `needle` in `haystack`.
///
/// This is the counting counterpart of [`memchr`], and it is what the
/// iterator's `count` uses: a scan that pops the bits out of the vector
/// masks counts the whole haystack in one pass, where an iterator that
/// restarts the scan at every hit pays a call per hit.
#[inline]
fn count_one(needle: u8, haystack: &[u8]) -> usize {
    #[cfg(target_arch = "x86_64")]
    {
        if crate::detect::avx2() {
            // SAFETY: the probe read the CPU's own feature bits and the
            // operating system's register state, and it said the AVX2
            // instructions may run here.
            return unsafe { x86_64::one_count(needle, haystack) };
        }
    }
    portable::count(needle, haystack)
}

/// Returns an iterator over all occurrences of the needle in a haystack.
#[inline]
pub fn memchr_iter<'h>(needle: u8, haystack: &'h [u8]) -> Memchr<'h> {
    Memchr::new(needle, haystack)
}

/// Returns an iterator over all occurrences of the needle in a haystack, in
/// reverse.
#[inline]
pub fn memrchr_iter<'h>(needle: u8, haystack: &'h [u8]) -> Rev<Memchr<'h>> {
    Memchr::new(needle, haystack).rev()
}

/// Returns an iterator over all occurrences of the needles in a haystack.
#[inline]
pub fn memchr2_iter<'h>(needle1: u8, needle2: u8, haystack: &'h [u8]) -> Memchr2<'h> {
    Memchr2::new(needle1, needle2, haystack)
}

/// Returns an iterator over all occurrences of the needles in a haystack, in
/// reverse.
#[inline]
pub fn memrchr2_iter<'h>(needle1: u8, needle2: u8, haystack: &'h [u8]) -> Rev<Memchr2<'h>> {
    Memchr2::new(needle1, needle2, haystack).rev()
}

/// Returns an iterator over all occurrences of the needles in a haystack.
#[inline]
pub fn memchr3_iter<'h>(needle1: u8, needle2: u8, needle3: u8, haystack: &'h [u8]) -> Memchr3<'h> {
    Memchr3::new(needle1, needle2, needle3, haystack)
}

/// Returns an iterator over all occurrences of the needles in a haystack, in
/// reverse.
#[inline]
pub fn memrchr3_iter<'h>(
    needle1: u8,
    needle2: u8,
    needle3: u8,
    haystack: &'h [u8],
) -> Rev<Memchr3<'h>> {
    Memchr3::new(needle1, needle2, needle3, haystack).rev()
}

/// The portable scan: a word at a time, with the tail byte by byte.
///
/// **The word is read little-endian whatever the host's byte order is**, so
/// the offset the mask reports is an offset in the haystack and not an
/// offset in the machine's own representation of it. A scan that read the
/// word in the host's order would report the right byte on a little-endian
/// machine and the wrong one on a big-endian one, and the difference would
/// only show up where nobody looks.
mod portable {
    /// The number of bytes one word of the scan covers.
    const WORD: usize = core::mem::size_of::<usize>();

    /// `0x0101..01`: one in every byte.
    const LO: usize = usize::MAX / 255;
    /// `0x8080..80`: the high bit of every byte.
    const HI: usize = LO * 128;
    /// `0x7f7f..7f`: the low seven bits of every byte.
    const LOW7: usize = LO * 127;

    /// The byte `b` in every position of a word.
    #[inline]
    fn splat(b: u8) -> usize {
        LO * (b as usize)
    }

    /// The high bit of every byte of `word` that holds `needle`.
    ///
    /// **The test is `(x & 0x7f) + 0x7f`, and not the usual `x - 0x01..01`.**
    /// The usual one borrows from the byte above, and a borrow shows up as a
    /// false positive in the byte *above* the one that matched: harmless when
    /// the scan wants the first match, and wrong when it wants the last one,
    /// because the false positive is the higher of the two addresses. This
    /// one cannot carry out of a byte -- `0x7f + 0x7f` is `0xfe` -- so no
    /// byte can lend to the one above it, and every bit the mask sets is a
    /// byte that really matched.
    #[inline]
    fn hits_mask(word: usize, needle: u8) -> usize {
        let x = word ^ splat(needle);
        let low = (x & LOW7).wrapping_add(LOW7);
        !(low | x) & HI
    }

    /// The word at the start of `chunk`, read little-endian.
    #[inline]
    fn word_le(chunk: &[u8]) -> usize {
        let mut buf = [0u8; WORD];
        buf.copy_from_slice(chunk);
        usize::from_le_bytes(buf)
    }

    /// The scans, one per needle count.
    ///
    /// The body is the same for all three; only the number of needles
    /// differs, and the mask is the union of the needles' masks, so a byte
    /// that matches either of two needles is a candidate for both.
    macro_rules! scans {
        ($($find:ident $rfind:ident [$($n:ident),+];)+) => {
            $(
                /// The first byte that matches one of the needles.
                pub(super) fn $find($($n: u8),+, haystack: &[u8]) -> Option<usize> {
                    let needles = [$($n),+];
                    let len = haystack.len();
                    let mut base = 0usize;
                    while base + WORD <= len {
                        let word = word_le(&haystack[base..base + WORD]);
                        let mut hits = 0usize;
                        $( hits |= hits_mask(word, $n); )+
                        if hits != 0 {
                            return Some(base + (hits.trailing_zeros() / 8) as usize);
                        }
                        base += WORD;
                    }
                    for (i, &b) in haystack[base..].iter().enumerate() {
                        if needles.contains(&b) {
                            return Some(base + i);
                        }
                    }
                    None
                }

                /// The last byte that matches one of the needles.
                pub(super) fn $rfind($($n: u8),+, haystack: &[u8]) -> Option<usize> {
                    let needles = [$($n),+];
                    let mut end = haystack.len();
                    while end >= WORD {
                        let start = end - WORD;
                        let word = word_le(&haystack[start..end]);
                        let mut hits = 0usize;
                        $( hits |= hits_mask(word, $n); )+
                        if hits != 0 {
                            let byte =
                                ((usize::BITS as usize) - 1 - hits.leading_zeros() as usize) / 8;
                            return Some(start + byte);
                        }
                        end = start;
                    }
                    for i in (0..end).rev() {
                        if needles.contains(&haystack[i]) {
                            return Some(i);
                        }
                    }
                    None
                }
            )+
        };
    }

    scans! {
        one_find  one_rfind  [needle];
        two_find  two_rfind  [needle1, needle2];
        three_find three_rfind [needle1, needle2, needle3];
    }

    /// How many bytes of `haystack` hold `needle`.
    pub(super) fn count(needle: u8, haystack: &[u8]) -> usize {
        let len = haystack.len();
        let mut total = 0usize;
        let mut base = 0usize;
        while base + WORD <= len {
            let word = word_le(&haystack[base..base + WORD]);
            total += hits_mask(word, needle).count_ones() as usize;
            base += WORD;
        }
        for &b in &haystack[base..] {
            if b == needle {
                total += 1;
            }
        }
        total
    }
}

/// The AVX2 kernels, written out with `core::arch`.
///
/// The block is 32 bytes: one unaligned load, one comparison per needle,
/// the comparisons ORed into one mask, and the mask's bits are the
/// candidates. A block with a candidate is answered from the mask alone --
/// the lowest bit for a forward search, the highest for a backward one --
/// because the answer is the *first* (or the *last*) match and not merely a
/// match.
///
/// The `#[target_feature]` on each function is what lets the vector code
/// inline into it: without it, the AVX2 code would be a separate call per
/// search, and the whole point of the exercise is that it is not.
#[cfg(target_arch = "x86_64")]
mod x86_64 {
    use core::arch::x86_64::{
        __m256i, _mm256_cmpeq_epi8, _mm256_loadu_si256, _mm256_movemask_epi8, _mm256_or_si256,
        _mm256_set1_epi8, _mm256_setzero_si256,
    };

    /// The number of bytes one block covers.
    const BLOCK: usize = 32;

    /// The vector kernels, one per needle count.
    ///
    /// The haystack is walked in blocks from the front (or from the back),
    /// and the last block is the same 32 bytes loaded at the end of the
    /// haystack with the lanes the blocks already covered masked off. That
    /// is what keeps the answer exact at the boundary: a load that ran past
    /// the end of the haystack would be reading somebody else's memory.
    macro_rules! kernels {
        ($($find:ident $rfind:ident [$($n:ident => $v:ident),+];)+) => {
            $(
                /// # Safety
                ///
                /// The caller must have established that this CPU can run the
                /// AVX2 instructions.
                #[target_feature(enable = "sse2", enable = "avx2")]
                pub(super) unsafe fn $find($($n: u8),+, haystack: &[u8]) -> Option<usize> {
                    let len = haystack.len();
                    if len < BLOCK {
                        return super::portable::$find($($n),+, haystack);
                    }
                    let ptr = haystack.as_ptr();
                    $( let $v = _mm256_set1_epi8($n as i8); )+
                    let mut at = 0usize;
                    while at + BLOCK <= len {
                        // SAFETY: `at + BLOCK <= len`, so the load is inside the
                        // haystack.
                        let chunk = _mm256_loadu_si256(ptr.add(at) as *const __m256i);
                        let mut hits = _mm256_setzero_si256();
                        $( hits = _mm256_or_si256(hits, _mm256_cmpeq_epi8(chunk, $v)); )+
                        let mask = _mm256_movemask_epi8(hits) as u32;
                        if mask != 0 {
                            return Some(at + mask.trailing_zeros() as usize);
                        }
                        at += BLOCK;
                    }
                    if at == len {
                        return None;
                    }
                    // The tail: the last 32 bytes of the haystack, with the
                    // lanes the blocks already covered masked off.
                    let last = len - BLOCK;
                    // SAFETY: `len >= BLOCK`, so the load is inside the
                    // haystack.
                    let chunk = _mm256_loadu_si256(ptr.add(last) as *const __m256i);
                    let mut hits = _mm256_setzero_si256();
                    $( hits = _mm256_or_si256(hits, _mm256_cmpeq_epi8(chunk, $v)); )+
                    let mut mask = _mm256_movemask_epi8(hits) as u32;
                    mask &= u32::MAX << (at - last);
                    if mask != 0 {
                        return Some(last + mask.trailing_zeros() as usize);
                    }
                    None
                }

                /// # Safety
                ///
                /// The caller must have established that this CPU can run the
                /// AVX2 instructions.
                #[target_feature(enable = "sse2", enable = "avx2")]
                pub(super) unsafe fn $rfind($($n: u8),+, haystack: &[u8]) -> Option<usize> {
                    let len = haystack.len();
                    if len < BLOCK {
                        return super::portable::$rfind($($n),+, haystack);
                    }
                    let ptr = haystack.as_ptr();
                    $( let $v = _mm256_set1_epi8($n as i8); )+
                    let mut end = len;
                    while end >= BLOCK {
                        let at = end - BLOCK;
                        // SAFETY: `end >= BLOCK`, so the load is inside the
                        // haystack.
                        let chunk = _mm256_loadu_si256(ptr.add(at) as *const __m256i);
                        let mut hits = _mm256_setzero_si256();
                        $( hits = _mm256_or_si256(hits, _mm256_cmpeq_epi8(chunk, $v)); )+
                        let mask = _mm256_movemask_epi8(hits) as u32;
                        if mask != 0 {
                            return Some(at + (31 - mask.leading_zeros() as usize));
                        }
                        end = at;
                    }
                    // The tail: the first `end` bytes, which are the ones the
                    // blocks did not cover.
                    // SAFETY: `len >= BLOCK`, so the load is inside the
                    // haystack.
                    let chunk = _mm256_loadu_si256(ptr as *const __m256i);
                    let mut hits = _mm256_setzero_si256();
                    $( hits = _mm256_or_si256(hits, _mm256_cmpeq_epi8(chunk, $v)); )+
                    let mut mask = _mm256_movemask_epi8(hits) as u32;
                    mask &= (1u32 << end) - 1;
                    if mask != 0 {
                        return Some(31 - mask.leading_zeros() as usize);
                    }
                    None
                }
            )+
        };
    }

    kernels! {
        one_find one_rfind [needle => vneedle];
        two_find two_rfind [needle1 => vneedle1, needle2 => vneedle2];
        three_find three_rfind [needle1 => vneedle1, needle2 => vneedle2, needle3 => vneedle3];
    }

    /// How many bytes of `haystack` hold `needle`.
    ///
    /// The count is the mask's bits popped out of it: one pass over the
    /// haystack, where an iterator that restarts the scan at every hit pays
    /// a call per hit.
    ///
    /// # Safety
    ///
    /// The caller must have established that this CPU can run the AVX2
    /// instructions.
    #[target_feature(enable = "sse2", enable = "avx2")]
    pub(super) unsafe fn one_count(needle: u8, haystack: &[u8]) -> usize {
        let len = haystack.len();
        if len < BLOCK {
            return super::portable::count(needle, haystack);
        }
        let ptr = haystack.as_ptr();
        let vneedle = _mm256_set1_epi8(needle as i8);
        let mut total = 0usize;
        let mut at = 0usize;
        while at + BLOCK <= len {
            // SAFETY: `at + BLOCK <= len`, so the load is inside the haystack.
            let chunk = _mm256_loadu_si256(ptr.add(at) as *const __m256i);
            let mask = _mm256_movemask_epi8(_mm256_cmpeq_epi8(chunk, vneedle)) as u32;
            total += mask.count_ones() as usize;
            at += BLOCK;
        }
        if at == len {
            return total;
        }
        let last = len - BLOCK;
        // SAFETY: `len >= BLOCK`, so the load is inside the haystack.
        let chunk = _mm256_loadu_si256(ptr.add(last) as *const __m256i);
        let mask = _mm256_movemask_epi8(_mm256_cmpeq_epi8(chunk, vneedle)) as u32;
        total += (mask & (u32::MAX << (at - last))).count_ones() as usize;
        total
    }
}

/// The three iterators, which differ only in how many needles they carry.
///
/// The upstream crate's iterators are a restart loop over the search
/// function, and this is the same shape: each `next` re-enters the search
/// at the byte after the last hit. The one special case is the single-byte
/// iterator's `count`, which the upstream crate also specialises -- a scan
/// that pops the bits out of the vector masks counts the whole haystack in
/// one pass, where an iterator that restarts the scan at every hit pays a
/// call per hit.
macro_rules! byte_iterator {
    (
        $(#[$doc:meta])*
        $name:ident,
        needles: [$($needle:ident),+],
        find: $find:path,
        rfind: $rfind:path,
        count: $count:path $(,)?
    ) => {
        $(#[$doc])*
        #[derive(Clone)]
        pub struct $name<'h> {
            $($needle: u8,)+
            haystack: &'h [u8],
            /// The offset of the first byte still to be searched.
            front: usize,
            /// The offset just past the last byte still to be searched.
            back: usize,
        }

        impl<'h> $name<'h> {
            /// Returns an iterator over all occurrences of the needle bytes
            /// in the given haystack.
            ///
            /// The iterator returned implements `DoubleEndedIterator`. This
            /// means it can also be used to find occurrences in reverse
            /// order.
            #[inline]
            pub fn new($($needle: u8),+, haystack: &'h [u8]) -> $name<'h> {
                $name {
                    $($needle,)+
                    haystack,
                    front: 0,
                    back: haystack.len(),
                }
            }
        }

        impl<'h> Iterator for $name<'h> {
            type Item = usize;

            #[inline]
            fn next(&mut self) -> Option<usize> {
                if self.front >= self.back {
                    return None;
                }
                let at = $find(
                    $(self.$needle),+,
                    &self.haystack[self.front..self.back],
                )?;
                self.front += at + 1;
                Some(self.front - 1)
            }

            #[inline]
            fn count(self) -> usize {
                $count(
                    $(self.$needle),+,
                    &self.haystack[self.front..self.back],
                )
            }

            #[inline]
            fn size_hint(&self) -> (usize, Option<usize>) {
                (0, Some(self.back - self.front))
            }
        }

        impl<'h> DoubleEndedIterator for $name<'h> {
            #[inline]
            fn next_back(&mut self) -> Option<usize> {
                if self.front >= self.back {
                    return None;
                }
                let at = $rfind(
                    $(self.$needle),+,
                    &self.haystack[self.front..self.back],
                )?;
                self.back = self.front + at;
                Some(self.back)
            }
        }

        impl<'h> core::iter::FusedIterator for $name<'h> {}

        impl<'h> fmt::Debug for $name<'h> {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.debug_struct(stringify!($name))
                    .field("needles", &[$(&self.$needle),+])
                    .field("at", &self.front)
                    .finish()
            }
        }
    };
}

/// The counting counterpart of the two- and three-byte searches.
///
/// The upstream crate has no `count_raw` for them either, so the count is
/// the iterator's own: one search per hit.
fn count_many(needles: &[u8], haystack: &[u8]) -> usize {
    let mut n = 0usize;
    let mut front = 0usize;
    while front < haystack.len() {
        let at = match needles {
            [a, b] => memchr2(*a, *b, &haystack[front..]),
            [a, b, c] => memchr3(*a, *b, *c, &haystack[front..]),
            _ => break,
        };
        match at {
            Some(at) => {
                n += 1;
                front += at + 1;
            }
            None => break,
        }
    }
    n
}

/// The count for the two-byte iterator.
fn count_two(needle1: u8, needle2: u8, haystack: &[u8]) -> usize {
    count_many(&[needle1, needle2], haystack)
}

/// The count for the three-byte iterator.
fn count_three(needle1: u8, needle2: u8, needle3: u8, haystack: &[u8]) -> usize {
    count_many(&[needle1, needle2, needle3], haystack)
}

byte_iterator! {
    /// An iterator over all occurrences of a single byte in a haystack.
    ///
    /// This iterator implements `DoubleEndedIterator`, which means it can also
    /// be used to find occurrences in reverse order.
    Memchr,
    needles: [needle],
    find: memchr,
    rfind: memrchr,
    count: count_one,
}

byte_iterator! {
    /// An iterator over all occurrences of two possible bytes in a haystack.
    ///
    /// This iterator implements `DoubleEndedIterator`, which means it can also
    /// be used to find occurrences in reverse order.
    Memchr2,
    needles: [needle1, needle2],
    find: memchr2,
    rfind: memrchr2,
    count: count_two,
}

byte_iterator! {
    /// An iterator over all occurrences of three possible bytes in a
    /// haystack.
    ///
    /// This iterator implements `DoubleEndedIterator`, which means it can also
    /// be used to find occurrences in reverse order.
    Memchr3,
    needles: [needle1, needle2, needle3],
    find: memchr3,
    rfind: memrchr3,
    count: count_three,
}

#[cfg(test)]
mod tests {
    use alloc::vec::Vec;

    /// The number of bytes one word of the portable scan covers.
    const WORD: usize = core::mem::size_of::<usize>();

    /// The oracle: a byte at a time.
    ///
    /// It is three lines and it is obviously right, and that is the whole
    /// point of an oracle -- it must not share a single line with the code
    /// it judges. The upstream crate cannot be the oracle any more: it is
    /// the thing this module replaces, and an oracle that *is* the
    /// implementation proves only that the implementation agrees with
    /// itself.
    fn naive_find(needle: u8, haystack: &[u8]) -> Option<usize> {
        haystack.iter().position(|&b| b == needle)
    }

    /// The oracle for the last occurrence.
    fn naive_rfind(needle: u8, haystack: &[u8]) -> Option<usize> {
        haystack.iter().rposition(|&b| b == needle)
    }

    /// The oracle for two needles.
    fn naive_find2(needle1: u8, needle2: u8, haystack: &[u8]) -> Option<usize> {
        haystack.iter().position(|&b| b == needle1 || b == needle2)
    }

    /// The oracle for two needles, backwards.
    fn naive_rfind2(needle1: u8, needle2: u8, haystack: &[u8]) -> Option<usize> {
        haystack.iter().rposition(|&b| b == needle1 || b == needle2)
    }

    /// The oracle for three needles.
    fn naive_find3(needle1: u8, needle2: u8, needle3: u8, haystack: &[u8]) -> Option<usize> {
        haystack
            .iter()
            .position(|&b| b == needle1 || b == needle2 || b == needle3)
    }

    /// The oracle for three needles, backwards.
    fn naive_rfind3(needle1: u8, needle2: u8, needle3: u8, haystack: &[u8]) -> Option<usize> {
        haystack
            .iter()
            .rposition(|&b| b == needle1 || b == needle2 || b == needle3)
    }

    /// The oracle for the iterator: every offset, from the front.
    fn naive_iter(needle: u8, haystack: &[u8]) -> Vec<usize> {
        haystack
            .iter()
            .enumerate()
            .filter(|(_, &b)| b == needle)
            .map(|(at, _)| at)
            .collect()
    }

    /// The oracle for the reverse iterator: every offset, from the back.
    fn naive_riter(needle: u8, haystack: &[u8]) -> Vec<usize> {
        let mut at: Vec<usize> = naive_iter(needle, haystack);
        at.reverse();
        at
    }

    /// The oracle for the two-needle iterator.
    fn naive_iter2(needle1: u8, needle2: u8, haystack: &[u8]) -> Vec<usize> {
        haystack
            .iter()
            .enumerate()
            .filter(|(_, &b)| b == needle1 || b == needle2)
            .map(|(at, _)| at)
            .collect()
    }

    /// The oracle for the two-needle iterator, backwards.
    fn naive_riter2(needle1: u8, needle2: u8, haystack: &[u8]) -> Vec<usize> {
        let mut at: Vec<usize> = naive_iter2(needle1, needle2, haystack);
        at.reverse();
        at
    }

    /// The oracle for the three-needle iterator.
    fn naive_iter3(needle1: u8, needle2: u8, needle3: u8, haystack: &[u8]) -> Vec<usize> {
        haystack
            .iter()
            .enumerate()
            .filter(|(_, &b)| b == needle1 || b == needle2 || b == needle3)
            .map(|(at, _)| at)
            .collect()
    }

    /// The oracle for the three-needle iterator, backwards.
    fn naive_riter3(needle1: u8, needle2: u8, needle3: u8, haystack: &[u8]) -> Vec<usize> {
        let mut at: Vec<usize> = naive_iter3(needle1, needle2, needle3, haystack);
        at.reverse();
        at
    }

    /// The haystacks every path is checked against.
    ///
    /// The shapes are the ones that break a scanner: the empty haystack, the
    /// single byte, the needle at the start, at the end and in the middle,
    /// the needle that is not there at all, the needle that is the zero
    /// byte, the haystack shorter than a word, the haystack exactly a word
    /// long, the haystack one byte over a word, and the vector block's own
    /// boundaries.
    fn corpus() -> Vec<Vec<u8>> {
        vec![
            Vec::new(),
            vec![b'a'],
            vec![b'x'],
            // The needle at the start, at the end, and in the middle.
            b"a needle in a haystack".to_vec(),
            b"a haystack with a needle".to_vec(),
            b"a haystack with a needle in it".to_vec(),
            // The needle that is not there at all.
            b"nothing to see here".to_vec(),
            // The zero byte, alone and inside.
            vec![0],
            b"a\0b\0c".to_vec(),
            // Around the word's own boundaries.
            vec![b'x'; WORD - 1],
            vec![b'x'; WORD],
            vec![b'x'; WORD + 1],
            // Around the vector block's own boundaries.
            vec![b'x'; 31],
            vec![b'x'; 32],
            vec![b'x'; 33],
            vec![b'x'; 64],
            vec![b'x'; 65],
            // A haystack that is entirely the needle.
            vec![b'a'; 40],
        ]
    }

    /// The portable scan and the naive one must answer the same question the
    /// same way.
    ///
    /// The portable scan is the one every CPU without AVX2 runs, and it is
    /// the one the vector path is checked against, so it is the one that has
    /// to be right on its own.
    #[test]
    fn the_portable_scan_agrees_with_the_naive_one() {
        for hay in corpus() {
            for &needle in &[b'a', b'x', b'z', 0] {
                assert_eq!(
                    super::portable::one_find(needle, &hay),
                    naive_find(needle, &hay),
                    "find({needle:?}) over {} bytes",
                    hay.len()
                );
                assert_eq!(
                    super::portable::one_rfind(needle, &hay),
                    naive_rfind(needle, &hay),
                    "rfind({needle:?}) over {} bytes",
                    hay.len()
                );
                assert_eq!(
                    super::portable::count(needle, &hay),
                    naive_iter(needle, &hay).len(),
                    "count({needle:?}) over {} bytes",
                    hay.len()
                );
            }
            for &(n1, n2) in &[(b'a', b'x'), (b'a', b'a'), (b'z', 0u8)] {
                assert_eq!(
                    super::portable::two_find(n1, n2, &hay),
                    naive_find2(n1, n2, &hay),
                    "find2({n1:?}, {n2:?}) over {} bytes",
                    hay.len()
                );
                assert_eq!(
                    super::portable::two_rfind(n1, n2, &hay),
                    naive_rfind2(n1, n2, &hay),
                    "rfind2({n1:?}, {n2:?}) over {} bytes",
                    hay.len()
                );
            }
            for &(n1, n2, n3) in &[(b'a', b'x', b'z'), (b'a', b'a', b'a'), (b'a', b'x', b'a')] {
                assert_eq!(
                    super::portable::three_find(n1, n2, n3, &hay),
                    naive_find3(n1, n2, n3, &hay),
                    "find3({n1:?}, {n2:?}, {n3:?}) over {} bytes",
                    hay.len()
                );
                assert_eq!(
                    super::portable::three_rfind(n1, n2, n3, &hay),
                    naive_rfind3(n1, n2, n3, &hay),
                    "rfind3({n1:?}, {n2:?}, {n3:?}) over {} bytes",
                    hay.len()
                );
            }
        }
    }

    /// The vector scan and the naive one must answer the same question the
    /// same way.
    ///
    /// The vector path only runs on a CPU that has AVX2, so the check runs
    /// only there: on a machine without it the portable path is the whole
    /// answer, and the oracle holds it to that alone.
    #[cfg(target_arch = "x86_64")]
    #[test]
    fn the_vector_scan_agrees_with_the_naive_one() {
        if !crate::detect::avx2() {
            return;
        }
        for hay in corpus() {
            for &needle in &[b'a', b'x', b'z', 0] {
                // SAFETY: the probe above established that this CPU can run
                // the AVX2 instructions.
                unsafe {
                    assert_eq!(
                        super::x86_64::one_find(needle, &hay),
                        naive_find(needle, &hay),
                        "one_find({needle:?}) over {} bytes",
                        hay.len()
                    );
                    assert_eq!(
                        super::x86_64::one_rfind(needle, &hay),
                        naive_rfind(needle, &hay),
                        "one_rfind({needle:?}) over {} bytes",
                        hay.len()
                    );
                    assert_eq!(
                        super::x86_64::one_count(needle, &hay),
                        naive_iter(needle, &hay).len(),
                        "one_count({needle:?}) over {} bytes",
                        hay.len()
                    );
                }
            }
            for &(n1, n2) in &[(b'a', b'x'), (b'a', b'a'), (b'z', 0u8)] {
                // SAFETY: the probe above established that this CPU can run
                // the AVX2 instructions.
                unsafe {
                    assert_eq!(
                        super::x86_64::two_find(n1, n2, &hay),
                        naive_find2(n1, n2, &hay),
                        "two_find({n1:?}, {n2:?}) over {} bytes",
                        hay.len()
                    );
                    assert_eq!(
                        super::x86_64::two_rfind(n1, n2, &hay),
                        naive_rfind2(n1, n2, &hay),
                        "two_rfind({n1:?}, {n2:?}) over {} bytes",
                        hay.len()
                    );
                }
            }
            for &(n1, n2, n3) in &[(b'a', b'x', b'z'), (b'a', b'a', b'a'), (b'a', b'x', b'a')] {
                // SAFETY: the probe above established that this CPU can run
                // the AVX2 instructions.
                unsafe {
                    assert_eq!(
                        super::x86_64::three_find(n1, n2, n3, &hay),
                        naive_find3(n1, n2, n3, &hay),
                        "three_find({n1:?}, {n2:?}, {n3:?}) over {} bytes",
                        hay.len()
                    );
                    assert_eq!(
                        super::x86_64::three_rfind(n1, n2, n3, &hay),
                        naive_rfind3(n1, n2, n3, &hay),
                        "three_rfind({n1:?}, {n2:?}, {n3:?}) over {} bytes",
                        hay.len()
                    );
                }
            }
        }
    }

    /// The dispatch and the naive one must answer the same question the same
    /// way.
    ///
    /// This is the check that covers whichever kernel the machine running the
    /// test has, so it is the one that holds the whole module to the oracle
    /// on every machine.
    #[test]
    fn the_dispatch_agrees_with_the_naive_one() {
        for hay in corpus() {
            for &needle in &[b'a', b'x', b'z', 0] {
                assert_eq!(
                    super::memchr(needle, &hay),
                    naive_find(needle, &hay),
                    "memchr({needle:?}) over {} bytes",
                    hay.len()
                );
                assert_eq!(
                    super::memrchr(needle, &hay),
                    naive_rfind(needle, &hay),
                    "memrchr({needle:?}) over {} bytes",
                    hay.len()
                );
                assert_eq!(
                    super::memchr_iter(needle, &hay).collect::<Vec<_>>(),
                    naive_iter(needle, &hay),
                    "memchr_iter({needle:?}) over {} bytes",
                    hay.len()
                );
                assert_eq!(
                    super::memchr_iter(needle, &hay).count(),
                    naive_iter(needle, &hay).len(),
                    "memchr_iter({needle:?}).count() over {} bytes",
                    hay.len()
                );
                assert_eq!(
                    super::memrchr_iter(needle, &hay).collect::<Vec<_>>(),
                    naive_riter(needle, &hay),
                    "memrchr_iter({needle:?}) over {} bytes",
                    hay.len()
                );
            }
            for &(n1, n2) in &[(b'a', b'x'), (b'a', b'a'), (b'z', 0u8)] {
                assert_eq!(
                    super::memchr2(n1, n2, &hay),
                    naive_find2(n1, n2, &hay),
                    "memchr2({n1:?}, {n2:?}) over {} bytes",
                    hay.len()
                );
                assert_eq!(
                    super::memrchr2(n1, n2, &hay),
                    naive_rfind2(n1, n2, &hay),
                    "memrchr2({n1:?}, {n2:?}) over {} bytes",
                    hay.len()
                );
                assert_eq!(
                    super::memchr2_iter(n1, n2, &hay).collect::<Vec<_>>(),
                    naive_iter2(n1, n2, &hay),
                    "memchr2_iter({n1:?}, {n2:?}) over {} bytes",
                    hay.len()
                );
                assert_eq!(
                    super::memchr2_iter(n1, n2, &hay).count(),
                    naive_iter2(n1, n2, &hay).len(),
                    "memchr2_iter({n1:?}, {n2:?}).count() over {} bytes",
                    hay.len()
                );
                assert_eq!(
                    super::memrchr2_iter(n1, n2, &hay).collect::<Vec<_>>(),
                    naive_riter2(n1, n2, &hay),
                    "memrchr2_iter({n1:?}, {n2:?}) over {} bytes",
                    hay.len()
                );
            }
            for &(n1, n2, n3) in &[(b'a', b'x', b'z'), (b'a', b'a', b'a'), (b'a', b'x', b'a')] {
                assert_eq!(
                    super::memchr3(n1, n2, n3, &hay),
                    naive_find3(n1, n2, n3, &hay),
                    "memchr3({n1:?}, {n2:?}, {n3:?}) over {} bytes",
                    hay.len()
                );
                assert_eq!(
                    super::memrchr3(n1, n2, n3, &hay),
                    naive_rfind3(n1, n2, n3, &hay),
                    "memrchr3({n1:?}, {n2:?}, {n3:?}) over {} bytes",
                    hay.len()
                );
                assert_eq!(
                    super::memchr3_iter(n1, n2, n3, &hay).collect::<Vec<_>>(),
                    naive_iter3(n1, n2, n3, &hay),
                    "memchr3_iter({n1:?}, {n2:?}, {n3:?}) over {} bytes",
                    hay.len()
                );
                assert_eq!(
                    super::memchr3_iter(n1, n2, n3, &hay).count(),
                    naive_iter3(n1, n2, n3, &hay).len(),
                    "memchr3_iter({n1:?}, {n2:?}, {n3:?}).count() over {} bytes",
                    hay.len()
                );
                assert_eq!(
                    super::memrchr3_iter(n1, n2, n3, &hay).collect::<Vec<_>>(),
                    naive_riter3(n1, n2, n3, &hay),
                    "memrchr3_iter({n1:?}, {n2:?}, {n3:?}) over {} bytes",
                    hay.len()
                );
            }
        }
    }

    /// The iterator must yield the same offsets whether it is walked from
    /// the front, from the back, or both.
    ///
    /// The forward and the backward walks share one pair of cursors, so a
    /// walk from both ends has to leave the middle alone -- and the set of
    /// offsets it yields has to be the same set the forward walk yields.
    #[test]
    fn the_iterator_walks_the_same_offsets_from_both_ends() {
        let hay = b"the quick brown fox jumps over the lazy dog";
        let forward: Vec<usize> = super::memchr_iter(b'o', hay).collect();
        let backward: Vec<usize> = super::memrchr_iter(b'o', hay).collect();
        let mut both = super::memchr_iter(b'o', hay);
        let mut mixed = Vec::new();
        while let Some(at) = both.next() {
            mixed.push(at);
            if let Some(at) = both.next_back() {
                mixed.push(at);
            }
        }
        mixed.sort_unstable();
        assert_eq!(forward, mixed);
        let mut backward_sorted = backward.clone();
        backward_sorted.sort_unstable();
        assert_eq!(forward, backward_sorted);
    }
}
