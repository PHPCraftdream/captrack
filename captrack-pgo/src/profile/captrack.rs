//! captrack JSON dump profile backend.
//!
//! Parses the JSON produced by `captrack::dump_capacity_stats(...)` —
//! see the captrack crate's `src/dump.rs` for the canonical schema.
//!
//! Unlike the dhat backend, captrack reports ELEMENTS (`Unit::Elements`)
//! and carries the raw per-instance samples array, so true `p50`/`p95` are
//! computed via `captrack::SampleStats` rather than collapsed to peak.

use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::Deserialize;

use crate::model::{SiteKey, SiteStats, Unit};
use crate::profile::Profile;

#[derive(Debug, Deserialize)]
struct Dump {
    #[allow(dead_code)]
    version: u32,
    stats: Vec<Entry>,
}

#[derive(Debug, Deserialize)]
struct Entry {
    #[allow(dead_code)]
    name: String,
    file: PathBuf,
    line: u32,
    column: u32,
    creation_count: u64,
    samples: Vec<usize>,
}

pub struct CaptrackProfile {
    file: PathBuf,
}

impl CaptrackProfile {
    pub fn new(file: impl Into<PathBuf>) -> Self {
        Self { file: file.into() }
    }
}

impl Profile for CaptrackProfile {
    fn sites(&self) -> Result<Vec<SiteStats>> {
        let raw = std::fs::read_to_string(&self.file)
            .with_context(|| format!("read captrack dump {}", self.file.display()))?;
        let dump: Dump = serde_json::from_str(&raw)
            .with_context(|| format!("parse captrack dump {}", self.file.display()))?;
        Ok(dump.stats.into_iter().map(entry_to_site_stats).collect())
    }
}

fn entry_to_site_stats(e: Entry) -> SiteStats {
    let key = SiteKey {
        file: e.file,
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
        }
    } else {
        // Empty samples (instance never dropped before dump, or just born).
        // Record zeros — planner's rules will Skip via "peak == 0".
        SiteStats {
            key,
            unit: Unit::Elements,
            peak: 0,
            p50: 0,
            p95: 0,
            count: e.creation_count,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn parses_sample_fixture() {
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("fixtures")
            .join("captrack-dump.sample.json");
        let p = CaptrackProfile::new(fixture);
        let stats = p.sites().expect("parse must succeed");
        assert_eq!(stats.len(), 2);

        // First entry: known samples → real p50/p95
        let hot = stats
            .iter()
            .find(|s| s.key.file == *"crates/engine/src/write.rs")
            .expect("hot entry exists");
        assert_eq!(hot.unit, Unit::Elements);
        assert_eq!(hot.key.line, 142);
        assert_eq!(hot.key.col, 17);
        assert_eq!(hot.peak, 1024);
        assert_eq!(hot.count, 1234);
        // For sorted [16,32,32,64,64,64,128,128,256,1024]:
        // n=10, p50 = nearest-rank ceil(0.5*10)=5 → sorted[4] = 64
        // p95 = ceil(0.95*10)=10 → sorted[9] = 1024
        assert_eq!(hot.p50, 64);
        assert_eq!(hot.p95, 1024);

        // Second entry: empty samples — planner will see peak=0
        let cold = stats
            .iter()
            .find(|s| s.key.file == *"crates/engine/src/cold.rs")
            .expect("cold entry exists");
        assert_eq!(cold.peak, 0);
        assert_eq!(cold.count, 1);
    }
}
