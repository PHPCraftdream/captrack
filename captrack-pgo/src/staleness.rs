//! Staleness guard between `instrument` and `apply`.
//!
//! `instrument` rewrites bare constructors into `TrackedX::with_capacity_named`
//! calls at specific `(file, line, column)` source locations.  `apply` later
//! reads a captrack profile keyed by those same locations and rewrites the
//! discovered constructors again (this time injecting capacity/hasher hints).
//! If the workspace sources change between the two steps (hand edits, a
//! rebase, a stray `cargo fmt`), the line/column offsets recorded in the
//! profile can silently point at the wrong code by the time `apply` runs.
//!
//! To catch this, `instrument` snapshots the SHA-256 of every `.rs` file in
//! the workspace right after it finishes and writes them to
//! `target/captrack-pgo/last-instrument-hashes.json`.  `apply` reads this
//! file (if present) and refuses to proceed when any tracked file's current
//! hash no longer matches, unless `--force` is passed.
//!
//! Absence of the hashes file is NOT an error — `apply` can be run standalone
//! against a profile produced without ever calling `instrument` through this
//! binary (e.g. a hand-rolled captrack integration), so the guard is purely
//! opportunistic.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

use crate::lint_apply::hex_sha256;
use crate::workspace;

/// One file's recorded hash at `instrument` time.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StalenessFileEntry {
    /// Absolute path to the Rust source file.
    pub path: PathBuf,
    /// SHA-256 of the file's contents immediately after `instrument` ran.
    pub sha256: String,
}

/// Manifest written by `instrument`, consulted by `apply`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StalenessManifest {
    /// Always 1 — bump if the schema changes.
    pub version: u32,
    /// Unix seconds (`SystemTime::now()` at write time).
    pub timestamp: u64,
    /// One entry per `.rs` file under the workspace at `instrument` time.
    pub files: Vec<StalenessFileEntry>,
}

/// Returns the staleness manifest path:
/// `<workspace>/target/captrack-pgo/last-instrument-hashes.json`.
///
/// Deliberately distinct from `last-instrument.json` (the `LintRunManifest`
/// written by `lint_instrument.rs`, which records before/after file contents
/// for `undo`) — this manifest only records post-instrument hashes for the
/// staleness check.
pub fn default_staleness_manifest_path(workspace_root: &Path) -> PathBuf {
    workspace_root
        .join("target")
        .join("captrack-pgo")
        .join("last-instrument-hashes.json")
}

/// Snapshot every `.rs` file in the workspace and write a `StalenessManifest`
/// to `default_staleness_manifest_path(workspace_root)`.
///
/// Called by `instrument` immediately after a successful `--fix` run.
pub fn write_staleness_snapshot(workspace_root: &Path) -> Result<PathBuf> {
    let mut files = Vec::new();
    for file in workspace::walk_rust_files(workspace_root) {
        let content = std::fs::read_to_string(&file)
            .with_context(|| format!("staleness snapshot read {}", file.display()))?;
        files.push(StalenessFileEntry {
            path: file,
            sha256: hex_sha256(content.as_bytes()),
        });
    }

    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    let manifest = StalenessManifest {
        version: 1,
        timestamp,
        files,
    };

    let manifest_path = default_staleness_manifest_path(workspace_root);
    if let Some(parent) = manifest_path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create dir {}", parent.display()))?;
    }
    let json = serde_json::to_string_pretty(&manifest).context("serialize staleness manifest")?;
    std::fs::write(&manifest_path, &json)
        .with_context(|| format!("write staleness manifest {}", manifest_path.display()))?;

    Ok(manifest_path)
}

