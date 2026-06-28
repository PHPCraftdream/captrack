//! dhat-heap.json profile backend.
//!
//! Parses the JSON produced by <https://docs.rs/dhat> (the `heap` mode dump
//! that the `Profiler` writes on Drop) and aggregates per-source-location
//! peak/count statistics suitable for the planner.
//!
//! ## Limitations
//!
//! dhat reports BYTES, not elements — `SiteStats::unit == Bytes`.  Callers
//! that need element counts must divide by `size_of::<T>()` at the site,
//! which dhat alone cannot know.  See `docs/CAPACITY-PGO-PLAN.md` Q2.
//!
//! dhat also lacks a per-allocation size distribution (only aggregates),
//! so `p50` and `p95` are populated with `peak` — documented at the parser
//! level so downstream rules can detect "no distribution" if they care.

// Functions and types here will be consumed by cli.rs / plan.rs in later
// steps.  Suppress dead-code lints until the wiring is complete.
#![allow(dead_code)]

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Deserialize;

use crate::model::{SiteKey, SiteStats, Unit};
use crate::profile::Profile;

/// One program-point in a dhat heap dump.
#[derive(Debug, Deserialize)]
struct Pp {
    /// max-bytes ever live for this program-point
    mb: u64,
    /// total blocks allocated (lifetime count)
    tbk: u64,
    /// frame-table indices, leaf-first
    fs: Vec<usize>,
}

#[derive(Debug, Deserialize)]
struct DhatFile {
    #[serde(rename = "dhatFileVersion")]
    dhat_file_version: u32,
    pps: Vec<Pp>,
    ftbl: Vec<String>,
}

/// dhat-heap.json backend.
///
/// Filters frames by whether the `(file:line:col)` path is a relative path
/// (dhat emits relative paths for user-workspace code and absolute paths
/// starting with `/rustc/` or the cargo registry for std/deps).  Frames
/// without parseable file info are skipped.
pub struct DhatProfile {
    file: PathBuf,
    /// Workspace root — used only for resolving absolute user-code paths that
    /// might appear in some toolchain configurations.  Relative paths in the
    /// dump are always treated as workspace-relative user code.
    workspace_root: PathBuf,
}

impl DhatProfile {
    pub fn new(file: impl Into<PathBuf>, workspace_root: impl Into<PathBuf>) -> Self {
        Self {
            file: file.into(),
            workspace_root: workspace_root.into(),
        }
    }
}

impl Profile for DhatProfile {
    fn sites(&self) -> Result<Vec<SiteStats>> {
        let raw = std::fs::read_to_string(&self.file)
            .with_context(|| format!("read dhat heap file {}", self.file.display()))?;
        let dump: DhatFile = serde_json::from_str(&raw)
            .with_context(|| format!("parse dhat heap file {}", self.file.display()))?;
        if dump.dhat_file_version != 2 {
            anyhow::bail!(
                "unsupported dhatFileVersion {}: only v2 is supported",
                dump.dhat_file_version
            );
        }
        aggregate_pps(&dump.pps, &dump.ftbl, &self.workspace_root)
    }
}

fn aggregate_pps(pps: &[Pp], ftbl: &[String], workspace_root: &Path) -> Result<Vec<SiteStats>> {
    use std::collections::HashMap;

    // Aggregate by user-code SiteKey: max(peak) and sum(count) when several
    // pps map to the same source location (e.g. via inlined callers).
    let mut agg: HashMap<SiteKey, (u64, u64)> = HashMap::new();
    for pp in pps {
        let Some(key) = user_top_frame(&pp.fs, ftbl, workspace_root) else {
            continue;
        };
        let entry = agg.entry(key).or_insert((0, 0));
        entry.0 = entry.0.max(pp.mb);
        entry.1 = entry.1.saturating_add(pp.tbk);
    }

    let mut out: Vec<SiteStats> = agg
        .into_iter()
        .map(|(key, (peak_bytes, count))| {
            let peak = peak_bytes as usize;
            SiteStats {
                key,
                unit: Unit::Bytes,
                peak,
                p50: peak, // dhat lacks distribution; collapse to peak
                p95: peak, // ditto — see module docs
                count,
            }
        })
        .collect();
    // Stable ordering helps tests and diffing reports.
    out.sort_by(|a, b| a.key.cmp(&b.key));
    Ok(out)
}

/// Returns true if the given path looks like a std/rustc/cargo-registry frame
/// that should be excluded from user-code attribution.
fn is_std_path(path: &Path) -> bool {
    let s = path.to_string_lossy();
    // Absolute rustc sysroot paths always contain one of these markers.
    s.contains("/rustc/")
        || s.contains("\\rustc\\")
        || s.contains("/.cargo/registry/")
        || s.contains("\\.cargo\\registry\\")
        || s.contains("/.cargo/git/")
        || s.contains("\\.cargo\\git\\")
        // Some toolchains embed library/ under the sysroot.
        || (path.is_absolute() && s.contains("/library/"))
        || (path.is_absolute() && s.contains("\\library\\"))
}

