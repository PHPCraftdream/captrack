//! Shared data types for captrack-pgo.
//!
//! All other modules (`profile`, `scan`, `plan`, `apply`, `undo`) consume
//! these.  Designed to be backend-agnostic — `SiteStats` carries whatever
//! aggregate the profile backend produced, and the patcher doesn't care
//! where the numbers came from.

// Public API types are declared here for use by sibling modules that do not
// exist yet (profile, scan, plan, apply, undo).  Suppress the false-positive
// dead-code lint until those modules are wired in.
#![allow(dead_code)]

use std::ops::Range;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Unique identifier for an allocation call-site: source location.
///
/// Two sites at the same `(file, line, col)` are considered the same — this
/// is the contract that lets the AST scanner match against profile data.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct SiteKey {
    pub file: PathBuf,
    pub line: u32,
    pub col: u32,
}

/// Whether `SiteStats::peak` (and the percentile fields) are in bytes or
/// in elements.
///
/// dhat-backed profiles produce byte counts (allocator has no T info);
/// captrack-backed profiles produce element counts (each Tracked* knows T).
/// Rules need this to decide whether to recommend a `with_capacity(N)`
/// directly (elements) or report `N bytes — divide by size_of::<T>` (bytes).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Unit {
    Bytes,
    Elements,
}

/// Aggregated allocation statistics for one source call-site, computed by a
/// profile backend.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SiteStats {
    pub key: SiteKey,
    pub unit: Unit,
    pub peak: usize,
    pub p50: usize,
    pub p95: usize,
    pub count: u64,
}

/// Constructor kind found at an allocation site by the AST scanner.
///
/// Used to decide which capacity-bearing replacement to emit (`Vec::new()` →
/// `Vec::with_capacity(N)` is straightforward; `with_capacity_and_hasher` is
/// preserved verbatim with just the cap argument replaced).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Ctor {
    Vec,
    VecDeque,
    HashMap,
    HashSet,
    BTreeMap,
    BTreeSet,
}

/// What the current source code says the capacity is at this site.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum CapExpr {
    /// `with_capacity(N)` with a literal integer N.
    Literal(usize),
    /// `Vec::new()` / `vec![]` / `BTreeMap::new()` — no capacity hint.
    Zero,
    /// `with_capacity(expr)` where expr is not a literal (e.g. `input.len()`).
    /// Preserves the source text verbatim for reporting; patcher should
    /// Skip such sites unless explicitly forced.
    Dynamic(String),
}

/// One allocation site discovered by the AST scanner.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AllocSite {
    pub key: SiteKey,
    pub ctor: Ctor,
    pub current_cap: CapExpr,
    /// Byte range within the source file that should be REPLACED by the
    /// patcher.  For `Vec::with_capacity(0)` this is the `0` token; for
    /// `Vec::new()` this is the entire `Vec::new()` call; for `vec![]` this
    /// is the entire `vec![]` invocation.  The replacement text is the
    /// full new constructor call when needed.
    pub span_bytes: Range<usize>,
}

/// One patch decision the planner made for a matched (site, stats) pair.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PatchEntry {
    pub key: SiteKey,
    pub ctor: Ctor,
    pub from: CapExpr,
    pub to: usize,
    pub span_bytes: Range<usize>,
    pub reason: String,
}

/// The full plan produced by `plan::build_plan`: what to patch and what was
/// looked at but deliberately skipped (with reason).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PatchPlan {
    pub entries: Vec<PatchEntry>,
    pub skipped: Vec<(SiteKey, String)>,
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
        };
        let json = serde_json::to_string(&s).unwrap();
        let back: SiteStats = serde_json::from_str(&json).unwrap();
        assert_eq!(s, back);
    }

    #[test]
    fn patch_plan_round_trip() {
        let plan = PatchPlan {
            entries: vec![PatchEntry {
                key: sample_key(),
                ctor: Ctor::Vec,
                from: CapExpr::Zero,
                to: 64,
                span_bytes: 100..115,
                reason: "next_pow2(p95)".to_string(),
            }],
            skipped: vec![(sample_key(), "low count".to_string())],
        };
        let json = serde_json::to_string(&plan).unwrap();
        let back: PatchPlan = serde_json::from_str(&json).unwrap();
        assert_eq!(plan, back);
    }

    #[test]
    fn cap_expr_dynamic_round_trip() {
        let c = CapExpr::Dynamic("input.len() * 2".to_string());
        let json = serde_json::to_string(&c).unwrap();
        let back: CapExpr = serde_json::from_str(&json).unwrap();
        assert_eq!(c, back);
    }

    #[test]
    fn site_key_is_ord_for_btreemap_use() {
        let mut keys = [
            SiteKey {
                file: PathBuf::from("b.rs"),
                line: 1,
                col: 1,
            },
            SiteKey {
                file: PathBuf::from("a.rs"),
                line: 1,
                col: 1,
            },
            SiteKey {
                file: PathBuf::from("a.rs"),
                line: 2,
                col: 1,
            },
        ];
        keys.sort();
        assert_eq!(keys[0].file, PathBuf::from("a.rs"));
        assert_eq!(keys[1].line, 2);
        assert_eq!(keys[2].file, PathBuf::from("b.rs"));
    }
}
