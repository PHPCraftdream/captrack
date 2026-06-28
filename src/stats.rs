//! Statistical summaries for capacity sample lists.
//!
//! `SampleStats::from_samples(&[usize])` turns a raw `samples` list (the one
//! recorded per call-site in the registry / dumped to JSON) into a compact
//! summary: `count`, `min`, `max`, `mean`, `median`, `p90`, `p95`, `p99`,
//! `stddev`.
//!
//! Always compiled — pure arithmetic on `&[usize]`, no telemetry dep.  Useful
//! for post-processing a `dump_capacity_stats(...)` JSON file: deserialize the
//! `samples` array, run it through `SampleStats::from_samples`, and decide
//! what capacity to set at the call-site.
//!
//! # Choosing a capacity from the stats
//!
//! * **`max`** — never reallocates, may waste memory on outliers.
//! * **`p99`** — covers 99% of cases, one realloc per 100 instances.
//! * **`p95`** — balance memory vs realloc rate.
//! * **`median`** — typical case; cheap memory, may realloc often on the tail.
//!
//! Capacity samples are typically heavily right-skewed (geometric growth:
//! 4, 8, 16, 32, 64, …).  They are **NOT Gaussian** — `mean + 2*stddev` is a
//! misleading recommendation for this distribution.  Use `p95` or `p99`
//! directly; they return actual observed capacities that match how `Vec` grows.
//!
//! # Example
//!
//! ```
//! use captrack::SampleStats;
//!
//! let samples = [16, 32, 32, 64, 64, 64, 128, 128, 256, 1024];
//! let s = SampleStats::from_samples(&samples).unwrap();
//! assert_eq!(s.count, 10);
//! assert_eq!(s.min, 16);
//! assert_eq!(s.max, 1024);
//! assert_eq!(s.median, 64);
//! assert_eq!(s.p99, 1024);
//! ```

#[cfg(feature = "telemetry")]
use serde::Serialize;

/// Summary statistics computed from a capacity-samples list.
///
/// `Serialize` is implemented only with the `telemetry` feature (since that's
/// when `serde` is in the dependency graph).  The struct itself is always
/// available; `from_samples` does pure `&[usize]` math.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "telemetry", derive(Serialize))]
pub struct SampleStats {
    /// Number of samples.
    pub count: usize,
    /// Smallest sample.
    pub min: usize,
    /// Largest sample (== p100).
    pub max: usize,
    /// Arithmetic mean.
    pub mean: f64,
    /// 50th percentile (median, nearest-rank — lower median for even-length).
    pub median: usize,
    /// 90th percentile (nearest-rank).
    pub p90: usize,
    /// 95th percentile (nearest-rank).
    pub p95: usize,
    /// 99th percentile (nearest-rank).
    pub p99: usize,
    /// Population standard deviation (divides by N, not N-1).
    pub stddev: f64,
}

impl SampleStats {
    /// Compute the summary.  Returns `None` for an empty slice.
    ///
    /// Allocates a sorted copy of the input internally (samples are usually
    /// small per call-site; if you have millions of entries per site and care,
    /// sort in place once and use `from_sorted_samples`).
    pub fn from_samples(samples: &[usize]) -> Option<Self> {
        if samples.is_empty() {
            return None;
        }
        let mut sorted: Vec<usize> = samples.to_vec();
        sorted.sort_unstable();
        Some(Self::from_sorted_samples(&sorted))
    }

    /// Same as `from_samples` but assumes the input is already sorted
    /// ascending.  Skips the internal allocation + sort.
    ///
    /// # Panics
    ///
    /// Panics if `sorted` is empty.  (`from_samples` returns `Option`; this
    /// variant is the unchecked fast path.)
    pub fn from_sorted_samples(sorted: &[usize]) -> Self {
        let count = sorted.len();
        assert!(count > 0, "from_sorted_samples requires a non-empty slice");
        let min = sorted[0];
        let max = sorted[count - 1];

        // Use u128 to avoid overflow when summing many large usizes.
        let sum: u128 = sorted.iter().map(|&x| x as u128).sum();
        let mean = sum as f64 / count as f64;

        let variance = sorted
            .iter()
            .map(|&x| {
                let d = x as f64 - mean;
                d * d
            })
            .sum::<f64>()
            / count as f64;
        let stddev = variance.sqrt();

        Self {
            count,
            min,
            max,
            mean,
            median: percentile(sorted, 50),
            p90: percentile(sorted, 90),
            p95: percentile(sorted, 95),
            p99: percentile(sorted, 99),
            stddev,
        }
    }
}

