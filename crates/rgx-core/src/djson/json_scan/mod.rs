//! Structural JSON scanner, vendored from upstream `src/b02/json_scan` at 841d589.
//!
//! **The name is upstream's, kept on purpose.** The suite this sits in is
//! `crate::djson`; the scanner inside it is `json_scan`, which is what the
//! pin in `scripts/vendored-sources.tsv` names and what upstream calls it. An
//! umbrella name on the module itself would hide the lineage the pin exists
//! to preserve.
//!
//! **Provenance.** upstream is the source of truth. This is a byte-for-byte copy
//! of that commit's `src/b02/json_scan/{mod,avx2,tests}.rs` with the two
//! copy-time changes noted on `mod detect` below, and nothing else.
//!
//! **The name.** It is called `djson` and it is NOT a JSON parser. It answers
//! one question -- which bytes of a document are structural, and which are
//! string delimiters -- as one bit per byte for each, so that a stage-2 parser
//! walks the two bitsets and never looks at the document again. The `d` is
//! for "not a `json` yet", which is the honest description: the stage-2 is not
//! written, and `crate::json` is still the parser the six call sites use.
//!
//! **Where the speed is.** Every byte of a document is classified sixteen at a
//! time on the AVX2 path, and the string state crosses block boundaries in a
//! carry-less multiply rather than byte by byte.

//! Stage 1 of a two-stage JSON parser: the structural index.
//!
//! Given a byte slice, produce a bit per byte saying whether that byte is a
//! **structural position** — one the grammar can act on — and a second bit per
//! byte saying whether it is a **string delimiter**. Everything a stage-2
//! parser has to do is walk those two bitsets; nothing else in this module
//! interprets the document.
//!
//! | bit | set when |
//! |-----|----------|
//! | `structural` | the byte is `{ } [ ] : ,` or whitespace **and** it is not inside a string, **or** it is a quote that opens or closes a string |
//! | `quote` | the byte is a quote that opens or closes a string |
//!
//! A quote that is *escaped* (`\"`) is neither: it is a byte of string
//! content. That is the whole reason the scanner exists — the quote that
//! delimits a string and the quote that a `\` makes literal are the same byte,
//! and only the backslash run in front of them tells them apart. An unescaped
//! quote outside a string is still a delimiter (it *opens* a string), so the
//! state is a toggle, not a flag: a lone `"` at offset 0 sets both bits.
//!
//! ## The predicate
//!
//! For a byte `b` at offset `i`, with `in_string` the state *before* `b`:
//!
//! ```text
//! escaped(i)    = (the run of backslashes ending at i-1 has odd length)
//! boundary(i)   = b is '"' and not escaped(i)
//! structural(i) = boundary(i) or (b is ws or punctuation and not in_string)
//! quote(i)      = boundary(i)
//! ```
//!
//! and the state advances by `in_string ^= boundary(i)` plus the backslash
//! run bookkeeping. [`crate::djson::json_scan::scan_scalar`] is that, one byte at a time, in the
//! clearest possible form. Everything else here is a faster spelling of it,
//! and the tests hold all of them to it.
//!
//! ## Why a union of rectangles, not one lookup pair
//!
//! The standard `pshufb` trick ("shufti": one 16-byte table per nibble, ANDed)
//! is a **rectangle detector** — it selects exactly `{b : b & 0x0F in L,
//! b >> 4 in H}`, one rectangle of the high-nibble × low-nibble grid. One AND
//! instead of a chain, but for this byte set **no single rectangle is the
//! structural set**: `,` (0x2C) and `:` (0x3A) are both structural, so a
//! rectangle containing them needs low nibbles `C` and `A` lit and rows 2 and 3
//! selected — and row 2 then drags in `0x2A` (`#`, content) because `A` is lit.
//! Nor can the two bits be a 4-distinct-class code, which is the same
//! obstruction from the other side: rows 2 and 3 must be selected and `A` and
//! `C` lit, while row 2's other members — `"` and `0x2A` — must come out as
//! their own codes, which the AND cannot give them.
//!
//! A *union* of rectangles has no such problem, so that is what this uses:
//! six of them, one `pshufb` pair each, still a single pass. Each is a byte set
//! this module can name, generate from a nibble mask, and test exhaustively,
//! which a single fused pair is not. `struct` is the first five, `quote` the
//! sixth on its own.
//!
//! ## Bytes >= 0x80
//!
//! No rectangle selects a row above `7`, so UTF-8 lead and continuation bytes
//! are OTHER by construction and no table index can run past 15 — `b >> 4` is
//! at most 15 for every `u8`. That is the hazard a hand-rolled nibble table
//! walks into when it forgets the `pshufb` high-bit rule, or that the high
//! nibble of `0x8E` is `8`; here it is a property of the tables, and
//! `tests::hi_nibble_rows_above_7_are_inert` pins it.
//!
//! ## Dispatch
//!
//! [`crate::djson::json_scan::scan`] checks AVX2 and PCLMULQDQ at runtime and calls the vector path;
//! everything else lands in [`crate::djson::json_scan::scan_scalar`], which is the same predicate one
//! byte at a time. Under the `std` feature the check *is*
//! `is_x86_feature_detected!`; `no_std` builds cannot call that macro (it
//! lives in `std`, not `core`), so they read CPUID and XGETBV themselves — see
//! [`crate::djson::json_scan::has_avx2_pclmulqdq`], and the test that holds the two to each other.
//! The `unsafe` is confined to the `#[target_feature]` functions in the private
//! `avx2` submodule, each of which documents the check that guards it.
//! [`crate::djson::json_scan::scan_swar`] is the block-level algebra in portable scalar code: the identical
//! 64-byte mask pipeline with a shift-chain prefix-XOR instead of the
//! carry-less multiply, and the tests use it to hold the two prefix-XOR
//! implementations to each other.

