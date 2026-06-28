//! Patch-decision rules.
//!
//! Pure function `propose_cap(stats, current) → Decision`.  Takes the
//! profile-derived statistics for one site and the source's current cap
//! expression; returns either a concrete `Patch { to, reason }` or a
//! `Skip { reason }` with the rationale preserved for the report.
//!
//! ## Rule precedence (first match wins)
//!
//! 1. `count < MIN_COUNT` → Skip "low frequency, not statistically sound"
//! 2. `peak == 0`         → Skip "phantom site (no allocation observed)"
//! 3. `current` is Dynamic → Skip "dynamic cap expression (manual review)"
//! 4. `current::Literal(N) >= peak` → Skip "current cap already sufficient"
//! 5. `current::Zero && peak >= 4`              → Patch(round(p95))
//! 6. `current::Literal(c > 0) && peak >= 4*c` → Patch(round(p95))
//! 7. otherwise                                 → Skip "variation within
//!    normal bounds"
//!
//! Why **p95** not peak: peak catches outliers; p95 is a stable upper bound
//! that matches "cover 95% of cases without one realloc".  See docs/
//! CAPACITY-PGO-PLAN.md §5.
//!
//! Rounding: `next_pow2` by default (matches how `Vec` grows internally —
//! suggesting non-pow2 forces an extra realloc).  `round_to_8` available
//! via the `Rounding` knob.

use serde::{Deserialize, Serialize};

use crate::model::{CapExpr, SiteStats, Unit};

/// Minimum count for a site to be considered statistically meaningful.
pub const DEFAULT_MIN_COUNT: u64 = 10;

/// Growth factor: only patch if observed peak is at least this many times
/// the current cap (rule 6).
pub const DEFAULT_GROWTH_FACTOR: usize = 4;

/// Cap-rounding strategy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum Rounding {
    /// Round up to the next power of two (default).
    #[default]
    NextPow2,
    /// Round up to the next multiple of 8.
    To8,
    /// Use exact value (no rounding).
    Exact,
}

/// Decision the rules engine made for one (stats, current) pair.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Decision {
    Patch { to: usize, reason: String },
    Skip { reason: String },
}

/// Configurable thresholds.  All optional — defaults match the plan.
#[derive(Debug, Clone, Copy)]
pub struct RuleConfig {
    pub min_count: u64,
    pub growth_factor: usize,
    pub rounding: Rounding,
}

impl Default for RuleConfig {
    fn default() -> Self {
        Self {
            min_count: DEFAULT_MIN_COUNT,
            growth_factor: DEFAULT_GROWTH_FACTOR,
            rounding: Rounding::NextPow2,
        }
    }
}

/// Apply rounding to `n` according to the configured strategy.
pub fn apply_rounding(n: usize, mode: Rounding) -> usize {
    match mode {
        Rounding::Exact => n,
        Rounding::To8 => {
            if n == 0 {
                0
            } else {
                n.div_ceil(8).saturating_mul(8)
            }
        }
        Rounding::NextPow2 => {
            if n <= 1 {
                n
            } else {
                // checked_next_power_of_two on usize is stable.
                n.checked_next_power_of_two().unwrap_or(usize::MAX)
            }
        }
    }
}

/// Core decision routine.  Pure: no side effects, no I/O.
///
/// Convenience wrapper around `propose_cap_with` with default `RuleConfig`.
/// Primarily used in unit tests; production code goes through `build_plan`.
#[allow(dead_code)]
pub fn propose_cap(stats: &SiteStats, current: &CapExpr) -> Decision {
    propose_cap_with(stats, current, RuleConfig::default())
}

