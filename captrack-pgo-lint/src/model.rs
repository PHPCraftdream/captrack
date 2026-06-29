//! Subset of captrack-pgo types needed by the Dylint plugin.
//!
//! `SiteKey`, `SiteStats`, `Unit`, `SitePolicy`, `CapFrom`, and `CapRound` are
//! byte-identical to what `captrack-pgo` serialises (same field names, same enum
//! variant names) so profiles can be round-tripped between the two crates without
//! conversion.
//!
//! `CapExpr` is a self-contained copy of the same enum from
//! `captrack-pgo::model` — the plugin can't take a direct dependency on the
//! stable `captrack-pgo` crate because the plugin runs on nightly.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Unique identifier for an allocation call-site: source location.
///
/// `(file, line, col)` is the canonical triple that the captrack registry
/// records via `file!()`, `line!()`, `column!()` at macro expansion time.
/// Both `line` and `col` are **1-based** — matching the Rust built-in macros.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct SiteKey {
    pub file: PathBuf,
    pub line: u32,
    pub col: u32,
}

/// Whether the numeric fields in `SiteStats` are in bytes or in elements.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Unit {
    Bytes,
    Elements,
}

/// Aggregated allocation statistics for one source call-site.
///
/// ## Backward compatibility
///
/// Pre-M10 profile JSON (without `mean`, `p99`, or `policy` fields) deserialises
/// cleanly: the three new fields all carry `#[serde(default)]` and are typed as
/// `Option<_>`, so they arrive as `None` when absent.  M11 rule code handles the
/// `None` case by falling back: `mean.unwrap_or(peak as f64)`, `p99.unwrap_or(p95)`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SiteStats {
    pub key: SiteKey,
    pub unit: Unit,
    pub peak: usize,
    pub p50: usize,
    pub p95: usize,
    pub count: u64,

    /// Arithmetic mean of all capacity/length samples observed at this site.
    ///
    /// `None` when the profile backend cannot produce a true mean (e.g. dhat, which
    /// only aggregates internally).  M11 falls back to `peak as f64` when `None`.
    #[serde(default)]
    pub mean: Option<f64>,

    /// 99th-percentile capacity/length (nearest-rank).
    ///
    /// `None` when the profile backend does not supply a per-site distribution
    /// (e.g. dhat).  M11 falls back to `p95` when `None`.
    #[serde(default)]
    pub p99: Option<usize>,

    /// Per-site policy override.  When present, each non-`None` field overrides
    /// the corresponding global CLI default for this site only.  `None` means "use
    /// whatever the CLI flags say".
    #[serde(default)]
    pub policy: Option<SitePolicy>,
}

/// Per-site capacity decision policy, embedded in the profile JSON so individual
/// hot-path sites can diverge from the workspace-wide CLI defaults.
///
/// Every field is independently optional: a `SitePolicy { cap_from: Some(Max),
/// cap_mul: None, cap_round: None }` overrides only the source statistic; the
/// multiplier and rounding mode still come from the CLI defaults.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct SitePolicy {
    /// Which statistic to use as the raw capacity value before multiplying.
    #[serde(default)]
    pub cap_from: Option<CapFrom>,
    /// Multiply the chosen statistic by this factor before rounding.
    #[serde(default)]
    pub cap_mul: Option<f64>,
    /// Rounding mode applied after multiplication.
    #[serde(default)]
    pub cap_round: Option<CapRound>,
}

/// Which aggregate statistic is used as the base capacity value.
///
/// Formula: `cap = round_mode( cap_from_value × cap_mul )`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CapFrom {
    /// Use the observed maximum (never reallocates; may waste memory on outliers).
    Max,
    /// Use the arithmetic mean.  Only meaningful for captrack-backed profiles;
    /// dhat-loaded profiles set `mean = peak` — see `profile/dhat.rs`.
    Mean,
    /// Use the 50th percentile (median).
    Median,
    /// Use the 95th percentile.
    P95,
    /// Use the 99th percentile.
    P99,
}

/// Rounding mode applied to `cap_from_value × cap_mul`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CapRound {
    /// Round up to the next power of two (default; matches `Vec` doubling strategy).
    Pow2,
    /// Round up to the nearest multiple of 8.
    To8,
    /// No rounding; use the exact computed value (truncated to `usize`).
    Exact,
}