use alloc::vec::Vec;

#[cfg(target_arch = "x86_64")]
mod avx2;

#[cfg(target_arch = "x86_64")]
pub use avx2::scan_avx2;

#[cfg(test)]
mod tests;

// ---------------------------------------------------------------------------
// Classification
// ---------------------------------------------------------------------------

/// A byte that carries no meaning for the grammar.
pub const CLASS_OTHER: u8 = 0;
/// `{ } [ ] : ,` or one of the four JSON whitespace bytes.
pub const CLASS_STRUCTURAL: u8 = 1;
/// `"`.
pub const CLASS_QUOTE: u8 = 2;
/// `\`.
///
/// Not a plane: it is neither structural nor a quote, and two bits have no
/// room for a fifth class. [`classify16`] spells it as `0b11` in the packed
/// code so a caller can read one number instead of three masks, but **nothing
/// outside [`classify16`] may read the codes as bit flags** -- bit 0 of
/// `CLASS_BACKSLASH` is set, and a `class & CLASS_STRUCTURAL` test classifies
/// every backslash in the document as structural. Every other use in this
/// module compares the code for equality, and
/// `tests::class_codes_are_compared_not_masked` is what keeps it that way.
pub const CLASS_BACKSLASH: u8 = 3;

/// The 256-entry lookup the scalar path and the tests classify through.
///
/// Three codes, compared for equality everywhere except inside
/// [`classify16`]: bit 0 of the structural plane and bit 1 of the quote plane
/// are the only two bits the vector path computes, and they cannot be read
/// back out of this table as a mask (see [`CLASS_BACKSLASH`]).
pub const CLASS_TABLE: [u8; 256] = build_class_table();

/// The classification code for one byte.
#[must_use]
pub const fn class_of(b: u8) -> u8 {
    match b {
        b' ' | b'\t' | b'\n' | b'\r' => CLASS_STRUCTURAL,
        b'{' | b'}' | b'[' | b']' | b':' | b',' => CLASS_STRUCTURAL,
        b'"' => CLASS_QUOTE,
        b'\\' => CLASS_BACKSLASH,
        _ => CLASS_OTHER,
    }
}

/// Whether `b` is one of RFC 8259's four whitespace bytes.
#[must_use]
pub const fn is_whitespace(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\n' | b'\r')
}

const fn build_class_table() -> [u8; 256] {
    let mut table = [0u8; 256];
    let mut i = 0usize;
    while i < 256 {
        table[i] = class_of(i as u8);
        i += 1;
    }
    table
}

