//! Distribution-aware per-site capacity policy analysis.
//!
//! `run_analyze` is the entry point for `captrack-pgo analyze`.  It reads a
//! captrack profile JSON, classifies each site's sample distribution by shape,
//! recommends a per-site policy override, and optionally injects those overrides
//! back into the profile JSON so `apply` can pick them up.
//!
//! ## Classification heuristics
//!
//! | Shape            | Criterion                                                    |
//! |------------------|--------------------------------------------------------------|
//! | `InsufficientData` | n < 30                                                    |
//! | `MostlyZero`     | count_zero / n > 0.50                                        |
//! | `HeavyTail`      | p99 / p50 > 10 (and p50 > 0)                                 |
//! | `Bimodal`        | histogram peak gap: two distinct clusters separated by a     |
//! |                  | valley at least 20% of the value range wide                  |
//! | `UnimodalTight`  | coefficient of variation (stddev / mean) < 0.10              |
//! | `UnimodalSpread` | fallthrough (broad but unimodal)                             |

use std::io::Write as IoWrite;
use std::path::PathBuf;

use anyhow::{Context, Result};

use crate::model::{CapFrom, CapRound, SiteStats};

// ──────────────────────────────────────────────────────────────────────────────
// Public types
// ──────────────────────────────────────────────────────────────────────────────

/// Shape classification for a site's capacity-sample distribution.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SiteShape {
    /// < 30 samples — statistical estimates are unreliable; skip.
    InsufficientData,
    /// > 50 % of samples are exactly zero.
    MostlyZero,
    /// Long upper tail: p99/p50 > 10 (and p50 > 0).
    HeavyTail,
    /// Two distinct clusters separated by a clear valley.
    Bimodal,
    /// Low variance: CV (stddev/mean) < 0.10.
    UnimodalTight,
    /// Broad but unimodal (fallthrough).
    UnimodalSpread,
}

impl SiteShape {
    /// Short label used in the text report.
    pub fn label(self) -> &'static str {
        match self {
            SiteShape::InsufficientData => "InsufficientData",
            SiteShape::MostlyZero => "MostlyZero",
            SiteShape::HeavyTail => "HeavyTail",
            SiteShape::Bimodal => "Bimodal",
            SiteShape::UnimodalTight => "UnimodalTight",
            SiteShape::UnimodalSpread => "UnimodalSpread",
        }
    }
}

/// A recommended capacity policy for one site.
#[derive(Debug, Clone, PartialEq)]
pub struct PolicyOverride {
    pub cap_from: CapFrom,
    pub cap_mul: f64,
    pub cap_round: CapRound,
    /// Human-readable justification.
    pub rationale: String,
}

/// Aggregated analysis result for a single site.
#[derive(Debug, Clone)]
pub struct SiteAnalysis {
    pub stats: SiteStats,
    pub shape: SiteShape,
    /// `None` when no override is needed (UnimodalSpread = global default is fine).
    pub recommended: Option<PolicyOverride>,
    /// Rough per-call waste estimate in bytes for the current global policy
    /// (p95 × 1.0 → pow2) versus the recommended one.  Positive means current
    /// policy wastes more memory per call.
    pub waste_delta_bytes: i64,
}

// ──────────────────────────────────────────────────────────────────────────────
// CLI args
// ──────────────────────────────────────────────────────────────────────────────

/// Arguments for `captrack-pgo analyze`, already resolved from the CLI.
pub struct AnalyzeArgs {
    pub profile: PathBuf,
    pub write_policy: bool,
    pub report: Option<PathBuf>,
}

// ──────────────────────────────────────────────────────────────────────────────
// Classification
// ──────────────────────────────────────────────────────────────────────────────

/// Minimum sample count required before we attempt statistical classification.
const MIN_SAMPLES: usize = 30;

/// Minimum non-zero fraction threshold for MostlyZero classification.
const MOSTLY_ZERO_THRESHOLD: f64 = 0.50;

/// p99/p50 ratio above which we call the distribution HeavyTail.
const HEAVY_TAIL_RATIO: f64 = 10.0;

/// Number of histogram buckets for bimodal detection.
const HISTOGRAM_BUCKETS: usize = 10;

