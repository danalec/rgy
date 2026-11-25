// SPDX-License-Identifier: Apache-2.0

//! The ranking key: the one `u64` the final order compares.
//!
//! Until now rgy ranked with a tuple: an fzf-v1 score descending, then the
//! entry id ascending. The top-K selection already packed that pair into one
//! `u64`, but the *final* order was the tuple, and the location prior was a
//! term added to the score rather than a level of its own.
//!
//! A term added to the score loses to a better name. A file in `Documents`
//! called `configuration.txt` and a file in `node_modules` called
//! `config.json` are the case: the second is the better name match, and the
//! first is the file the user wants. The ranking corpus
//! (`tests/ranking.rs`) is what says so.
//!
//! So the priority became a **level** rather than a term, and the level
//! lives in the high bits of the key, where nothing else can outrank it.
//! That is the reference's shape: its ranking is a packed 64-bit composite
//! (§5.8.1 of the reverse engineering report), with the priority ladder
//! (Ignored < Low < Normal < Internal < High < HighFile, §4.6) applied on
//! three axes -- path, attribute and name -- each with its own tier.
//!
//! What came from the report is the *shape*, not the bits: the report itself
//! says the per-axis weights are bit positions it could not tabulate
//! (§10.2), so the layout below is rgy's own and says so.
//!
//! ```text
//!     63..61  path tier        3 bits   the ladder, on the path axis
//!     60..58  name tier        3 bits   the ladder, on the name axis
//!     57..55  attribute tier   3 bits   the ladder, on the attribute axis
//!     54..32  score           23 bits   biased by 2^22, quantised
//!     31..0   !entry          32 bits   the tie-break: the whole index
//! ```
//!
//! The score field is 23 bits and the entry field 32, so neither of them
//! needs a mask: the bias puts the score in the field's range, and the
//! entry's index is a `u32`. The old key was `(score << 32) | !index` with
//! the score's field 32 bits wide, and every instruction the tier and the
//! narrower field cost was measured on the name bench.

/// The priority ladder.
///
/// The reference's `DiskSearchPriority` is a ladder of six levels (§4.6), and the
/// levels are what a rule sets: a path the user cares about is `High`, a
/// build directory is `Low`, and `Ignored` is the one that takes a record out
/// of the live set entirely.
///
/// rgy's ladder starts with the levels the location prior can express. The
/// rules that set them are the next phase; until then the path tier is
/// derived from the prior and the other two are `Normal`.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Default)]
#[repr(u8)]
pub enum Tier {
    /// Outside the live set: not searched at all.
    Ignored = 0,
    /// Demoted: still found, always behind the normal ones.
    Low = 1,
    /// The default: no rule says otherwise.
    #[default]
    Normal = 2,
    /// Above normal: the user's own files.
    Internal = 3,
    /// Promoted.
    High = 4,
    /// Promoted, and ahead of the folders.
    HighFile = 5,
}

impl Tier {
    /// The level a location prior falls in.
    ///
    /// The prior is a sum over the parent chain, clamped to `[-100, 60]`, and
    /// it is what rgy's table of directory names produces. Bucketing it is
    /// what turns it from a term into a level: a directory the table does not
    /// recognise says nothing, so it is `Normal`, and the directories the
    /// table *does* recognise are the ones that move.
    ///
    /// `Ignored` is not reachable from a prior. A prior says "this is noise",
    /// not "this is not there"; taking a record out of the live set is a
    /// rule's business, not a heuristic's.
    ///
    /// The comparisons are written as sums and not as a match, because this
    /// is on the hot path of every candidate the search scores, and a
    /// branch there is a branch the predictor has to get right.
    pub const fn from_prior(prior: i32) -> Tier {
        let level = (prior >= -10) as u8 + (prior >= 10) as u8 + (prior >= 40) as u8;
        match level {
            0 => Tier::Low,
            1 => Tier::Normal,
            2 => Tier::Internal,
            _ => Tier::High,
        }
    }

    /// The level as the three bits the key carries it in.
    pub const fn bits(self) -> u64 {
        self as u64
    }

    /// The level the three bits stand for.
    pub const fn from_bits(bits: u64) -> Tier {
        match bits & 0b111 {
            0 => Tier::Ignored,
            1 => Tier::Low,
            2 => Tier::Normal,
            3 => Tier::Internal,
            4 => Tier::High,
            _ => Tier::HighFile,
        }
    }
}

/// The bits of an entry's kind byte that carry its priority tier.
///
/// The kind byte holds the entry's type in its low two bits and the hidden
/// and mount flags above them; bits 5 to 7 were free, and they carry the
/// tier the entry inherits from its directory.
pub const KIND_TIER_SHIFT: u32 = 5;

/// The tier an entry's kind byte carries.
pub const fn tier_of_kind(kind: u8) -> Tier {
    Tier::from_bits((kind >> KIND_TIER_SHIFT) as u64 & 0b111)
}

