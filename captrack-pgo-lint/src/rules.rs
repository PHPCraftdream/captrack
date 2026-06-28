//! Capacity-decision rules — self-contained copy of `captrack_pgo::rules`.
//!
//! The plugin can't depend on the stable `captrack-pgo` crate (it compiles on
//! nightly against `rustc_private`), so the logic is reproduced here.
//!
//! ## Rule precedence (first match wins)
//!
//! 1. `count < MIN_COUNT` → Skip "low frequency, not statistically sound"
//! 2. `peak == 0`         → Skip "phantom site (no allocation observed)"
//! 3. `current` is Dynamic → Skip "dynamic cap expression (manual review)"
//! 4. `current::Literal(N) >= peak` → Skip "current cap already sufficient"
//! 5. `current::Zero && peak >= 4`              → Patch(round(p95))
//! 6. `current::Literal(c > 0) && peak >= 4*c` → Patch(round(p95))
//! 7. otherwise                                 → Skip "variation within normal bounds"
//!
//! Rounding: `next_pow2` by default.
//! `Unit::Bytes` sites produce a `Patch` with a note that the caller must
//! divide by `size_of::<T>` — the plugin emits a warning-only in that case.
//!
//! These constants and behaviour are **identical** to those in
//! `captrack-pgo/src/rules.rs` so that running `captrack-pgo apply` and
//! `cargo dylint --fix` produce the same numerical results for the same
//! profile data.

use crate::model::{CapExpr, SiteStats, Unit};

/// Minimum observation count for a site to be considered statistically sound.
pub const MIN_COUNT: u64 = 10;

/// Growth factor: only patch if observed peak is at least this many times
/// the current literal cap (rule 6).
pub const GROWTH_FACTOR: usize = 4;

/// Decision produced by the rules engine for one (stats, current-cap) pair.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    /// Emit a suggestion replacing the constructor with `with_capacity(to)`.
    Patch {
        /// Proposed new capacity (already rounded).
        to: usize,
        /// Human-readable rationale for the suggestion message.
        reason: String,
        /// Whether the replacement is safe to apply automatically.
        applicability: Applicability,
    },
    /// Emit a warning only — no fix-it suggestion.
    Skip { reason: String },
}

/// Mirrors `rustc_errors::Applicability` so `rules.rs` stays free of rustc
/// imports (the main lint code maps this to the real enum).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Applicability {
    MachineApplicable,
    MaybeIncorrect,
}

/// Core decision routine.  Pure: no side effects, no I/O.
pub fn propose_cap(stats: &SiteStats, current: &CapExpr) -> Decision {
    // Rule 1 — low frequency.
    if stats.count < MIN_COUNT {
        return Decision::Skip {
            reason: format!(
                "count={} < {} (low frequency, not statistically sound)",
                stats.count, MIN_COUNT,
            ),
        };
    }
    // Rule 2 — phantom site.
    if stats.peak == 0 {
        return Decision::Skip {
            reason: "peak=0 (phantom site, no allocation observed)".into(),
        };
    }
    // Rule 3 — dynamic cap expression.
    if matches!(current, CapExpr::Dynamic(_)) {
        return Decision::Skip {
            reason: "current cap is a dynamic expression (manual review)".into(),
        };
    }
    // Rule 4 — current literal already covers peak.
    if let CapExpr::Literal(c) = current {
        if *c >= stats.peak {
            return Decision::Skip {
                reason: format!(
                    "current cap {} already covers observed peak {}",
                    c, stats.peak
                ),
            };
        }
    }
    // Rules 5 + 6 — growth gate.
    let current_lit: usize = match current {
        CapExpr::Zero => 0,
        CapExpr::Literal(c) => *c,
        CapExpr::Dynamic(_) => unreachable!("filtered by rule 3"),
    };
    let growth_threshold = if current_lit == 0 {
        4
    } else {
        current_lit.saturating_mul(GROWTH_FACTOR)
    };
    if stats.peak < growth_threshold {
        return Decision::Skip {
            reason: format!(
                "peak={} < {}x current={} (variation within normal bounds)",
                stats.peak, GROWTH_FACTOR, current_lit,
            ),
        };
    }

    // Proposed cap: p95 rounded up to next power of two.
    let proposed = next_pow2(stats.p95.max(1));

    // Byte-unit sites: the plugin cannot know size_of::<T> at lint time, so we
    // still produce a Patch (the number is in bytes) but flag it MaybeIncorrect
    // so the user knows to divide by element size.
    let (reason, applicability) = match stats.unit {
        Unit::Elements => (
            format!(
                "peak={} p95={} count={} → NextPow2",
                stats.peak, stats.p95, stats.count,
            ),
            Applicability::MachineApplicable,
        ),
        Unit::Bytes => (
            format!(
                "peak={} p95={} count={} → NextPow2 (bytes — divide by size_of::<T>)",
                stats.peak, stats.p95, stats.count,
            ),
            Applicability::MaybeIncorrect,
        ),
    };

    Decision::Patch {
        to: proposed,
        reason,
        applicability,
    }
}