/// Encode one 16-byte window as a `u32` of two-bit codes: byte `i`'s code
/// sits at bits `2i..2i+2`.
///
/// The whitespace and backslash planes come along beside it, because two bits
/// cannot carry five classes and the two extra ones are exactly the two the
/// code cannot distinguish.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Classified16 {
    /// Two bits per byte: [`CLASS_OTHER`], [`CLASS_STRUCTURAL`],
    /// [`CLASS_QUOTE`] or [`CLASS_BACKSLASH`].
    pub code: u32,
    /// Bit `i` set when byte `i` is whitespace.
    pub whitespace: u16,
    /// Bit `i` set when byte `i` is a backslash.
    pub backslash: u16,
}

/// Classify 16 bytes, two bits per byte.
///
/// The portable reference for [`classify16_avx2`]; the two are compared
/// window-by-window over all 256 byte values.
#[must_use]
pub fn classify16(bytes: &[u8; 16]) -> Classified16 {
    let mut out = Classified16::default();
    for (i, &b) in bytes.iter().enumerate() {
        // Matched, never masked: the packed code of a backslash is `0b11`,
        // whose low two bits would read as "structural and quote" to any
        // bitwise test.
        let packed = match CLASS_TABLE[b as usize] {
            CLASS_BACKSLASH => u32::from(CLASS_BACKSLASH),
            CLASS_QUOTE => u32::from(CLASS_QUOTE),
            CLASS_STRUCTURAL => u32::from(CLASS_STRUCTURAL),
            _ => 0,
        };
        out.code |= packed << (2 * i);
        if is_whitespace(b) {
            out.whitespace |= 1 << i;
        }
        if b == b'\\' {
            out.backslash |= 1 << i;
        }
    }
    out
}

// --- the pshufb rectangles -------------------------------------------------

/// Build the (low-table, high-table) pair for one rectangle of the
/// high-nibble x low-nibble grid. `lo_set` and `hi_set` are nibble bitmasks.
const fn rect(lo_set: u16, hi_set: u16) -> ([u8; 16], [u8; 16]) {
    let mut lo = [0u8; 16];
    let mut hi = [0u8; 16];
    let mut i = 0usize;
    while i < 16 {
        lo[i] = if (lo_set >> i) & 1 == 1 { 0xFF } else { 0x00 };
        hi[i] = if (hi_set >> i) & 1 == 1 { 0xFF } else { 0x00 };
        i += 1;
    }
    (lo, hi)
}

/// `0x20`.
pub(crate) const RECT_SPACE: ([u8; 16], [u8; 16]) = rect(1 << 0x0, 1 << 0x2);
/// `0x09 0x0A 0x0D` — tab, line feed, carriage return.
pub(crate) const RECT_CTRL_WS: ([u8; 16], [u8; 16]) =
    rect((1 << 0x9) | (1 << 0xA) | (1 << 0xD), 1 << 0x0);
/// `0x2C`.
pub(crate) const RECT_COMMA: ([u8; 16], [u8; 16]) = rect(1 << 0xC, 1 << 0x2);
/// `0x3A`.
pub(crate) const RECT_COLON: ([u8; 16], [u8; 16]) = rect(1 << 0xA, 1 << 0x3);
/// `0x5B 0x5D 0x7B 0x7D` — `[ ] { }`.
pub(crate) const RECT_BRACKET: ([u8; 16], [u8; 16]) =
    rect((1 << 0xB) | (1 << 0xD), (1 << 0x5) | (1 << 0x7));
/// `0x22`.
pub(crate) const RECT_QUOTE: ([u8; 16], [u8; 16]) = rect(1 << 0x2, 1 << 0x2);

/// Every rectangle, with the byte values it is supposed to select.
#[cfg(test)]
#[allow(clippy::type_complexity)]
pub(crate) const RECTANGLES: [(&str, ([u8; 16], [u8; 16]), &[u8]); 6] = [
    ("space", RECT_SPACE, &[0x20]),
    ("ctrl-ws", RECT_CTRL_WS, &[0x09, 0x0A, 0x0D]),
    ("comma", RECT_COMMA, &[0x2C]),
    ("colon", RECT_COLON, &[0x3A]),
    ("bracket", RECT_BRACKET, &[0x5B, 0x5D, 0x7B, 0x7D]),
    ("quote", RECT_QUOTE, &[0x22]),
];