/// The fractional bits of the fixed-point score.
///
/// The reference's score is fixed-point with 13 fractional bits, and the search
/// quantises it before comparing (§5.8.1). rgy's matchers score in whole
/// points today, so the fraction is zero -- but the score is a fixed-point
/// value all the same, so an axis that carries a fractional weight has
/// somewhere to put it.
pub const FRAC_BITS: u32 = 13;

/// One whole point, in the fixed-point score's units.
pub const FRAC_ONE: i32 = 1 << FRAC_BITS;

/// A score in 1/8192 of a point.
///
/// The value is kept as the two parts it is, not as one scaled integer: a
/// scaled integer has to be divided to be read back, and the quantisation
/// below is on the hot path of every candidate the search scores.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Default)]
pub struct Score {
    /// The whole points.
    points: i32,
    /// The fraction of a point, in 1/8192 of one.
    frac: u16,
}

impl Score {
    /// A score in whole points.
    pub const fn from_points(points: i32) -> Score {
        Score { points, frac: 0 }
    }

    /// A score with a fraction of a point.
    pub const fn from_fixed(points: i32, frac: u16) -> Score {
        Score { points, frac }
    }

    /// The score's fractional part, in 1/8192 of a point.
    pub const fn fraction(self) -> u16 {
        self.frac
    }

    /// The score in whole points, rounded **up**.
    ///
    /// This is the quantisation the reference's search does: the ceiling of the
    /// fixed-point value, so a score that is a fraction of a point above
    /// another still lands in the bucket above it. Two scores that differ by
    /// less than a point are one bucket, and the key carries the bucket.
    pub const fn quantised(self) -> i32 {
        self.points + (self.frac != 0) as i32
    }
}

/// The bias of the score field: 23 bits, so the usable range is
/// `[-2^22, 2^22 - 1]`, and recovering the signed value is one subtraction.
///
/// The score the ranking produces -- the fzf score plus the folder bonuses
/// plus the prior -- is well inside it, and the field is narrower than
/// the reference's 24 so that the entry's index keeps its 32 bits: a mask on the
/// hot path is an instruction the whole scan pays for every candidate.
const SCORE_BIAS: i32 = 1 << 22;

/// The packed ranking key.
///
/// One `u64`, and the order of the results is its order. The tier is in the
/// high bits because it has to be there: a priority that is only a term
/// added to the score loses to a better name, and the corpus says the
/// priority is what the user meant.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Default)]
pub struct RankKey(u64);

impl RankKey {
    /// The entry field of a key that stands for no entry.
    ///
    /// The overlay's hits have no entry in the index, and their key has to
    /// lose the tie to a base entry's: the merge has always put the base
    /// entries first, and the entry field is where that order lives.
    pub const NO_ENTRY: u32 = 0xffff_ffff;

    /// The key of an entry: its three tiers, its score and its place in the
    /// order.
    ///
    /// `entry` is the entry's index, and it is the tie-break: two entries
    /// with the same tier and the same score come out in index order, which
    /// is the order the index itself is in.
    pub const fn new(path: Tier, name: Tier, attribute: Tier, score: Score, entry: u32) -> RankKey {
        let biased = score.quantised().saturating_add(SCORE_BIAS);
        RankKey((path.bits() << 61) | (name.bits() << 58) | (attribute.bits() << 55) | ((biased as u64 & 0x7f_ffff) << 32) | (!entry as u64))
    }

    /// The key of an entry whose tier rides in its kind byte.
    ///
    /// The path's tier is in the kind byte's bits 5 to 7, so it can go into
    /// the key's tier field without being unpacked first: the search reads
    /// the kind byte anyway, and unpacking the tier there and repacking it
    /// here cost 13% of the scan.
    pub const fn for_entry(kind: u8, score: Score, entry: u32) -> RankKey {
        let biased = score.quantised().saturating_add(SCORE_BIAS);
        RankKey(
            ((kind as u64 & 0xe0) << 56)
                | (Tier::Normal.bits() << 58)
                | (Tier::Normal.bits() << 55)
                | ((biased as u64 & 0x7f_ffff) << 32)
                | (!entry as u64),
        )
    }

    /// The key's bits.
    pub const fn bits(self) -> u64 {
        self.0
    }

    /// The key the bits stand for.
    pub const fn from_bits(bits: u64) -> RankKey {
        RankKey(bits)
    }

    /// The entry the key stands for.
    pub const fn entry(self) -> u32 {
        !(self.0 as u32)
    }

    /// The score, in whole points.
    pub const fn points(self) -> i32 {
        ((self.0 >> 32) & 0x7f_ffff) as i32 - SCORE_BIAS
    }

    /// The tier on the path axis.
    pub const fn path_tier(self) -> Tier {
        Tier::from_bits(self.0 >> 61)
    }

