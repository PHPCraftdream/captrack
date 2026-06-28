//! Subset of captrack-pgo types needed by the Dylint plugin.
//!
//! `SiteKey`, `SiteStats`, and `Unit` are byte-identical to what
//! `captrack-pgo` serialises (same field names, same enum variant names) so
//! profiles can be round-tripped between the two crates without conversion.
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SiteStats {
    pub key: SiteKey,
    pub unit: Unit,
    pub peak: usize,
    pub p50: usize,
    pub p95: usize,
    pub count: u64,
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