/// Nearest-rank percentile on a sorted slice.
///
/// Definition: `ceil(p/100 * n)`-th element (1-indexed), clamped to `[1, n]`.
/// `p = 100` returns the max, `p = 0` is treated as 1 (returns min).
fn percentile(sorted: &[usize], p: u8) -> usize {
    let n = sorted.len();
    debug_assert!(n > 0);
    let rank = ((p as f64 / 100.0) * n as f64).ceil() as usize;
    let idx = rank.saturating_sub(1).min(n - 1);
    sorted[idx]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_is_none() {
        assert!(SampleStats::from_samples(&[]).is_none());
    }

    #[test]
    fn single_sample() {
        let s = SampleStats::from_samples(&[42]).unwrap();
        assert_eq!(s.count, 1);
        assert_eq!(s.min, 42);
        assert_eq!(s.max, 42);
        assert_eq!(s.mean, 42.0);
        assert_eq!(s.median, 42);
        assert_eq!(s.p99, 42);
        assert_eq!(s.stddev, 0.0);
    }

    #[test]
    fn identical_values_have_zero_stddev() {
        let s = SampleStats::from_samples(&[64; 50]).unwrap();
        assert_eq!(s.count, 50);
        assert_eq!(s.min, 64);
        assert_eq!(s.max, 64);
        assert_eq!(s.mean, 64.0);
        assert_eq!(s.median, 64);
        assert_eq!(s.stddev, 0.0);
    }

    #[test]
    fn unsorted_input_gets_sorted() {
        let s = SampleStats::from_samples(&[3, 1, 4, 1, 5, 9, 2, 6, 5, 3]).unwrap();
        assert_eq!(s.count, 10);
        assert_eq!(s.min, 1);
        assert_eq!(s.max, 9);
    }

    #[test]
    fn percentiles_nearest_rank() {
        // 100 samples 1..=100; percentile p returns ceil(p/100*100) = p (clamped).
        let samples: Vec<usize> = (1..=100).collect();
        let s = SampleStats::from_samples(&samples).unwrap();
        assert_eq!(s.median, 50);
        assert_eq!(s.p90, 90);
        assert_eq!(s.p95, 95);
        assert_eq!(s.p99, 99);
        assert_eq!(s.max, 100);
    }

    #[test]
    fn from_sorted_samples_skips_sort() {
        let sorted = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10];
        let s = SampleStats::from_sorted_samples(&sorted);
        assert_eq!(s.median, 5);
        assert_eq!(s.max, 10);
    }

    #[test]
    #[should_panic]
    fn from_sorted_samples_panics_on_empty() {
        let _ = SampleStats::from_sorted_samples(&[]);
    }

    #[test]
    fn outliers_drive_max_not_median() {
        // 99 samples of 64, one sample of 100_000.
        let mut samples = vec![64usize; 99];
        samples.push(100_000);
        let s = SampleStats::from_samples(&samples).unwrap();
        assert_eq!(s.median, 64);
        assert_eq!(s.p95, 64);
        assert_eq!(s.p99, 64); // 99th of 100 = 64
        assert_eq!(s.max, 100_000);
        assert!(s.stddev > 0.0);
    }

    #[test]
    fn known_mean_and_stddev() {
        // [2, 4, 4, 4, 5, 5, 7, 9] — textbook example.
        // mean = 5, population stddev = 2 (variance = 4).
        // Median by nearest-rank for even n=8: rank = ceil(0.5 * 8) = 4,
        // sorted[3] = 4 (lower median, not the interpolated 4.5).
        let s = SampleStats::from_samples(&[2, 4, 4, 4, 5, 5, 7, 9]).unwrap();
        assert_eq!(s.mean, 5.0);
        assert!((s.stddev - 2.0).abs() < 1e-12);
        assert_eq!(s.min, 2);
        assert_eq!(s.max, 9);
        assert_eq!(s.median, 4);
    }
}
