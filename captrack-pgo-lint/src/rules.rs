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
//! 5. `current::Zero && peak >= 4`              → Patch(round(policy_value))
//! 6. `current::Literal(c > 0) && peak >= 4*c` → Patch(round(policy_value))
//! 7. otherwise                                 → Skip "variation within normal bounds"
//!
//! Rounding: `next_pow2` by default (controlled by `PolicyDefaults.cap_round`).
//! The source statistic defaults to `p95` (controlled by `PolicyDefaults.cap_from`).
//! A multiplier (default `1.0`) is applied before rounding.
//!
//! Per-site `SiteStats.policy` fields override the corresponding `PolicyDefaults`
//! for that one site.
//!
//! `Unit::Bytes` sites produce a `Patch` with a note that the caller must
//! divide by `size_of::<T>` — the plugin emits a warning-only in that case.
//!
//! These constants and behaviour are **identical** to those in
//! `captrack-pgo/src/rules.rs` so that running `captrack-pgo apply` and
//! `cargo dylint --fix` produce the same numerical results for the same
//! profile data.

use crate::model::{CapExpr, CapFrom, CapRound, SiteStats, Unit};

/// Global policy defaults threaded through `propose_cap`.
///
/// Constructed once in `lib.rs::check_call_site` from the OnceLock readers and
/// passed down so `propose_cap` stays a pure function with no I/O.
#[derive(Debug, Clone, Copy)]
pub struct PolicyDefaults {
    /// Which statistic is the base capacity value (default `P95`).
    pub cap_from: CapFrom,
    /// Multiplier applied before rounding (default `1.0`).
    pub cap_mul: f64,
    /// Rounding mode (default `Pow2`).
    pub cap_round: CapRound,
}