/// Walk frame indices from leaf upward; return the first one that is
/// identified as user-workspace code.
///
/// Identification rules (in priority order):
///
/// 1. If the parsed file path is **relative** — it is workspace user-code
///    (dhat emits relative paths only for crates built from the workspace).
/// 2. If the parsed file path is **absolute** and passes `is_std_path` —
///    it is std/deps; skip.
/// 3. If absolute and starts with `workspace_root` — it is user-code.
/// 4. Otherwise skip (unknown absolute path outside the workspace).
///
/// Frames without parseable `(file:line:col)` info (e.g. `[root]`, JIT) are
/// always skipped.
fn user_top_frame(fs: &[usize], ftbl: &[String], workspace_root: &Path) -> Option<SiteKey> {
    for &idx in fs {
        let Some(frame) = ftbl.get(idx) else { continue };
        let Some((file, line, col)) = parse_frame_location(frame) else {
            continue;
        };

        // Filter out std/rustc/cargo-registry frames regardless of whether the
        // path is absolute or relative.  On Windows, `/rustc/...` paths parsed
        // from a Linux-generated dhat dump are NOT absolute (no drive letter),
        // so we must inspect the string representation unconditionally.
        if is_std_path(&file) {
            continue;
        }

        if file.is_absolute() {
            // Absolute user-code path: accept only if under workspace_root.
            if file.starts_with(workspace_root) {
                return Some(SiteKey { file, line, col });
            }
        } else {
            // Relative path that is not a std path → workspace user-code.
            return Some(SiteKey { file, line, col });
        }
    }
    None
}

/// Extract `(file, line, col)` from a dhat frame string.
///
/// Frame format observed in dhat v2:
///   `"0xADDR: symbol::path (relative/path/to/file.rs:LINE:COL)"`
/// Some frames have no location parenthesis (`[root]`, JIT, etc.) — those
/// return `None`.
fn parse_frame_location(frame: &str) -> Option<(PathBuf, u32, u32)> {
    let open = frame.rfind('(')?;
    let close = frame.rfind(')')?;
    if close <= open + 1 {
        return None;
    }
    let inner = &frame[open + 1..close];
    // Split from the right by ':' twice to get COL then LINE; remainder is the
    // file path (which may itself contain ':' on Windows drive letters).
    let mut parts = inner.rsplitn(3, ':');
    let col_s = parts.next()?;
    let line_s = parts.next()?;
    let file_s = parts.next()?;
    let col: u32 = col_s.parse().ok()?;
    let line: u32 = line_s.parse().ok()?;
    if file_s.is_empty() {
        return None;
    }
    Some((PathBuf::from(file_s), line, col))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_frame_location_basic() {
        let f = "0xABC: my_crate::foo (crates/my-crate/src/lib.rs:42:17)";
        let (file, line, col) = parse_frame_location(f).unwrap();
        assert_eq!(file, PathBuf::from("crates/my-crate/src/lib.rs"));
        assert_eq!(line, 42);
        assert_eq!(col, 17);
    }

    #[test]
    fn parse_frame_location_no_parens_returns_none() {
        assert!(parse_frame_location("[root]").is_none());
        assert!(parse_frame_location("0xABC: weird symbol").is_none());
    }

    #[test]
    fn parse_frame_location_windows_abs_path() {
        // Windows drive letter contains ':', so rsplitn(3) must correctly
        // extract the file portion.
        let f = "0xABC: my_crate::bar (D:/dev/rust/myapp/src/main.rs:10:5)";
        let (file, line, col) = parse_frame_location(f).unwrap();
        assert_eq!(file, PathBuf::from("D:/dev/rust/myapp/src/main.rs"));
        assert_eq!(line, 10);
        assert_eq!(col, 5);
    }

    #[test]
    fn parses_sample_fixture() {
        // workspace_root doesn't matter here because the fixture uses relative
        // paths for user-code frames, which are always accepted.
        let workspace_root = std::env::current_dir().unwrap();
        let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("fixtures")
            .join("dhat-heap.sample.json");
        let raw = std::fs::read_to_string(&fixture).expect("fixture must exist");
        let dump: DhatFile = serde_json::from_str(&raw).unwrap();
        let stats = aggregate_pps(&dump.pps, &dump.ftbl, &workspace_root).unwrap();
        // Fixture has two pps mapping to the SAME user source location;
        // aggregation should produce ONE SiteStats with peak = max, count = sum.
        assert_eq!(stats.len(), 1, "expected 1 aggregated site, got: {stats:?}");
        let s = &stats[0];
        assert_eq!(s.unit, Unit::Bytes);
        assert_eq!(s.peak, 4096); // max(2048, 4096)
        assert_eq!(s.count, 30); // sum(10, 20)
                                 // p50/p95 collapse to peak when no distribution is available
        assert_eq!(s.p50, 4096);
        assert_eq!(s.p95, 4096);
    }

    #[test]
    fn unknown_version_errors() {
        let dump = serde_json::json!({
            "dhatFileVersion": 99,
            "pps": [],
            "ftbl": []
        });
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), dump.to_string()).unwrap();
        let p = DhatProfile::new(tmp.path(), std::env::current_dir().unwrap());
        let err = p.sites().unwrap_err();
        assert!(
            err.to_string().contains("dhatFileVersion"),
            "expected dhatFileVersion in error, got: {err}"
        );
    }

    #[test]
    fn std_frames_are_filtered_out() {
        // A dump where every frame is a std/alloc frame — expect zero sites.
        let pps = vec![Pp {
            mb: 1024,
            tbk: 5,
            fs: vec![0, 1],
        }];
        let ftbl = vec![
            "[root]".to_string(),
            "0xAA: alloc::alloc::alloc (/rustc/abc/library/alloc/src/alloc.rs:97:9)".to_string(),
        ];
        let stats = aggregate_pps(&pps, &ftbl, Path::new("/workspace")).unwrap();
        assert!(stats.is_empty(), "std frames should be filtered: {stats:?}");
    }
}
