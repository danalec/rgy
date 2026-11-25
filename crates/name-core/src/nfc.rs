// SPDX-License-Identifier: Apache-2.0

//! Unicode NFC normalization (UAX #15), the second half of the comparison
//! fold (see [`crate::fold`]).
//!
//! ## Why this exists
//!
//! macOS stores a file name in NFD -- "café" is "cafe" plus a combining
//! acute -- while a person types NFC, where that same name is one
//! character. The two are different byte sequences, so a comparison of the
//! typed query against the stored name misses a file that exists. That is
//! parity row 1.12, and this module is the part of it that the ASCII
//! case-fold cannot do.
//!
//! ## What it is not
//!
//! This is a normalizer, not a case-folder. The fold's ASCII case-fold
//! happens where the bytes are compared (see `name_query::fold`), because
//! the name's own case carries meaning for the scorer -- a camelCase
//! boundary is a scoring bonus, and lowercasing the name would throw it
//! away.
//!
//! ## The algorithm
//!
//! UAX #15's three steps, in order:
//!
//! 1. **Canonical decomposition** -- every character is replaced by its
//!    canonical decomposition, applied recursively. Hangul syllables have no
//!    table: they decompose arithmetically.
//! 2. **Canonical ordering** -- the combining marks of each segment are
//!    sorted by their combining class, stably.
//! 3. **Canonical composition** -- the starter and the marks that follow it
//!    are composed back where the Unicode tables say they can be.
//!
//! The quick check comes first: a string whose characters are all
//! `NFC_QC=Yes` and whose combining classes are already in canonical order
//! is in NFC, and is returned untouched. The check is deliberately
//! conservative -- a character that is `NFC_QC=Maybe` sends the string down
//! the long path even when it would have been fine -- because a quick check
//! that is wrong in one direction silently changes a name.
//!
//! ## The tables
//!
//! [`crate::nfc_table`] holds the Unicode Character Database's canonical
//! decompositions, combining classes, primary composites and quick-check
//! sets. They are generated, not written by hand: see
//! `crates/name-core/tools/gen_nfc_table.py`.

use crate::nfc_table::{CCC, COMPOSE, DECOMP, QC_MAYBE, QC_NO};
use alloc::borrow::Cow;
use alloc::vec::Vec;

/// Hangul, the one part of Unicode normalization with no table: the
/// syllables are composed and decomposed arithmetically (UAX #15 §16).
const L_BASE: u32 = 0x1100;
const V_BASE: u32 = 0x1161;
const T_BASE: u32 = 0x11A7;
const L_COUNT: u32 = 19;
const V_COUNT: u32 = 21;
const T_COUNT: u32 = 28;
const N_COUNT: u32 = V_COUNT * T_COUNT;
const S_COUNT: u32 = L_COUNT * N_COUNT;
const S_BASE: u32 = 0xAC00;

/// The NFC form of `s`.
///
/// Returns the input untouched when it is already in NFC, which is the
/// common case: a name typed on a keyboard is NFC, and so is every name
/// Windows and Linux hold.
pub fn nfc(s: &str) -> Cow<'_, str> {
    if is_nfc(s) {
        return Cow::Borrowed(s);
    }
    // Decompose, order, compose.
    let mut chars: Vec<char> = Vec::with_capacity(s.len());
    for c in s.chars() {
        decompose_into(&mut chars, c);
    }
    canonical_order(&mut chars);
    let composed = compose_all(&chars);
    Cow::Owned(composed.into_iter().collect())
}

/// Is `s` in NFC already?
///
/// The quick check (UAX #15 §9.2): a string is in NFC when it contains no
/// character that decomposes (`NFC_QC=No`), no character that could compose
/// with the one before it (`NFC_QC=Maybe`), and its combining classes are
/// already in canonical order.
///
/// The `Maybe` set is refused rather than analysed: the full check has to
/// decide whether the character before it blocks the composition, and a
/// string that carries combining marks is rare enough that the long path
/// costs nothing worth the risk of a wrong answer.
fn is_nfc(s: &str) -> bool {
    let mut last = 0u8;
    for c in s.chars() {
        let cp = c as u32;
        if cp < 0x0300 {
            last = 0;
            continue;
        }
        if in_ranges(QC_NO, cp) || in_ranges(QC_MAYBE, cp) {
            return false;
        }
        let class = ccc(cp);
        if class == 0 {
            last = 0;
        } else {
            if class < last {
                return false;
            }
            last = class;
        }
    }
    true
}

/// The canonical combining class of `cp`: 0 for a starter.
///
/// Public to the crate: the highlight maps the folded name's characters
/// back to the raw name's, and it needs to know which of the raw's
/// characters are the marks that were composed into the character before
/// them.
pub(crate) fn ccc(cp: u32) -> u8 {
    if cp < 0x0300 {
        return 0;
    }
    match CCC.binary_search_by(|&(lo, hi, _)| {
        if cp < lo {
            core::cmp::Ordering::Greater
        } else if cp > hi {
            core::cmp::Ordering::Less
        } else {
            core::cmp::Ordering::Equal
        }
    }) {
        Ok(i) => CCC[i].2,
        Err(_) => 0,
    }
}