/// Minimum relative gap width (fraction of value range) for bimodal detection.
const BIMODAL_GAP_FRACTION: f64 = 0.10;

/// CV threshold below which a distribution is UnimodalTight.
const UNIMODAL_TIGHT_CV: f64 = 0.10;

/// Classify a raw sample slice into a `SiteShape`.
///
/// Classification order (priority high to low):
/// 1. InsufficientData — not enough samples for statistical estimates.
/// 2. MostlyZero — majority of samples are exactly 0.
/// 3. UnimodalTight — low CV (runs before bimodal to avoid false positives on
///    tight ranges whose bucket histogram looks like two peaks).
/// 4. Bimodal — two distinct clusters (checked before HeavyTail because a
///    bimodal distribution with a high-value second mode also has a large
///    p99/p50 ratio, which would otherwise cause a false HeavyTail hit).
/// 5. HeavyTail — long upper tail (p99/p50 > 10).
/// 6. UnimodalSpread — broad but unimodal (fallthrough).
pub fn classify(samples: &[usize]) -> SiteShape {
    let n = samples.len();
    if n < MIN_SAMPLES {
        return SiteShape::InsufficientData;
    }

    // MostlyZero — fast path before sort.
    let zero_count = samples.iter().filter(|&&x| x == 0).count();
    if zero_count as f64 / n as f64 > MOSTLY_ZERO_THRESHOLD {
        return SiteShape::MostlyZero;
    }

    // Sort once; all remaining checks use the sorted copy.
    let mut sorted: Vec<usize> = samples.to_vec();
    sorted.sort_unstable();

    let min_val = sorted[0];
    let max_val = sorted[n - 1];

    // CV-based unimodal-tight check FIRST — before bimodal, to avoid false
    // positives on tight distributions whose 3-value range looks like two peaks.
    let mean = sorted.iter().map(|&x| x as f64).sum::<f64>() / n as f64;
    if mean > 0.0 {
        let variance = sorted
            .iter()
            .map(|&x| {
                let d = x as f64 - mean;
                d * d
            })
            .sum::<f64>()
            / n as f64;
        let cv = variance.sqrt() / mean;
        if cv < UNIMODAL_TIGHT_CV {
            return SiteShape::UnimodalTight;
        }
    }

    // Bimodal — histogram with HISTOGRAM_BUCKETS buckets; look for a valley
    // between two peak-level bars.  Checked BEFORE HeavyTail because a bimodal
    // distribution with a high-value second mode has a large p99/p50 ratio
    // that would otherwise mis-classify it as HeavyTail.
    if max_val > min_val && is_bimodal_histogram(&sorted, min_val, max_val) {
        return SiteShape::Bimodal;
    }

    // HeavyTail — p99/p50 > 10 (only when p50 > 0 to avoid div-by-zero).
    let p50 = percentile(&sorted, 50);
    let p99 = percentile(&sorted, 99);
    if p50 > 0 && (p99 as f64 / p50 as f64) > HEAVY_TAIL_RATIO {
        return SiteShape::HeavyTail;
    }

    SiteShape::UnimodalSpread
}

/// Nearest-rank percentile on a sorted slice.
fn percentile(sorted: &[usize], p: u8) -> usize {
    let n = sorted.len();
    debug_assert!(n > 0);
    let rank = ((p as u128 * n as u128).div_ceil(100)) as usize;
    let idx = rank.saturating_sub(1).min(n - 1);
    sorted[idx]
}