pub fn propose_cap_with(stats: &SiteStats, current: &CapExpr, cfg: RuleConfig) -> Decision {
    // Rule 1 — low frequency.
    if stats.count < cfg.min_count {
        return Decision::Skip {
            reason: format!(
                "count={} < {} (low frequency, not statistically sound)",
                stats.count, cfg.min_count
            ),
        };
    }
    // Rule 2 — phantom site (allocator never recorded a non-zero peak).
    if stats.peak == 0 {
        return Decision::Skip {
            reason: "peak=0 (phantom site, no allocation observed)".into(),
        };
    }
    // Rule 3 — dynamic cap expression.  Patcher refuses to replace these;
    // a human needs to either prove the dynamic expr already gives the
    // right value or change it to a literal.
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
        current_lit.saturating_mul(cfg.growth_factor)
    };
    if stats.peak < growth_threshold {
        return Decision::Skip {
            reason: format!(
                "peak={} < {}x current={} (variation within normal bounds)",
                stats.peak, cfg.growth_factor, current_lit
            ),
        };
    }
    // Proposed cap: use p95 (stable upper bound), then round.
    let proposed = apply_rounding(stats.p95.max(1), cfg.rounding);

    let unit_tag = match stats.unit {
        Unit::Bytes => " bytes (needs manual divide by size_of::<T>)",
        Unit::Elements => "",
    };

    Decision::Patch {
        to: proposed,
        reason: format!(
            "peak={} p95={} count={} → {:?}{}",
            stats.peak, stats.p95, stats.count, cfg.rounding, unit_tag
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::SiteKey;
    use std::path::PathBuf;

    fn k() -> SiteKey {
        SiteKey {
            file: PathBuf::from("x.rs"),
            line: 1,
            col: 1,
        }
    }

    fn stats(peak: usize, p95: usize, count: u64, unit: Unit) -> SiteStats {
        SiteStats {
            key: k(),
            unit,
            peak,
            p50: p95,
            p95,
            count,
        }
    }

    // ── Rule 1: low count ────────────────────────────────────────────
    #[test]
    fn skips_low_count() {
        let d = propose_cap(&stats(64, 64, 5, Unit::Elements), &CapExpr::Zero);
        assert!(matches!(d, Decision::Skip { .. }));
        if let Decision::Skip { reason } = d {
            assert!(reason.contains("low frequency"));
        }
    }

    // ── Rule 2: phantom site ─────────────────────────────────────────
    #[test]
    fn skips_phantom_site() {
        let d = propose_cap(&stats(0, 0, 1000, Unit::Elements), &CapExpr::Zero);
        assert!(matches!(d, Decision::Skip { reason } if reason.contains("phantom")));
    }

    // ── Rule 3: dynamic ──────────────────────────────────────────────
    #[test]
    fn skips_dynamic_cap_expr() {
        let d = propose_cap(
            &stats(100, 80, 1000, Unit::Elements),
            &CapExpr::Dynamic("input.len()".into()),
        );
        assert!(matches!(d, Decision::Skip { reason } if reason.contains("dynamic")));
    }

    // ── Rule 4: current literal sufficient ───────────────────────────
    #[test]
    fn skips_when_current_literal_covers_peak() {
        let d = propose_cap(&stats(50, 50, 1000, Unit::Elements), &CapExpr::Literal(64));
        assert!(matches!(d, Decision::Skip { reason } if reason.contains("already covers")));
    }

    // ── Rule 5: Zero current, peak >= 4 → patch ──────────────────────
    #[test]
    fn patches_zero_current_when_peak_meaningful() {
        let d = propose_cap(&stats(60, 60, 1000, Unit::Elements), &CapExpr::Zero);
        if let Decision::Patch { to, .. } = d {
            // p95 = 60 → next_pow2 = 64
            assert_eq!(to, 64);
        } else {
            panic!("expected Patch, got {:?}", d);
        }
    }

    // ── Rule 5 negative: Zero current, peak < 4 → skip ───────────────
    #[test]
    fn skips_zero_current_when_peak_tiny() {
        let d = propose_cap(&stats(3, 3, 1000, Unit::Elements), &CapExpr::Zero);
        assert!(matches!(d, Decision::Skip { reason } if reason.contains("variation")));
    }

    // ── Rule 6: literal current, peak >= 4*current → patch ───────────
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

    // ── Rule 6 negative: peak < 4*current → skip ─────────────────────
    #[test]
    fn skips_when_within_growth_factor() {
        let d = propose_cap(&stats(20, 18, 1000, Unit::Elements), &CapExpr::Literal(8));
        assert!(matches!(d, Decision::Skip { reason } if reason.contains("variation")));
    }

    // ── Rounding ────────────────────────────────────────────────────
    #[test]
    fn next_pow2_rounding() {
        assert_eq!(apply_rounding(1, Rounding::NextPow2), 1);
        assert_eq!(apply_rounding(2, Rounding::NextPow2), 2);
        assert_eq!(apply_rounding(3, Rounding::NextPow2), 4);
        assert_eq!(apply_rounding(60, Rounding::NextPow2), 64);
        assert_eq!(apply_rounding(65, Rounding::NextPow2), 128);
    }

    #[test]
    fn round_to_8() {
        assert_eq!(apply_rounding(0, Rounding::To8), 0);
        assert_eq!(apply_rounding(1, Rounding::To8), 8);
        assert_eq!(apply_rounding(8, Rounding::To8), 8);
        assert_eq!(apply_rounding(9, Rounding::To8), 16);
        assert_eq!(apply_rounding(60, Rounding::To8), 64);
    }

    #[test]
    fn exact_rounding() {
        assert_eq!(apply_rounding(63, Rounding::Exact), 63);
        assert_eq!(apply_rounding(0, Rounding::Exact), 0);
    }

    // ── Unit annotation in reason ────────────────────────────────────
    #[test]
    fn bytes_unit_tags_reason() {
        let d = propose_cap(&stats(4096, 4096, 1000, Unit::Bytes), &CapExpr::Zero);
        if let Decision::Patch { reason, .. } = d {
            assert!(reason.contains("bytes"));
            assert!(reason.contains("size_of"));
        } else {
            panic!("expected Patch");
        }
    }
}