/// Is `cp` in one of the `(lo, hi)` ranges?
fn in_ranges(ranges: &[(u32, u32)], cp: u32) -> bool {
    ranges
        .binary_search_by(|&(lo, hi)| {
            if cp < lo {
                core::cmp::Ordering::Greater
            } else if cp > hi {
                core::cmp::Ordering::Less
            } else {
                core::cmp::Ordering::Equal
            }
        })
        .is_ok()
}

/// The canonical decomposition of `cp`: `(first, second)`, where a `second`
/// of 0 is a one-character decomposition.
fn decomposition(cp: u32) -> Option<(u32, u32)> {
    DECOMP.binary_search_by_key(&cp, |&(c, _, _)| c).ok().map(|i| (DECOMP[i].1, DECOMP[i].2))
}

/// The Hangul syllable `cp` as the jamo it is made of: `(L, V, T)`, where
/// a `T` of 0 means the syllable has no trailing jamo.
fn hangul_parts(cp: u32) -> (u32, u32, u32) {
    let s = cp - S_BASE;
    (L_BASE + s / N_COUNT, V_BASE + (s % N_COUNT) / T_COUNT, s % T_COUNT)
}

/// Append `c` to `out`, canonically decomposed.
fn decompose_into(out: &mut Vec<char>, c: char) {
    let cp = c as u32;
    // Hangul syllables decompose arithmetically.
    if (S_BASE..S_BASE + S_COUNT).contains(&cp) {
        let (l, v, t) = hangul_parts(cp);
        out.push(char::from_u32(l).expect("a Hangul jamo"));
        out.push(char::from_u32(v).expect("a Hangul jamo"));
        if t != 0 {
            out.push(char::from_u32(T_BASE + t).expect("a Hangul jamo"));
        }
        return;
    }
    match decomposition(cp) {
        Some((a, b)) => {
            decompose_into(out, char::from_u32(a).expect("a canonical decomposition is a scalar value"));
            if b != 0 {
                decompose_into(out, char::from_u32(b).expect("a canonical decomposition is a scalar value"));
            }
        }
        None => out.push(c),
    }
}

/// Sort each segment's combining marks by their combining class (UAX #15,
/// D109). The sort is stable, so marks of the same class keep their order.
fn canonical_order(chars: &mut [char]) {
    for i in 1..chars.len() {
        let class = ccc(chars[i] as u32);
        if class == 0 {
            continue;
        }
        let mut j = i;
        while j > 0 {
            let prev = ccc(chars[j - 1] as u32);
            if prev == 0 || prev <= class {
                break;
            }
            chars.swap(j - 1, j);
            j -= 1;
        }
    }
}

/// Compose the decomposed characters back (UAX #15 §9.3).
///
/// A character is blocked from the starter before it when a character
/// between them has a combining class of 0 or one that is not lower than
/// its own; the canonical ordering means the last character before it is
/// the one to ask.
fn compose_all(chars: &[char]) -> Vec<char> {
    let mut out: Vec<char> = Vec::with_capacity(chars.len());
    // The index in `out` of the starter the next character may compose with.
    let mut starter: Option<usize> = None;
    // The combining class of the last character appended after that starter.
    let mut last_class = 0u8;
    for &c in chars {
        let class = ccc(c as u32);
        if let Some(s) = starter
            && (last_class == 0 || last_class < class)
            && let Some(composite) = compose_pair(out[s], c)
        {
            out[s] = composite;
            continue;
        }
        if class == 0 {
            starter = Some(out.len());
            last_class = 0;
        } else {
            last_class = class;
        }
        out.push(c);
    }
    out
}