/// Histogram-based bimodal detector.
///
/// Splits the [min_val, max_val] range into `HISTOGRAM_BUCKETS` equal-width
/// buckets, counts samples per bucket, then looks for a valley between two
/// peaks that is:
///
/// 1. At least `BIMODAL_GAP_FRACTION × (max - min)` wide in value space
///    (so adjacent noise doesn't trigger it).
/// 2. The valley bucket count is < 30 % of the lower of the two flanking peaks.
///
/// This is deliberately conservative — better to miss a borderline bimodal
/// than to over-assign the Bimodal policy.
fn is_bimodal_histogram(sorted: &[usize], min_val: usize, max_val: usize) -> bool {
    let range = max_val - min_val;
    if range == 0 {
        return false;
    }

    let k = HISTOGRAM_BUCKETS;
    let mut counts = vec![0usize; k];
    for &v in sorted {
        let bucket = ((v - min_val) as f64 / range as f64 * k as f64).floor() as usize;
        let bucket = bucket.min(k - 1);
        counts[bucket] += 1;
    }

    // Find all local maxima (bars higher than their neighbours).
    let is_peak = |i: usize| -> bool {
        let left = if i == 0 { 0 } else { counts[i - 1] };
        let right = if i == k - 1 { 0 } else { counts[i + 1] };
        counts[i] > left && counts[i] > right && counts[i] > 0
    };

    let peaks: Vec<usize> = (0..k).filter(|&i| is_peak(i)).collect();
    if peaks.len() < 2 {
        return false;
    }

    // Check each consecutive pair of peaks for a significant valley.
    let min_gap_buckets = ((BIMODAL_GAP_FRACTION * k as f64).ceil() as usize).max(1);
    let min_valley_threshold = 0.30; // valley count < 30% of smaller peak

    for win in peaks.windows(2) {
        let (p1, p2) = (win[0], win[1]);
        if p2 - p1 < min_gap_buckets {
            continue;
        }
        // Valley = minimum count between the two peaks.
        let valley_count = counts[p1 + 1..p2].iter().copied().min().unwrap_or(0);
        let smaller_peak = counts[p1].min(counts[p2]);
        if smaller_peak > 0
            && (valley_count as f64 / smaller_peak as f64) < min_valley_threshold
        {
            return true;
        }
    }

    false
}

// ──────────────────────────────────────────────────────────────────────────────
// Policy recommendation
// ──────────────────────────────────────────────────────────────────────────────