    /// The tier on the name axis.
    pub const fn name_tier(self) -> Tier {
        Tier::from_bits(self.0 >> 58)
    }

    /// The tier on the attribute axis.
    pub const fn attribute_tier(self) -> Tier {
        Tier::from_bits(self.0 >> 55)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The key is one number and the order is its order: a higher tier wins
    /// whatever the score says, and within a tier the score decides.
    #[test]
    fn a_higher_tier_outranks_any_score() {
        let low = RankKey::new(Tier::Low, Tier::Normal, Tier::Normal, Score::from_points(9000), 1);
        let normal = RankKey::new(Tier::Normal, Tier::Normal, Tier::Normal, Score::from_points(-9000), 2);
        assert!(normal > low, "the tier decides, not the score");
    }

    /// Within a tier, the score decides; and the tie-break is the entry's
    /// index, so the order is total.
    #[test]
    fn within_a_tier_the_score_decides() {
        let a = RankKey::new(Tier::Normal, Tier::Normal, Tier::Normal, Score::from_points(100), 7);
        let b = RankKey::new(Tier::Normal, Tier::Normal, Tier::Normal, Score::from_points(99), 3);
        assert!(a > b, "the better score wins");
        let c = RankKey::new(Tier::Normal, Tier::Normal, Tier::Normal, Score::from_points(100), 8);
        assert!(a > c, "the same score is broken by the index");
    }

    /// The key carries the score whole, so the score a caller reads back is
    /// the score it put in.
    #[test]
    fn the_key_carries_the_score() {
        for points in [-130_000, -1, 0, 1, 130_000] {
            let k = RankKey::new(Tier::Normal, Tier::Normal, Tier::Normal, Score::from_points(points), 42);
            assert_eq!(k.points(), points, "the score survives the packing");
            assert_eq!(k.entry(), 42, "the entry survives the packing");
        }
    }

    /// The score field is 23 bits wide, so the range it holds is
    /// `[-2^22, 2^22 - 1]`. The score the ranking produces -- the fzf score
    /// plus the folder bonuses plus the prior -- is well inside it, and the
    /// entry's index keeps its 32 bits beside it.
    #[test]
    fn the_score_fits_its_field() {
        for points in [-2_097_151, -130_000, -1, 0, 1, 130_000, 2_097_151] {
            let k = RankKey::new(Tier::Normal, Tier::Normal, Tier::Normal, Score::from_points(points), 0);
            assert_eq!(k.points(), points, "the score survives the packing");
        }
    }

    /// The entry's index is a `u32`, and the key keeps all of it.
    #[test]
    fn the_entry_keeps_its_whole_index() {
        for entry in [0u32, 1, 65_535, 5_000_000, u32::MAX] {
            let k = RankKey::new(Tier::Normal, Tier::Normal, Tier::Normal, Score::from_points(7), entry);
            assert_eq!(k.entry(), entry, "the entry survives the packing");
        }
    }

    /// The score is fixed-point with 13 fractional bits, and the quantisation
    /// rounds up: a score a fraction of a point above another is still a
    /// bucket above it.
    #[test]
    fn the_score_is_fixed_point_and_quantises_upwards() {
        assert_eq!(Score::from_points(100).fraction(), 0, "a whole point has no fraction");
        let half = Score::from_fixed(0, (FRAC_ONE / 2) as u16);
        assert_eq!(half.fraction(), (FRAC_ONE / 2) as u16);
        assert_eq!(half.quantised(), 1, "half a point rounds up to one");
        assert_eq!(Score::from_points(100).quantised(), 100);
    }

    /// The ladder has six levels and they are ordered.
    #[test]
    fn the_ladder_is_ordered_and_round_trips() {
        let all = [Tier::Ignored, Tier::Low, Tier::Normal, Tier::Internal, Tier::High, Tier::HighFile];
        for (i, t) in all.iter().enumerate() {
            assert_eq!(Tier::from_bits(t.bits()), *t, "the bits round trip");
            if i > 0 {
                assert!(*t > all[i - 1], "the ladder ascends");
            }
        }
    }

    /// The prior maps onto the ladder, and the mapping is monotone: a
    /// directory the table recognises moves down, one it does not stays.
    #[test]
    fn the_prior_maps_onto_the_ladder() {
        assert_eq!(Tier::from_prior(0), Tier::Normal, "an unrecognised directory is neutral");
        assert_eq!(Tier::from_prior(-30), Tier::Low, "a build directory is demoted");
        assert_eq!(Tier::from_prior(-100), Tier::Low);
        assert_eq!(Tier::from_prior(15), Tier::Internal);
        assert_eq!(Tier::from_prior(60), Tier::High);
        assert!(Tier::from_prior(-30) < Tier::from_prior(0));
        assert!(Tier::from_prior(0) < Tier::from_prior(15));
    }
}
