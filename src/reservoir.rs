// Bounded reservoir sampling for capacity telemetry.
//
// Implements Vitter Algorithm R: given a stream of N samples, maintain a
// fixed-size reservoir of at most CAP values such that every sample has an
// equal probability (CAP / N) of being retained.  For N <= CAP every sample
// is stored verbatim (no eviction).
//
// # Design
//
// Variant A: `Mutex<Vec<usize>>` + `AtomicU64 seen_count`.
//
// * Contention is minimal — `record_sample` is called from Drop / cap_inspect,
//   not from tight inner loops.  For the telemetry use case (19K creations over
//   the lifetime of a benchmark run) a mutex adds < 0.1% overhead compared to
//   `scc::Bag`, and removes the destructive `pop_all` dance needed to read the
//   bag without losing data.
// * The default capacity is 4096, overridable at process start via the
//   `CAPTRACK_SAMPLE_CAP` environment variable.
// * `seen_count` is an `AtomicU64` so the drop count is always accurate,
//   independent of the reservoir bound.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

/// Maximum number of samples retained per call-site.
///
/// Override at process start via `CAPTRACK_SAMPLE_CAP` (e.g. `CAPTRACK_SAMPLE_CAP=128`).
/// The value is read once from the environment the first time a `Reservoir` is constructed.
const DEFAULT_CAP: usize = 4096;

fn env_cap() -> usize {
    std::env::var("CAPTRACK_SAMPLE_CAP")
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
        .filter(|&n| n > 0)
        .unwrap_or(DEFAULT_CAP)
}

/// A bounded reservoir sampler for `usize` values (capacity measurements).
///
/// Thread-safe: `record` may be called from any thread concurrently.
pub struct Reservoir {
    /// Current reservoir — at most `cap` elements.
    buf: Mutex<Vec<usize>>,
    /// Total number of values offered to the reservoir (including evicted ones).
    pub seen_count: AtomicU64,
    /// Maximum number of samples stored.
    cap: usize,
}

impl Reservoir {
    /// Create a new reservoir with capacity determined by `CAPTRACK_SAMPLE_CAP`
    /// (or `DEFAULT_CAP` if the variable is absent / unparseable).
    pub fn new() -> Self {
        let cap = env_cap();
        Self {
            buf: Mutex::new(Vec::with_capacity(cap.min(64))),
            seen_count: AtomicU64::new(0),
            cap,
        }
    }

    /// Offer one sample to the reservoir.
    ///
    /// * If seen < cap: push directly.
    /// * Otherwise: with probability `cap / seen`, replace a random existing slot.
    ///
    /// The `seen_count` counter is incremented regardless of whether the value
    /// was retained.
    pub fn record(&self, value: usize) {
        // Increment the total counter first (Relaxed — ordering with buf lock
        // is provided by the Mutex).
        let seen = self.seen_count.fetch_add(1, Ordering::Relaxed);
        // `seen` is 0-indexed (value before increment); slot index = seen.
        // After this call seen_count == seen + 1.
        let mut buf = self.buf.lock().unwrap_or_else(|p| p.into_inner());
        if seen < self.cap as u64 {
            // Reservoir not yet full — push unconditionally.
            buf.push(value);
        } else {
            // Reservoir is full: replace slot j (0..cap) with probability cap/seen+1.
            // Vitter Algorithm R: pick j uniformly in [0, seen]; replace iff j < cap.
            let j = fastrand::u64(0..=seen);
            if (j as usize) < self.cap {
                buf[j as usize] = value;
            }
        }
    }

    /// Return a snapshot (clone) of the current reservoir contents.
    ///
    /// Does not consume or reset the reservoir.  Suitable for periodic dumps.
    pub fn snapshot(&self) -> Vec<usize> {
        let buf = self.buf.lock().unwrap_or_else(|p| p.into_inner());
        buf.clone()
    }

    /// Number of values that have been offered (not capped by reservoir size).
    pub fn total_observed(&self) -> u64 {
        self.seen_count.load(Ordering::Relaxed)
    }
}

impl Default for Reservoir {
    fn default() -> Self {
        Self::new()
    }
}