/// The bytes one (low-table, high-table) pair selects, computed the way
/// `pshufb` computes it: `low[b & 0x0F] & high[b >> 4]`.
///
/// The high-nibble index is `b >> 4`, which is at most 15 for every `u8` —
/// that bound is the reason bytes `>= 0x80` are safe here, and the reason
/// the tables have no row for them.
#[must_use]
pub const fn rect_bytes((lo, hi): ([u8; 16], [u8; 16])) -> [u8; 256] {
    let mut out = [0u8; 256];
    let mut b = 0usize;
    while b < 256 {
        let byte = b as u8;
        out[b] = lo[(byte & 0x0F) as usize] & hi[(byte >> 4) as usize];
        b += 1;
    }
    out
}

// ---------------------------------------------------------------------------
// Output
// ---------------------------------------------------------------------------

/// A dense bit set over a known byte range: bit `i` of word `i / 64` describes
/// byte `i`.
///
/// The word layout is the input's, not a re-packing: a 64-byte block of input
/// is one word of output, so the scanner never shifts between the two and the
/// block loop writes words straight out. Bits past [`BitSet::len`] are zero.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BitSet {
    words: Vec<u64>,
    len: usize,
    any: bool,
}

impl BitSet {
    /// An all-zero set over `len` bytes.
    #[must_use]
    pub fn with_len(len: usize) -> Self {
        Self {
            words: alloc::vec![0u64; len.div_ceil(64)],
            len,
            any: false,
        }
    }

    /// The number of bytes this set describes. This is the *range*, not the
    /// population: a set over 100 bytes with nothing in it has `len() == 100`
    /// and `is_empty() == true`.
    #[must_use]
    pub fn len(&self) -> usize {
        self.len
    }

    /// Whether no byte is in the set.
    ///
    /// "Empty" means no bits set, as it does for any other collection --
    /// *not* "describes no bytes", which is [`BitSet::len`] being zero.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        !self.any
    }

    /// Whether byte `i` is in the set. `false` past the end.
    #[must_use]
    pub fn contains(&self, i: usize) -> bool {
        if i >= self.len {
            return false;
        }
        self.words[i / 64] & (1u64 << (i % 64)) != 0
    }

    /// How many bytes are in the set.
    #[must_use]
    pub fn count_ones(&self) -> usize {
        self.words.iter().map(|w| w.count_ones() as usize).sum()
    }

    /// The backing words, low word first.
    #[must_use]
    pub fn words(&self) -> &[u64] {
        &self.words
    }

    /// The byte positions in the set, ascending.
    pub fn iter_set(&self) -> impl Iterator<Item = usize> + '_ {
        self.words.iter().enumerate().flat_map(|(wi, &w)| {
            (0..64).filter_map(move |b| {
                if w & (1u64 << b) != 0 {
                    Some(wi * 64 + b)
                } else {
                    None
                }
            })
        })
    }

    fn set(&mut self, i: usize) {
        self.words[i / 64] |= 1u64 << (i % 64);
        self.any = true;
    }

    /// Store a whole 64-byte word of output. Word `k` covers exactly the
    /// bytes `64k..64k + 64`, so the block loop needs no shifting.
    fn set_word(&mut self, word: usize, w: u64) {
        self.words[word] = w;
        self.any |= w != 0;
    }
}

/// The stage-1 output: where the structural bytes are, where the string
/// delimiters are, and whether the input ended inside a string.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StructuralIndex {
    /// Structural bytes, delimiters included.
    pub structural: BitSet,
    /// The quotes that open and close strings, and only those.
    pub quote: BitSet,
    /// The length of the scanned input.
    pub len: usize,
    /// Whether a string was still open at the end of the input. Stage 2
    /// treats this as a truncated document rather than reaching a terminal.
    pub end_in_string: bool,
}