/// The canonical composition of `a` and `b`, if there is one.
fn compose_pair(a: char, b: char) -> Option<char> {
    let (ac, bc) = (a as u32, b as u32);
    // Hangul: L + V is a syllable, and that syllable + T is one more.
    if (L_BASE..L_BASE + L_COUNT).contains(&ac) && (V_BASE..V_BASE + V_COUNT).contains(&bc) {
        let index = (ac - L_BASE) * N_COUNT + (bc - V_BASE) * T_COUNT;
        return char::from_u32(S_BASE + index);
    }
    if (S_BASE..S_BASE + S_COUNT).contains(&ac) && (T_BASE + 1..T_BASE + T_COUNT).contains(&bc) && hangul_parts(ac).2 == 0 {
        // The syllable has no trailing jamo of its own, so the new one
        // hangs off it.
        return char::from_u32(ac + (bc - T_BASE));
    }
    COMPOSE.binary_search_by_key(&(ac, bc), |&(x, y, _)| (x, y)).ok().and_then(|i| char::from_u32(COMPOSE[i].2))
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::string::String;

    /// The UCD's own conformance suite, as vectors generated from
    /// `NormalizationTest.txt` (see `nfc_vectors`).
    fn hex_to_string(hex: &str) -> String {
        hex.split_whitespace().map(|h| char::from_u32(u32::from_str_radix(h, 16).expect("hex")).expect("a scalar value")).collect()
    }

    #[test]
    fn the_ucd_conformance_vectors_hold() {
        let mut checked = 0;
        for (src, want) in crate::nfc_vectors::CONFORMANCE {
            let source = hex_to_string(src);
            let got = nfc(&source);
            assert_eq!(&got[..], hex_to_string(want), "NFC of {src} is {got}, not {want}");
            checked += 1;
        }
        assert!(checked > 1000, "the conformance vectors are suspiciously few: {checked}");
    }

    /// The whole UCD suite, when the machine has it: `RGY_UCD` points at a
    /// directory holding `NormalizationTest.txt`. The generated vectors are
    /// a sample of it; this is the whole of it.
    #[test]
    #[ignore = "needs the UCD's NormalizationTest.txt; set RGY_UCD to run it"]
    fn the_whole_ucd_conformance_suite_holds() {
        let Ok(dir) = std::env::var("RGY_UCD") else {
            panic!("RGY_UCD is not set");
        };
        let path = std::path::Path::new(&dir).join("NormalizationTest.txt");
        let text = std::fs::read_to_string(&path).expect("NormalizationTest.txt");
        let mut checked = 0;
        let mut failed = 0;
        for line in text.lines() {
            let line = line.split('#').next().unwrap_or("").trim();
            if line.is_empty() || line.starts_with('@') {
                continue;
            }
            let cols: Vec<&str> = line.split(';').map(str::trim).filter(|c| !c.is_empty()).collect();
            if cols.len() < 2 {
                continue;
            }
            let src = hex_to_string(cols[0]);
            let got = nfc(&src);
            let want = hex_to_string(cols[1]);
            if got != want {
                failed += 1;
                eprintln!("NFC of {} is {}, not {}", cols[0], got, cols[1]);
            }
            checked += 1;
        }
        assert!(checked > 1000, "the conformance suite is suspiciously short: {checked}");
        assert_eq!(failed, 0, "{failed} of {checked} conformance cases failed");
        println!("{checked} UCD conformance cases checked, {failed} failed");
    }

    #[test]
    fn a_composed_letter_is_left_alone() {
        assert_eq!(nfc("café"), "café");
        assert_eq!(nfc("café"), "café");
    }

    #[test]
    fn a_decomposed_letter_is_composed() {
        assert_eq!(nfc("cafe\u{301}"), "café");
        assert_eq!(nfc("cafe\u{301}.txt"), "café.txt");
    }

    #[test]
    fn combining_marks_are_ordered_by_their_class() {
        // "q" + dot above (class 230) + dot below (class 220): the canonical
        // order puts the lower class first. "q" has no precomposed form with
        // either mark, so nothing composes.
        assert_eq!(nfc("q\u{307}\u{323}"), "q\u{323}\u{307}");
        // A mark of class 0 is a starter and starts a new segment, so the
        // order does not cross it.
        assert_eq!(nfc("a\u{307}b"), "\u{227}b");
        // "a" + dot above is a precomposed character, so it composes.
        assert_eq!(nfc("a\u{307}"), "\u{227}");
    }

    #[test]
    fn hangul_is_composed_and_decomposed_arithmetically() {
        // The choseong + jungseong -> syllable.
        assert_eq!(nfc("\u{1100}\u{1161}"), "\u{AC00}");
        // The syllable + jongseong -> the syllable with a trailing jamo.
        assert_eq!(nfc("\u{AC00}\u{11A8}"), "\u{AC01}");
        // A precomposed syllable is left alone.
        assert_eq!(nfc("\u{AC00}"), "\u{AC00}");
    }

    #[test]
    fn a_composition_exclusion_is_not_composed_back() {
        // U+0344 decomposes and does not recompose: it is excluded.
        assert_eq!(nfc("\u{0344}"), "\u{0308}\u{0301}");
        // U+0958 has a canonical decomposition that is not a primary
        // composite, so it stays decomposed.
        assert_eq!(nfc("\u{0958}"), "\u{0915}\u{093C}");
    }

    #[test]
    fn nfc_is_idempotent() {
        for s in ["cafe\u{301}", "cafe\u{301}.txt", "q\u{307}\u{323}", "\u{AC00}\u{11A8}", "\u{0344}", "plain"] {
            let once = nfc(s);
            assert_eq!(nfc(&once), once, "NFC is not idempotent on {s:?}");
        }
    }
}
