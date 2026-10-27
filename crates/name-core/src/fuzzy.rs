// SPDX-License-Identifier: Apache-2.0

//! Case-insensitive fuzzy matching for the interactive search UI.
//!
//! Interactive search ranks on *how well* a name matched, not merely whether it
//! did: `fse` should surface `fse.go` above a scattered `FSEvents.mm`. So a
//! successful match is described by a [`Match`] and results are ordered by it
//! rather than by arrival.
//!
//! **Not on the query path.** As of 2026-10-12 nothing outside this module's own
//! tests calls [`fuzzy_match`] or [`score`]; `Searcher` scores names with the
//! fzf-v1 scorer in `name_query::token_score`, which is a different
//! implementation. This module is the zen-rs matcher carried across the merge,
//! kept because it is public, tested, and the natural place to put a
//! kind-ranked UI matcher. Deleting it is a deliberate call, not a cleanup —
//! check with `grep -rn 'fuzzy::' --include=*.rs` before assuming it is dead.

use alloc::string::String;
use alloc::vec::Vec;

/// How tightly the needle matched the name. Lower ranks better.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum MatchKind {
    /// The needle covers the whole name.
    Exact,
    /// The needle is a leading run of the name.
    Prefix,
    /// The needle appears contiguously somewhere inside the name.
    Substring,
    /// The needle's characters appear in order but not contiguously.
    Scattered,
}

/// A successful fuzzy match.
///
/// The derived [`Ord`] *is* the ranking the UI wants — tighter kind first, then
/// the earliest start, then the shortest span — so sorting a result list by this
/// value needs no bespoke comparator.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Match {
    pub kind: MatchKind,
    /// Offset of the first matched character, counted in `char`s of the folded
    /// name.
    pub start: usize,
    /// Distance from `start` to the last matched character. Zero for a
    /// single-character match.
    pub span: usize,
}

impl Match {
    const fn new(kind: MatchKind, start: usize, span: usize) -> Self {
        Match { kind, start, span }
    }
}

/// Case-fold to `char`s so both sides of a comparison stay aligned.
///
/// Folding can map one char to several (Turkish dotted capital I), which is why
/// everything below indexes the folded vectors instead of the originals.
fn fold(s: &str) -> Vec<char> {
    s.chars().flat_map(char::to_lowercase).collect()
}

/// Classify a contiguous run: whether it covers the whole name, starts it, or
/// sits inside it.
fn rank(start: usize, matched: usize, total: usize) -> Match {
    let kind = if start == 0 && matched == total {
        MatchKind::Exact
    } else if start == 0 {
        MatchKind::Prefix
    } else {
        MatchKind::Substring
    };
    Match::new(kind, start, matched - 1)
}

/// Walk a subsequence greedily leftmost over a slice of comparable items.
///
/// Takes the earliest position for every character, which is tight enough to
/// rank on without the quadratic table a truly minimal span would need.
fn scatter<T: Copy + PartialEq>(hay: &[T], pin: &[T]) -> Option<Match> {
    let mut cursor = 0usize;
    let mut first = 0usize;
    for (i, c) in pin.iter().enumerate() {
        let offset = hay[cursor..].iter().position(|h| h == c)?;
        cursor += offset;
        if i == 0 {
            first = cursor;
        }
        cursor += 1;
    }
    Some(Match::new(MatchKind::Scattered, first, cursor - 1 - first))
}

/// Byte-wise match for the ASCII case, which allocates nothing.
///
/// Interactive search scores every indexed entry on every keystroke, so the two
/// `Vec<char>` the generic path allocates are the largest single cost in a
/// whole-index scan. Filenames are overwhelmingly ASCII.
fn match_ascii(name: &[u8], needle: &[u8]) -> Option<Match> {
    if needle.len() > name.len() {
        return None;
    }

    // Contiguous first: the common case, and it ranks tighter than a scattered
    // match can.
    if let Some(start) = name.windows(needle.len()).position(|window| window.eq_ignore_ascii_case(needle)) {
        return Some(rank(start, needle.len(), name.len()));
    }

    // Otherwise an in-order subsequence, compared byte by byte. Most names are
    // rejected by the walk below, so it must not allocate on the way out.
    let mut cursor = 0usize;
    let mut first = 0usize;
    for (i, c) in needle.iter().enumerate() {
        let lower = c.to_ascii_lowercase();
        let offset = name[cursor..].iter().position(|h| h.to_ascii_lowercase() == lower)?;
        cursor += offset;
        if i == 0 {
            first = cursor;
        }
        cursor += 1;
    }
    Some(Match::new(MatchKind::Scattered, first, cursor - 1 - first))
}

/// `char`-wise match, used when either side has a non-ASCII character.
fn match_chars(name: &str, needle: &str) -> Option<Match> {
    let hay = fold(name);
    let pin = fold(needle);
    if pin.len() > hay.len() {
        return None;
    }

    for start in 0..=(hay.len() - pin.len()) {
        if hay[start..start + pin.len()] == pin[..] {
            return Some(rank(start, pin.len(), hay.len()));
        }
    }
    scatter(&hay, &pin)
}

/// Score `name` against a single `needle`, ignoring case. `None` when the
/// needle's characters do not all appear in order.
///
/// An empty needle matches everything as [`MatchKind::Exact`], which lets a
/// caller score an unfiltered query without special-casing it.
pub fn fuzzy_match(name: &str, needle: &str) -> Option<Match> {
    if needle.is_empty() {
        return Some(Match::new(MatchKind::Exact, 0, 0));
    }
    if name.is_ascii() && needle.is_ascii() {
        return match_ascii(name.as_bytes(), needle.as_bytes());
    }
    match_chars(name, needle)
}