impl StructuralIndex {
    /// The structural byte positions, ascending.
    pub fn structural_positions(&self) -> impl Iterator<Item = usize> + '_ {
        self.structural.iter_set()
    }

    /// The string-delimiter positions, ascending.
    pub fn quote_positions(&self) -> impl Iterator<Item = usize> + '_ {
        self.quote.iter_set()
    }
}

// ---------------------------------------------------------------------------
// The block algebra
// ---------------------------------------------------------------------------

/// The three pieces of state that cross a block boundary.
///
/// A quote's meaning depends on a backslash run that can start in one block
/// and end in another, and the string flag can be set by a quote in the last
/// byte of a block — so "process 64 bytes independently" is false and these
/// three bits are what make it true.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Carry {
    /// The input was inside a string when this block starts.
    pub in_string: bool,
    /// The last byte of the previous block was a backslash, so a run is open.
    pub run: bool,
    /// That open run's length so far is odd.
    pub parity: bool,
}

/// The whole 64-byte predicate, in masks.
///
/// `structural_raw` is the structural plane (`ws | punctuation`), `quote` the
/// quote plane, `backslash` the backslash plane — all three one bit per byte,
/// bit `i` being byte `i`. `prefix` supplies the inclusive prefix-XOR of a
/// word; the vector path passes a carry-less multiply and [`crate::djson::json_scan::scan_swar`]
/// passes [`prefix_xor`]. It is a parameter rather than a `cfg` so that both
/// spellings are monomorphised into the same algebra and the tests can run
/// the identical code around either.
///
/// The two hard steps:
///
/// **Escape parity.** `odd` ends up with bit `i` set exactly when byte `i` is
/// a backslash and the run ending at `i` has odd length — so a quote at `i+1`
/// is escaped exactly when `odd` is set at `i`, with no lookahead and no need
/// to know where the run ends. Getting there is a recurrence two bytes at a
/// time, `odd[i] = start[i] | (b[i] & b[i-1] & odd[i-2])`, because a run's
/// odd/even alternates with distance from its start. A stride-2 chain like
/// that cannot be a global prefix-XOR, so it is unrolled by doubling instead:
/// each step appends the runs reachable through a gate twice as long, and
/// five steps cover every even distance from 0 to 62, which is every one a
/// 64-byte block has. The carry enters at the bottom as the two virtual
/// inputs `odd[-1]` and `odd[-2]` that the recurrence reaches lanes 1 and 0
/// through.
///
/// **String state.** `toggle` is the quotes that are not escaped, and the
/// in-string state after byte `i` is their inclusive prefix-XOR. Bit 0 of
/// the result is the state before the block, and the quote at position `i`
/// is a delimiter exactly where that state changes — one XOR with the word
/// shifted up by one, and the shift's zero at the bottom is where the carry
/// goes back in.
#[inline]
pub(crate) fn block_with<F>(
    structural_raw: u64,
    quote: u64,
    backslash: u64,
    carry: &mut Carry,
    prefix: F,
) -> (u64, u64)
where
    F: Fn(u64) -> u64,
{
    // --- escapes -------------------------------------------------------
    // The recurrence is `odd[i] = start[i] | (b[i] & b[i-1] & odd[i-2])`, so
    // a run crossing the boundary reaches this block through *two* virtual
    // inputs, not one: `odd[-1]`, which is lane 1's predecessor, and
    // `odd[-2]`, which is lane 0's. `carry.parity` is the first of them and
    // its negation the second -- with one exception, which is not an
    // exception: a run of length 1 has odd parity, so `!parity` is 0, and a
    // byte at lane 0 then has no virtual `b[-1]` to be reached through
    // either. Injecting lane 0 alone is the bug an earlier version had, and
    // it made every block whose second byte was the end of an odd run crossing
    // the boundary lose one backslash.
    let open = 0u64.wrapping_sub(u64::from(carry.run));
    let start = backslash & !((backslash << 1) | u64::from(carry.run));
    let mut odd = start
        | (backslash & open & u64::from(!carry.parity))
        | (backslash & (backslash << 1) & (u64::from(carry.parity) << 1) & open);
    let mut gate = backslash & (backslash << 1);
    odd |= gate & (odd << 2);
    gate &= gate << 2;
    odd |= gate & (odd << 4);
    gate &= gate << 4;
    odd |= gate & (odd << 8);
    gate &= gate << 8;
    odd |= gate & (odd << 16);
    gate &= gate << 16;
    odd |= gate & (odd << 32);

    // `odd` is the escape parity for this block's bytes, but lane 0 has no
    // byte in front of it in this block: a quote there is escaped by whatever
    // run is still open from the previous one. That bit is why the carry
    // holds a parity and not just a flag, and it is the same `carry.run &&
    // carry.parity` that seeded `odd` above.
    let escaped = ((odd << 1) | u64::from(carry.run && carry.parity)) & quote;
    let toggle = quote & !escaped;

    // --- string state --------------------------------------------------
    let in_string_mask = prefix(toggle) ^ 0u64.wrapping_sub(u64::from(carry.in_string));
    let boundary = in_string_mask ^ ((in_string_mask << 1) | u64::from(carry.in_string));
    let quote_mask = boundary & quote;

    // The quote plane is deliberately absent from `structural_raw`: an
    // escaped quote outside a string would otherwise be reported as
    // structural, which the byte-at-a-time reference does not do.
    let structural = (structural_raw & !in_string_mask) | quote_mask;

    carry.in_string = in_string_mask >> 63 == 1;
    carry.run = backslash >> 63 == 1;
    carry.parity = odd >> 63 == 1;

    (structural, quote_mask)
}

