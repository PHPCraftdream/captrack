//! Shared data types for captrack-pgo.
//!
//! `profile/dhat.rs` and `profile/captrack.rs` produce `Vec<SiteStats>`.
//! The new Dylint-based `apply` pipeline (orchestrated by `lint_apply.rs`)
//! consumes those stats to set the `CAPTRACK_PGO_PROFILE` environment variable
//! that the plugin reads during compilation.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Unique identifier for an allocation call-site: source location.
///
/// Two sites at the same `(file, line, col)` are considered the same — this
/// is the contract that lets the profile loader match against plugin-detected sites.
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
