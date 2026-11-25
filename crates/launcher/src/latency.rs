// SPDX-License-Identifier: Apache-2.0

//! How long the answers take.
//!
//! The meter is the launcher's, not the terminal's: the window and the
//! terminal are two faces of the same thing, and a face that shows the
//! time and a face that does not are two products. So the meter lives in
//! the model — what the engine answered and how long it took to answer
//! is a decision — and both faces read it from there.
//!
//! One cold-start sample plus a rolling window of up to 64 microsecond
//! samples. The cold start is kept **out** of the window: the first
//! answer of a session pays for the page faults and the cold index, and
//! a percentile that counts it describes a session that has not started
//! yet.
//!
//! The crate is `no_std` + `alloc`, so the window is a `VecDeque` from
//! `alloc` and nothing else.

use alloc::collections::VecDeque;
use alloc::string::String;
use alloc::vec::Vec;

use crate::text::Text;

/// Rolling-window capacity (samples after the cold-start `first`).
const WINDOW_CAP: usize = 64;

/// Sparkline blocks, low to high (`'▁'` .. `'█'`).
const BLOCKS: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];

/// Latency meter: a cold-start `first` sample plus a rolling window of up
/// to 64 microsecond samples. `first` is never part of the window,
/// percentiles, or the sparkline.
pub struct LatencyMeter {
    first: Option<u128>,
    window: VecDeque<u128>,
}

impl Default for LatencyMeter {
    fn default() -> Self {
        Self::new()
    }
}

impl LatencyMeter {
    /// Empty meter: no `first`, empty window.
    pub fn new() -> Self {
        Self {
            first: None,
            window: VecDeque::with_capacity(WINDOW_CAP),
        }
    }

    /// Record one query latency in microseconds. The FIRST recorded sample is
    /// the cold-start `first` and is kept OUT of the rolling window.
    pub fn record(&mut self, us: u128) {
        if self.first.is_none() {
            self.first = Some(us);
            return;
        }
        if self.window.len() == WINDOW_CAP {
            self.window.pop_front();
        }
        self.window.push_back(us);
    }

    /// Cold-start latency (first query only).
    pub fn first(&self) -> Option<u128> {
        self.first
    }

    /// Most recent sample (the cold `first` before the window has any).
    pub fn last(&self) -> Option<u128> {
        self.window.back().copied().or(self.first)
    }

    /// Nearest-rank percentile over the rolling window (excludes `first`).
    /// For n sorted samples: index = ceil(p*n)-1, clamped to [0, n-1].
    pub fn p50(&self) -> Option<u128> {
        self.percentile(1, 2)
    }

    /// Nearest-rank percentile over the rolling window (excludes `first`).
    /// For n sorted samples: index = ceil(p*n)-1, clamped to [0, n-1].
    pub fn p90(&self) -> Option<u128> {
        self.percentile(9, 10)
    }

    /// Window occupancy (not counting `first`).
    pub fn window_len(&self) -> usize {
        self.window.len()
    }

    /// Log-scaled bar chart of the window, newest sample rightmost, `width`
    /// chars. Scaled against the slowest sample on a binary logarithm so
    /// one cold bar does not flatten the rest. Blocks: '▁▂▃▄▅▆▇█'.
    ///
    /// With fewer samples than `width`, bars are right-aligned (left-padded
    /// with spaces). With more, samples are bucketed column-wise by max so
    /// spikes survive. `width == 0` or an empty window gives an empty string.
    pub fn sparkline(&self, width: usize) -> String {
        if width == 0 || self.window.is_empty() {
            return String::new();
        }

        let n = self.window.len();
        let mut out = String::with_capacity(width);
        let mut cols: Vec<u128> = Vec::with_capacity(width);

        if n >= width {
            let vals: Vec<u128> = self.window.iter().copied().collect();
            for i in 0..width {
                let start = i * n / width;
                let end = (i + 1) * n / width;
                cols.push(vals[start..end].iter().copied().max().unwrap_or(0));
            }
        } else {
            for _ in 0..(width - n) {
                out.push(' ');
            }
            cols.extend(self.window.iter().copied());
        }

        // The scale is the ratio of two binary logarithms: the slowest
        // sample is the top of the chart, and the bar of a sample is
        // where it falls between the bottom and the top.
        let top = ilog2(cols.iter().copied().max().unwrap_or(0));
        let levels = (BLOCKS.len() - 1) as u32;
        for v in cols {
            // A chart with no slowest sample has no scale: the division
            // by zero is the answer, and the answer is the bottom bar.
            let level = (ilog2(v) * levels)
                .checked_div(top)
                .unwrap_or(0)
                .min(levels);
            out.push(BLOCKS[level as usize]);
        }
        out
    }