/// Inclusive prefix-XOR: bit `i` of the result is the XOR of bits `0..=i`.
///
/// The carry-less multiply in the `avx2` module computes the same
/// function in one instruction; this is the portable spelling, and the tests
/// hold the two to each other on random words.
#[must_use]
pub fn prefix_xor(x: u64) -> u64 {
    let mut x = x;
    x ^= x << 1;
    x ^= x << 2;
    x ^= x << 4;
    x ^= x << 8;
    x ^= x << 16;
    x ^= x << 32;
    x
}

// ---------------------------------------------------------------------------
// Scanning
// ---------------------------------------------------------------------------

/// Collects output words and carries the boundary state between blocks.
pub(crate) struct Builder {
    structural: BitSet,
    quote: BitSet,
    carry: Carry,
    next_word: usize,
}

impl Builder {
    pub(crate) fn new(len: usize) -> Self {
        Self {
            structural: BitSet::with_len(len),
            quote: BitSet::with_len(len),
            carry: Carry::default(),
            next_word: 0,
        }
    }

    /// Store one 64-byte block of output. Word `k` is exactly the byte range    /// `64k..64k + 64`, so no shifting is involved.
    pub(crate) fn push_word(&mut self, structural: u64, quote: u64) {
        self.structural.set_word(self.next_word, structural);
        self.quote.set_word(self.next_word, quote);
        self.next_word += 1;
    }

    /// The predicate for one byte, in the clearest form there is. This is
    /// [`crate::djson::json_scan::scan_scalar`]'s body and the tail of every other path.
    pub(crate) fn step(&mut self, at: usize, b: u8) {
        let escaped = self.carry.run && self.carry.parity;
        let class = CLASS_TABLE[b as usize];
        let boundary = class == CLASS_QUOTE && !escaped;
        let structural = boundary || (class == CLASS_STRUCTURAL && !self.carry.in_string);

        if class == CLASS_BACKSLASH {
            self.carry.run = true;
            self.carry.parity = !self.carry.parity;
        } else {
            self.carry.run = false;
            self.carry.parity = false;
        }
        if boundary {
            self.carry.in_string = !self.carry.in_string;
        }

        if structural {
            self.structural.set(at);
        }
        if boundary {
            self.quote.set(at);
        }
    }

    pub(crate) fn finish(self) -> StructuralIndex {
        let len = self.structural.len;
        StructuralIndex {
            structural: self.structural,
            quote: self.quote,
            len,
            end_in_string: self.carry.in_string,
        }
    }
}

// ---------------------------------------------------------------------------
// Dispatch
// ---------------------------------------------------------------------------