/// Verify that every file recorded in the staleness manifest (if any) still
/// matches its recorded hash.
///
/// - If `default_staleness_manifest_path(workspace_root)` does not exist,
///   this is a no-op — `Ok(())` — since `apply` may legitimately run without
///   a prior `instrument` step recorded by this binary.
/// - If `force` is `true`, the check is skipped entirely (still `Ok(())`)
///   even when the manifest exists and files have changed.
/// - Otherwise, every recorded file is re-hashed; any mismatch (including a
///   file that has since been deleted) causes a `bail!` naming every stale
///   file and hinting at the fix.
pub fn check_staleness(workspace_root: &Path, force: bool) -> Result<()> {
    if force {
        return Ok(());
    }

    let manifest_path = default_staleness_manifest_path(workspace_root);
    if !manifest_path.is_file() {
        // No prior instrument snapshot recorded — nothing to check.
        return Ok(());
    }

    let raw = std::fs::read_to_string(&manifest_path)
        .with_context(|| format!("read staleness manifest {}", manifest_path.display()))?;
    let manifest: StalenessManifest = serde_json::from_str(&raw)
        .with_context(|| format!("parse staleness manifest {}", manifest_path.display()))?;

    if manifest.version != 1 {
        bail!(
            "unsupported staleness manifest version {} at {}: only v1 supported",
            manifest.version,
            manifest_path.display()
        );
    }

    let mut stale: Vec<String> = Vec::new();

    for entry in &manifest.files {
        match std::fs::read_to_string(&entry.path) {
            Ok(content) => {
                let cur = hex_sha256(content.as_bytes());
                if cur != entry.sha256 {
                    stale.push(format!("{} (modified)", entry.path.display()));
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                stale.push(format!("{} (deleted)", entry.path.display()));
            }
            Err(e) => {
                return Err(e).with_context(|| {
                    format!("staleness check: read {}", entry.path.display())
                });
            }
        }
    }

    if !stale.is_empty() {
        bail!(
            "source files changed since `instrument` ran; the profile's (file, line, column) \
             sites may no longer point at the intended code:\n  {}\n\
             hint: re-run `instrument` (or `measure`) to refresh the snapshot, or pass --force \
             to override this check.",
            stale.join("\n  ")
        );
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_snapshot_then_check_passes_when_unchanged() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::write(root.join("Cargo.toml"), "[workspace]\nmembers=[]\n").unwrap();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src").join("lib.rs"), "fn a() {}\n").unwrap();

        let manifest_path = write_staleness_snapshot(root).unwrap();
        assert!(manifest_path.is_file());

        check_staleness(root, false).expect("unchanged files must pass the staleness check");
    }

    #[test]
    fn check_fails_when_file_modified_after_snapshot() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::write(root.join("Cargo.toml"), "[workspace]\nmembers=[]\n").unwrap();
        std::fs::create_dir_all(root.join("src")).unwrap();
        let lib_rs = root.join("src").join("lib.rs");
        std::fs::write(&lib_rs, "fn a() {}\n").unwrap();

        write_staleness_snapshot(root).unwrap();

        // Mutate the file after the snapshot.
        std::fs::write(&lib_rs, "fn a() { /* changed */ }\n").unwrap();

        let err = check_staleness(root, false).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("lib.rs"), "expected file name in error; got: {msg}");
        assert!(msg.contains("--force"), "expected --force hint in error; got: {msg}");
    }

    #[test]
    fn check_force_skips_even_when_modified() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::write(root.join("Cargo.toml"), "[workspace]\nmembers=[]\n").unwrap();
        std::fs::create_dir_all(root.join("src")).unwrap();
        let lib_rs = root.join("src").join("lib.rs");
        std::fs::write(&lib_rs, "fn a() {}\n").unwrap();

        write_staleness_snapshot(root).unwrap();
        std::fs::write(&lib_rs, "fn a() { /* changed */ }\n").unwrap();

        check_staleness(root, true).expect("--force must bypass the staleness check");
    }

    #[test]
    fn check_is_noop_when_manifest_absent() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::write(root.join("Cargo.toml"), "[workspace]\nmembers=[]\n").unwrap();
        // No staleness manifest written — must be a no-op.
        check_staleness(root, false).expect("absent manifest must not error");
    }

    #[test]
    fn check_fails_when_file_deleted_after_snapshot() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::write(root.join("Cargo.toml"), "[workspace]\nmembers=[]\n").unwrap();
        std::fs::create_dir_all(root.join("src")).unwrap();
        let lib_rs = root.join("src").join("lib.rs");
        std::fs::write(&lib_rs, "fn a() {}\n").unwrap();

        write_staleness_snapshot(root).unwrap();
        std::fs::remove_file(&lib_rs).unwrap();

        let err = check_staleness(root, false).unwrap_err();
        assert!(err.to_string().contains("deleted"));
    }

    #[test]
    fn default_staleness_manifest_path_is_distinct_from_instrument_manifest() {
        let p = default_staleness_manifest_path(Path::new("/ws"));
        assert!(p.to_string_lossy().contains("last-instrument-hashes.json"));
        // Must NOT collide with the LintRunManifest path.
        assert_ne!(
            p,
            crate::lint_apply::default_instrument_manifest_path(Path::new("/ws"))
        );
    }
}