    /// The figures a face shows.
    ///
    /// The sparkline's width is the caller's: the terminal has eight
    /// columns for it and the window has as many as it has room for, and
    /// the meter does not know which face is asking.
    pub fn view(&self, spark_width: usize) -> Latency {
        Latency {
            last_us: self.last().map(|us| us as u64),
            first_us: self.first().map(|us| us as u64),
            p50_us: self.p50().map(|us| us as u64),
            p90_us: self.p90().map(|us| us as u64),
            spark: self.sparkline(spark_width),
        }
    }

    /// The figures a face shows, in the words of the language.
    ///
    /// The words are the vocabulary's and the numbers are the meter's, and
    /// the composition is here so that the two faces cannot say the same
    /// thing in two ways.
    pub fn stats(&self, text: &Text) -> Option<String> {
        stats(self.first(), self.p50(), self.p90(), text)
    }

    /// Nearest-rank percentile over the sorted window at p = num/den.
    /// Index = ceil(p*n)-1, clamped to [0, n-1]. Empty window -> None.
    fn percentile(&self, num: u128, den: u128) -> Option<u128> {
        let n = self.window.len();
        if n == 0 {
            return None;
        }
        let mut sorted: Vec<u128> = self.window.iter().copied().collect();
        sorted.sort_unstable();
        // ceil(n * num / den) without float.
        let ceil_pn = (n as u128 * num).div_ceil(den);
        let idx = (ceil_pn as usize).saturating_sub(1).min(n - 1);
        Some(sorted[idx])
    }
}

/// What a face shows about the time the answers take.
///
/// The numbers are microseconds and they are `Option`: before the first
/// answer there is nothing to show, and a zero is a measurement, not an
/// absence.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Latency {
    /// The last answer's time.
    pub last_us: Option<u64>,
    /// The cold start, kept out of the window.
    pub first_us: Option<u64>,
    /// The nearest-rank p50 of the window.
    pub p50_us: Option<u64>,
    /// The nearest-rank p90 of the window.
    pub p90_us: Option<u64>,
    /// The sparkline of the last few dozen answers.
    pub spark: String,
}

impl Latency {
    /// The last answer's time, as a person reads it: `8.1 ms`.
    pub fn last_label(&self) -> Option<String> {
        self.last_us.map(|us| duration(us as u128))
    }

    /// The figures a face shows, in the words of the language.
    ///
    /// The words are the vocabulary's and the numbers are the meter's, and
    /// the composition is here so that the two faces cannot say the same
    /// thing in two ways.
    pub fn stats(&self, text: &Text) -> Option<String> {
        stats(
            self.first_us.map(u128::from),
            self.p50_us.map(u128::from),
            self.p90_us.map(u128::from),
            text,
        )
    }

    /// Whether there is anything to show.
    ///
    /// A face that draws an empty figure is a face that says "0 ms"
    /// before the first answer, which is a measurement the launcher never
    /// took.
    pub fn is_empty(&self) -> bool {
        self.last_us.is_none()
    }
}

/// The position of the highest set bit — the integer logarithm.
///
/// `core` has no float maths: no `ln`, no `sqrt`, no `powi`. A sparkline
/// does not need them — the ratio of two binary logarithms is the scale,
/// and the scale of a chart is eight bars wide.
fn ilog2(v: u128) -> u32 {
    if v == 0 {
        return 0;
    }
    127 - v.leading_zeros()
}