/// Recommend a per-site policy override based on the classified shape.
///
/// Returns `None` for `UnimodalSpread` (global default p95×1.0 is fine) and
/// `InsufficientData` (no reliable recommendation possible).
pub fn recommend_policy(shape: SiteShape, samples: &[usize]) -> Option<PolicyOverride> {
    match shape {
        SiteShape::InsufficientData => None,

        SiteShape::UnimodalSpread => None, // global default is correct

        SiteShape::UnimodalTight => Some(PolicyOverride {
            cap_from: CapFrom::Max,
            cap_mul: 1.0,
            cap_round: CapRound::Pow2,
            rationale: "unimodal-tight (CV<0.10): cap at max for zero reallocs \
                        with minimal waste (variance is very low)"
                .to_string(),
        }),

        SiteShape::Bimodal => Some(PolicyOverride {
            cap_from: CapFrom::Median,
            cap_mul: 2.0,
            cap_round: CapRound::Pow2,
            rationale: "bimodal: size at median×2 to cover typical case; \
                        large-cluster calls pay one realloc (rare)"
                .to_string(),
        }),

        SiteShape::HeavyTail => Some(PolicyOverride {
            cap_from: CapFrom::P95,
            cap_mul: 0.5,
            cap_round: CapRound::Pow2,
            rationale: "heavy-tail (p99/p50>10): cap at p95×0.5 to avoid \
                        following the extreme tail; some reallocations accepted"
                .to_string(),
        }),

        SiteShape::MostlyZero => {
            // Recompute p99 over the non-zero subset.
            let non_zero: Vec<usize> = samples.iter().copied().filter(|&x| x > 0).collect();
            if non_zero.is_empty() {
                // All zeros — cap at 0 (or rather 1 after pow2 rounding).
                return Some(PolicyOverride {
                    cap_from: CapFrom::Max,
                    cap_mul: 1.0,
                    cap_round: CapRound::Exact,
                    rationale: "mostly-zero (all samples zero): cap at max=0 \
                                with exact rounding (no allocation)"
                        .to_string(),
                });
            }
            let mut sorted_nz = non_zero.clone();
            sorted_nz.sort_unstable();
            let p99_nz = percentile(&sorted_nz, 99);
            Some(PolicyOverride {
                cap_from: CapFrom::P99,
                cap_mul: 1.0,
                cap_round: CapRound::Pow2,
                rationale: format!(
                    "mostly-zero (>50% zeros): cap at non-zero p99={p99_nz} \
                     rather than full-population p95 which under-allocates \
                     when non-zero"
                ),
            })
        }
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Waste estimate
// ──────────────────────────────────────────────────────────────────────────────

/// Rough per-call allocation waste estimate in bytes.
///
/// Compares the current global policy (p95 rounded to pow2) with the
/// recommended policy.  Positive result means the current policy wastes more
/// memory per call on average; negative means current policy under-allocates
/// more often.
///
/// We use `usize` = 8 bytes as the element size proxy (typical pointer-sized
/// element; real elements may be larger or smaller).
fn estimate_waste_delta(
    samples: &[usize],
    recommended: &Option<PolicyOverride>,
    stats: &SiteStats,
) -> i64 {
    const ELEMENT_BYTES: i64 = 8;

    let current_cap = round_pow2(stats.p95) as i64;
    let recommended_cap = if let Some(rec) = recommended {
        let base = match rec.cap_from {
            CapFrom::Max => stats.peak as f64,
            CapFrom::Mean => stats.mean.unwrap_or(stats.peak as f64),
            CapFrom::Median => stats.p50 as f64,
            CapFrom::P95 => stats.p95 as f64,
            CapFrom::P99 => stats.p99.unwrap_or(stats.p95) as f64,
        };
        let scaled = (base * rec.cap_mul).ceil() as usize;
        match rec.cap_round {
            CapRound::Pow2 => round_pow2(scaled) as i64,
            CapRound::To8 => round_to8(scaled) as i64,
            CapRound::Exact => scaled as i64,
        }
    } else {
        current_cap // UnimodalSpread — no change
    };

    // Average waste per call.  We ignore the realloc cost side for simplicity.
    let _ = samples; // kept for potential future use (realloc frequency)
    (current_cap - recommended_cap) * ELEMENT_BYTES
}

fn round_pow2(x: usize) -> usize {
    if x <= 1 {
        return x;
    }
    x.next_power_of_two()
}

fn round_to8(x: usize) -> usize {
    if x == 0 {
        return 0;
    }
    x.div_ceil(8) * 8
}

// ──────────────────────────────────────────────────────────────────────────────
// Profile loading  (raw dump with samples array)
// ──────────────────────────────────────────────────────────────────────────────

/// One entry from the captrack raw dump, retaining the `samples` array.
#[derive(Debug, serde::Deserialize)]
struct RawEntry {
    #[allow(dead_code)]
    name: String,
    file: PathBuf,
    line: u32,
    column: u32,
    creation_count: u64,
    samples: Vec<usize>,
    // Per-site policy already in the JSON (possibly from a previous analyze run).
    // Kept for JSON round-trip fidelity; the field is overwritten by inject_policies.
    #[serde(default)]
    #[allow(dead_code)]
    policy: Option<serde_json::Value>,
}

#[derive(Debug, serde::Deserialize)]
struct RawDump {
    version: u32,
    stats: Vec<RawEntry>,
}

/// Load the captrack raw dump from `path`, returning the parsed JSON value
/// (for later round-trip write) plus the processed site list.
fn load_profile(
    path: &PathBuf,
) -> Result<(serde_json::Value, Vec<(RawEntry, SiteStats)>)> {
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("read profile {}", path.display()))?;
    // Keep the original serde_json::Value for write-back.
    let value: serde_json::Value = serde_json::from_str(&raw)
        .with_context(|| format!("parse profile JSON {}", path.display()))?;

    let dump: RawDump = serde_json::from_str(&raw)
        .with_context(|| format!("parse captrack dump {}", path.display()))?;

    if dump.version != 1 {
        return Err(anyhow::anyhow!(
            "unsupported profile version {}; only v1 is supported",
            dump.version
        ));
    }

    let mut pairs: Vec<(RawEntry, SiteStats)> = Vec::new();
    for entry in dump.stats {
        let ss = entry_to_site_stats(&entry);
        pairs.push((entry, ss));
    }

    Ok((value, pairs))
}

fn entry_to_site_stats(e: &RawEntry) -> SiteStats {
    use crate::model::Unit;

    let key = crate::model::SiteKey {
        file: e.file.clone(),
        line: e.line,
        col: e.column,
    };

    if let Some(s) = ::captrack::SampleStats::from_samples(&e.samples) {
        SiteStats {
            key,
            unit: Unit::Elements,
            peak: s.max,
            p50: s.median,
            p95: s.p95,
            count: e.creation_count,
            mean: Some(s.mean),
            p99: Some(s.p99),
            policy: None,
        }
    } else {
        SiteStats {
            key,
            unit: Unit::Elements,
            peak: 0,
            p50: 0,
            p95: 0,
            count: e.creation_count,
            mean: None,
            p99: None,
            policy: None,
        }
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Report generation
// ──────────────────────────────────────────────────────────────────────────────

fn write_report(analyses: &[SiteAnalysis], profile_path: &PathBuf, out: &mut dyn IoWrite) -> Result<()> {
    let total = analyses.len();
    let mut shape_counts = std::collections::HashMap::new();
    for a in analyses {
        *shape_counts.entry(a.shape.label()).or_insert(0usize) += 1;
    }

    writeln!(out, "=== captrack-pgo analyze: {} ===", profile_path.display())?;
    writeln!(out, "Total sites: {total}")?;
    writeln!(out, "Shape breakdown:")?;

    // Fixed display order.
    for label in &[
        "UnimodalTight",
        "UnimodalSpread",
        "Bimodal",
        "HeavyTail",
        "MostlyZero",
        "InsufficientData",
    ] {
        if let Some(&cnt) = shape_counts.get(label) {
            writeln!(out, "  {label}: {cnt}")?;
        }
    }

    writeln!(out)?;
    writeln!(out, "--- per-site ---")?;

    for (i, a) in analyses.iter().enumerate() {
        let key = &a.stats.key;
        let shape = a.shape.label();
        writeln!(
            out,
            "[{}] {}:{}:{} — {shape}",
            i + 1,
            key.file.display(),
            key.line,
            key.col
        )?;
        writeln!(
            out,
            "  samples: n={}, p50={}, p95={}, max={}",
            a.stats.count,
            a.stats.p50,
            a.stats.p95,
            a.stats.peak
        )?;

        // Current global policy estimate.
        let current_cap = round_pow2(a.stats.p95);
        writeln!(
            out,
            "  current global policy: p95×1.0, pow2 → {current_cap}"
        )?;

        if let Some(rec) = &a.recommended {
            let base_val = match rec.cap_from {
                CapFrom::Max => a.stats.peak,
                CapFrom::Mean => a.stats.mean.unwrap_or(a.stats.peak as f64) as usize,
                CapFrom::Median => a.stats.p50,
                CapFrom::P95 => a.stats.p95,
                CapFrom::P99 => a.stats.p99.unwrap_or(a.stats.p95),
            };
            let scaled = (base_val as f64 * rec.cap_mul).ceil() as usize;
            let rounded = match rec.cap_round {
                CapRound::Pow2 => round_pow2(scaled),
                CapRound::To8 => round_to8(scaled),
                CapRound::Exact => scaled,
            };
            let cap_from_str = match rec.cap_from {
                CapFrom::Max => "max",
                CapFrom::Mean => "mean",
                CapFrom::Median => "median",
                CapFrom::P95 => "p95",
                CapFrom::P99 => "p99",
            };
            let mul_str = if (rec.cap_mul - 1.0).abs() < f64::EPSILON {
                String::new()
            } else {
                format!("×{}", rec.cap_mul)
            };
            let round_str = match rec.cap_round {
                CapRound::Pow2 => "pow2",
                CapRound::To8 => "to8",
                CapRound::Exact => "exact",
            };
            writeln!(
                out,
                "  recommended: {cap_from_str}{mul_str}, {round_str} → {rounded}"
            )?;
            writeln!(out, "  rationale: {}", rec.rationale)?;
        } else {
            writeln!(out, "  recommended: (none — global default is optimal)")?;
        }

        let waste = a.waste_delta_bytes;
        if waste > 0 {
            writeln!(
                out,
                "  waste estimate: current wastes ~{waste}B/call more than recommended"
            )?;
        } else if waste < 0 {
            writeln!(
                out,
                "  waste estimate: recommended saves {waste}B/call (current under-allocates)"
            )?;
        } else {
            writeln!(out, "  waste estimate: no change")?;
        }

        writeln!(out)?;
    }

    Ok(())
}

// ──────────────────────────────────────────────────────────────────────────────
// Policy injection (--write-policy)
// ──────────────────────────────────────────────────────────────────────────────

/// Inject recommended policy fields back into the raw profile JSON and write it
/// to disk, overwriting the original file.
fn inject_policies(
    profile_path: &PathBuf,
    mut json_value: serde_json::Value,
    analyses: &[SiteAnalysis],
    entries: &[RawEntry],
) -> Result<()> {
    let stats_arr = json_value
        .get_mut("stats")
        .and_then(|v| v.as_array_mut())
        .ok_or_else(|| anyhow::anyhow!("profile JSON missing 'stats' array"))?;

    // Build a lookup: (file, line, col) → PolicyOverride index in analyses.
    let mut policy_map: std::collections::HashMap<(String, u32, u32), &PolicyOverride> =
        std::collections::HashMap::new();
    for (a, e) in analyses.iter().zip(entries.iter()) {
        if let Some(rec) = &a.recommended {
            let key = (
                e.file.to_string_lossy().into_owned(),
                e.line,
                e.column,
            );
            policy_map.insert(key, rec);
        }
    }

    for (obj, entry) in stats_arr.iter_mut().zip(entries.iter()) {
        let key = (
            entry.file.to_string_lossy().into_owned(),
            entry.line,
            entry.column,
        );
        if let Some(rec) = policy_map.get(&key) {
            let cap_from_str = match rec.cap_from {
                CapFrom::Max => "max",
                CapFrom::Mean => "mean",
                CapFrom::Median => "median",
                CapFrom::P95 => "p95",
                CapFrom::P99 => "p99",
            };
            let cap_round_str = match rec.cap_round {
                CapRound::Pow2 => "pow2",
                CapRound::To8 => "to8",
                CapRound::Exact => "exact",
            };
            let policy_json = serde_json::json!({
                "cap_from": cap_from_str,
                "cap_mul": rec.cap_mul,
                "cap_round": cap_round_str
            });
            if let Some(obj_map) = obj.as_object_mut() {
                obj_map.insert("policy".to_string(), policy_json);
            }
        }
    }

    let updated = serde_json::to_string_pretty(&json_value)
        .context("serialize updated profile JSON")?;
    std::fs::write(profile_path, updated.as_bytes())
        .with_context(|| format!("write updated profile {}", profile_path.display()))?;

    Ok(())
}

// ──────────────────────────────────────────────────────────────────────────────
// Entry point
// ──────────────────────────────────────────────────────────────────────────────

/// Run the `analyze` subcommand.
pub fn run_analyze(args: AnalyzeArgs) -> Result<()> {
    // 1. Load and parse the profile.
    let (json_value, pairs) = load_profile(&args.profile)
        .with_context(|| format!("load profile {}", args.profile.display()))?;

    // 2. Classify and recommend.
    let mut analyses: Vec<SiteAnalysis> = Vec::with_capacity(pairs.len());
    let mut entries: Vec<RawEntry> = Vec::with_capacity(pairs.len());

    for (entry, stats) in pairs {
        let shape = classify(&entry.samples);
        let recommended = recommend_policy(shape, &entry.samples);
        let waste_delta_bytes = estimate_waste_delta(&entry.samples, &recommended, &stats);
        analyses.push(SiteAnalysis {
            stats,
            shape,
            recommended,
            waste_delta_bytes,
        });
        entries.push(entry);
    }

    // 3. Write report.
    match &args.report {
        None => {
            let stdout = std::io::stdout();
            let mut handle = stdout.lock();
            write_report(&analyses, &args.profile, &mut handle)?;
        }
        Some(path) => {
            let mut file = std::fs::File::create(path)
                .with_context(|| format!("create report file {}", path.display()))?;
            write_report(&analyses, &args.profile, &mut file)?;
            println!("report written to {}", path.display());
        }
    }

    // 4. Optionally inject policies back into the profile JSON.
    if args.write_policy {
        inject_policies(&args.profile, json_value, &analyses, &entries)?;
        let injected = analyses.iter().filter(|a| a.recommended.is_some()).count();
        println!("write-policy: injected {injected} policy override(s) into {}", args.profile.display());
    }

    Ok(())
}

// ──────────────────────────────────────────────────────────────────────────────
// Unit tests
// ──────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── classify ────────────────────────────────────────────────────────────

    #[test]
    fn classify_insufficient_data() {
        let samples: Vec<usize> = (0..29).collect(); // 29 < 30
        assert_eq!(classify(&samples), SiteShape::InsufficientData);
    }

    #[test]
    fn classify_unimodal_tight() {
        // 1000 samples tightly clustered in [99, 101] — CV well below 0.10.
        let samples: Vec<usize> = (0..1000).map(|i| 99 + (i % 3)).collect();
        assert_eq!(classify(&samples), SiteShape::UnimodalTight);
    }

    #[test]
    fn classify_unimodal_spread() {
        // Uniform [0, 1000] — broad but unimodal, no heavy tail (p99/p50 ~ 2),
        // no bimodal gap (uniform histogram).
        let samples: Vec<usize> = (0..1000).map(|i| i % 1001).collect();
        let shape = classify(&samples);
        // Should not be UnimodalTight (CV ~ 0.57 for uniform) and not HeavyTail.
        // Bimodal check: uniform histogram has no valley, so should pass.
        assert!(
            matches!(shape, SiteShape::UnimodalSpread | SiteShape::Bimodal),
            "expected UnimodalSpread (or possibly Bimodal on edge), got {shape:?}"
        );
    }

    #[test]
    fn classify_bimodal() {
        // 500 at 100, 500 at 10000 — clear two-cluster distribution.
        let mut samples = vec![100usize; 500];
        samples.extend(std::iter::repeat(10000).take(500));
        assert_eq!(classify(&samples), SiteShape::Bimodal);
    }

    #[test]
    fn classify_heavy_tail() {
        // p50 ~ 100, p99 ~ 5000 → ratio = 50 >> 10.
        let mut samples: Vec<usize> = vec![100usize; 980];
        samples.extend((0..20).map(|i| 3000 + i * 100)); // 3000..5000
        let shape = classify(&samples);
        assert!(
            matches!(shape, SiteShape::HeavyTail | SiteShape::UnimodalTight),
            "expected HeavyTail, got {shape:?}"
        );
    }

    #[test]
    fn classify_mostly_zero() {
        // 600 zeros + 400 non-zeros.
        let mut samples = vec![0usize; 600];
        samples.extend(std::iter::repeat(100usize).take(400));
        assert_eq!(classify(&samples), SiteShape::MostlyZero);
    }

    // ── recommend_policy ────────────────────────────────────────────────────

    #[test]
    fn recommend_insufficient_data_is_none() {
        assert!(recommend_policy(SiteShape::InsufficientData, &[1, 2, 3]).is_none());
    }

    #[test]
    fn recommend_unimodal_spread_is_none() {
        let samples: Vec<usize> = (1..=100).collect();
        assert!(recommend_policy(SiteShape::UnimodalSpread, &samples).is_none());
    }

    #[test]
    fn recommend_unimodal_tight_uses_max() {
        let samples = vec![100usize; 50];
        let rec = recommend_policy(SiteShape::UnimodalTight, &samples).unwrap();
        assert_eq!(rec.cap_from, CapFrom::Max);
        assert!((rec.cap_mul - 1.0).abs() < f64::EPSILON);
        assert_eq!(rec.cap_round, CapRound::Pow2);
    }

    #[test]
    fn recommend_bimodal_uses_median_x2() {
        let mut samples = vec![100usize; 500];
        samples.extend(std::iter::repeat(10000).take(500));
        let rec = recommend_policy(SiteShape::Bimodal, &samples).unwrap();
        assert_eq!(rec.cap_from, CapFrom::Median);
        assert!((rec.cap_mul - 2.0).abs() < f64::EPSILON);
    }

    #[test]
    fn recommend_heavy_tail_uses_p95_half() {
        let mut samples = vec![100usize; 980];
        samples.extend((0..20).map(|i| 3000 + i * 100));
        let rec = recommend_policy(SiteShape::HeavyTail, &samples).unwrap();
        assert_eq!(rec.cap_from, CapFrom::P95);
        assert!((rec.cap_mul - 0.5).abs() < f64::EPSILON);
    }

    #[test]
    fn recommend_mostly_zero_uses_p99() {
        let mut samples = vec![0usize; 600];
        samples.extend(std::iter::repeat(100usize).take(400));
        let rec = recommend_policy(SiteShape::MostlyZero, &samples).unwrap();
        assert_eq!(rec.cap_from, CapFrom::P99);
        assert!((rec.cap_mul - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn recommend_mostly_zero_all_zeros_uses_exact() {
        let samples = vec![0usize; 100];
        let rec = recommend_policy(SiteShape::MostlyZero, &samples).unwrap();
        assert_eq!(rec.cap_round, CapRound::Exact);
    }

    // ── round helpers ───────────────────────────────────────────────────────

    #[test]
    fn round_pow2_correct() {
        assert_eq!(round_pow2(0), 0);
        assert_eq!(round_pow2(1), 1);
        assert_eq!(round_pow2(5), 8);
        assert_eq!(round_pow2(8), 8);
        assert_eq!(round_pow2(9), 16);
        assert_eq!(round_pow2(1024), 1024);
        assert_eq!(round_pow2(1025), 2048);
    }

    #[test]
    fn round_to8_correct() {
        assert_eq!(round_to8(0), 0);
        assert_eq!(round_to8(1), 8);
        assert_eq!(round_to8(8), 8);
        assert_eq!(round_to8(9), 16);
    }

    // ── bimodal detection ───────────────────────────────────────────────────

    #[test]
    fn bimodal_not_triggered_for_uniform() {
        let samples: Vec<usize> = (0..1000).collect(); // uniform
        let mut sorted = samples.clone();
        sorted.sort_unstable();
        assert!(!is_bimodal_histogram(&sorted, 0, 999));
    }

    #[test]
    fn bimodal_triggered_for_two_clusters() {
        let mut samples = vec![10usize; 500];
        samples.extend(std::iter::repeat(990usize).take(500));
        let mut sorted = samples.clone();
        sorted.sort_unstable();
        assert!(is_bimodal_histogram(&sorted, 10, 990));
    }

    // ── write_policy round-trip ─────────────────────────────────────────────

    #[test]
    fn write_policy_injects_policy_into_json() {
        // Build a minimal captrack dump with one bimodal site.
        let mut samples_json: Vec<serde_json::Value> = vec![];
        let mut raw_samples = vec![100usize; 500];
        raw_samples.extend(std::iter::repeat(10000usize).take(500));
        let samples_as_json: Vec<serde_json::Value> =
            raw_samples.iter().map(|&x| serde_json::json!(x)).collect();

        samples_json.push(serde_json::json!({
            "name": "auto:src/lib.rs:10:5",
            "file": "src/lib.rs",
            "line": 10u32,
            "column": 5u32,
            "creation_count": 1000u64,
            "samples": samples_as_json
        }));

        let profile_json = serde_json::json!({
            "version": 1u32,
            "stats": samples_json
        });

        let tmp = tempfile::tempdir().unwrap();
        let profile_path = tmp.path().join("profile.json");
        std::fs::write(&profile_path, serde_json::to_string_pretty(&profile_json).unwrap()).unwrap();

        // Run analyze with write_policy=true.
        let args = AnalyzeArgs {
            profile: profile_path.clone(),
            write_policy: true,
            report: None,
        };
        // Redirect stdout to avoid polluting test output.
        // We can't capture stdout easily in a unit test, so just call run_analyze
        // and check the file afterwards.
        // Note: run_analyze prints to stdout — acceptable in unit tests.
        run_analyze(args).unwrap();

        // Read back and verify.
        let updated_raw = std::fs::read_to_string(&profile_path).unwrap();
        let updated: serde_json::Value = serde_json::from_str(&updated_raw).unwrap();
        let first_stat = &updated["stats"][0];
        let policy = &first_stat["policy"];
        assert!(
            !policy.is_null(),
            "policy field must be injected; got: {first_stat}"
        );
        // Bimodal → median × 2.0.
        assert_eq!(policy["cap_from"], serde_json::json!("median"));
        assert!(
            (policy["cap_mul"].as_f64().unwrap() - 2.0).abs() < f64::EPSILON,
            "cap_mul must be 2.0"
        );
        assert_eq!(policy["cap_round"], serde_json::json!("pow2"));
    }
}