/// What the current source code says the capacity is at a matched call-site.
///
/// This is a self-contained copy of `captrack_pgo::model::CapExpr` so the
/// plugin does not need a direct dep on the stable `captrack-pgo` crate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CapExpr {
    /// `Vec::new()` / `BTreeMap::new()` — no capacity hint.
    Zero,
    /// `with_capacity(N)` with a compile-time integer literal N.
    Literal(usize),
    /// `with_capacity(expr)` where expr is not a plain integer literal (e.g.
    /// `input.len()`).  The patcher should skip such sites.
    Dynamic(String),
}

/// Constructor kind recognised at a matched call-site.
///
/// Used to decide which suggestion form to emit:
/// - `BTreeMap` / `BTreeSet` → no suggestion (they have no `with_capacity`).
/// - Hash-keyed types with `with_capacity_and_hasher` → preserve the hasher arg.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ctor {
    Vec,
    VecDeque,
    HashMap,
    HashSet,
    BTreeMap,
    BTreeSet,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_key() -> SiteKey {
        SiteKey {
            file: PathBuf::from("crates/foo/src/lib.rs"),
            line: 42,
            col: 17,
        }
    }

    #[test]
    fn site_stats_round_trip() {
        let s = SiteStats {
            key: sample_key(),
            unit: Unit::Elements,
            peak: 1024,
            p50: 64,
            p95: 256,
            count: 4231,
            mean: None,
            p99: None,
            policy: None,
        };
        let json = serde_json::to_string(&s).unwrap();
        let back: SiteStats = serde_json::from_str(&json).unwrap();
        assert_eq!(s, back);
    }

    #[test]
    fn site_stats_round_trip_with_new_fields() {
        let s = SiteStats {
            key: sample_key(),
            unit: Unit::Elements,
            peak: 1024,
            p50: 64,
            p95: 256,
            count: 100,
            mean: Some(96.4),
            p99: Some(512),
            policy: Some(SitePolicy {
                cap_from: Some(CapFrom::Max),
                cap_mul: Some(2.0),
                cap_round: Some(CapRound::Pow2),
            }),
        };
        let json = serde_json::to_string(&s).unwrap();
        let back: SiteStats = serde_json::from_str(&json).unwrap();
        assert_eq!(s, back);
    }

    /// Pre-M10 JSON (no mean/p99/policy fields) must deserialise with None for new fields.
    #[test]
    fn backward_compat_pre_m10_json() {
        let json = r#"{
            "key": {"file": "crates/foo/src/lib.rs", "line": 42, "col": 17},
            "unit": "Elements",
            "peak": 1024,
            "p50": 64,
            "p95": 256,
            "count": 4231
        }"#;
        let s: SiteStats = serde_json::from_str(json).unwrap();
        assert_eq!(s.peak, 1024);
        assert_eq!(s.mean, None, "mean should be None for pre-M10 JSON");
        assert_eq!(s.p99, None, "p99 should be None for pre-M10 JSON");
        assert_eq!(s.policy, None, "policy should be None for pre-M10 JSON");
    }

    #[test]
    fn cap_from_serde_lowercase() {
        assert_eq!(
            serde_json::to_string(&CapFrom::Max).unwrap(),
            r#""max""#
        );
        assert_eq!(
            serde_json::from_str::<CapFrom>(r#""p99""#).unwrap(),
            CapFrom::P99
        );
        assert_eq!(
            serde_json::from_str::<CapFrom>(r#""mean""#).unwrap(),
            CapFrom::Mean
        );
        assert_eq!(
            serde_json::from_str::<CapFrom>(r#""median""#).unwrap(),
            CapFrom::Median
        );
        assert_eq!(
            serde_json::from_str::<CapFrom>(r#""p95""#).unwrap(),
            CapFrom::P95
        );
    }

    #[test]
    fn cap_round_serde_lowercase() {
        assert_eq!(
            serde_json::to_string(&CapRound::Pow2).unwrap(),
            r#""pow2""#
        );
        assert_eq!(
            serde_json::from_str::<CapRound>(r#""to8""#).unwrap(),
            CapRound::To8
        );
        assert_eq!(
            serde_json::from_str::<CapRound>(r#""exact""#).unwrap(),
            CapRound::Exact
        );
    }

    #[test]
    fn site_policy_partial_override() {
        // Only cap_from set; other fields None.
        let p = SitePolicy {
            cap_from: Some(CapFrom::P99),
            cap_mul: None,
            cap_round: None,
        };
        let json = serde_json::to_string(&p).unwrap();
        let back: SitePolicy = serde_json::from_str(&json).unwrap();
        assert_eq!(p, back);
        assert_eq!(back.cap_from, Some(CapFrom::P99));
        assert_eq!(back.cap_mul, None);
        assert_eq!(back.cap_round, None);
    }
}