/// Score `name` against every needle, or `None` if any needle fails.
///
/// The weakest term decides the result: a name that exactly matches one word and
/// only scatters another is still an overall scattered match. With no needles
/// the name matches as [`MatchKind::Exact`].
pub fn score(name: &str, needles: &[String]) -> Option<Match> {
    let mut worst: Option<Match> = None;
    for needle in needles {
        let m = fuzzy_match(name, needle)?;
        worst = Some(match worst {
            Some(w) if w > m => w,
            _ => m,
        });
    }
    Some(worst.unwrap_or(Match::new(MatchKind::Exact, 0, 0)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::string::ToString;
    use alloc::vec;

    fn needle(s: &str) -> Vec<String> {
        vec![s.to_string()]
    }

    #[test]
    fn exact_match_outranks_prefix_which_outranks_substring() {
        let exact = fuzzy_match("main", "main").unwrap();
        let prefix = fuzzy_match("main.rs", "main").unwrap();
        let substring = fuzzy_match("src/main.rs", "main").unwrap();

        assert_eq!(exact.kind, MatchKind::Exact);
        assert_eq!(prefix.kind, MatchKind::Prefix);
        assert_eq!(substring.kind, MatchKind::Substring);

        assert!(exact < prefix, "exact must rank above prefix");
        assert!(prefix < substring, "prefix must rank above substring");
    }

    #[test]
    fn matching_ignores_case() {
        assert_eq!(fuzzy_match("Wallpaper.tsx", "wall"), fuzzy_match("Wallpaper.tsx", "WALL"));
        // `readme` is only a prefix of `README.md`, not the whole name.
        assert_eq!(fuzzy_match("README.md", "readme").map(|m| m.kind), Some(MatchKind::Prefix));
    }

    #[test]
    fn an_abbreviation_still_finds_a_long_name() {
        // The interactive demo searches `wallp` and expects
        // `WallPropertyFix.java`: an abbreviation has to reach past the head of
        // a name that is far longer than the query.
        let m = fuzzy_match("WallPropertyFix.java", "wallp").unwrap();
        assert_eq!(m.kind, MatchKind::Prefix);
        assert_eq!(m.start, 0);

        // Characters with no run between them are a scattered match, and the
        // only kind that survives when nothing lines up.
        let scattered = fuzzy_match("abcxdxf.rs", "adr").unwrap();
        assert_eq!(scattered.kind, MatchKind::Scattered);
        assert_eq!(scattered.start, 0);
    }

    #[test]
    fn subsequence_rejects_characters_out_of_order() {
        assert_eq!(fuzzy_match("main", "nam"), None);
        assert_eq!(fuzzy_match("ab", "ba"), None);
    }

    #[test]
    fn a_needle_longer_than_the_name_cannot_match() {
        assert_eq!(fuzzy_match("a.rs", "main.rs"), None);
    }

    #[test]
    fn scattered_ranking_prefers_the_tightest_match() {
        // Both scatter `adr`; the characters sit close together in `ad.rs`, so
        // it must outrank a name that only matches them far apart.
        let tight = fuzzy_match("ad.rs", "adr").unwrap();
        let loose = fuzzy_match("abcxdxf.rs", "adr").unwrap();
        assert_eq!(tight.kind, MatchKind::Scattered);
        assert_eq!(loose.kind, MatchKind::Scattered);
        assert!(tight < loose, "tighter span must rank better");
    }

    #[test]
    fn an_empty_needle_matches_everything_exactly() {
        assert_eq!(fuzzy_match("", ""), Some(Match::new(MatchKind::Exact, 0, 0)));
        assert_eq!(fuzzy_match("anything.rs", ""), Some(Match::new(MatchKind::Exact, 0, 0)));
    }

    #[test]
    fn score_requires_every_needle_to_match() {
        let needles = needle("wall").into_iter().chain(needle("jpg")).collect::<Vec<_>>();
        assert!(score("wallpaper.jpg", &needles).is_some());
        assert!(score("wallpaper.png", &needles).is_none());
    }

    #[test]
    fn score_takes_the_weakest_needle() {
        let needles = vec!["main".to_string(), "zzzz".to_string()];
        assert_eq!(score("main.rs", &needles), None);

        let mixed = vec!["main".to_string(), "rs".to_string()];
        let m = score("main.rs", &mixed).unwrap();
        // `main` is a prefix but `rs` is only a substring, so the substring
        // decides: one weak term drags the whole name down.
        assert_eq!(m.kind, MatchKind::Substring);
    }

    #[test]
    fn score_with_no_needles_is_an_exact_match() {
        assert_eq!(score("whatever.txt", &[]), Some(Match::new(MatchKind::Exact, 0, 0)));
    }

    #[test]
    fn non_ascii_names_still_fold_without_panicking() {
        // Case folding must not desynchronise the two sides of the comparison:
        // 'İ' folds to two chars, so the offsets are folded-char offsets.
        let m = fuzzy_match("Ärger.txt", "ä").unwrap();
        assert_eq!(m.kind, MatchKind::Prefix);
        assert!(fuzzy_match("日本語.txt", "本語").is_some());
    }
}