/// Whether this CPU can run the AVX2 path.
///
/// `is_x86_feature_detected!` lives in `std` and not in `core`, so a `no_std`
/// build -- which is the *primary* target of this crate -- cannot call it.
/// Under `std` this is that macro; without it, this is the same test written
/// out, because the bits it reads are fixed by the architecture:
///
/// * `CPUID.(EAX=7, ECX=0):EBX[5]` -- AVX2.
/// * `CPUID.(EAX=1):ECX[27]` -- OSXSAVE, and then `XCR0[2:1] == 0b11`.
///
/// The last line is the one a naive check leaves out, and the omission is not
/// a slow path: on a kernel that does not save the YMM state, an AVX2
/// instruction traps. `tests::cpuid_detection_matches_the_std_macro` holds
/// the two implementations to each other.
#[must_use]
pub fn has_avx2() -> bool {
    #[cfg(target_arch = "x86_64")]
    {
        #[cfg(feature = "std")]
        {
            std::is_x86_feature_detected!("avx2")
        }
        #[cfg(not(feature = "std"))]
        {
            cpu_avx2_bits()
        }
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        false
    }
}

/// Whether this CPU can run the block path: AVX2 *and* PCLMULQDQ, the latter
/// because the string state is carried across blocks by a carry-less multiply.
#[must_use]
pub fn has_avx2_pclmulqdq() -> bool {
    #[cfg(target_arch = "x86_64")]
    {
        #[cfg(feature = "std")]
        {
            std::is_x86_feature_detected!("avx2") && std::is_x86_feature_detected!("pclmulqdq")
        }
        #[cfg(not(feature = "std"))]
        {
            cpu_pclmulqdq_bits() && cpu_avx2_bits()
        }
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        false
    }
}

/// The no_std half of the detection: CPUID and XGETBV, by hand.
///
/// Compiled on every x86_64 build so that the `std` path can be tested
/// against it rather than trusted.
///
/// [Copy-time change.] `dead_code` is allowed here and only here. The module
/// is meant to exist on every x86_64 build, so in a build without the test
/// harness -- `cargo build --release` of the lib -- nothing calls it, and
/// five warnings would stand where the module is the thing that is correct.
#[cfg(target_arch = "x86_64")]
#[allow(dead_code)]
pub(crate) mod detect {
    use core::arch::x86_64::{__cpuid, __cpuid_count};

    /// Read `XCR0`, the set of register states the OS saves on a context
    /// switch.
    ///
    /// # Safety
    ///
    /// Requires XSAVE support, which is what `CPUID.1:ECX[27]` (OSXSAVE)
    /// reports. The caller must have established it.
    #[target_feature(enable = "xsave")]
    pub(crate) unsafe fn xgetbv0() -> u64 {
        // SAFETY: the target feature is this function's contract.
        unsafe { core::arch::x86_64::_xgetbv(0) }
    }

    /// `CPUID.(EAX=1):ECX[1]`.
    #[allow(unused_unsafe)]
    pub(crate) fn pclmulqdq() -> bool {
        // SAFETY: __cpuid is safe to call on x86_64.
        unsafe { __cpuid(1) }.ecx & (1 << 1) != 0
    }

    /// `CPUID.(EAX=7, ECX=0):EBX[5]`, and the OS support an AVX2
    /// instruction needs on top of it.
    #[allow(unused_unsafe)]
    pub(crate) fn avx2() -> bool {
        let leaf1 = unsafe { __cpuid(1) };
        let osxsave = leaf1.ecx & (1 << 27) != 0;
        let avx = leaf1.ecx & (1 << 28) != 0;
        if !(unsafe { __cpuid_count(7, 0) }.ebx & (1 << 5) != 0 && osxsave && avx) {
            return false;
        }
        // SAFETY: OSXSAVE was just read as set, which is the precondition of
        // `xgetbv0` -- without OSXSAVE the instruction faults, and a CPU that
        // sets it also supports XSAVE.
        let xcr0 = unsafe { xgetbv0() };
        (xcr0 & 0b110) == 0b110
    }
}

