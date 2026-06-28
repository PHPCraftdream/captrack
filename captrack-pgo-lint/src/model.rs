//! Subset of captrack-pgo types needed by the Dylint plugin.
//!
//! Only `SiteKey`, `SiteStats`, and `Unit` are copied here — the planner
//! types (`Ctor`, `CapExpr`, `AllocSite`, `PatchEntry`, `PatchPlan`) belong
//! to M3 and are not needed for detection.
//!
//! The serde representation is byte-identical to what `captrack-pgo` serialises
//! (same field names, same enum variant names) so profiles can be round-tripped
//! between the two crates without conversion.

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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SiteStats {
    pub key: SiteKey,
    pub unit: Unit,
    pub peak: usize,
    pub p50: usize,
    pub p95: usize,
    pub count: u64,
}
