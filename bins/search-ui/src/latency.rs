// SPDX-License-Identifier: Apache-2.0

//! Query-latency metering for the search UI: one cold-start sample plus a
//! 64-wide rolling window with nearest-rank percentiles and a log-scaled
//! sparkline.

use std::collections::VecDeque;

/// Rolling-window capacity (samples after the cold-start `first`).
const WINDOW_CAP: usize = 64;

/// Sparkline blocks, low to high (`'▁'` .. `'█'`).
const BLOCKS: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];

/// Latency meter: a cold-start `first` sample plus a rolling window of up to
/// 64 microsecond samples. `first` is never part of the window, percentiles,
/// or the sparkline.
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
    /// chars. Scaled against the slowest sample on a ln_1p curve so one cold
    /// bar does not flatten the rest. Blocks: '▁▂▃▄▅▆▇█'.
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

        let slowest = cols.iter().copied().max().unwrap_or(0);
        let denom = (slowest as f64).ln_1p();
        for v in cols {
            let score = if denom > 0.0 {
                (v as f64).ln_1p() / denom
            } else {
                0.0
            };
            let idx = (score * (BLOCKS.len() - 1) as f64).round() as usize;
            out.push(BLOCKS[idx.min(BLOCKS.len() - 1)]);
        }
        out
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
}