// The two shims the `not(feature = "std")` branch of the detection calls.
// They are unused in that configuration on x86_64, and used nowhere else, for
// the same reason `detect` is compiled anyway: both halves ship and only one
// is live. [Copy-time change.]
#[cfg(target_arch = "x86_64")]
#[allow(dead_code)]
fn cpu_avx2_bits() -> bool {
    detect::avx2()
}

#[cfg(target_arch = "x86_64")]
#[allow(dead_code)]
fn cpu_pclmulqdq_bits() -> bool {
    detect::pclmulqdq()
}

/// Scan `bytes` with the best implementation this CPU admits to.
///
/// The vector path is the AVX2 one; everything else falls back to
/// [`crate::djson::json_scan::scan_scalar`], which is the same predicate one byte at a time.
#[must_use]
pub fn scan(bytes: &[u8]) -> StructuralIndex {
    #[cfg(target_arch = "x86_64")]
    if has_avx2_pclmulqdq() {
        // SAFETY: `has_avx2_pclmulqdq` just reported both features, which is
        // the precondition of `scan_avx2`; see its documentation.
        return unsafe { avx2::scan_avx2(bytes) };
    }
    scan_scalar(bytes)
}

/// The reference: the predicate, one byte at a time.
///
/// The fallback for CPUs without AVX2, and the oracle the vector path and
/// [`crate::djson::json_scan::scan_swar`] are held to.
#[must_use]
pub fn scan_scalar(bytes: &[u8]) -> StructuralIndex {
    let mut builder = Builder::new(bytes.len());
    for (at, &b) in bytes.iter().enumerate() {
        builder.step(at, b);
    }
    builder.finish()
}

/// The block algebra in portable scalar code: the same 64-byte pipeline as
/// the vector path, with [`prefix_xor`] where that path has a carry-less
/// multiply.
///
/// It is the honest way to read the vector path without a CPU, and it is how
/// the two prefix-XOR implementations are compared. It is **not** a fast
/// path, and deliberately so: its classification still runs one byte at a
/// time, so all it saves is the per-byte state machine. Measured on this
/// machine it lands within a few percent of [`crate::djson::json_scan::scan_scalar`] (0.63 GB/s
/// against 0.54 GB/s, where the vector path does 10.5 GB/s), which is the
/// whole point of the numbers being in a test: the mask algebra is not where
/// the time goes, the byte loop is.
#[must_use]
pub fn scan_swar(bytes: &[u8]) -> StructuralIndex {
    let mut builder = Builder::new(bytes.len());
    let mut at = 0usize;
    while at + 64 <= bytes.len() {
        let mut structural_raw = 0u64;
        let mut quote = 0u64;
        let mut backslash = 0u64;
        for (lane, &b) in bytes[at..at + 64].iter().enumerate() {
            // Equality, not `& 1` / `& 2`: the codes are an enum, and
            // `CLASS_BACKSLASH` is `0b11`. See `CLASS_BACKSLASH`.
            match CLASS_TABLE[b as usize] {
                CLASS_STRUCTURAL => structural_raw |= 1u64 << lane,
                CLASS_QUOTE => quote |= 1u64 << lane,
                CLASS_BACKSLASH => backslash |= 1u64 << lane,
                _ => {}
            }
        }
        let (structural, quote_mask) = block_with(
            structural_raw,
            quote,
            backslash,
            &mut builder.carry,
            prefix_xor,
        );
        builder.push_word(structural, quote_mask);
        at += 64;
    }
    while at < bytes.len() {
        builder.step(at, bytes[at]);
        at += 1;
    }
    builder.finish()
}

/// Classify 16 bytes with `pshufb`, falling back to [`classify16`] on a CPU
/// without AVX2.
///
/// The vector counterpart of [`classify16`]; present so the rectangle tables
/// can be held against the 256-entry table on a real CPU.
#[cfg(target_arch = "x86_64")]
#[must_use]
pub fn classify16_avx2(bytes: &[u8; 16]) -> Classified16 {
    if has_avx2() {
        // SAFETY: `has_avx2` just reported AVX2, which is the precondition of
        // `classify16_avx2_inner`; `bytes` is a `&[u8; 16]`, so the 16-byte
        // load is in bounds by the type.
        return unsafe { avx2::classify16_avx2_inner(bytes) };
    }
    classify16(bytes)
}