/// A duration, as a person reads it: `41 µs` or `3.5 ms`.
///
/// The unit is the one the number deserves. A measurement under a
/// millisecond reads in microseconds, because "0.0 ms" is a number that
/// says nothing -- and the answers of a warm daemon are under a
/// millisecond. One above reads in milliseconds, because "5000000 µs" is
/// a number nobody reads at a glance.
///
/// The unit is not a sentence: it is the same in every language a person
/// reads, and a vocabulary of units would be a table of the same word
/// twice.
pub fn duration(us: u128) -> String {
    if us < 1000 {
        alloc::format!("{} \u{b5}s", us)
    } else {
        alloc::format!("{} ms", fmt_us(us))
    }
}

/// Render a microsecond duration the way the timings read.
pub fn fmt_us(us: u128) -> String {
    if us < 10_000 {
        alloc::format!("{:.1}", us as f64 / 1000.0)
    } else {
        alloc::format!("{}", (us + 500) / 1000)
    }
}

/// The figures of the meter, in the words of the language.
///
/// `first 40.0 ms  p50 8.1 ms  p90 12.4 ms` — the cold start on its own,
/// because it is the outlier the chart exists to show and it must not sit
/// inside p50/p90.
///
/// `None` when the meter has nothing at all: a face that draws "—" before
/// the first answer is a face that reports a measurement it never took.
pub fn stats(
    first: Option<u128>,
    p50: Option<u128>,
    p90: Option<u128>,
    text: &Text,
) -> Option<String> {
    if first.is_none() && p50.is_none() && p90.is_none() {
        return None;
    }
    let fmt = |us: Option<u128>| match us {
        Some(us) => duration(us),
        None => String::from("\u{2014}"),
    };
    Some(alloc::format!(
        "{} {}  {} {}  {} {}",
        text.latency_first,
        fmt(first),
        text.latency_p50,
        fmt(p50),
        text.latency_p90,
        fmt(p90),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_excluded_from_percentiles() {
        let mut m = LatencyMeter::new();
        m.record(1000);
        for i in 1..=10u128 {
            m.record(i);
        }
        assert_eq!(m.first(), Some(1000));
        assert_eq!(m.window_len(), 10);
        assert_eq!(m.p50(), Some(5));
        assert_eq!(m.p90(), Some(9));
    }

    #[test]
    fn nearest_rank_percentiles_1_to_10() {
        let mut m = LatencyMeter::new();
        m.record(999);
        for i in 1..=10u128 {
            m.record(i);
        }
        assert_eq!(m.p50(), Some(5));
        assert_eq!(m.p90(), Some(9));
    }

    #[test]
    fn empty_window_is_none() {
        let m = LatencyMeter::new();
        assert_eq!(m.first(), None);
        assert_eq!(m.p50(), None);
        assert_eq!(m.p90(), None);
        assert_eq!(m.window_len(), 0);

        let mut one = LatencyMeter::new();
        one.record(42);
        assert_eq!(one.first(), Some(42));
        assert_eq!(one.window_len(), 0);
        assert_eq!(one.p50(), None);
        assert_eq!(one.p90(), None);
    }

    #[test]
    fn window_eviction_keeps_last_64() {
        let mut m = LatencyMeter::new();
        for i in 0..100u128 {
            m.record(i);
        }
        assert_eq!(m.first(), Some(0));
        assert_eq!(m.window_len(), 64);
        // Survivors are 36..=99; nearest-rank p50 index 31 -> 67,
        // p90 index 57 -> 93.
        assert_eq!(m.p50(), Some(67));
        assert_eq!(m.p90(), Some(93));
    }

    #[test]
    fn sparkline_width_zero_is_empty() {
        let mut m = LatencyMeter::new();
        m.record(1);
        m.record(2);
        m.record(3);
        assert_eq!(m.sparkline(0), "");
    }

    #[test]
    fn sparkline_right_aligns_and_uses_blocks() {
        let mut m = LatencyMeter::new();
        m.record(1);
        m.record(10);
        m.record(100);
        let s = m.sparkline(5);
        assert_eq!(s.chars().count(), 5);
        // 2 samples in 5 cols -> 3 pad spaces, then one block per sample.
        assert!(s.starts_with("   "));
        assert!(s.chars().skip(3).all(|c| BLOCKS.contains(&c)));
        // Slowest sample (newest, rightmost) saturates to the top block.
        assert!(s.ends_with('█'));
    }

    /// The duration reads the way the timings read: sub-millisecond
    /// precision where it matters, whole milliseconds above that.
    #[test]
    fn the_duration_reads_below_and_above_ten_milliseconds() {
        assert_eq!(fmt_us(3_500), "3.5");
        assert_eq!(fmt_us(999), "1.0");
        assert_eq!(fmt_us(10_000), "10");
        assert_eq!(fmt_us(28_400), "28");
        assert_eq!(duration(3_500), "3.5 ms");
        assert_eq!(duration(28_400), "28 ms");
    }

    /// A measurement under a millisecond reads in microseconds.
    ///
    /// "0.0 ms" is a number that says nothing, and the answers of a warm
    /// daemon are under a millisecond: the unit that hides the number is
    /// the unit that hides the answer.
    #[test]
    fn a_measurement_under_a_millisecond_reads_in_microseconds() {
        assert_eq!(duration(0), "0 \u{b5}s");
        assert_eq!(duration(41), "41 \u{b5}s");
        assert_eq!(duration(999), "999 \u{b5}s");
        // A millisecond is a millisecond: the unit changes there.
        assert_eq!(duration(1_000), "1.0 ms");
        assert_eq!(duration(1_500), "1.5 ms");
    }

    /// The figures are said in the words of the language, and the two
    /// languages say the same thing in their own words.
    #[test]
    fn the_figures_are_said_in_the_words_of_the_language() {
        let mut m = LatencyMeter::new();
        m.record(40_000);
        m.record(8_100);
        m.record(12_400);

        let english = m.stats(Text::english()).expect("the meter has figures");
        assert_eq!(english, "first 40 ms  p50 8.1 ms  p90 12 ms");

        let portuguese = m.stats(Text::portuguese()).expect("the meter has figures");
        assert_eq!(portuguese, "primeiro 40 ms  p50 8.1 ms  p90 12 ms");
    }

    /// A meter with nothing in it says nothing: a face that draws "—"
    /// before the first answer is a face that reports a measurement it
    /// never took.
    #[test]
    fn an_empty_meter_has_no_figures_to_say() {
        let m = LatencyMeter::new();
        assert!(m.stats(Text::english()).is_none());
        assert!(m.view(8).stats(Text::english()).is_none());
        assert!(m.view(8).last_label().is_none());
    }

    /// The cold start is on its own: it is the outlier the chart exists
    /// to show, and it must not sit inside p50/p90.
    #[test]
    fn the_cold_start_is_reported_apart() {
        let mut m = LatencyMeter::new();
        m.record(40_000);
        let stats = m.stats(Text::english()).expect("the meter has figures");
        assert!(stats.starts_with("first 40 ms"), "{stats}");
        assert!(stats.contains("p50 —"), "{stats}");
        assert!(stats.contains("p90 —"), "{stats}");
    }

    /// The figures a face shows come from the meter, and an empty meter
    /// shows nothing at all.
    #[test]
    fn the_view_is_the_meters_figures() {
        let mut m = LatencyMeter::new();
        assert!(m.view(8).is_empty());

        m.record(1000);
        let view = m.view(8);
        assert!(!view.is_empty());
        assert_eq!(view.last_us, Some(1000));
        assert_eq!(view.first_us, Some(1000));
        assert_eq!(view.p50_us, None);
        assert_eq!(view.spark, "");
    }
}