/// Round `n` up to the next power of two (same as `Rounding::NextPow2` in
/// `captrack-pgo/src/rules.rs`).
fn next_pow2(n: usize) -> usize {
    if n <= 1 {
        n
    } else {
        n.checked_next_power_of_two().unwrap_or(usize::MAX)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{SiteKey, Unit};
    use std::path::PathBuf;

    fn key() -> SiteKey {
        SiteKey {
            file: PathBuf::from("x.rs"),
            line: 1,
            col: 1,
        }
    }

    fn stats(peak: usize, p95: usize, count: u64, unit: Unit) -> SiteStats {
        SiteStats {
            key: key(),
            unit,
            peak,
            p50: p95,
            p95,
            count,
        }
    }

    #[test]
    fn skips_low_count() {
        let d = propose_cap(&stats(64, 64, 5, Unit::Elements), &CapExpr::Zero);
        assert!(matches!(d, Decision::Skip { .. }));
        if let Decision::Skip { reason } = d {
            assert!(reason.contains("low frequency"));
        }
    }

    #[test]
    fn skips_phantom_site() {
        let d = propose_cap(&stats(0, 0, 1000, Unit::Elements), &CapExpr::Zero);
        assert!(matches!(d, Decision::Skip { reason } if reason.contains("phantom")));
    }

    #[test]
    fn skips_dynamic_cap_expr() {
        let d = propose_cap(
            &stats(100, 80, 1000, Unit::Elements),
            &CapExpr::Dynamic("input.len()".into()),
        );
        assert!(matches!(d, Decision::Skip { reason } if reason.contains("dynamic")));
    }

    #[test]
    fn skips_when_current_literal_covers_peak() {
        let d = propose_cap(&stats(50, 50, 1000, Unit::Elements), &CapExpr::Literal(64));
        assert!(matches!(d, Decision::Skip { reason } if reason.contains("already covers")));
    }

    #[test]
    fn patches_zero_current_when_peak_meaningful() {
        let d = propose_cap(&stats(60, 60, 1000, Unit::Elements), &CapExpr::Zero);
        if let Decision::Patch {
            to, applicability, ..
        } = d
        {
            assert_eq!(to, 64); // p95=60 → next_pow2 = 64
            assert_eq!(applicability, Applicability::MachineApplicable);
        } else {
            panic!("expected Patch, got {:?}", d);
        }
    }

    #[test]
    fn skips_zero_current_when_peak_tiny() {
        let d = propose_cap(&stats(3, 3, 1000, Unit::Elements), &CapExpr::Zero);
        assert!(matches!(d, Decision::Skip { reason } if reason.contains("variation")));
    }

    #[test]
    fn patches_when_peak_exceeds_growth_factor() {
        let d = propose_cap(&stats(100, 90, 1000, Unit::Elements), &CapExpr::Literal(8));
        if let Decision::Patch { to, .. } = d {
            // peak=100 >= 4*8=32 ✓; p95=90 → next_pow2 = 128
            assert_eq!(to, 128);
        } else {
            panic!("expected Patch, got {:?}", d);
        }
    }

    #[test]
    fn skips_when_within_growth_factor() {
        let d = propose_cap(&stats(20, 18, 1000, Unit::Elements), &CapExpr::Literal(8));
        assert!(matches!(d, Decision::Skip { reason } if reason.contains("variation")));
    }

    #[test]
    fn bytes_unit_produces_maybe_incorrect() {
        let d = propose_cap(&stats(4096, 4096, 1000, Unit::Bytes), &CapExpr::Zero);
        if let Decision::Patch {
            applicability,
            reason,
            ..
        } = d
        {
            assert_eq!(applicability, Applicability::MaybeIncorrect);
            assert!(reason.contains("bytes"));
            assert!(reason.contains("size_of"));
        } else {
            panic!("expected Patch");
        }
    }

    #[test]
    fn next_pow2_helper() {
        assert_eq!(next_pow2(1), 1);
        assert_eq!(next_pow2(2), 2);
        assert_eq!(next_pow2(3), 4);
        assert_eq!(next_pow2(60), 64);
        assert_eq!(next_pow2(65), 128);
        assert_eq!(next_pow2(512), 512);
    }
}