impl Default for PolicyDefaults {
    fn default() -> Self {
        PolicyDefaults {
            cap_from: CapFrom::P95,
            cap_mul: 1.0,
            cap_round: CapRound::Pow2,
        }
    }
}

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
///
/// `globals` is the workspace-wide policy derived from CLI flags / env vars.
/// Per-site `stats.policy` fields override individual `globals` fields.
pub fn propose_cap(stats: &SiteStats, current: &CapExpr, globals: PolicyDefaults) -> Decision {
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

    // Resolve effective policy: per-site override beats globals.
    let cap_from = stats
        .policy
        .as_ref()
        .and_then(|p| p.cap_from)
        .unwrap_or(globals.cap_from);
    let cap_mul = stats
        .policy
        .as_ref()
        .and_then(|p| p.cap_mul)
        .unwrap_or(globals.cap_mul);
    let cap_round = stats
        .policy
        .as_ref()
        .and_then(|p| p.cap_round)
        .unwrap_or(globals.cap_round);

    // Validate cap_mul (guard against negative/NaN that would cause usize
    // overflow in `(source * cap_mul).ceil() as usize`).
    let cap_mul = if cap_mul <= 0.0 || cap_mul.is_nan() {
        // Caller should have validated and warned; fall back to 1.0 here as a
        // belt-and-suspenders guard.
        1.0_f64
    } else {
        cap_mul
    };

    // Compute source value.
    let source: f64 = match cap_from {
        CapFrom::Max => stats.peak as f64,
        CapFrom::Mean => stats.mean.unwrap_or(stats.peak as f64),
        CapFrom::Median => stats.p50 as f64,
        CapFrom::P95 => stats.p95 as f64,
        CapFrom::P99 => stats.p99.unwrap_or(stats.p95) as f64,
    };
    let scaled = (source * cap_mul).ceil() as usize;
    let proposed = match cap_round {
        CapRound::Pow2 => next_pow2(scaled.max(1)),
        CapRound::To8 => scaled.div_ceil(8).saturating_mul(8).max(8),
        CapRound::Exact => scaled,
    };

    // Build a human-readable formula description for the reason string.
    let from_label = match cap_from {
        CapFrom::Max => "max",
        CapFrom::Mean => "mean",
        CapFrom::Median => "median",
        CapFrom::P95 => "p95",
        CapFrom::P99 => "p99",
    };
    let round_label = match cap_round {
        CapRound::Pow2 => "NextPow2",
        CapRound::To8 => "To8",
        CapRound::Exact => "Exact",
    };
    let formula = if (cap_mul - 1.0).abs() < f64::EPSILON {
        format!("{round_label}({from_label})")
    } else {
        format!("{round_label}({from_label}×{cap_mul})")
    };

    // Byte-unit sites: the plugin cannot know size_of::<T> at lint time, so we
    // still produce a Patch (the number is in bytes) but flag it MaybeIncorrect
    // so the user knows to divide by element size.
    let (reason, applicability) = match stats.unit {
        Unit::Elements => (
            format!(
                "peak={} p95={} count={} → {}",
                stats.peak, stats.p95, stats.count, formula,
            ),
            Applicability::MachineApplicable,
        ),
        Unit::Bytes => (
            format!(
                "peak={} p95={} count={} → {} (bytes — divide by size_of::<T>)",
                stats.peak, stats.p95, stats.count, formula,
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
    use crate::model::{SiteKey, SitePolicy, Unit};
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
            p50: p95 / 2, // p50 = half of p95 for most tests
            p95,
            count,
            mean: None,
            p99: None,
            policy: None,
        }
    }

    /// Defaults used by all pre-M11 tests — must produce identical results to
    /// the old hard-coded `next_pow2(p95)` formula.
    fn defaults() -> PolicyDefaults {
        PolicyDefaults::default() // cap_from=P95, cap_mul=1.0, cap_round=Pow2
    }

    #[test]
    fn skips_low_count() {
        let d = propose_cap(
            &stats(64, 64, 5, Unit::Elements),
            &CapExpr::Zero,
            defaults(),
        );
        assert!(matches!(d, Decision::Skip { .. }));
        if let Decision::Skip { reason } = d {
            assert!(reason.contains("low frequency"));
        }
    }

    #[test]
    fn skips_phantom_site() {
        let d = propose_cap(
            &stats(0, 0, 1000, Unit::Elements),
            &CapExpr::Zero,
            defaults(),
        );
        assert!(matches!(d, Decision::Skip { reason } if reason.contains("phantom")));
    }

    #[test]
    fn skips_dynamic_cap_expr() {
        let d = propose_cap(
            &stats(100, 80, 1000, Unit::Elements),
            &CapExpr::Dynamic("input.len()".into()),
            defaults(),
        );
        assert!(matches!(d, Decision::Skip { reason } if reason.contains("dynamic")));
    }

    #[test]
    fn skips_when_current_literal_covers_peak() {
        let d = propose_cap(
            &stats(50, 50, 1000, Unit::Elements),
            &CapExpr::Literal(64),
            defaults(),
        );
        assert!(matches!(d, Decision::Skip { reason } if reason.contains("already covers")));
    }

    #[test]
    fn patches_zero_current_when_peak_meaningful() {
        let d = propose_cap(
            &stats(60, 60, 1000, Unit::Elements),
            &CapExpr::Zero,
            defaults(),
        );
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
        let d = propose_cap(
            &stats(3, 3, 1000, Unit::Elements),
            &CapExpr::Zero,
            defaults(),
        );
        assert!(matches!(d, Decision::Skip { reason } if reason.contains("variation")));
    }

    #[test]
    fn patches_when_peak_exceeds_growth_factor() {
        let d = propose_cap(
            &stats(100, 90, 1000, Unit::Elements),
            &CapExpr::Literal(8),
            defaults(),
        );
        if let Decision::Patch { to, .. } = d {
            // peak=100 >= 4*8=32 ✓; p95=90 → next_pow2 = 128
            assert_eq!(to, 128);
        } else {
            panic!("expected Patch, got {:?}", d);
        }
    }

    #[test]
    fn skips_when_within_growth_factor() {
        let d = propose_cap(
            &stats(20, 18, 1000, Unit::Elements),
            &CapExpr::Literal(8),
            defaults(),
        );
        assert!(matches!(d, Decision::Skip { reason } if reason.contains("variation")));
    }

    #[test]
    fn bytes_unit_produces_maybe_incorrect() {
        let d = propose_cap(
            &stats(4096, 4096, 1000, Unit::Bytes),
            &CapExpr::Zero,
            defaults(),
        );
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

    // ── M11 policy tests ─────────────────────────────────────────────────────

    /// `--cap-from max` uses the peak value, not p95.
    #[test]
    fn cap_from_max_uses_peak() {
        // peak=120, p95=90 — with cap_from=Max the source is 120 → next_pow2(120) = 128
        let globals = PolicyDefaults {
            cap_from: CapFrom::Max,
            cap_mul: 1.0,
            cap_round: CapRound::Pow2,
        };
        let s = stats(120, 90, 1000, Unit::Elements);
        let d = propose_cap(&s, &CapExpr::Zero, globals);
        if let Decision::Patch { to, .. } = d {
            assert_eq!(to, 128, "next_pow2(120) = 128");
        } else {
            panic!("expected Patch, got {:?}", d);
        }
    }

    /// `--cap-from median --cap-mul 2.0` uses p50 × 2.0, rounded to pow2.
    #[test]
    fn cap_from_median_with_mul_2() {
        // Build a stats with p50=50, p95=200, peak=800.
        let mut s = SiteStats {
            key: key(),
            unit: Unit::Elements,
            peak: 800,
            p50: 50,
            p95: 200,
            count: 1000,
            mean: None,
            p99: None,
            policy: None,
        };
        let globals = PolicyDefaults {
            cap_from: CapFrom::Median,
            cap_mul: 2.0,
            cap_round: CapRound::Pow2,
        };
        // source = p50=50, ×2 = 100, next_pow2(100) = 128
        let d = propose_cap(&s, &CapExpr::Zero, globals);
        if let Decision::Patch { to, .. } = d {
            assert_eq!(to, 128, "next_pow2(ceil(50×2.0)) = 128");
        } else {
            panic!("expected Patch, got {:?}", d);
        }

        // Also verify cap_round=Exact gives the unrounded value.
        let globals_exact = PolicyDefaults {
            cap_from: CapFrom::Median,
            cap_mul: 2.0,
            cap_round: CapRound::Exact,
        };
        s.p50 = 53; // 53 × 2 = 106.0, ceil = 106
        let d2 = propose_cap(&s, &CapExpr::Zero, globals_exact);
        if let Decision::Patch { to, .. } = d2 {
            assert_eq!(to, 106, "exact: ceil(53×2.0)=106");
        } else {
            panic!("expected Patch, got {:?}", d2);
        }
    }

    /// `--cap-from p99` with `p99 = None` falls back to `p95`.
    #[test]
    fn cap_from_p99_falls_back_to_p95_when_none() {
        let s = SiteStats {
            key: key(),
            unit: Unit::Elements,
            peak: 500,
            p50: 100,
            p95: 200,
            count: 1000,
            mean: None,
            p99: None, // absent → fall back
            policy: None,
        };
        let globals = PolicyDefaults {
            cap_from: CapFrom::P99,
            cap_mul: 1.0,
            cap_round: CapRound::Pow2,
        };
        // source = p95=200 (fallback), next_pow2(200) = 256
        let d = propose_cap(&s, &CapExpr::Zero, globals);
        if let Decision::Patch { to, .. } = d {
            assert_eq!(to, 256, "falls back to p95=200, next_pow2=256");
        } else {
            panic!("expected Patch, got {:?}", d);
        }
    }

    /// `--cap-from mean` with `mean = None` falls back to `peak`.
    #[test]
    fn cap_from_mean_falls_back_to_peak_when_none() {
        let s = SiteStats {
            key: key(),
            unit: Unit::Elements,
            peak: 64,
            p50: 20,
            p95: 40,
            count: 1000,
            mean: None, // absent → fall back to peak
            p99: None,
            policy: None,
        };
        let globals = PolicyDefaults {
            cap_from: CapFrom::Mean,
            cap_mul: 1.0,
            cap_round: CapRound::Pow2,
        };
        // source = peak=64, next_pow2(64) = 64
        let d = propose_cap(&s, &CapExpr::Zero, globals);
        if let Decision::Patch { to, .. } = d {
            assert_eq!(to, 64, "falls back to peak=64, next_pow2=64");
        } else {
            panic!("expected Patch, got {:?}", d);
        }
    }

    /// `--cap-round exact` disables rounding.
    #[test]
    fn cap_round_exact_no_rounding() {
        let s = SiteStats {
            key: key(),
            unit: Unit::Elements,
            peak: 300,
            p50: 100,
            p95: 200,
            count: 1000,
            mean: None,
            p99: Some(250),
            policy: None,
        };
        let globals = PolicyDefaults {
            cap_from: CapFrom::P99,
            cap_mul: 1.0,
            cap_round: CapRound::Exact,
        };
        // source=250, exact → proposed=250
        let d = propose_cap(&s, &CapExpr::Zero, globals);
        if let Decision::Patch { to, .. } = d {
            assert_eq!(to, 250, "exact: p99=250, no rounding");
        } else {
            panic!("expected Patch, got {:?}", d);
        }
    }

    /// Per-site `policy.cap_from = Max` overrides `globals.cap_from = P95`.
    #[test]
    fn per_site_policy_overrides_global_cap_from() {
        // Globals say P95, but the site has policy.cap_from=Max.
        // peak=200, p95=90 — with policy override, source=200 → next_pow2(200)=256.
        let s = SiteStats {
            key: key(),
            unit: Unit::Elements,
            peak: 200,
            p50: 40,
            p95: 90,
            count: 1000,
            mean: None,
            p99: None,
            policy: Some(SitePolicy {
                cap_from: Some(CapFrom::Max),
                cap_mul: None,
                cap_round: None,
            }),
        };
        let globals = PolicyDefaults {
            cap_from: CapFrom::P95,
            cap_mul: 1.0,
            cap_round: CapRound::Pow2,
        };
        let d = propose_cap(&s, &CapExpr::Zero, globals);
        if let Decision::Patch { to, .. } = d {
            assert_eq!(to, 256, "per-site cap_from=Max: next_pow2(200)=256");
        } else {
            panic!("expected Patch, got {:?}", d);
        }
    }

    /// Per-site `policy.cap_round = To8` overrides `globals.cap_round = Pow2`.
    #[test]
    fn per_site_policy_overrides_global_cap_round() {
        let s = SiteStats {
            key: key(),
            unit: Unit::Elements,
            peak: 100,
            p50: 40,
            p95: 60,
            count: 1000,
            mean: None,
            p99: None,
            policy: Some(SitePolicy {
                cap_from: None,
                cap_mul: None,
                cap_round: Some(CapRound::To8),
            }),
        };
        let globals = PolicyDefaults {
            cap_from: CapFrom::P95,
            cap_mul: 1.0,
            cap_round: CapRound::Pow2, // would be next_pow2(60)=64 without override
        };
        // With To8: ceil(60/8)*8 = 8*8 = 64 (60/8=7.5, ceil=8, 8*8=64)
        let d = propose_cap(&s, &CapExpr::Zero, globals);
        if let Decision::Patch { to, .. } = d {
            assert_eq!(to, 64, "To8: ceil(60/8)*8 = 64");
        } else {
            panic!("expected Patch, got {:?}", d);
        }
    }

    /// `cap_mul <= 0.0` is treated as 1.0 (guard against usize overflow).
    #[test]
    fn cap_mul_zero_degrades_to_one() {
        let s = stats(100, 60, 1000, Unit::Elements);
        let globals = PolicyDefaults {
            cap_from: CapFrom::P95,
            cap_mul: 0.0, // invalid — must fall back to 1.0
            cap_round: CapRound::Pow2,
        };
        // Falls back to cap_mul=1.0 → source=60 → next_pow2(60)=64
        let d = propose_cap(&s, &CapExpr::Zero, globals);
        if let Decision::Patch { to, .. } = d {
            assert_eq!(to, 64, "cap_mul=0.0 degrades to 1.0; next_pow2(60)=64");
        } else {
            panic!("expected Patch, got {:?}", d);
        }
    }

    /// `cap_round = To8` rounding — verify boundary cases.
    #[test]
    fn cap_round_to8_boundary() {
        let s = SiteStats {
            key: key(),
            unit: Unit::Elements,
            peak: 100,
            p50: 40,
            p95: 56,
            count: 1000,
            mean: None,
            p99: None,
            policy: None,
        };
        let globals = PolicyDefaults {
            cap_from: CapFrom::P95,
            cap_mul: 1.0,
            cap_round: CapRound::To8,
        };
        // p95=56, div_ceil(8)=7, *8=56, max(8)=56
        let d = propose_cap(&s, &CapExpr::Zero, globals);
        if let Decision::Patch { to, .. } = d {
            assert_eq!(to, 56, "To8 when already multiple of 8");
        } else {
            panic!("expected Patch, got {:?}", d);
        }
    }
}
